//! RUSTUDIO_LOAD_TRACE=1: where a (headless) load spends its time. Prints
//! `LOADTRACE <seconds since the first rustudio process started> <what>` to
//! stderr: process starts (the headless wrapper hands its start time to the
//! child in RUSTUDIO_LOAD_T0), the place read / parsed / opened, the first
//! frame, Play's DataModels, every frame over 100 ms with its biggest bench
//! sections, and the asset queues each time they change.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_LOAD_TRACE").is_some_and(|v| v != "0"))
}

fn epoch() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The first process's start (seconds since the epoch).
fn t0() -> f64 {
    static T0: OnceLock<f64> = OnceLock::new();
    *T0.get_or_init(|| {
        std::env::var("RUSTUDIO_LOAD_T0").ok().and_then(|s| s.parse().ok()).unwrap_or_else(epoch)
    })
}

/// At process start: fixes t0 and passes it on to child processes.
pub fn init() {
    if !on() {
        return;
    }
    let t = t0();
    // SAFETY: set before any other thread starts (first thing in main).
    std::env::set_var("RUSTUDIO_LOAD_T0", format!("{t:.6}"));
    let who = if std::env::var_os("RUSTUDIO_HEADLESS_CHILD").is_some() { "child" } else { "main" };
    mark(&format!("process start ({who}, pid {})", std::process::id()));
}

pub fn mark(what: &str) {
    if on() {
        eprintln!("LOADTRACE {:8.3} {what}", epoch() - t0());
    }
    if startup_trace() {
        play_step(what);
    }
}

/// Android (logcat `RustStdoutStderr`), or RUSTUDIO_STARTUP_TRACE: the
/// `[startup]` lines (the renderer, the pipelines, the first frame, and
/// Play's start below) without the full trace.
pub fn startup_trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| cfg!(target_os = "android") || std::env::var_os("RUSTUDIO_STARTUP_TRACE").is_some())
}

/// Play's start under way: when it was asked, when the last step ended,
/// the steps so far (name, ms).
static PLAY_START: Mutex<Option<(Instant, Instant, Vec<(String, u128)>)>> = Mutex::new(None);
/// When the last Play was asked (the follow-up line a few seconds in).
static PLAY_T0: Mutex<Option<Instant>> = Mutex::new(None);
static PLAY_LATER_DONE: AtomicBool = AtomicBool::new(false);

/// Scripts compiled since the last summary: count, source bytes, loaded
/// from the bytecode cache, marked --!native, nanoseconds in the compiler
/// (and the native code generator).
static COMPILED: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];

/// A script's Source went through the compiler (runtime.rs
/// compile_script): for the `[startup]` summary.
pub fn note_compile(bytes: usize, cached: bool, native: bool, took: std::time::Duration) {
    COMPILED[0].fetch_add(1, Ordering::Relaxed);
    COMPILED[1].fetch_add(bytes as u64, Ordering::Relaxed);
    COMPILED[2].fetch_add(cached as u64, Ordering::Relaxed);
    COMPILED[3].fetch_add(native as u64, Ordering::Relaxed);
    COMPILED[4].fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
}

/// The deferred native builds of a VM's --!native scripts finished
/// (runtime.rs native_drain): one `[startup]` line.
pub fn native_built(n: usize, bytes: usize, took: std::time::Duration, frames: u32) {
    if startup_trace() {
        eprintln!(
            "[startup] native code for {n} --!native script(s) ({} KB) built in {} ms over {frames} frame(s) after their load",
            bytes / 1024,
            took.as_millis()
        );
    }
}

/// `; N scripts compiled (X KB, K from the bytecode cache, M --!native)
/// in T ms` since the last call, which resets the counts.
fn compile_summary() -> String {
    let [n, bytes, cached, native, ns] = COMPILED.each_ref().map(|c| c.swap(0, Ordering::Relaxed));
    if n == 0 {
        return String::new();
    }
    format!(
        "; {n} scripts compiled ({} KB, {cached} from the bytecode cache, {native} --!native) in {} ms",
        bytes / 1024,
        ns / 1_000_000
    )
}

/// The "play: …" marks of start_play as one `[startup]` line when the
/// DataModels run: how long each step took, and the scripts compiled by
/// then.
fn play_step(what: &str) {
    let Some(step) = what.strip_prefix("play: ") else { return };
    let now = Instant::now();
    let mut g = PLAY_START.lock().unwrap_or_else(|e| e.into_inner());
    if step == "start asked" {
        *g = Some((now, now, Vec::new()));
        *PLAY_T0.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
        PLAY_LATER_DONE.store(false, Ordering::Relaxed);
        return;
    }
    let Some((t0, last, steps)) = g.as_mut() else { return };
    steps.push((step.to_string(), now.duration_since(*last).as_millis()));
    *last = now;
    if step == "DataModels running" {
        let total = now.duration_since(*t0).as_millis();
        let list: Vec<String> = steps.iter().map(|(s, ms)| format!("{s} {ms}")).collect();
        eprintln!("[startup] Play started in {total} ms: {}{}", list.join(", "), compile_summary());
        *g = None;
    }
}

/// Once, a few seconds into Play (each frame of play_tick calls it): the
/// scripts compiled since the start line (modules load as they are
/// required, so most of a big game's compiling comes after it).
pub fn play_frame() {
    if !startup_trace() || PLAY_LATER_DONE.load(Ordering::Relaxed) {
        return;
    }
    let t0 = *PLAY_T0.lock().unwrap_or_else(|e| e.into_inner());
    let Some(t0) = t0 else { return };
    if t0.elapsed() < std::time::Duration::from_secs(8) {
        return;
    }
    PLAY_LATER_DONE.store(true, Ordering::Relaxed);
    let summary = compile_summary();
    if !summary.is_empty() {
        eprintln!("[startup] 8 s into Play{summary}");
    }
}

/// At the start of each frame, for the frame before (`ms` = the time
/// between the two starts): one over 100 ms is printed with the bench
/// sections that grew most during it.
pub fn frame(frame_no: u64, ms: f64) {
    if !on() {
        return;
    }
    static PREV: Mutex<Vec<(&'static str, f64)>> = Mutex::new(Vec::new());
    let mut prev = PREV.lock().unwrap_or_else(|e| e.into_inner());
    let before = std::mem::replace(&mut *prev, crate::bench::totals());
    let after = &*prev;
    static FIRST: OnceLock<()> = OnceLock::new();
    if FIRST.set(()).is_ok() {
        mark("first frame");
    }
    if ms < 100.0 {
        return;
    }
    let mut grew: Vec<(&str, f64)> = after
        .iter()
        .map(|(k, v)| (*k, v - before.iter().find(|(b, _)| b == k).map(|b| b.1).unwrap_or(0.0)))
        .filter(|(_, d)| *d > 5.0)
        .collect();
    grew.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top: Vec<String> = grew.iter().take(8).map(|(k, d)| format!("{k}={d:.0}")).collect();
    mark(&format!("slow frame {frame_no}: {ms:.0} ms | {}", top.join(" ")));
}

/// The asset queues, printed when they change (at most every 250 ms).
pub fn assets(meshes: usize, textures_pending: usize, textures: usize) {
    if !on() {
        return;
    }
    static LAST: Mutex<(usize, usize, usize, f64)> = Mutex::new((usize::MAX, usize::MAX, usize::MAX, 0.0));
    let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = epoch();
    if (l.0, l.1, l.2) != (meshes, textures_pending, textures) && now - l.3 >= 0.25 {
        *l = (meshes, textures_pending, textures, now);
        mark(&format!("assets: meshes {meshes}, textures {textures} (+{textures_pending} decoding)"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_summary_accumulates_and_resets() {
        let _ = compile_summary();
        note_compile(4096, true, false, std::time::Duration::from_millis(5));
        note_compile(2048, false, true, std::time::Duration::from_millis(7));
        let summary = compile_summary();
        assert_eq!(
            summary,
            "; 2 scripts compiled (6 KB, 1 from the bytecode cache, 1 --!native) in 12 ms"
        );
        assert!(compile_summary().is_empty());
    }
}
