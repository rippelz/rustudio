//! `--profile-capture OUT [--seconds N]`: record N seconds (after the same
//! warm-up as `--bench`, from Play's start with `--play`) and write them to
//! OUT (`.rsprof` native, anything else Chrome trace JSON), then end the run.

use std::path::PathBuf;
use std::time::Instant;

use super::analysis;

pub struct CliCapture {
    pub out: PathBuf,
    pub seconds: f32,
    pub warmup: f32,
    /// Counts from Play's start.
    pub play: bool,
    pub done: bool,
    started: Option<Instant>,
    measuring: Option<Instant>,
    enabled: bool,
}

impl CliCapture {
    /// `instr`: scopes also count the thread's instructions (`--instr`, or
    /// RUSTUDIO_BENCH_INSTR / RUSTUDIO_PROFILE_INSTR set).
    pub fn new(out: PathBuf, seconds: f32, play: bool, instr: bool) -> CliCapture {
        let warmup = std::env::var("RUSTUDIO_BENCH_WARMUP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::mech::BENCH_WARMUP_S);
        let mut cfg = super::config();
        // keep the whole window, whatever the ring's usual length
        cfg.history_secs = cfg.history_secs.max(seconds + warmup + 2.0);
        // (RUSTUDIO_PROFILE_NO_GPU: no timestamp queries, to tell their cost apart)
        if std::env::var_os("RUSTUDIO_PROFILE_NO_GPU").is_some() {
            cfg.gpu_timing = false;
        }
        cfg.instr_mode = instr
            || std::env::var_os("RUSTUDIO_PROFILE_INSTR").is_some()
            || std::env::var_os("RUSTUDIO_BENCH_INSTR").is_some();
        super::set_config(cfg);
        // (recording through the warm-up too: Play's first frames are worth having
        // if the run is short, and it shows the profiler's own cost in --bench runs)
        super::enable();
        CliCapture { out, seconds, warmup, play, done: false, started: None, measuring: None, enabled: true }
    }

    /// Once per display frame; `ready` once there is something to measure
    /// (Play started, for a Play capture). True on the frame the file is written.
    pub fn tick(&mut self, ready: bool) -> bool {
        if self.done || !ready {
            return false;
        }
        let clock = crate::sim_clock::now();
        let started = *self.started.get_or_insert(clock);
        let Some(m) = self.measuring else {
            if clock.duration_since(started).as_secs_f32() >= self.warmup {
                super::clear();
                self.measuring = Some(clock);
            }
            return false;
        };
        if clock.duration_since(m).as_secs_f32() < self.seconds {
            return false;
        }
        self.finish();
        true
    }

    fn finish(&mut self) {
        self.done = true;
        super::pause();
        let cap = super::snapshot();
        let stats = analysis::frame_stats(cap.render_frames());
        match super::export::save(&cap, &self.out) {
            Ok(()) => println!(
                "[profile] {} frames, {:.1} fps, avg {:.2} ms, p95 {:.2} ms, max {:.2} ms, {} spans on {} lanes -> {}",
                stats.count,
                stats.fps,
                stats.avg_ms,
                stats.p95_ms,
                stats.max_ms,
                cap.span_count(),
                cap.tracks.len(),
                self.out.display()
            ),
            Err(e) => eprintln!("[profile] can't write {}: {e}", self.out.display()),
        }
        if self.enabled {
            self.enabled = false;
            super::resume();
            super::disable();
        }
    }
}

/// A plain-text report of a capture (`--profile-report FILE`).
pub fn report(cap: &super::Capture, top: usize) -> String {
    super::report::report_text(cap, top, 5)
}
