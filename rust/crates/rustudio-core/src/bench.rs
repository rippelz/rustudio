//! Frame-time measurement: `--bench OUT.json` and the self-test's perf step,
//! plus a tiny section profiler (`prof::scope`) whose totals go in the report.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

thread_local! {
    static SECTIONS: RefCell<Vec<(&'static str, f64, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Adds the time until the guard drops to section `name`.
/// Also a profiler scope (`profiler::scope`, category from the name) while
/// the profiler records.
pub struct Scope {
    name: &'static str,
    t0: Instant,
    i0: u64,
    _prof: crate::profiler::Guard,
}

#[track_caller]
pub fn scope(name: &'static str) -> Scope {
    Scope {
        name,
        t0: Instant::now(),
        i0: if instr_on() { instructions() } else { 0 },
        _prof: crate::profiler::scope(name, crate::profiler::Cat::Auto),
    }
}

/// RUSTUDIO_BENCH_INSTR=1: sections count the thread's user-space
/// instructions (millions) instead of milliseconds: a measure of work that
/// a loaded machine doesn't move (comparing builds while other jobs run).
fn instr_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_BENCH_INSTR").is_some())
}

/// This thread's user-space instructions retired so far (perf_event_open
/// on itself; 0 if unavailable).
#[cfg(target_os = "linux")]
pub fn instructions() -> u64 {
    thread_local! {
        static FD: i32 = unsafe {
            // struct perf_event_attr: type, size, config, …, flags.
            let mut attr = [0u64; 16];
            let a = attr.as_mut_ptr() as *mut u32;
            *a = 0; // PERF_TYPE_HARDWARE
            *a.add(1) = 128; // size
            attr[1] = 1; // PERF_COUNT_HW_INSTRUCTIONS
            attr[5] = (1 << 5) | (1 << 6); // exclude_kernel | exclude_hv
            libc::syscall(libc::SYS_perf_event_open, attr.as_ptr(), 0, -1, -1, 0) as i32
        };
    }
    FD.with(|fd| {
        if *fd < 0 {
            return 0;
        }
        let mut v = 0u64;
        unsafe { libc::read(*fd, &mut v as *mut u64 as *mut libc::c_void, 8) };
        v
    })
}

/// Windows has no per-thread instruction counter without admin-only
/// profilers: this thread's CPU cycles instead (QueryThreadCycleTime), which
/// like instructions don't stretch while the thread waits for a core. The
/// report says so (`work_unit`): cycles, not instructions.
#[cfg(windows)]
pub fn instructions() -> u64 {
    use windows_sys::Win32::System::{Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime};
    let mut cycles = 0u64;
    // SAFETY: the current thread's pseudo-handle and a u64 out-pointer.
    unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
    cycles
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn instructions() -> u64 {
    0
}

/// What RUSTUDIO_BENCH_INSTR counts on this platform when it isn't Linux's
/// instructions (None on Linux, or when the variable is unset).
fn instr_note() -> Option<&'static str> {
    if !instr_on() || cfg!(target_os = "linux") {
        None
    } else if cfg!(windows) {
        Some("millions of CPU cycles (QueryThreadCycleTime): Windows counts cycles, not instructions")
    } else {
        Some("not supported on this platform: sections read 0")
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let ms = if instr_on() {
            instructions().saturating_sub(self.i0) as f64 / 1e6
        } else {
            self.t0.elapsed().as_secs_f64() * 1000.0
        };
        SECTIONS.with(|s| {
            let mut s = s.borrow_mut();
            match s.iter_mut().find(|e| std::ptr::eq(e.0, self.name) || e.0 == self.name) {
                Some(e) => {
                    e.1 += ms;
                    e.2 += 1;
                }
                None => s.push((self.name, ms, 1)),
            }
        });
    }
}

/// This thread's section totals so far (ms, or M instructions).
pub fn totals() -> Vec<(&'static str, f64)> {
    SECTIONS.with(|s| s.borrow().iter().map(|e| (e.0, e.1)).collect())
}

// ------------------------------------------------------------ Lua timing

thread_local! {
    /// On while a bench runs: Lua time per connection / thread.
    static LUA_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Resumes in progress (only the outermost one is timed).
    static LUA_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Key (function / thread pointer) → (label, ms, calls).
    static LUA: RefCell<std::collections::HashMap<usize, (String, f64, u64)>> =
        RefCell::new(std::collections::HashMap::new());
}

pub fn lua_prof_on() -> bool {
    LUA_ON.with(|c| c.get())
}

/// A Play worker follows the display thread's benchmark window.
pub fn set_lua_profiling(on: bool) {
    LUA_ON.with(|c| c.set(on));
}

/// Times one resume of Lua code under `key`, labelled by `label` the first
/// time the key is seen. Nested resumes count in the outermost only.
pub fn lua_timed<R>(key: usize, label: impl FnOnce() -> String, f: impl FnOnce() -> R) -> R {
    let outer = LUA_DEPTH.with(|d| {
        d.set(d.get() + 1);
        d.get() == 1
    });
    let t0 = Instant::now();
    let r = f();
    LUA_DEPTH.with(|d| d.set(d.get() - 1));
    if outer {
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        LUA.with(|m| {
            let mut m = m.borrow_mut();
            let e = m.entry(key).or_insert_with(|| (label(), 0.0, 0));
            if ms > 20.0 && hitch_on() {
                eprintln!("HITCH lua {ms:.1} ms {}", e.0);
            }
            e.1 += ms;
            e.2 += 1;
        });
    }
    r
}

/// Whether an engine call has per-call diagnostics to keep (RUSTUDIO_HITCH,
/// RUSTUDIO_API_TOTALS, RUSTUDIO_API_SITES): the runtime's fast member
/// reads (runtime_routes.rs) leave those calls to the full path.
pub fn call_probes_on() -> bool {
    hitch_on() || totals_on() || sites_on()
}

/// RUSTUDIO_API_TOTALS=1: every engine call's time, summed per thread and
/// printed (top 15) to stderr every 5 s — where a slow script's time goes.
fn totals_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_API_TOTALS").is_some())
}

thread_local! {
    static TOTALS: RefCell<(Option<Instant>, std::collections::HashMap<String, (f64, u64)>)> =
        RefCell::new((None, std::collections::HashMap::new()));
}

fn add_total(kind: &str, name: &str, ms: f64) {
    TOTALS.with(|t| {
        let mut t = t.borrow_mut();
        let e = t.1.entry(format!("{kind} {name}")).or_insert((0.0, 0));
        e.0 += ms;
        e.1 += 1;
        let last = *t.0.get_or_insert_with(Instant::now);
        if last.elapsed().as_secs_f64() >= 5.0 {
            t.0 = Some(Instant::now());
            let mut v: Vec<_> = t.1.iter().map(|(k, (ms, n))| (k.clone(), *ms, *n)).collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            let th = std::thread::current().name().unwrap_or("?").to_string();
            for (k, ms, n) in v.into_iter().take(15) {
                eprintln!("APITOTAL [{th}] {ms:10.1} ms {n:9}x {k}");
            }
            SECTIONS.with(|s| {
                for (k, ms, n) in s.borrow().iter() {
                    eprintln!("APISECT [{th}] {ms:10.1} ms {n:9}x {k}");
                }
            });
        }
    });
}

thread_local! {
    /// RUSTUDIO_HITCH: engine calls since the last `hitch_api_take`.
    static HITCH_API: RefCell<std::collections::HashMap<String, (f64, u64)>> =
        RefCell::new(std::collections::HashMap::new());
}

/// RUSTUDIO_HITCH=1: print every Lua resume over 10 ms (with the engine
/// calls inside one over 300 ms) and every engine call over 5 ms, to find
/// hitches (round 20). Read once.
pub fn hitch_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_HITCH").is_some())
}

/// RUSTUDIO_HITCH: the engine calls gathered since last time, slowest first.
pub fn hitch_api_take() -> Vec<(String, f64, u64)> {
    let mut v: Vec<(String, f64, u64)> = HITCH_API
        .with(|m| std::mem::take(&mut *m.borrow_mut()))
        .into_iter()
        .map(|(k, (ms, n))| (k, ms, n))
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

thread_local! {
    /// Engine API time while a bench runs: name hash → (label, ms, calls).
    static API: RefCell<std::collections::HashMap<u64, (String, f64, u64)>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Times one engine API call from Lua (`kind` index / newindex / call).
pub fn api_timed<R>(kind: &'static str, name: &str, f: impl FnOnce() -> R) -> R {
    let hitch = hitch_on();
    let totals = totals_on();
    if !lua_prof_on() && !hitch && !totals {
        return f();
    }
    let t0 = Instant::now();
    let r = f();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if totals {
        add_total(kind, name, ms);
    }
    if hitch && ms > 5.0 {
        eprintln!("HITCH api {ms:.1} ms {kind} {name}");
    }
    if hitch {
        HITCH_API.with(|m| {
            let mut m = m.borrow_mut();
            let e = m.entry(format!("{kind} {name}")).or_insert((0.0, 0));
            e.0 += ms;
            e.1 += 1;
        });
    }
    if !lua_prof_on() {
        return r;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, name).hash(&mut h);
    API.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(h.finish()).or_insert_with(|| (format!("{kind} {name}"), 0.0, 0));
        e.1 += ms;
        e.2 += 1;
    });
    r
}

thread_local! {
    /// RUSTUDIO_API_SITES: "kind member @ chunk:line" → calls, while a bench measures.
    static SITES: RefCell<std::collections::HashMap<String, u64>> =
        RefCell::new(std::collections::HashMap::new());
}

/// RUSTUDIO_API_SITES=FILE: with `--bench`, counts every engine call by the
/// script line that made it (the nearest Lua frame, and the nearest game
/// frame when that one is rustudio's own library), written to FILE (one file
/// per Luau state: FILE.<thread name>.<n>) when the measurement ends. Slow
/// (walks the Lua stack on every call): for finding who calls what, not timing.
pub fn sites_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_API_SITES").is_some())
}

/// Counts one engine call at `key` (built by `lua_bridge::site` from the Lua
/// stack; see `sites_on`).
pub fn record_site(key: String) {
    SITES.with(|m| *m.borrow_mut().entry(key).or_insert(0) += 1);
}

fn write_sites(frames: usize) {
    let Some(base) = std::env::var_os("RUSTUDIO_API_SITES") else { return };
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let mut v: Vec<(String, u64)> = SITES.with(|m| std::mem::take(&mut *m.borrow_mut())).into_iter().collect();
    if v.is_empty() {
        return;
    }
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let frames = frames.max(1) as f64;
    let text: String = v.iter().map(|(k, n)| format!("{n:9} {:9.2}/f  {k}\n", *n as f64 / frames)).collect();
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = std::fs::write(format!("{}.{n}", base.to_string_lossy()), text);
}

fn take_api() -> Vec<(String, f64, u64)> {
    let mut v: Vec<(String, f64, u64)> = API.with(|m| std::mem::take(&mut *m.borrow_mut())).into_values().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

/// Lua totals by label, slowest first: (label, ms, calls).
pub fn take_lua() -> Vec<(String, f64, u64)> {
    let m = LUA.with(|m| std::mem::take(&mut *m.borrow_mut()));
    let mut by: std::collections::HashMap<String, (f64, u64)> = std::collections::HashMap::new();
    for (_, (l, ms, n)) in m {
        let e = by.entry(l).or_default();
        e.0 += ms;
        e.1 += n;
    }
    let mut v: Vec<(String, f64, u64)> = by.into_iter().map(|(l, (ms, n))| (l, ms, n)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

/// This thread's work so far in the bench's unit (milliseconds since an
/// arbitrary start, or millions of instructions under RUSTUDIO_BENCH_INSTR):
/// differences of two reads go to `add` for sections timed in a loop.
/// 0 while no bench is measuring (no clock read per loop item then).
pub fn now() -> f64 {
    thread_local! { static T0: Instant = Instant::now(); }
    if !lua_prof_on() {
        0.0
    } else if instr_on() {
        instructions() as f64 / 1e6
    } else {
        T0.with(|t| t.elapsed().as_secs_f64() * 1000.0)
    }
}

/// Adds `ms` and `calls` to section `name` without a guard (for totals
/// gathered in a loop, or counts with `ms` 0).
/// Runs `f` and returns its result with the work it took on this thread:
/// milliseconds, or millions of instructions under RUSTUDIO_BENCH_INSTR
/// (work done on a helper thread, credited to the caller's section by `add`).
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, f64) {
    let (t0, i0) = (Instant::now(), if instr_on() { instructions() } else { 0 });
    let r = f();
    let v = if instr_on() {
        instructions().saturating_sub(i0) as f64 / 1e6
    } else {
        t0.elapsed().as_secs_f64() * 1000.0
    };
    (r, v)
}

pub fn add(name: &'static str, ms: f64, calls: u64) {
    SECTIONS.with(|s| {
        let mut s = s.borrow_mut();
        match s.iter_mut().find(|e| std::ptr::eq(e.0, name) || e.0 == name) {
            Some(e) => {
                e.1 += ms;
                e.2 += calls;
            }
            None => s.push((name, ms, calls)),
        }
    });
}

/// Section `name`'s running total (ms, calls) since the last bench report
/// took the sections (Stats.PhysicsStepTime reads its deltas).
pub fn section_total(name: &str) -> (f64, u64) {
    SECTIONS.with(|s| {
        s.borrow().iter().find(|e| e.0 == name).map(|e| (e.1, e.2)).unwrap_or((0.0, 0))
    })
}

/// Drain the current thread's section totals for merging into a frame report.
pub fn take_sections() -> Vec<(&'static str, f64, u64)> {
    SECTIONS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// CPU time this thread has used, in ms (not the time it waited for a core:
/// on a busy machine frame times stretch, this much less so).
fn thread_cpu_ms() -> f64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid timespec out-pointer and a clock id the OS defines.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0 {
            return ts.tv_sec as f64 * 1000.0 + ts.tv_nsec as f64 / 1e6;
        }
    }
    // (kernel + user time in 100 ns units, advanced at the scheduler's tick:
    // fine for a bench's total, not for one frame)
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (z, z, z, z);
        // SAFETY: the current thread's pseudo-handle and four FILETIME out-pointers.
        if unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) } != 0 {
            let ms = |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) as f64 / 1e4;
            return ms(kernel) + ms(user);
        }
    }
    // (NaN: null in the report, "not supported on this platform")
    #[cfg(not(any(unix, windows)))]
    return f64::NAN;
    0.0
}

/// GPU timing (`gpu_timing::take` in render): the bench asks render for the
/// GPU milliseconds per label and each finished frame's total. Installed once
/// by `gpu_timing::init`; not installed, the bench reports no GPU figures.
static GPU_TIMING: std::sync::OnceLock<fn() -> (Vec<(&'static str, f64)>, Vec<f64>)> =
    std::sync::OnceLock::new();

pub fn set_gpu_timing(take: fn() -> (Vec<(&'static str, f64)>, Vec<f64>)) {
    let _ = GPU_TIMING.set(take);
}

fn gpu_take() -> (Vec<(&'static str, f64)>, Vec<f64>) {
    match GPU_TIMING.get() {
        Some(take) => take(),
        None => (Vec::new(), Vec::new()),
    }
}

/// Collects frame intervals for a fixed time after a warm-up.
pub struct Bench {
    pub out: Option<PathBuf>,
    pub label: String,
    pub play: bool,
    pub seconds: f32,
    pub warmup: f32,
    started: Option<Instant>,
    measuring: Option<Instant>,
    last: Option<Instant>,
    intervals: Vec<f32>,
    work: Vec<f32>,
    /// Section totals over the measured frames: name → (ms, calls).
    totals: Vec<(&'static str, f64, u64)>,
    /// The slowest frames: (frame ms, its sections).
    worst: Vec<(f32, Vec<(&'static str, f64)>)>,
    pub done: bool,
    /// RUSTUDIO_GPU_TIMING: each measured frame's GPU milliseconds (the sum
    /// of the timed spans, gpu_timing.rs), in the order they finished.
    gpu_frames: Vec<f64>,
    timeline: Vec<[f32; 5]>,
    /// Lua time by connection / thread over the measured frames.
    lua: Vec<(String, f64, u64)>,
    /// Engine API time by member over the measured frames.
    api: Vec<(String, f64, u64)>,
    /// The main thread's CPU time when measuring began, and over the window.
    cpu0: f64,
    cpu_ms: f64,
    /// Per measured frame: the main thread's instructions over the frame
    /// (millions, 0 off Linux / without a counter) and its sections in the
    /// bench's unit. Written beside the report as OUT.frames.json.
    per_frame: Vec<(f64, f32, Vec<(&'static str, f64)>)>,
    instr_last: u64,
}

impl Bench {
    pub fn new(out: Option<PathBuf>, label: &str, play: bool, seconds: f32) -> Bench {
        Bench {
            out,
            label: label.to_string(),
            play,
            seconds,
            warmup: std::env::var("RUSTUDIO_BENCH_WARMUP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(crate::mech::BENCH_WARMUP_S),
            started: None,
            measuring: None,
            last: None,
            intervals: Vec::new(),
            work: Vec::new(),
            totals: Vec::new(),
            worst: Vec::new(),
            done: false,
            gpu_frames: Vec::new(),
            timeline: Vec::new(),
            lua: Vec::new(),
            api: Vec::new(),
            cpu0: 0.0,
            cpu_ms: 0.0,
            per_frame: Vec::new(),
            instr_last: 0,
        }
    }

    /// Called once per frame with the time the frame's own work took (ms).
    /// Returns true when the measurement window is over.
    pub fn frame(&mut self, work_ms: f32) -> bool {
        let now = Instant::now();
        // The window (warm-up, seconds) on the game's clock: under --fixed-dt
        // the simulated one, so a window covers the same frames of a
        // deterministic run however fast the machine is.
        let clock = crate::sim_clock::now();
        let started = *self.started.get_or_insert(clock);
        if self.measuring.is_none() {
            if clock.duration_since(started).as_secs_f32() < self.warmup {
                self.last = Some(now);
                gpu_take();
                return false;
            }
            // This frame's sections are partly from before: it isn't counted.
            take_sections();
            gpu_take();
            take_lua();
            take_api();
            // RUSTUDIO_BENCH_LUA=0: no per-call Lua API timing (two clock
            // reads and a map update on every Lua→engine call) while
            // measuring, so Lua-heavy benches cost what a session does.
            LUA_ON.with(|c| c.set(std::env::var_os("RUSTUDIO_BENCH_LUA").is_none_or(|v| v != "0")));
            self.cpu0 = thread_cpu_ms();
            if let Some(note) = instr_note() {
                eprintln!("RUSTUDIO_BENCH_INSTR: sections are in {note}");
            }
            self.measuring = Some(clock);
            self.last = Some(now);
            self.instr_last = instructions();
            return false;
        }
        let mut frame_sections = take_sections();
        // The renderer's last frame counts (perf::draw): draw calls and
        // thousands of triangles per pass, averaged like the sections.
        let pf = perf::frame();
        for (i, name) in [
            "count: scene draws", "count: scene ktris", "count: shadow draws", "count: shadow ktris",
        ]
        .into_iter()
        .enumerate()
        {
            let v = pf.n[i] as f64 / if i % 2 == 1 { 1000.0 } else { 1.0 };
            if v > 0.0 {
                frame_sections.push((name, v, 1));
            }
        }
        // GPU spans that finished since the last frame (a frame or two late),
        // in GPU milliseconds whatever the bench's unit.
        let (gpu, gpu_totals) = gpu_take();
        frame_sections.extend(gpu.into_iter().map(|(n, ms)| (n, ms, 1)));
        let gpu_last = gpu_totals.last().copied().unwrap_or(-1.0) as f32;
        self.gpu_frames.extend(gpu_totals);
        if let (Some(last), Some(m)) = (self.last, self.measuring) {
            let rebuild = frame_sections.iter().filter(|e| e.0 == "render_static_rebuild").map(|e| e.1).sum::<f64>() as f32;
            self.timeline.push([m.elapsed().as_secs_f32(), now.duration_since(last).as_secs_f32() * 1000.0, work_ms, rebuild, gpu_last]);
        }
        if let Some(last) = self.last {
            let ms = now.duration_since(last).as_secs_f32() * 1000.0;
            self.intervals.push(ms);
            self.work.push(work_ms);
            // Time between frames outside the frame's own work (present, GPU waits).
            frame_sections.push(("between_frames", (ms - work_ms).max(0.0) as f64, 1));
            let i = instructions();
            let mi = i.saturating_sub(self.instr_last) as f64 / 1e6;
            self.instr_last = i;
            let mut keep: Vec<(&'static str, f64)> =
                frame_sections.iter().filter(|e| e.1 > 0.0).map(|(n, t, _)| (*n, *t)).collect();
            keep.sort_by(|a, b| b.1.total_cmp(&a.1));
            // (RUSTUDIO_BENCH_FRAME_SECTIONS=N keeps more per frame, to
            // take slow frames apart)
            static KEEP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            keep.truncate(*KEEP.get_or_init(|| {
                std::env::var("RUSTUDIO_BENCH_FRAME_SECTIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(8)
            }));
            self.per_frame.push((mi, ms, keep));
            for (n, t, c) in &frame_sections {
                match self.totals.iter_mut().find(|e| e.0 == *n) {
                    Some(e) => {
                        e.1 += t;
                        e.2 += c;
                    }
                    None => self.totals.push((n, *t, *c)),
                }
            }
            if self.worst.len() < 5 || ms > self.worst.last().map(|w| w.0).unwrap_or(0.0) {
                self.worst.push((
                    ms,
                    frame_sections.iter().map(|(n, t, _)| (*n, *t)).collect(),
                ));
                self.worst.sort_by(|a, b| b.0.total_cmp(&a.0));
                self.worst.truncate(5);
            }
        }
        self.last = Some(now);
        if clock.duration_since(self.measuring.unwrap()).as_secs_f32() >= self.seconds {
            self.done = true;
            self.cpu_ms = thread_cpu_ms() - self.cpu0;
            LUA_ON.with(|c| c.set(false));
            self.lua = take_lua();
            self.api = take_api();
            write_sites(self.intervals.len());
        }
        self.done
    }

    /// Every measured frame: [M instructions, ms, {top sections}].
    pub fn frames_json(&self) -> serde_json::Value {
        self.per_frame
            .iter()
            .map(|(mi, ms, secs)| {
                let m: serde_json::Map<String, serde_json::Value> =
                    secs.iter().map(|(n, t)| (n.to_string(), serde_json::json!((t * 100.0).round() / 100.0))).collect();
                serde_json::json!([(mi * 100.0).round() / 100.0, (*ms * 100.0).round() / 100.0, m])
            })
            .collect()
    }

    pub fn report(&self) -> serde_json::Value {
        let mut report = self.report_base();
        if let Some(p) = std::env::var_os("RUSTUDIO_BENCH_TIMELINE") {
            let _ = std::fs::write(p, serde_json::to_string(&self.timeline).unwrap_or_default());
        }
        // Frame interval and work quantiles (ms), and RUSTUDIO_GPU_TIMING's
        // GPU ms per frame over the timed encoders (per-pass figures are the
        // "gpu: …" sections).
        let q = |v: &[f64]| {
            let mut v = v.to_vec();
            v.sort_by(|a, b| a.total_cmp(b));
            let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            serde_json::json!({"median": at(0.5), "p90": at(0.9), "p99": at(0.99), "max": at(1.0), "n": v.len()})
        };
        if !self.intervals.is_empty() {
            report["frame_ms"] = q(&self.intervals.iter().map(|v| *v as f64).collect::<Vec<_>>());
            report["work_ms"] = q(&self.work.iter().map(|v| *v as f64).collect::<Vec<_>>());
        }
        if !self.gpu_frames.is_empty() {
            report["gpu_frame_ms"] = q(&self.gpu_frames);
            report["gpu_frame_ms"]["note"] = "sum of timed GPU spans (3D view, its extra submits, offscreen egui); not the windowed egui pass or present".into();
        }
        report
    }

    fn report_base(&self) -> serde_json::Value {
        let stats = |v: &[f32]| {
            if v.is_empty() {
                return (0.0, 0.0);
            }
            let avg = v.iter().sum::<f32>() / v.len() as f32;
            let mut s = v.to_vec();
            s.sort_by(|a, b| b.total_cmp(a));
            let n = (s.len() / 100).max(1);
            let low = s[..n].iter().sum::<f32>() / n as f32;
            (avg, low)
        };
        let (avg, low) = stats(&self.intervals);
        let (wavg, wlow) = stats(&self.work);
        let frames = self.intervals.len().max(1) as f64;
        let sections: Vec<serde_json::Value> = {
            let mut s = self.totals.clone();
            s.sort_by(|a, b| b.1.total_cmp(&a.1));
            s.iter()
                .map(|(n, ms, c)| serde_json::json!({"name": n, "ms_per_frame": ms / frames, "calls": c}))
                .collect()
        };
        let mut report = serde_json::json!({
            "label": self.label,
            "mode": if self.play { "play" } else { "edit" },
            "frames": self.intervals.len(),
            "avg_frame_ms": avg,
            "low1_frame_ms": low,
            "avg_fps": if avg > 0.0 { 1000.0 / avg } else { 0.0 },
            "low1_fps": if low > 0.0 { 1000.0 / low } else { 0.0 },
            "avg_work_ms": wavg,
            // The main thread's CPU time per frame: steadier than the wall
            // times above when other programs compete for the cores.
            "avg_cpu_ms": self.cpu_ms / frames,
            "low1_work_ms": wlow,
            "sections": sections,
            // Slowest Lua: "chunk:line event" per connection, "chunk:line"
            // per resumed thread (waits, spawns), "start Script" per script.
            // Slowest engine members called from Lua (index / newindex / call).
            "api": self
                .api
                .iter()
                .take(30)
                .map(|(l, ms, n)| serde_json::json!({"what": l, "ms_per_frame": ms / frames, "calls": n}))
                .collect::<Vec<_>>(),
            "lua": self
                .lua
                .iter()
                .take(30)
                .map(|(l, ms, n)| serde_json::json!({"what": l, "ms_per_frame": ms / frames, "calls": n}))
                .collect::<Vec<_>>(),
            "worst_frames": self
                .worst
                .iter()
                .map(|(ms, secs)| {
                    let mut secs = secs.clone();
                    secs.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let parts: Vec<String> = secs.iter().take(6).map(|(n, t)| format!("{n} {t:.1}")).collect();
                    format!("{ms:.1} ms: {}", parts.join(", "))
                })
                .collect::<Vec<_>>(),
        });
        // Main-thread instructions per frame (millions): a spike in these is
        // real work, not the machine's load. Median, p99, max, and the
        // largest multiple of the median (a periodic stall shows here).
        let mut mi: Vec<f64> = self.per_frame.iter().map(|f| f.0).collect();
        if !mi.is_empty() && mi.iter().any(|v| *v > 0.0) {
            mi.sort_by(|a, b| a.total_cmp(b));
            let q = |p: f64| mi[((mi.len() - 1) as f64 * p).round() as usize];
            let med = q(0.5);
            report["frame_minstr"] = serde_json::json!({
                "median": med, "p99": q(0.99), "max": q(1.0),
                "max_over_median": if med > 0.0 { q(1.0) / med } else { 0.0 },
            });
        }
        // Off Linux, what RUSTUDIO_BENCH_INSTR's section figures are.
        if let Some(note) = instr_note() {
            report["work_unit"] = note.into();
        }
        report
    }
}

// ------------------------------------------------------- frame counters

/// Engine counters Stats reads, fed by the renderer, the GUI passes,
/// physics and replication (one call each; see services.md). They live here,
/// in core, so the renderer and physics can reach them; the Engine's Stats
/// domain reads them through `services::perf`.
pub mod perf {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::sync::Mutex;

    pub const SCENE: usize = 0;
    pub const SHADOW: usize = 2;
    pub const UI2D: usize = 4;
    pub const UI3D: usize = 6;

    /// This frame's running counts: (calls, triangles) per kind.
    static ACC: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

    #[derive(Clone, Copy, Default)]
    pub struct Frame {
        pub n: [u64; 8],
        pub cpu_s: f64,
        pub gpu_s: f64,
    }
    static LAST: Mutex<Frame> = Mutex::new(Frame { n: [0; 8], cpu_s: 0.0, gpu_s: 0.0 });

    /// One draw of `kind`: `calls` draw calls covering `tris` triangles.
    pub fn draw(kind: usize, calls: u64, tris: u64) {
        ACC[kind].fetch_add(calls, Relaxed);
        ACC[kind + 1].fetch_add(tris, Relaxed);
    }

    /// The renderer finished a frame: its counts become the last frame's.
    /// `cpu_s`: the frame's render encoding on the CPU; `gpu_s`: GPU time
    /// when known (0 otherwise).
    pub fn end_frame(cpu_s: f64, gpu_s: f64) {
        let mut f = Frame { n: [0; 8], cpu_s, gpu_s };
        for (i, a) in ACC.iter().enumerate() {
            f.n[i] = a.swap(0, Relaxed);
        }
        if crate::profiler::active() {
            use crate::profiler::{counter, counter_unit, Cat, Unit};
            const NAMES: [&str; 8] = [
                "scene draw calls", "scene triangles", "shadow draw calls", "shadow triangles",
                "UI 2D draw calls", "UI 2D triangles", "UI 3D draw calls", "UI 3D triangles",
            ];
            for (i, n) in NAMES.iter().enumerate() {
                counter(n, Cat::Render, f.n[i] as f64);
            }
            counter("draw calls (all)", Cat::Render, (f.n[0] + f.n[2] + f.n[4] + f.n[6]) as f64);
            counter("triangles (all)", Cat::Render, (f.n[1] + f.n[3] + f.n[5] + f.n[7]) as f64);
            counter_unit("render CPU encode", Cat::Render, Unit::Ms, cpu_s * 1000.0);
            if gpu_s > 0.0 {
                counter_unit("GPU frame", Cat::Gpu, Unit::Ms, gpu_s * 1000.0);
            }
        }
        if let Ok(mut l) = LAST.lock() {
            *l = f;
        }
    }

    pub fn frame() -> Frame {
        LAST.lock().map(|f| *f).unwrap_or_default()
    }

    // --------------------------------------------------------- contacts

    static CONTACTS: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);

    /// Physics stepped DataModel `doc`: its touching collider pairs.
    pub fn set_contacts(doc: u64, n: usize) {
        if let Ok(mut m) = CONTACTS.lock() {
            m.get_or_insert_with(HashMap::new).insert(doc, n);
        }
    }

    pub fn contacts(doc: u64) -> usize {
        CONTACTS.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(&doc).copied())).unwrap_or(0)
    }
}
