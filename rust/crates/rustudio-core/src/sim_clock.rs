//! `--fixed-dt DT`: the run's clocks follow simulated time.
//!
//! Off (the default) every function here is the wall clock, so nothing
//! changes. On, each frame advances simulated time by exactly DT (the app
//! calls `advance` once per frame), and the clocks a game or the harness
//! reads follow it: the harness's --key/--luau/--shots times, os.clock (as
//! RUSTUDIO_GAME_CLOCK), tick(), os.time(), DateTime.now(),
//! GetServerTimeNow, task.wait's returned time, RunService.FrameTime, sound
//! positions, water and the environment probe. Unseeded randomness
//! (math.random's start state, Random.new(), BrickColor.Random()) takes
//! seeds in a fixed order instead of the time; the sky, light grid and
//! light occlusion jobs land the frame after they start, and their
//! refresh intervals and the clouds run on simulated time. Frames run as fast as
//! the machine allows, and a run does the same work on a loaded machine as
//! on an idle one.
//!
//! The absolute clocks (tick, os.time, os.date, DateTime.now) start from a
//! fixed epoch, 2026-01-01 00:00 UTC, or RUSTUDIO_EPOCH=SECONDS: games
//! seed from them (Roblox's Animate script runs math.randomseed(tick())),
//! so a run's start time must not leak in.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixed {
    dt: f64,
    base: Instant,
    epoch: f64,
}

static FIXED: OnceLock<Fixed> = OnceLock::new();
/// 2026-01-01 00:00:00 UTC.
const FIXED_EPOCH: f64 = 1_767_225_600.0;
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Turns simulated time on (once, at startup, before any clock is read).
pub fn enable(dt: f64) {
    let epoch = std::env::var("RUSTUDIO_EPOCH")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(FIXED_EPOCH);
    let _ = FIXED.set(Fixed { dt, base: Instant::now(), epoch });
}

pub fn on() -> bool {
    FIXED.get().is_some()
}

pub fn dt() -> Option<f32> {
    FIXED.get().map(|f| f.dt as f32)
}

/// One frame's step of simulated time (no-op on the wall clock).
pub fn advance() {
    if on() {
        FRAMES.fetch_add(1, Ordering::Relaxed);
    }
}

fn sim_secs(f: &Fixed) -> f64 {
    FRAMES.load(Ordering::Relaxed) as f64 * f.dt
}

fn wall_epoch() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// `Instant::now()`, or the start plus simulated time.
pub fn now() -> Instant {
    match FIXED.get() {
        Some(f) => f.base + Duration::from_secs_f64(sim_secs(f)),
        None => Instant::now(),
    }
}

/// Simulated seconds so far (None on the wall clock).
pub fn seconds() -> Option<f64> {
    FIXED.get().map(sim_secs)
}

/// Time since `t` (an instant from `now`).
pub fn since(t: Instant) -> Duration {
    now().saturating_duration_since(t)
}

/// Seconds since the Unix epoch (wall, or epoch + simulated time).
pub fn epoch_secs() -> f64 {
    match FIXED.get() {
        Some(f) => f.epoch + sim_secs(f),
        None => wall_epoch(),
    }
}

/// A background job's result (the sky, the light grid, light occlusion):
/// whatever is there now on the wall clock; under --fixed-dt it waits, so
/// a job started in one frame always lands in the next, however busy the
/// machine is.
pub fn job_recv<T>(rx: &std::sync::mpsc::Receiver<T>) -> Result<T, std::sync::mpsc::TryRecvError> {
    if on() {
        rx.recv().map_err(|_| std::sync::mpsc::TryRecvError::Disconnected)
    } else {
        rx.try_recv()
    }
}

/// `1/60`, `0.0166…` or `60hz` as seconds.
pub fn parse_dt(s: &str) -> Option<f64> {
    let s = s.trim();
    let v = if let Some((a, b)) = s.split_once('/') {
        a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?
    } else if let Some(hz) = s.strip_suffix("hz").or_else(|| s.strip_suffix("Hz")) {
        1.0 / hz.trim().parse::<f64>().ok()?
    } else {
        s.parse::<f64>().ok()?
    };
    (v.is_finite() && v > 0.0 && v <= 1.0).then_some(v)
}
