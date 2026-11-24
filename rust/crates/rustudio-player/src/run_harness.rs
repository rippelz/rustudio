//! Timing, input and capture shared by Studio and the standalone player.

use std::{path::PathBuf, time::Instant};
use rustudio_core::{log::Level, mech};
use rustudio_engine::runtime::STACK_MARK;
use crate::game::Game;

pub struct RunHarness {
    pub input: HarnessInput,
    /// `--shot-after S`: take the screenshot S seconds after start instead of
    /// after SCREENSHOT_FRAMES frames (Play sessions need time to load).
    pub shot_after: Option<f32>,
    /// `--log FILE`: write the Output lines there on exit.
    pub log_out: Option<PathBuf>,
    pub started: Instant,
    pub screenshot: Option<PathBuf>,
    pub screenshot_requested: bool,
    /// `--play`: start a Play session right after loading.
    pub start_play: bool,
    /// `--bench`: frame-time measurement.
    pub bench: Option<rustudio_core::bench::Bench>,
    /// `--profile-capture OUT`: a profiler capture of `--seconds` written to OUT.
    pub profile: Option<rustudio_core::profiler::cli::CliCapture>,
    /// `--camera x,y,z,yaw,pitch`: starting camera (degrees).
    pub camera: Option<[f32; 5]>,
    /// `--shots T1,T2,…`: a window and a viewport screenshot at each time
    /// (FILE-<t>s.png / FILE-<t>s.view.png), the Output log with each, then exit.
    pub shots: Vec<f32>,
    pub shot_next: usize,
    /// `[shot] PATH` lines for the shots written so far (stdout and --log).
    pub shot_lines: Vec<String>,
    /// `--from-play`: harness times count from the moment Play starts (as a
    /// Studio playtest's do), not from program start.
    pub from_play: bool,
    pub play_started: Option<Instant>,
    /// Play's simulated seconds since it started, when frames step a fixed dt
    /// (RUSTUDIO_FIXED_DT): then the harness clock is this one, so a run's shots
    /// and exit come at the same game time however loaded the machine is.
    pub play_game_time: Option<f32>,
    /// Frame times (ms) since Play started, for the fidelity report.
    pub frame_ms: Vec<f32>,
    pub last_frame: Option<Instant>,
    /// `--no-ui`: the 3D view (or the game) alone over the whole window; no
    /// editor UI is built (RustudioApp::no_ui_frame).
    pub no_ui: bool,
    /// `--no-render`: no GPU device on purpose (src/offscreen.rs); shots
    /// write no picture.
    pub no_render: bool,
    /// `--seconds N` without --bench in a --no-ui run: end the run at N
    /// seconds on the harness clock (from Play's start with --from-play).
    pub end_after: Option<f32>,
    /// `--fail-on-error`: exit 1 when the Output has an error line.
    pub fail_on_error: bool,
    /// `--result FILE`: one JSON for CI at the end of the run (finish_run).
    pub result: Option<PathBuf>,
    /// StudioTestService:EndTest's value once a script ended the run.
    pub end_value: Option<String>,
    /// Every frame's interval and work (ms), from the first frame in Edit
    /// state, from Play's start in Play (the --result frame statistics).
    pub run_frames: Vec<(f32, f32)>,
    pub run_last: Option<Instant>,
    pub run_playing: bool,
    pub finished: bool,
    /// Seconds the (Edit state) schedule was held by a harness wait for an
    /// event (`toolbox:await-ask`): actions, shots and the end come that
    /// much later, so what follows the wait runs relative to the event.
    pub held: f32,
}

#[derive(Default)]
pub struct HarnessInput {
    /// `--click X,Y@S` / `--key NAME@S`: input at S seconds (X,Y in the 3D
    /// view, pixels from its top-left).
    pub actions: Vec<(f32, HarnessAction)>,
    /// The pointer's last window position seen in the input (harness clicks
    /// while the game holds the mouse press there).
    pub pointer: Option<egui::Pos2>,
    /// `--shift S:HOLD`: Shift held (egui reports it as a modifier, not a key).
    pub shift: bool,
}

/// The process's exit code once a harness run finished (finish_run).
pub static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

impl RunHarness {
    /// Whether StudioTestService:EndTest ends the whole run here (a --no-ui
    /// run, or any run with --result) rather than only stopping Play.
    pub fn ends_on_end_test(&self) -> bool {
        self.no_ui || self.result.is_some()
    }

    /// Seconds on the harness clock (None before Play when --from-play).
    pub fn now(&self) -> Option<f32> {
        if self.from_play {
            if self.play_started.is_some() && self.play_game_time.is_some() {
                return self.play_game_time;
            }
            self.play_started.map(|t| rustudio_core::sim_clock::since(t).as_secs_f32())
        } else {
            Some(self.unheld_now() - self.held)
        }
    }

    /// Seconds since start, holds included.
    pub fn unheld_now(&self) -> f32 {
        rustudio_core::sim_clock::since(self.started).as_secs_f32()
    }
}

impl RunHarness {
    /// The end of a harness run (shots taken, --bench measured, --seconds
    /// reached, EndTest): writes --log and --result, sets the exit code
    /// (--fail-on-error) and closes. Without --result / --fail-on-error /
    /// --no-ui it only closes, as runs always did.
    pub fn finish_run(&mut self, game: &Game,
        ctx: &egui::Context,
        reason: &str,
        extras: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
    ) {
        if self.finished {
            return;
        }
        self.finished = true;
        let errors = game.log.iter().filter(|l| l.level == Level::Error).count();
        let warnings = game.log.iter().filter(|l| l.level == Level::Warn).count();
        let code = i32::from(self.fail_on_error && errors > 0);
        if code != 0 {
            eprintln!("--fail-on-error: {errors} error line(s) in the Output");
        }
        EXIT_CODE.store(code, std::sync::atomic::Ordering::Relaxed);
        if reason != "shots" && reason != "bench" {
            if let Some(out) = &self.log_out {
                let text: String = game
                    .log
                    .iter()
                    .map(|l| format!("{:?}: {}\n", l.level, l.text.replace(STACK_MARK, "")))
                    .chain(self.shot_lines.iter().map(|l| format!("{l}\n")))
                    .collect();
                if let Err(e) = std::fs::write(out, text) {
                    eprintln!("log: can't write {}: {e}", out.display());
                }
            }
        }
        if let Some(out) = self.result.clone() {
            let mut body = self.result_json(game, reason, errors, warnings, code);
            let process = body.as_object_mut().unwrap().shift_remove("process").unwrap();
            let output = body.as_object_mut().unwrap().shift_remove("output").unwrap();
            for (key, value) in extras {
                body[key] = value;
            }
            body["process"] = process;
            body["output"] = output;
            if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&body).unwrap_or_default()) {
                eprintln!("--result: can't write {}: {e}", out.display());
            }
        }
        rustudio_core::telemetry::flush();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn result_json(&self, game: &Game, reason: &str, errors: usize, warnings: usize, code: i32) -> serde_json::Value {
        let output: Vec<serde_json::Value> = game
            .log
            .iter()
            .map(|l| {
                let level = match l.level {
                    Level::Info => "info",
                    Level::Warn => "warning",
                    Level::Error => "error",
                };
                serde_json::json!({"level": level, "text": l.text.replace(STACK_MARK, "")})
            })
            .collect();
        let mut ms: Vec<f32> = self.run_frames.iter().map(|f| f.0).collect();
        let work: f32 = self.run_frames.iter().map(|f| f.1).sum::<f32>() / self.run_frames.len().max(1) as f32;
        ms.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f32| ms.get(((ms.len() as f32 * p) as usize).min(ms.len().saturating_sub(1))).copied().unwrap_or(0.0);
        let mean = ms.iter().sum::<f32>() / ms.len().max(1) as f32;
        let shots: Vec<String> = self
            .shot_lines
            .iter()
            .filter_map(|l| l.strip_prefix("[shot] ").map(str::to_string))
            .collect();
        let value = self
            .end_value
            .as_deref()
            .map(|v| serde_json::from_str(v).unwrap_or(serde_json::Value::Null));
        let (cpu_s, peak_rss_mb) = process_usage();
        serde_json::json!({
            "ended": reason,
            "exit_code": code,
            "state": if self.play_started.is_some() { "play" } else { "edit" },
            "no_ui": self.no_ui,
            "end_test_value": value,
            "seconds": self.now(),
            "errors": errors,
            "warnings": warnings,
            "frames": {
                "count": ms.len(),
                "mean_ms": mean,
                "p50_ms": pct(0.5),
                "p99_ms": pct(0.99),
                "max_ms": ms.last().copied().unwrap_or(0.0),
                "mean_work_ms": work,
                "fixed_dt": game.fixed_dt,
            },
            "shots": shots,
            "process": {"cpu_s": cpu_s, "peak_rss_mb": peak_rss_mb, "rss_mb": current_rss_mb()},
            "output": output,
        })
    }
}

/// This process's resident memory now (MB; NaN where not supported).
fn current_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let pages = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<f64>().ok());
        // SAFETY: sysconf only reads a system constant.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as f64;
        pages.map_or(f64::NAN, |p| p * page / (1024.0 * 1024.0))
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a zeroed counters struct with its size set, as the API asks.
        let mut m: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) } != 0 {
            m.WorkingSetSize as f64 / (1024.0 * 1024.0)
        } else {
            f64::NAN
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        f64::NAN
    }
}

/// This process's CPU time (user + system, s) and peak resident memory (MB).
fn process_usage() -> (f64, f64) {
    #[cfg(unix)]
    {
        // SAFETY: getrusage fills the struct we pass.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        let cpu = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 * 1e-6
            + ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 * 1e-6;
        // (Linux reports kilobytes)
        (cpu, ru.ru_maxrss as f64 / 1024.0)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (z, z, z, z);
        let secs = |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 * 1e-7;
        // SAFETY: the current process's pseudo-handle and four FILETIME out-pointers.
        let cpu = if unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) } != 0 {
            secs(kernel) + secs(user)
        } else {
            f64::NAN
        };
        // SAFETY: a zeroed counters struct with its size set, as the API asks.
        let mut m: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let peak = if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb) } != 0 {
            m.PeakWorkingSetSize as f64 / (1024.0 * 1024.0)
        } else {
            f64::NAN
        };
        (cpu, peak)
    }
    // Not supported here: NaN, null in --result.
    #[cfg(not(any(unix, windows)))]
    {
        (f64::NAN, f64::NAN)
    }
}

impl RunHarness {
    pub fn tick_before_shot(&mut self, game: &mut Game, last_work_ms: f32) -> Option<&'static str> {
        if self.play_started.is_some() {
            let now = Instant::now();
            if let Some(last) = self.last_frame.replace(now) {
                self.frame_ms.push((now - last).as_secs_f32() * 1000.0);
            }
        }
        // --result's frame statistics: Edit state from the first frame, Play
        // from its start (the first Play frame clears the Edit ones).
        if self.result.is_some() {
            let now = Instant::now();
            let playing = self.play_started.is_some();
            if playing != self.run_playing {
                self.run_playing = playing;
                self.run_frames.clear();
                self.run_last = None;
            }
            if let Some(last) = self.run_last.replace(now) {
                self.run_frames.push(((now - last).as_secs_f32() * 1000.0, last_work_ms));
            }
        }
        if self.finished {
            return None;
        }
        // A script ended the run (StudioTestService:EndTest), or --seconds.
        if self.end_value.is_some() {
            return Some("EndTest");
        }
        if let (Some(end), Some(now)) = (self.end_after, self.now()) {
            if now >= end {
                return Some("seconds");
            }
        }
        // Timed Luau (`--luau`), once Play runs.
        let t = self.now()?;
        if game.is_playing() {
            let due: Vec<(bool, String)> = self.input
                .actions
                .iter()
                .filter_map(|(at, a)| match a {
                    HarnessAction::Luau(server, code) if *at <= t => Some((*server, code.clone())),
                    _ => None,
                })
                .collect();
            if !due.is_empty() {
                self.input
                    .actions
                    .retain(|(at, a)| !(matches!(a, HarnessAction::Luau(..)) && *at <= t));
                for (server, code) in due {
                    let side = if server {
                        rustudio_engine::runtime::Side::Server
                    } else {
                        rustudio_engine::runtime::Side::Client
                    };
                    if let Some(e) = game.run_luau(side, &code) {
                        game.error(format!("--luau: {e}"));
                    }
                }
            }
        }
        None
    }

    pub fn tick_shots(
        &mut self,
        game: &Game,
        ctx: &egui::Context,
        extra_shots: impl FnOnce(&egui::ColorImage, &std::path::Path, &mut Vec<PathBuf>),
    ) -> Option<&'static str> {
        let Some(path) = self.screenshot.clone() else {
            return None;
        };
        let now = self.now();
        // `--shots`: the next timed shot, else the single one.
        let multi = self.shot_next < self.shots.len();
        let due = if multi {
            now.is_some_and(|t| t >= self.shots[self.shot_next])
        } else if !self.shots.is_empty() {
            false
        } else {
            match self.shot_after {
                Some(s) => now.is_some_and(|t| t >= s),
                None => game.frame_no >= mech::SCREENSHOT_FRAMES,
            }
        };
        let path = if multi {
            let t = self.shots[self.shot_next];
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            path.with_file_name(format!("{stem}-{t}s.png"))
        } else {
            path
        };
        if due && !self.screenshot_requested {
            self.screenshot_requested = true;
            game.write_asset_report();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } if self.screenshot_requested => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            if let Ok(hdr_path) = std::env::var("RUSTUDIO_DUMP_HDR") {
                if let Some((w,h,data)) = game.renderer.as_ref().zip(game.render_state.as_ref()).and_then(|(r,rs)| r.read_hdr(rs)) {
                    if std::fs::write(&hdr_path, data).is_ok() { eprintln!("HDR {w} {h} RGBA16Float {hdr_path}"); }
                }
            }
            let mut written = Vec::new();
            // (--no-render: an empty picture, nothing to write)
            if !(image.size == [0, 0] && self.no_render) {
                match save_png(&image, &path) {
                    Ok(()) => written.push(path.clone()),
                    Err(e) => eprintln!("screenshot failed: {e}"),
                }
            }
            // Just the 3D view too (FILE.view.png), to compare with Studio's viewport.
            // (--no-ui: the window is the view, one picture is enough.)
            if (self.shot_after.is_some() || multi) && game.viewport_rect.is_positive() && !self.no_ui {
                let ppp = ctx.pixels_per_point();
                let r = game.viewport_rect;
                let (x0, y0) = ((r.min.x * ppp) as usize, (r.min.y * ppp) as usize);
                let (w, h) = (
                    ((r.width() * ppp) as usize).min(image.size[0].saturating_sub(x0)),
                    ((r.height() * ppp) as usize).min(image.size[1].saturating_sub(y0)),
                );
                let pixels = (0..h)
                    .flat_map(|y| (0..w).map(move |x| (x, y)))
                    .map(|(x, y)| image.pixels[(y0 + y) * image.size[0] + x0 + x])
                    .collect();
                let view = egui::ColorImage::new([w, h], pixels);
                let view_path = path.with_extension("view.png");
                if save_png(&view, &view_path).is_ok() {
                    written.push(view_path);
                }
            }
            extra_shots(&image, &path, &mut written);
            // The written paths, for callers that don't rebuild the names.
            for p in written {
                let p = std::path::absolute(&p).unwrap_or(p);
                let line = format!("[shot] {}", p.display());
                println!("{line}");
                self.shot_lines.push(line);
            }
            if let Some(out) = &self.log_out {
                let text: String = game
                    .log
                    .iter()
                    .map(|l| format!("{:?}: {}\n", l.level, l.text.replace(STACK_MARK, "")))
                    .chain(self.shot_lines.iter().map(|l| format!("{l}\n")))
                    .collect();
                if let Err(e) = std::fs::write(out, text) {
                    eprintln!("log: can't write {}: {e}", out.display());
                }
            }
            rustudio_core::telemetry::flush();
            if multi {
                self.shot_next += 1;
                self.screenshot_requested = false;
                self.write_frame_times(&path);
                game.write_cursor_state(&path);
                if self.shot_next < self.shots.len() {
                    return None;
                }
            }
            return Some("shots");
        }
        if self.shot_after.is_none()
            && self.shots.is_empty()
            && game.frame_no > mech::SCREENSHOT_FRAMES + 600
        {
            eprintln!("screenshot timed out");
            std::process::exit(2);
        }        None
    }

    pub fn bench_frame(&mut self, game: &mut Game, ctx: &egui::Context, work_ms: f32, dt: f32) -> bool {
        let mut bench_done = self.bench_frame_inner(game, ctx, work_ms, dt);
        // --profile-capture: the run ends once it and any --bench are both done.
        if let Some(p) = self.profile.as_mut() {
            let ready = !p.play || self.play_started.is_some();
            let wrote = p.tick(ready);
            ctx.request_repaint();
            let bench_over = self.bench.as_ref().is_none_or(|b| b.done);
            bench_done = (bench_done || wrote) && p.done && bench_over;
        }
        bench_done
    }

    fn bench_frame_inner(&mut self, game: &mut Game, ctx: &egui::Context, work_ms: f32, dt: f32) -> bool {
        let mut bench_done = false;
        if let Some(b) = self.bench.as_mut() {
            // Edit-mode benchmark: turn the camera slowly so several views count.
            // (RUSTUDIO_BENCH_STILL=1: the camera stays put, an idle editor)
            if !b.play && std::env::var_os("RUSTUDIO_BENCH_STILL").is_none() {
                game.cam.yaw += rustudio_core::mech::BENCH_TURN_DEG_PER_S.to_radians() * dt;
                game.cam.focus = game.cam.pos + game.cam.forward() * (game.cam.focus - game.cam.pos).length();
            }
            // Play benches count from Play's start (a live join can take a
            // minute before Play), not from the window opening.
            let ready = !b.play || self.play_started.is_some();
            if ready && !b.done && b.frame(work_ms) {
                let mut report = b.report();
                if let Some(frames) = report["frames"].as_f64().filter(|frames| *frames > 0.0) {
                    if let Some(profile) = game.worker_lua_profile(frames) {
                        report["gameplay_lua"] = profile;
                    }
                    if let Some(scripts) = rustudio_engine::script_profiler::bench_report() {
                        report["scripts"] = scripts;
                    }
                }
                if let Some(r) = game.renderer.as_ref() {
                    report["render"] = serde_json::json!(format!("{:?}", r.stats));
                }
                if let Some((physics, awake)) = game.worker_physics_report().or_else(|| game.physics_report()) {
                    report["physics"] = physics;
                    report["awake_sample"] = awake;
                }
                let text = serde_json::to_string_pretty(&report).unwrap_or_default();
                if let Some(out) = &self.log_out {
                    let log: String = game
                        .log
                        .iter()
                        .map(|l| format!("{:?}: {}\n", l.level, l.text.replace(STACK_MARK, "")))
                        .collect();
                    let _ = std::fs::write(out, log);
                }
                if let Some(out) = &b.out {
                    if let Err(e) = std::fs::write(out, &text) {
                        eprintln!("bench: can't write {}: {e}", out.display());
                    }
                    let _ = std::fs::write(out.with_extension("frames.json"), b.frames_json().to_string());
                }
                println!("{text}");
                bench_done = true;
            }
            ctx.request_repaint();
        }
        bench_done
    }
}

impl RunHarness {
    /// FILE.frames.json beside a shot: frame times since Play started.
    fn write_frame_times(&self, shot: &std::path::Path) {
        let mut ms = self.frame_ms.clone();
        if ms.is_empty() {
            return;
        }
        let mean = ms.iter().sum::<f32>() / ms.len() as f32;
        ms.sort_by(|a, b| a.total_cmp(b));
        let p99 = ms[((ms.len() as f32 * 0.99) as usize).min(ms.len() - 1)];
        let body = serde_json::json!({
            "frames": ms.len(),
            "mean_ms": mean,
            "p99_ms": p99,
            "fps": 1000.0 / mean.max(1e-3),
        });
        let _ = std::fs::write(shot.with_extension("frames.json"), body.to_string());
    }
}


#[derive(Debug, Clone)]
pub enum HarnessAction {
    Click(f32, f32),
    Key(egui::Key, egui::Modifiers),
    /// A key held down (true) or let go (false): `--key [MOD+]NAME@S:HOLD`.
    KeyState(egui::Key, egui::Modifiers, bool),
    /// `--paste TEXT@S`: paste text into a focused TextBox.
    Paste(String),
    /// `--wheel NOTCHES@S` over the 3D view's centre (positive = forward,
    /// zoom in), or `--wheel NOTCHES@S:X,Y` at that view position.
    Wheel(f32, Option<(f32, f32)>),
    /// `--type "TEXT"@S`: typed text (egui Text events: a focused TextBox
    /// receives the characters).
    Text(String),
    /// Shift held down or let go.
    Shift(bool),
    /// The right mouse button over the 3D view's centre, down or up.
    RightButton(bool),
    /// Raw mouse motion (pixels), as a grabbed cursor reports it.
    MouseMotion(f32, f32),
    /// The cursor at a view position (`--ldrag`), and the left button there
    /// going down / up.
    PointerAt(f32, f32),
    LeftButton(f32, f32, bool),
    /// `--luau server|client FILE@S`: run Luau on that side of Play.
    Luau(bool, String),
    /// `--action NAME@S`: a Studio menu action by id (`panel_output` closes
    /// or opens the Output panel, resizing the 3D view mid-run).
    UiAction(String),
    /// `--ui-click X,Y@S`: the pointer at WINDOW position X,Y (any panel,
    /// not just the 3D view), then the left button down and up on later
    /// frames, as a real click arrives (egui widgets need the hover first).
    UiPointer(f32, f32),
    UiButton(f32, f32, bool),
}

pub fn parse_harness_key(spec: &str) -> Option<(egui::Key, egui::Modifiers)> {
    let mut mods = egui::Modifiers::NONE;
    let mut key_name = spec.trim();
    if spec.contains('+') {
        let parts: Vec<&str> = spec.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }
        key_name = parts[parts.len() - 1];
        for m in &parts[..parts.len() - 1] {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => {
                    mods.ctrl = true;
                    mods.command = true;
                }
                "cmd" | "command" => {
                    mods.command = true;
                    mods.mac_cmd = true;
                }
                "shift" => mods.shift = true,
                "alt" | "opt" | "option" => mods.alt = true,
                _ => return None,
            }
        }
    }
    let key = match key_name.to_ascii_lowercase().as_str() {
        "left" => egui::Key::ArrowLeft,
        "right" => egui::Key::ArrowRight,
        "up" => egui::Key::ArrowUp,
        "down" => egui::Key::ArrowDown,
        "return" | "enter" => egui::Key::Enter,
        "backspace" | "bs" => egui::Key::Backspace,
        "delete" | "del" => egui::Key::Delete,
        "esc" | "escape" => egui::Key::Escape,
        "home" => egui::Key::Home,
        "end" => egui::Key::End,
        "tab" => egui::Key::Tab,
        "space" => egui::Key::Space,
        "slash" | "/" => egui::Key::Slash,
        _ => egui::Key::from_name(key_name)?,
    };
    Some((key, mods))
}

impl HarnessAction {
    /// "X,Y@S" for clicks, "[MOD+]NAME@S" for a key tap, "[MOD+]NAME@S:HOLD" to hold
    /// it HOLD seconds.
    pub fn parse(s: &str, click: bool) -> Option<Vec<(f32, HarnessAction)>> {
        let (what, at) = s.split_once('@')?;
        if click {
            let at: f32 = at.trim().parse().ok()?;
            let (x, y) = what.split_once(',')?;
            return Some(vec![(
                at,
                HarnessAction::Click(x.trim().parse().ok()?, y.trim().parse().ok()?),
            )]);
        }
        let (key, mods) = parse_harness_key(what)?;
        match at.split_once(':') {
            Some((at, hold)) => {
                let (at, hold): (f32, f32) = (at.trim().parse().ok()?, hold.trim().parse().ok()?);
                Some(vec![
                    (at, HarnessAction::KeyState(key, mods, true)),
                    (at + hold, HarnessAction::KeyState(key, mods, false)),
                ])
            }
            None => Some(vec![(at.trim().parse().ok()?, HarnessAction::Key(key, mods))]),
        }
    }

    /// `--dblclick X,Y@S`: two clicks 50ms apart in the 3D view.
    pub fn parse_dblclick(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (what, at) = s.split_once('@')?;
        let at: f32 = at.trim().parse().ok()?;
        let (x, y) = what.split_once(',')?;
        let (x, y): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
        Some(vec![
            (at, HarnessAction::Click(x, y)),
            (at + 0.05, HarnessAction::Click(x, y)),
        ])
    }

    /// `--paste TEXT@S`: paste text at S seconds.
    pub fn parse_paste(s: &str) -> Option<(f32, HarnessAction)> {
        let (text, at) = s.rsplit_once('@')?;
        Some((at.trim().parse().ok()?, HarnessAction::Paste(text.to_string())))
    }

    /// `--ui-click X,Y@S` (window coordinates): move there at S, press at
    /// S + 0.05, let go at S + 0.1.
    pub fn parse_ui_click(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (xy, at) = s.split_once('@')?;
        let at: f32 = at.trim().parse().ok()?;
        let (x, y) = xy.split_once(',')?;
        let (x, y): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
        Some(vec![
            (at, HarnessAction::UiPointer(x, y)),
            (at + 0.05, HarnessAction::UiButton(x, y, true)),
            (at + 0.1, HarnessAction::UiButton(x, y, false)),
        ])
    }

    /// `--wheel NOTCHES@S` or `--wheel NOTCHES@S:X,Y` (view coordinates).
    pub fn parse_wheel(s: &str) -> Option<(f32, HarnessAction)> {
        let (n, at) = s.split_once('@')?;
        let n: f32 = n.trim().parse().ok()?;
        let (at, pos) = match at.split_once(':') {
            Some((at, xy)) => {
                let (x, y) = xy.split_once(',')?;
                (at, Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
            }
            None => (at, None),
        };
        Some((at.trim().parse().ok()?, HarnessAction::Wheel(n, pos)))
    }

    /// `--type TEXT@S` (the last '@' splits, so the text may hold one).
    pub fn parse_type(s: &str) -> Option<(f32, HarnessAction)> {
        let (text, at) = s.rsplit_once('@')?;
        Some((at.trim().parse().ok()?, HarnessAction::Text(text.to_string())))
    }

    /// `--ldrag X1,Y1>X2,Y2@S`: left-drag in the 3D view (view coordinates):
    /// press at S, move in 10 steps over 0.5 s, let go at S + 0.6.
    /// `--lhold X,Y@S:HOLD`: the left button pressed at X,Y at S, let go at S + HOLD
    /// (a game's press-and-hold, e.g. carrying a grabbed part).
    pub fn parse_lhold(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (xy, when) = s.split_once('@')?;
        let (at, hold) = when.split_once(':')?;
        let (at, hold): (f32, f32) = (at.trim().parse().ok()?, hold.trim().parse().ok()?);
        let (x, y) = xy.split_once(',')?;
        let (x, y): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
        Some(vec![
            (at - 0.2, HarnessAction::PointerAt(x, y)),
            (at, HarnessAction::LeftButton(x, y, true)),
            (at + hold, HarnessAction::LeftButton(x, y, false)),
        ])
    }

    pub fn parse_ldrag(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (what, at) = s.split_once('@')?;
        let at: f32 = at.trim().parse().ok()?;
        let (a, b) = what.split_once('>')?;
        let xy = |p: &str| -> Option<(f32, f32)> {
            let (x, y) = p.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        };
        let ((x1, y1), (x2, y2)) = (xy(a)?, xy(b)?);
        let mut v = vec![(at - 0.2, HarnessAction::PointerAt(x1, y1)), (at, HarnessAction::LeftButton(x1, y1, true))];
        for k in 1..=10 {
            let f = k as f32 / 10.0;
            v.push((at + 0.05 * k as f32, HarnessAction::PointerAt(x1 + (x2 - x1) * f, y1 + (y2 - y1) * f)));
        }
        v.push((at + 0.6, HarnessAction::LeftButton(x2, y2, false)));
        Some(v)
    }

    /// `--rhold S:HOLD`: the right button held from S for HOLD seconds, the
    /// mouse still (a game's hold-to-aim).
    pub fn parse_rhold(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (at, hold) = s.split_once(':')?;
        let (at, hold): (f32, f32) = (at.trim().parse().ok()?, hold.trim().parse().ok()?);
        Some(vec![
            (at, HarnessAction::RightButton(true)),
            (at + hold, HarnessAction::RightButton(false)),
        ])
    }

    /// `--rdrag DX,DY@S`: right-drag over the view: press at S, move DX,DY
    /// pixels in 10 steps over 0.5 s, let go at S + 0.6.
    pub fn parse_rdrag(s: &str) -> Option<Vec<(f32, HarnessAction)>> {
        let (what, at) = s.split_once('@')?;
        let at: f32 = at.trim().parse().ok()?;
        let (x, y) = what.split_once(',')?;
        let (dx, dy): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
        let mut v = vec![(at, HarnessAction::RightButton(true))];
        for k in 1..=10 {
            v.push((
                at + 0.05 * k as f32,
                HarnessAction::MouseMotion(dx / 10.0, dy / 10.0),
            ));
        }
        v.push((at + 0.6, HarnessAction::RightButton(false)));
        Some(v)
    }
}

impl HarnessInput {
    /// Timed harness input, injected like real events.
    pub fn inject(&mut self, view: egui::Rect, locked: bool, now: Option<f32>, raw: &mut egui::RawInput, mut ui_action: impl FnMut(String, &mut egui::RawInput)) {
        let Some(t) = now else { return };
        // No 3D view yet (the start page): clicks are window coordinates.
        let vp = if view.is_finite() {
            view
        } else {
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::ZERO)
        };
        // The game holds the mouse (MouseBehavior.LockCenter): a held cursor
        // never travels, so clicks land where it is held and pointer moves
        // are dropped. Moving it would read as a huge mouse
        // delta and fling a first-person camera (only --rdrag's raw motion
        // turns it then).
        // Where the real pointer is now: a held cursor's clicks happen there.
        for e in &raw.events {
            if let egui::Event::PointerMoved(p) = e {
                self.pointer = Some(*p);
            }
        }
        let held_at = self.pointer.unwrap_or(vp.center());
        let mut i = 0;
        while i < self.actions.len() {
            if self.actions[i].0 > t {
                i += 1;
                continue;
            }
            if matches!(self.actions[i].1, HarnessAction::Luau(..)) {
                i += 1;
                continue;
            }
            let (_, a) = self.actions.remove(i);
            match a {
                HarnessAction::Luau(..) => {}
                HarnessAction::Click(x, y) => {
                    let p = if locked { held_at } else { vp.min + egui::vec2(x, y) };
                    if !locked {
                        raw.events.push(egui::Event::PointerMoved(p));
                    }
                    for pressed in [true, false] {
                        raw.events.push(egui::Event::PointerButton {
                            pos: p,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                }
                HarnessAction::Key(key, modifiers) => {
                    for pressed in [true, false] {
                        raw.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed,
                            repeat: false,
                            modifiers,
                        });
                        // A real keyboard also sends the key's character in
                        // the same frame (winit text) when no command/ctrl/alt
                        // modifier is held; "/" opening chat must not type itself.
                        let sym = key.symbol_or_name();
                        if pressed
                            && !modifiers.command
                            && !modifiers.ctrl
                            && !modifiers.alt
                            && sym.len() == 1
                            && sym.as_bytes()[0].is_ascii_graphic()
                        {
                            let ch = if modifiers.shift {
                                sym.to_uppercase()
                            } else {
                                sym.to_lowercase()
                            };
                            raw.events.push(egui::Event::Text(ch));
                        }
                    }
                }
                HarnessAction::KeyState(key, modifiers, pressed) => {
                    raw.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
                HarnessAction::Paste(text) => raw.events.push(egui::Event::Paste(text)),
                HarnessAction::Wheel(n, at) => {
                    let p = at.map_or(vp.center(), |(x, y)| vp.min + egui::vec2(x, y));
                    raw.events.push(egui::Event::PointerMoved(p));
                    raw.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, n),
                        modifiers: egui::Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                    });
                }
                HarnessAction::Text(text) => raw.events.push(egui::Event::Text(text)),
                HarnessAction::Shift(on) => self.shift = on,
                HarnessAction::RightButton(pressed) => {
                    let p = if locked { held_at } else { vp.center() };
                    if !locked {
                        raw.events.push(egui::Event::PointerMoved(p));
                    }
                    raw.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                HarnessAction::PointerAt(x, y) => {
                    if !locked {
                        raw.events.push(egui::Event::PointerMoved(vp.min + egui::vec2(x, y)));
                    }
                }
                HarnessAction::LeftButton(x, y, pressed) => {
                    let p = if locked { held_at } else { vp.min + egui::vec2(x, y) };
                    if !locked {
                        raw.events.push(egui::Event::PointerMoved(p));
                    }
                    raw.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                HarnessAction::MouseMotion(dx, dy) => {
                    raw.events.push(egui::Event::MouseMoved(egui::vec2(dx, dy)));
                }
                HarnessAction::UiAction(name) => ui_action(name, raw),
                HarnessAction::UiPointer(x, y) => {
                    raw.events.push(egui::Event::PointerMoved(egui::pos2(x, y)));
                }
                HarnessAction::UiButton(x, y, pressed) => {
                    let p = egui::pos2(x, y);
                    raw.events.push(egui::Event::PointerMoved(p));
                    raw.events.push(egui::Event::PointerButton {
                        pos: p,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
        }
        self.update_pointer(raw);
    }

    pub fn update_pointer(&mut self, raw: &egui::RawInput) {
        for e in &raw.events {
            if let egui::Event::PointerMoved(p) = e {
                self.pointer = Some(*p);
            }
        }
    }
}

impl Game {
    pub(crate) fn physics_report(&mut self) -> Option<(serde_json::Value, serde_json::Value)> {
        if self.play.as_ref()?.presentation.is_some() {
            return None;
        }
        self.wait_for_play_physics();
        let p = self.play.as_ref()?;
        let (dynamic, fixed) = p.physics.counts();
        let physics = serde_json::json!({"assemblies": dynamic, "awake": p.physics.awake(), "fixed_colliders": fixed, "steps": p.physics.steps, "stages": p.physics.stage_times()});
        let sample: Vec<String> = p
            .physics
            .awake_sample(25)
            .into_iter()
            .map(|(r, pos, vel, n)| {
                let mut path = Vec::new();
                let mut cur = r;
                while let Some(i) = self.doc.dom.get_by_ref(cur) {
                    path.push(i.name.clone());
                    cur = i.parent();
                }
                path.reverse();
                let kids: Vec<String> = self
                    .doc
                    .dom
                    .get_by_ref(r)
                    .map(|i| {
                        i.children()
                            .iter()
                            .filter_map(|c| self.doc.class_of(*c).map(|k| k.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let size =
                    rustudio_render::scene::vec3_of(self.doc.prop(r, "Size")).unwrap_or_default();
                format!(
                    "{} n={n} size={size:.1} pos={pos:.1} vel={vel:.1} kids={kids:?}",
                    path.join(".")
                )
            })
            .collect();
        Some((physics, serde_json::json!(sample)))
    }
}

pub fn save_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [w, h] = image.size;
    let mut bytes = Vec::with_capacity(w * h * 4);
    for c in &image.pixels {
        bytes.extend_from_slice(&[c.r(), c.g(), c.b(), 255]);
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    image::save_buffer(
        path,
        &bytes,
        w as u32,
        h as u32,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| e.to_string())
}

/// RUSTUDIO_FRAME_GRID=HZ (tests only): a frame's time snapped to whole
/// periods of a HZ display, the remainder carried, as a vsynced monitor
/// hands frames out. Headless runs pace frames with a timer that drifts
/// against the 240 Hz world clock; the drift changes on which tick a frame
/// starts (and with it where the Humanoid's 2-tick solves fall) at random
/// from run to run. Without the variable: `dt` as it is.
pub fn frame_grid(dt: f32) -> f32 {
    use std::sync::{Mutex, OnceLock};
    static HZ: OnceLock<Option<f32>> = OnceLock::new();
    static OWED: Mutex<f32> = Mutex::new(0.0);
    let Some(hz) = *HZ.get_or_init(|| std::env::var("RUSTUDIO_FRAME_GRID").ok().and_then(|v| v.parse::<f32>().ok()).filter(|h| *h > 0.0))
    else {
        return dt;
    };
    let mut owed = OWED.lock().unwrap();
    *owed += dt;
    let n = (*owed * hz).round().max(1.0);
    // (a frame shorter than a period still gets one: a headless pacer runs
    // such frames to catch up, a display never does. No more than a quarter
    // period of that is held against the frames that follow.)
    *owed = (*owed - n / hz).max(-0.25 / hz);
    n / hz
}
