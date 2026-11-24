//! Luau script profiler: what the scripts of every VM (edit plugins, Play's
//! server and client) cost, in two layers that can run alone or together.
//!
//! * **Live** (Script Performance): every resumption of a script thread is
//!   timed exactly (start, `task.wait` wake, event handler, `Wait()` wake,
//!   deferred). Per script: inclusive and self time, resumes, avg/max, the
//!   last second's activity % and rate, the Luau heap its memory category
//!   holds and its live threads; per entry (handler function + event): exact
//!   calls, total, avg, max.
//! * **Recording** (Script Profiler): a sampler thread arms the VM interrupt
//!   at the chosen frequency while a script runs; the interrupt walks the
//!   Luau stack and adds it to a call tree per (VM, resumed script). Function
//!   times are the exact script time split by sample share, so the tree adds
//!   up to the live numbers; each function also keeps its hot lines.
//!
//! `debug.profilebegin(label)` / `profileend()` become exact user scopes.
//! Every resumption is also a `Cat::Script` scope on the capture timeline
//! when the frame profiler runs (`timeline` below).
//!
//! Both layers are off by default; off costs one relaxed atomic load per
//! resumption. The gameplay worker is a thread of this process, so the
//! editor reads everything through `snapshot()`.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mlua::ffi;

// ------------------------------------------------------------------ switches

const LIVE: u8 = 1;
const RECORD: u8 = 2;
const EXACT: u8 = 4;
const VIEW: u8 = 8;
/// Exact mode was on at some point: threads may still be single-stepping.
static EXACT_USED: AtomicBool = AtomicBool::new(false);
static FLAGS: AtomicU8 = AtomicU8::new(0);
static PROFILE_WORK: AtomicBool = AtomicBool::new(false);
static SAMPLE_HZ: AtomicU32 = AtomicU32::new(1000);

/// Any layer is on (one relaxed load: the hot-path check).
#[inline]
pub fn on() -> bool {
    FLAGS.load(Relaxed) != 0 || timeline::on()
}

pub fn live() -> bool {
    FLAGS.load(Relaxed) & (LIVE | VIEW | RECORD | EXACT) != 0
}

/// Per-script stats are kept: live mode, or a frame-profiler capture is
/// recording (its saved file carries them).
#[inline]
fn live_stats() -> bool {
    live() || timeline::on()
}

pub fn recording() -> bool {
    FLAGS.load(Relaxed) & RECORD != 0
}

pub fn exact() -> bool {
    FLAGS.load(Relaxed) & EXACT != 0
}

/// Exact per-function timing: profiled threads run in Luau's single-step
/// interpreter with a call/return hook (every Lua and C call counted and
/// timed; native code runs interpreted meanwhile, so it costs more than
/// sampling). Implies live timing.
pub fn set_exact(on: bool) {
    if on {
        EXACT_USED.store(true, Relaxed);
        exact::install();
        FLAGS.fetch_or(EXACT, Relaxed);
    } else {
        FLAGS.fetch_and(!EXACT, Relaxed);
    }
}

/// Requests Script Performance stats while the live Scripts tab is visible.
pub fn set_view_live(on: bool) {
    if on {
        FLAGS.fetch_or(VIEW, Relaxed);
    } else {
        FLAGS.fetch_and(!VIEW, Relaxed);
    }
}

/// Requests Script Performance stats from explicit runtime configuration.
pub fn set_live(on: bool) {
    if on {
        FLAGS.fetch_or(LIVE, Relaxed);
    } else {
        FLAGS.fetch_and(!LIVE, Relaxed);
    }
}

/// Starts a sampled recording (keeps what was recorded unless `reset`).
/// Recording implies live timing (the tree is scaled by it).
pub fn start_recording(reset: bool) {
    if reset {
        clear_recording();
    }
    {
        let mut s = state();
        if s.rec_started.is_none() {
            s.rec_started = Some(Instant::now());
        }
    }
    FLAGS.fetch_or(RECORD, Relaxed);
    sampler::ensure_running();
}

pub fn stop_recording() {
    FLAGS.fetch_and(!RECORD, Relaxed);
    let mut s = state();
    if let Some(t0) = s.rec_started.take() {
        s.rec_elapsed += t0.elapsed();
    }
}

/// Clears the call tree and the exact totals (not the rolling 1 s window).
pub fn clear_recording() {
    let mut s = state();
    let names = std::mem::take(&mut s.names);
    let funcs = std::mem::take(&mut s.funcs);
    let func_index = std::mem::take(&mut s.func_index);
    let scripts_live: Vec<_> = s.scripts.iter().map(|(k, v)| (*k, v.window.clone(), v.heap, v.threads, v.native)).collect();
    *s = State { names, funcs, func_index, ..State::default() };
    exact::EPOCH.fetch_add(1, Relaxed);
    for (k, window, heap, threads, native) in scripts_live {
        let e = s.scripts.entry(k).or_default();
        e.window = window;
        e.heap = heap;
        e.threads = threads;
        e.native = native;
    }
    if recording() {
        s.rec_started = Some(Instant::now());
    }
}

/// Sampling frequency in Hz (clamped 50..=10000).
pub fn set_sample_hz(hz: u32) {
    SAMPLE_HZ.store(hz.clamp(50, 10_000), Relaxed);
}

pub fn sample_hz() -> u32 {
    SAMPLE_HZ.load(Relaxed)
}

/// RUSTUDIO_SCRIPT_PROFILE=1|live|exact|<hz>: start at launch (headless
/// captures, benches: `--bench` adds a "scripts" summary to its report and
/// RUSTUDIO_SCRIPT_PROFILE_OUT=PATH writes PATH.json + PATH.folded). Reads
/// the env once. With RUSTUDIO_BENCH_INSTR=1, script and callback
/// rows also count self work (instructions on Linux, cycles on Windows).
pub fn init_from_env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(v) = std::env::var_os("RUSTUDIO_SCRIPT_PROFILE") else { return };
        let v = v.to_string_lossy().to_string();
        PROFILE_WORK.store(std::env::var_os("RUSTUDIO_BENCH_INSTR").is_some(), Relaxed);
        match v.as_str() {
            "" | "0" => {}
            "live" => set_live(true),
            "exact" => set_exact(true),
            other => {
                if let Ok(hz) = other.parse::<u32>() {
                    if hz > 1 {
                        set_sample_hz(hz);
                    }
                }
                start_recording(true);
            }
        }
    });
}

// ------------------------------------------------------------------ model

/// Which VM a script ran in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum VmKind {
    /// The edit DataModel (plugins, command bar).
    Edit,
    Server,
    Client,
}

impl VmKind {
    pub fn label(self) -> &'static str {
        match self {
            VmKind::Edit => "Edit",
            VmKind::Server => "Server",
            VmKind::Client => "Client",
        }
    }
}

/// How a script thread was entered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    /// The script's first run.
    Start,
    /// A connected handler for an event.
    Event,
    /// A thread waiting on an event (`:Wait()`).
    Wait,
    /// `task.wait` / `wait` / `delay` / `task.delay` timers.
    Timer,
    /// `task.defer` / deferred signal handlers.
    Deferred,
    /// `task.spawn` / `coroutine`-style immediate starts.
    Spawn,
    /// RemoteFunction invokes and their replies.
    Remote,
    /// Anything else (engine library globals, plugin widgets, tests).
    Other,
}

impl EntryKind {
    pub fn label(self) -> &'static str {
        match self {
            EntryKind::Start => "start",
            EntryKind::Event => "event",
            EntryKind::Wait => "wait",
            EntryKind::Timer => "timer",
            EntryKind::Deferred => "deferred",
            EntryKind::Spawn => "spawn",
            EntryKind::Remote => "remote",
            EntryKind::Other => "other",
        }
    }
}

type Name = u32;

#[derive(Default)]
struct Names {
    list: Vec<Arc<str>>,
    index: HashMap<Arc<str>, Name>,
}

impl Names {
    fn get(&mut self, s: &str) -> Name {
        if let Some(&n) = self.index.get(s) {
            return n;
        }
        let a: Arc<str> = Arc::from(s);
        let n = self.list.len() as Name;
        self.list.push(a.clone());
        self.index.insert(a, n);
        n
    }

    fn str(&self, n: Name) -> Arc<str> {
        self.list.get(n as usize).cloned().unwrap_or_else(|| Arc::from("?"))
    }
}

/// One-second buckets for activity / rate (last 4 s kept).
#[derive(Clone, Default)]
struct Window {
    /// (second index, self ns, resumes)
    buckets: [(u64, u64, u32); 4],
}

impl Window {
    fn add(&mut self, sec: u64, ns: u64) {
        let b = &mut self.buckets[(sec % 4) as usize];
        if b.0 != sec {
            *b = (sec, 0, 0);
        }
        b.1 += ns;
        b.2 += 1;
    }

    /// The last full second (activity %, resumes/s).
    fn last_second(&self, now_sec: u64) -> (f64, u32) {
        let want = now_sec.saturating_sub(1);
        let b = &self.buckets[(want % 4) as usize];
        if b.0 == want {
            (b.1 as f64 / 1e7, b.2)
        } else {
            (0.0, 0)
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Acc {
    calls: u64,
    total_ns: u64,
    self_ns: u64,
    max_ns: u64,
    self_work: u64,
}

impl Acc {
    fn add(&mut self, total: u64, slf: u64) {
        self.calls += 1;
        self.total_ns += total;
        self.self_ns += slf;
        self.max_ns = self.max_ns.max(total);
    }
}

#[derive(Default, Clone)]
struct ScriptStats {
    acc: Acc,
    window: Window,
    /// (entry kind, handler "fn:line" or event name) → exact.
    entries: HashMap<(EntryKind, Name, Name), Acc>,
    heap: u64,
    threads: u32,
    native: bool,
}

/// A function seen in a sample.
#[derive(Clone)]
struct Func {
    /// Script path ("Workspace.Model.Script"), "[engine] x" or "[C]".
    script: Name,
    /// Function name ("" for anonymous / the script's main chunk).
    name: Name,
    line: u32,
    native: bool,
    engine: bool,
    c: bool,
    /// Self samples by line.
    lines: HashMap<u32, u64>,
}

#[derive(Clone, Copy)]
struct Node {
    func: u32,
    first_child: u32,
    next: u32,
    self_samples: u64,
    total_samples: u64,
}

const NONE: u32 = u32::MAX;

#[derive(Default, Clone)]
struct VmStats {
    frames: u64,
    heap_total: u64,
    /// Engine-attributed heap (memory category 0).
    heap_other: u64,
}

#[derive(Default)]
struct State {
    names: Names,
    funcs: Vec<Func>,
    /// (script name, fn name, line) → func index.
    func_index: HashMap<(Name, Name, u32), u32>,
    nodes: Vec<Node>,
    /// (vm, resumed script) → tree root node.
    roots: HashMap<(VmKind, Name), u32>,
    scripts: HashMap<(VmKind, Name), ScriptStats>,
    markers: HashMap<(VmKind, Name, Name), Acc>,
    vms: HashMap<VmKind, VmStats>,
    samples: u64,
    /// Samples dropped (interrupt fired outside a script resume).
    missed: u64,
    rec_started: Option<Instant>,
    rec_elapsed: Duration,
    /// Sampler overhead: ns spent in the interrupt walking stacks.
    sample_ns: u64,
    /// Exact-mode call tree (flushed from each OS thread's own tree).
    xnodes: Vec<exact::XNode>,
    xroots: HashMap<(VmKind, Name), u32>,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn epoch() -> Instant {
    static T0: OnceLock<Instant> = OnceLock::new();
    *T0.get_or_init(Instant::now)
}

fn now_sec() -> u64 {
    epoch().elapsed().as_secs()
}

// ------------------------------------------------------------------ VMs

/// Per-VM profiler slot: shared by the VM's thread (resumes, interrupt)
/// and the sampler thread (arming).
pub struct VmSlot {
    kind: AtomicU8,
    /// Nesting depth of timed resumes running right now.
    executing: AtomicU32,
    /// The script whose resumption is innermost (Name + 1; 0 = none).
    current: AtomicU32,
    /// The VM's interrupt callbacks and mlua's interrupt proc.
    callbacks: usize,
    mlua_proc: usize,
    /// Interrupt armed by the sampler at this time (ns since epoch; 0 none).
    armed_at: AtomicU64,
    last_heap: Mutex<Option<Instant>>,
}

impl VmSlot {
    fn kind(&self) -> VmKind {
        match self.kind.load(Relaxed) {
            1 => VmKind::Server,
            2 => VmKind::Client,
            _ => VmKind::Edit,
        }
    }
}

/// Lua app data: this VM's slot and its caches.
pub(crate) struct VmProf {
    slot: Arc<VmSlot>,
    /// Script ref → its interned path.
    script_names: HashMap<rbx_dom_weak::types::Ref, Name>,
    /// Thread pointer → script name, for threads the memory map doesn't know.
    thread_names: HashMap<usize, Option<Name>>,
}

type Proc = unsafe extern "C-unwind" fn(*mut ffi::lua_State, std::ffi::c_int);

fn slots() -> std::sync::MutexGuard<'static, Vec<Arc<VmSlot>>> {
    static S: Mutex<Vec<Arc<VmSlot>>> = Mutex::new(Vec::new());
    S.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registers a VM (before the watchdog takes its interrupt proc off
/// `callbacks`).
pub(crate) fn attach(lua: &mlua::Lua, callbacks: *mut ffi::lua_Callbacks) {
    let proc = unsafe { (*callbacks).interrupt }.map(|p| p as usize).unwrap_or(0);
    let slot = Arc::new(VmSlot {
        kind: AtomicU8::new(0),
        executing: AtomicU32::new(0),
        current: AtomicU32::new(0),
        callbacks: callbacks as usize,
        mlua_proc: proc,
        armed_at: AtomicU64::new(0),
        last_heap: Mutex::new(None),
    });
    slots().push(slot.clone());
    lua.set_app_data(VmProf { slot, script_names: HashMap::new(), thread_names: HashMap::new() });
    init_from_env();
    static EXT: std::sync::Once = std::sync::Once::new();
    // Saved captures (.rsprof / Chrome trace) carry the script profile.
    EXT.call_once(|| {
        rustudio_core::profiler::register_extension("scripts", || {
            // Drop the state guard before snapshot() takes the same lock.
            let has_scripts = !state().scripts.is_empty();
            has_scripts.then(|| snapshot().to_json())
        })
    });
}

pub(crate) fn detach(callbacks: usize) {
    slots().retain(|s| s.callbacks != callbacks);
}

fn vm_kind_of(lua: &mlua::Lua) -> VmKind {
    match lua.app_data_ref::<crate::runtime::World>() {
        Some(w) if w.mode == crate::runtime::RunMode::Edit => VmKind::Edit,
        Some(w) if w.side == crate::runtime::Side::Client => VmKind::Client,
        Some(_) => VmKind::Server,
        None => VmKind::Edit,
    }
}

/// Once per VM frame (Runtime::step): frame count, side, and the heap
/// per script (every 500 ms).
pub(crate) fn vm_frame(lua: &mlua::Lua) {
    if !on() {
        return;
    }
    if EXACT_USED.load(Relaxed) {
        exact::flush_idle();
    }
    let kind = vm_kind_of(lua);
    let Some(slot) = lua.app_data_ref::<VmProf>().map(|p| p.slot.clone()) else { return };
    slot.kind.store(kind as u8, Relaxed);
    state().vms.entry(kind).or_default().frames += 1;
    let due = {
        let mut last = slot.last_heap.lock().unwrap_or_else(|e| e.into_inner());
        let due = last.is_none_or(|t| t.elapsed() > Duration::from_millis(HEAP_POLL_MS.load(Relaxed)));
        if due {
            *last = Some(Instant::now());
        }
        due
    };
    if due {
        poll_heap(lua, kind);
    }
}

/// How often each VM's per-script heap is walked (ms). Tests set 0 so a run
/// shorter than the interval still sees its allocations.
pub(crate) static HEAP_POLL_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(500);

fn poll_heap(lua: &mlua::Lua, kind: VmKind) {
    let Some(mem) = crate::runtime::domains::assets_cloud::scene::memory_by_script(lua) else { return };
    let named: Vec<(Name, u64, u32)> = mem
        .scripts
        .iter()
        .filter_map(|&(r, bytes, threads)| script_name(lua, r).map(|n| (n, bytes as u64, threads)))
        .collect();
    let mut s = state();
    let vm = s.vms.entry(kind).or_default();
    vm.heap_total = mem.total as u64;
    vm.heap_other = mem.other as u64;
    for (k, st) in s.scripts.iter_mut() {
        if k.0 == kind {
            st.heap = 0;
            st.threads = 0;
        }
    }
    for (n, bytes, threads) in named {
        let st = s.scripts.entry((kind, n)).or_default();
        st.heap = bytes;
        st.threads = threads;
    }
}

/// A script ref's interned full name (cached per VM).
fn script_name(lua: &mlua::Lua, r: rbx_dom_weak::types::Ref) -> Option<Name> {
    if let Some(n) = lua.app_data_ref::<VmProf>().and_then(|p| p.script_names.get(&r).copied()) {
        return Some(n);
    }
    let (path, native) = {
        let w = lua.app_data_ref::<crate::runtime::World>()?;
        let d = w.doc.as_ref()?;
        d.dom.get_by_ref(r)?;
        let native = match d.prop(r, "Source") {
            Some(rbx_dom_weak::types::Variant::String(s)) => source_is_native(&s),
            _ => false,
        };
        (w.full_name(r), native)
    };
    let n = {
        let mut s = state();
        let n = s.names.get(&path);
        if native {
            s.scripts.entry((vm_kind_of(lua), n)).or_default().native = true;
        }
        n
    };
    if let Some(mut p) = lua.app_data_mut::<VmProf>() {
        if p.script_names.len() > 8192 {
            p.script_names.clear();
        }
        p.script_names.insert(r, n);
    }
    Some(n)
}

/// `--!native` among the leading comment directives.
pub fn source_is_native(src: &str) -> bool {
    for line in src.lines().take(32) {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(d) = t.strip_prefix("--!") {
            if d.trim() == "native" {
                return true;
            }
            continue;
        }
        if t.starts_with("--") {
            continue;
        }
        break;
    }
    false
}

/// "@Workspace.Script" → "Workspace.Script"; "=rustudio.x" → "[engine] x".
fn chunk_display(src: &[u8]) -> (String, bool) {
    let s = String::from_utf8_lossy(src);
    if let Some(rest) = s.strip_prefix("=rustudio.") {
        return (format!("[engine] {rest}"), true);
    }
    (s.trim_start_matches(['@', '=']).to_string(), false)
}

/// The script a thread belongs to.
fn thread_script(lua: &mlua::Lua, th: &mlua::Thread) -> Option<Name> {
    let ptr = th.to_pointer() as usize;
    if let Some(r) = crate::runtime::domains::assets_cloud::scene::thread_script(lua, ptr) {
        if let Some(n) = script_name(lua, r) {
            return Some(n);
        }
    }
    if let Some(n) = lua.app_data_ref::<VmProf>().and_then(|p| p.thread_names.get(&ptr).copied()) {
        return n;
    }
    // Not created for a script (coroutine.create, engine globals): the chunk
    // of the thread's outermost function.
    let mut src: Option<Vec<u8>> = None;
    let _ = unsafe {
        lua.exec_raw::<()>(th.clone(), |state| {
            let t = ffi::lua_tothread(state, -1);
            if t.is_null() {
                return;
            }
            let depth = ffi::lua_stackdepth(t);
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            let ok = if depth > 0 {
                ffi::lua_getinfo(t, depth - 1, c"s".as_ptr(), &mut ar)
            } else if ffi::lua_gettop(t) > 0 {
                ffi::lua_getinfo(t, -1, c"s".as_ptr(), &mut ar)
            } else {
                0
            };
            if ok != 0 && !ar.source.is_null() {
                src = Some(std::ffi::CStr::from_ptr(ar.source).to_bytes().to_vec());
            }
        })
    };
    let n = src.map(|b| {
        let (path, _) = chunk_display(&b);
        state().names.get(&path)
    });
    if let Some(mut p) = lua.app_data_mut::<VmProf>() {
        if p.thread_names.len() > 4096 {
            p.thread_names.clear();
        }
        p.thread_names.insert(ptr, n);
    }
    n
}

// ------------------------------------------------------------------ resumes

thread_local! {
    /// ns spent in nested timed resumes of the innermost open one.
    static CHILD_NS: Cell<u64> = const { Cell::new(0) };
    /// The entry the next resume belongs to (set by the dispatch site).
    static ENTRY: Cell<(EntryKind, Name, Name)> = const { Cell::new((EntryKind::Other, NONE, NONE)) };
}

/// Runs `f` (a resume) as `kind` entered through `handler` / `event`.
/// The labels are built only while profiling.
#[inline]
pub fn with_entry<R>(
    kind: EntryKind,
    labels: impl FnOnce() -> (Option<String>, Option<String>),
    f: impl FnOnce() -> R,
) -> R {
    if !live_stats() {
        return f();
    }
    let (h, e) = labels();
    let (h, e) = {
        let mut s = state();
        (h.map_or(NONE, |h| s.names.get(&h)), e.map_or(NONE, |e| s.names.get(&e)))
    };
    let prev = ENTRY.replace((kind, h, e));
    let r = f();
    ENTRY.set(prev);
    r
}

thread_local! {
    static CHILD_WORK: Cell<u64> = const { Cell::new(0) };
}

/// Timing for one resumption (from `runtime::resume`).
pub(crate) struct ResumeTimer {
    work: Option<(u64, u64)>,
    slot: Arc<VmSlot>,
    kind: VmKind,
    script: Option<Name>,
    entry: (EntryKind, Name, Name),
    prev_current: u32,
    outer_child: u64,
    t0: Instant,
    marks: usize,
    exact_prev: Option<exact::Prev>,
    th: usize,
    span: timeline::Span,
}

/// Starts timing a resume of `th`; None when the profiler is off.
#[inline]
pub(crate) fn begin_resume(lua: &mlua::Lua, th: &mlua::Thread) -> Option<ResumeTimer> {
    if !on() {
        if EXACT_USED.load(Relaxed) {
            exact::set_singlestep(th, false);
        }
        return None;
    }
    begin_resume_slow(lua, th)
}

#[cold]
fn begin_resume_slow(lua: &mlua::Lua, th: &mlua::Thread) -> Option<ResumeTimer> {
    let slot = lua.app_data_ref::<VmProf>()?.slot.clone();
    let kind = vm_kind_of(lua);
    slot.kind.store(kind as u8, Relaxed);
    let script = thread_script(lua, th);
    let span = timeline::begin(script);
    let entry = ENTRY.replace((EntryKind::Other, NONE, NONE));
    let prev_current = slot.current.swap(script.map_or(0, |n| n + 1), Relaxed);
    slot.executing.fetch_add(1, Relaxed);
    let outer_child = CHILD_NS.replace(0);
    let marks = MARKS.with(|m| m.borrow().len());
    let exact_on = exact();
    if EXACT_USED.load(Relaxed) {
        exact::set_singlestep(th, exact_on);
    }
    let exact_prev = exact_on.then(|| exact::enter(th.to_pointer() as usize, kind, script));
    let work = PROFILE_WORK.load(Relaxed).then(|| {
        (rustudio_core::bench::instructions(), CHILD_WORK.replace(0))
    });
    Some(ResumeTimer { work, slot, kind, script, entry, prev_current, outer_child, t0: Instant::now(), marks, exact_prev, th: th.to_pointer() as usize, span })
}

pub(crate) fn end_resume(t: ResumeTimer, finished: bool) {
    let self_work = t.work.map_or(0, |(start, outer)| {
        let work = rustudio_core::bench::instructions().saturating_sub(start);
        work.saturating_sub(CHILD_WORK.replace(outer + work))
    });
    let ns = t.t0.elapsed().as_nanos() as u64;
    if let Some(prev) = t.exact_prev {
        exact::leave(t.th, prev, finished);
    }
    let child = CHILD_NS.replace(t.outer_child + ns);
    let self_ns = ns.saturating_sub(child);
    t.slot.executing.fetch_sub(1, Relaxed);
    t.slot.current.store(t.prev_current, Relaxed);
    markers_truncate(t.marks);
    drop(t.span);
    if !live_stats() {
        return;
    }
    let Some(script) = t.script else { return };
    let sec = now_sec();
    let mut s = state();
    let st = s.scripts.entry((t.kind, script)).or_default();
    st.acc.add(ns, self_ns);
    st.acc.self_work += self_work;
    st.window.add(sec, self_ns);
    let (ek, h, e) = t.entry;
    let acc = st.entries.entry((ek, h, e)).or_default();
    acc.add(ns, self_ns);
    acc.self_work += self_work;
}

// ------------------------------------------------------------------ markers

thread_local! {
    /// Open debug.profilebegin markers: (label, start, child ns before).
    static MARKS: std::cell::RefCell<Vec<(Name, Instant, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// debug.profilebegin(label) while profiling.
pub(crate) fn marker_begin(label: &str) {
    if !live_stats() {
        return;
    }
    let n = state().names.get(label);
    timeline::marker_begin(label);
    MARKS.with(|m| m.borrow_mut().push((n, Instant::now(), CHILD_NS.get())));
}

/// debug.profileend() while profiling.
pub(crate) fn marker_end(lua: &mlua::Lua) {
    let Some((n, t0, _)) = MARKS.with(|m| m.borrow_mut().pop()) else { return };
    timeline::marker_end();
    if !live_stats() {
        return;
    }
    let ns = t0.elapsed().as_nanos() as u64;
    let kind = vm_kind_of(lua);
    let script = lua
        .app_data_ref::<VmProf>()
        .map(|p| p.slot.current.load(Relaxed))
        .filter(|&c| c > 0)
        .map_or(NONE, |c| c - 1);
    state().markers.entry((kind, script, n)).or_default().add(ns, ns);
}

/// Markers a resumption left open end with it (Studio drops them at a
/// yield).
fn markers_truncate(len: usize) {
    MARKS.with(|m| {
        let mut m = m.borrow_mut();
        while m.len() > len {
            m.pop();
            timeline::marker_end();
        }
    });
}

// ------------------------------------------------------------------ sampling

mod sampler {
    use super::*;

    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn ensure_running() {
        if RUNNING.swap(true, Relaxed) {
            return;
        }
        let ok = std::thread::Builder::new()
            .name("script-sampler".into())
            .spawn(|| {
                while recording() {
                    let hz = SAMPLE_HZ.load(Relaxed).max(1);
                    std::thread::sleep(Duration::from_nanos(1_000_000_000 / hz as u64));
                    let now = epoch().elapsed().as_nanos() as u64;
                    for slot in slots().iter() {
                        if slot.executing.load(Relaxed) == 0 || slot.mlua_proc == 0 {
                            continue;
                        }
                        slot.armed_at.store(now, Relaxed);
                        let cb = slot.callbacks as *mut ffi::lua_Callbacks;
                        // SAFETY: registered VMs live until detach, which
                        // takes the slots lock first (held here).
                        unsafe { std::ptr::write_volatile(&mut (*cb).interrupt, Some(sample_proc as Proc)) };
                    }
                }
                RUNNING.store(false, Relaxed);
                // A start racing the exit: run again.
                if recording() {
                    ensure_running();
                }
            })
            .is_ok();
        if !ok {
            RUNNING.store(false, Relaxed);
        }
    }

    const MAX_DEPTH: i32 = 64;


    struct Frame {
        source: *const std::ffi::c_char,
        name: *const std::ffi::c_char,
        /// A C frame's call-site name, copied (it may live in a buffer the
        /// next lookup reuses).
        cname: [u8; 64],
        cname_len: u8,
        line_defined: i32,
        current: i32,
        native: bool,
        c: bool,
    }

    /// The VM interrupt while a sample is wanted: walks the running
    /// thread's stack, then runs mlua's interrupt (script timeout check,
    /// disarm).
    pub(super) unsafe extern "C-unwind" fn sample_proc(l: *mut ffi::lua_State, gc: std::ffi::c_int) {
        let cb = ffi::lua_callbacks(l);
        let slot = slots().iter().find(|s| s.callbacks == cb as usize).cloned();
        let Some(slot) = slot else {
            (*cb).interrupt = None;
            return;
        };
        if gc >= 0 {
            // A GC step: stay armed for the next Lua safepoint.
            return;
        }
        // Back to mlua's proc for the watchdog's check (it disarms itself).
        (*cb).interrupt = std::mem::transmute::<usize, Option<Proc>>(slot.mlua_proc);
        let armed = slot.armed_at.swap(0, Relaxed);
        if armed != 0 && recording() {
            take_sample(l, &slot);
        }
        if let Some(p) = std::mem::transmute::<usize, Option<Proc>>(slot.mlua_proc) {
            p(l, gc);
        }
    }

    unsafe fn take_sample(l: *mut ffi::lua_State, slot: &VmSlot) {
        let t0 = Instant::now();
        let current = slot.current.load(Relaxed);
        if slot.executing.load(Relaxed) == 0 || current == 0 {
            state().missed += 1;
            return;
        }
        let mut frames: [std::mem::MaybeUninit<Frame>; MAX_DEPTH as usize] =
            [const { std::mem::MaybeUninit::uninit() }; MAX_DEPTH as usize];
        let mut n = 0usize;
        let depth = ffi::lua_stackdepth(l).min(MAX_DEPTH);
        for level in 0..depth {
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            if ffi::lua_getinfo(l, level, c"slnN".as_ptr(), &mut ar) == 0 {
                break;
            }
            let c = !ar.what.is_null() && *ar.what == b'C' as std::ffi::c_char;
            // A C function: the method / global its caller named.
            let mut cname = [0u8; 64];
            let mut cname_len = 0u8;
            if c {
                let site = ffi::lua_rs_callname(l, level);
                if !site.is_null() {
                    let b = std::ffi::CStr::from_ptr(site).to_bytes();
                    let n = b.len().min(64);
                    cname[..n].copy_from_slice(&b[..n]);
                    cname_len = n as u8;
                }
            }
            frames[n].write(Frame {
                source: ar.source,
                name: ar.name,
                cname,
                cname_len,
                line_defined: ar.linedefined,
                current: ar.currentline,
                native: ar.userdata as usize & 1 != 0,
                c,
            });
            n += 1;
        }
        let frames = std::slice::from_raw_parts(frames.as_ptr() as *const Frame, n);
        let mut s = state();
        let kind = slot.kind();
        let root_script = current - 1;
        let root = *s.roots.entry((kind, root_script)).or_insert_with(|| u32::MAX);
        let root = if root == u32::MAX {
            let func = intern_func(&mut s, b"", b"", 0, false, false, Some(root_script));
            let id = s.nodes.len() as u32;
            s.nodes.push(Node { func, first_child: NONE, next: NONE, self_samples: 0, total_samples: 0 });
            s.roots.insert((kind, root_script), id);
            id
        } else {
            root
        };
        let mut node = root;
        s.nodes[node as usize].total_samples += 1;
        // Outermost frame first.
        for (i, f) in frames.iter().enumerate().rev() {
            let src = if f.source.is_null() { &b""[..] } else { std::ffi::CStr::from_ptr(f.source).to_bytes() };
            let name = if f.cname_len > 0 {
                &f.cname[..f.cname_len as usize]
            } else if f.name.is_null() {
                &b""[..]
            } else {
                std::ffi::CStr::from_ptr(f.name).to_bytes()
            };
            let func = intern_func(&mut s, src, name, f.line_defined.max(0) as u32, f.native, f.c, None);
            node = child(&mut s, node, func);
            s.nodes[node as usize].total_samples += 1;
            if i == 0 {
                s.nodes[node as usize].self_samples += 1;
                if f.current > 0 {
                    *s.funcs[func as usize].lines.entry(f.current as u32).or_default() += 1;
                }
            }
        }
        if frames.is_empty() {
            s.nodes[root as usize].self_samples += 1;
        }
        s.samples += 1;
        s.sample_ns += t0.elapsed().as_nanos() as u64;
    }

    fn child(s: &mut State, parent: u32, func: u32) -> u32 {
        let mut c = s.nodes[parent as usize].first_child;
        while c != NONE {
            if s.nodes[c as usize].func == func {
                return c;
            }
            c = s.nodes[c as usize].next;
        }
        let id = s.nodes.len() as u32;
        let first = s.nodes[parent as usize].first_child;
        s.nodes.push(Node { func, first_child: NONE, next: first, self_samples: 0, total_samples: 0 });
        s.nodes[parent as usize].first_child = id;
        id
    }

    /// `script` set: the tree root for that resumed script.
    pub(super) fn intern_func(s: &mut State, src: &[u8], name: &[u8], line: u32, native: bool, c: bool, script: Option<Name>) -> u32 {
        let (script_n, engine) = match script {
            Some(n) => (n, false),
            None if c => (s.names.get("[C]"), false),
            None => {
                // Chunk names repeat: look the bytes up before building a
                // display string.
                let raw = std::str::from_utf8(src).unwrap_or("");
                match s.names.index.get(raw).copied() {
                    Some(n) => (n, raw.starts_with("=rustudio.")),
                    None => {
                        let (disp, engine) = chunk_display(src);
                        let d = s.names.get(&disp);
                        // Remember raw → display too.
                        let a: Arc<str> = Arc::from(raw);
                        s.names.index.insert(a, d);
                        (d, engine)
                    }
                }
            }
        };
        let name_n = match std::str::from_utf8(name) {
            Ok(n) => s.names.get(n),
            Err(_) => s.names.get(&String::from_utf8_lossy(name)),
        };
        let line = if script.is_some() { u32::MAX } else { line };
        if let Some(&f) = s.func_index.get(&(script_n, name_n, line)) {
            if native {
                s.funcs[f as usize].native = true;
            }
            return f;
        }
        let id = s.funcs.len() as u32;
        s.funcs.push(Func { script: script_n, name: name_n, line, native, engine, c, lines: HashMap::new() });
        s.func_index.insert((script_n, name_n, line), id);
        id
    }
}


// ------------------------------------------------------------------ exact

/// Exact mode: Luau's single-step interpreter calls `rustudio_callhook` on
/// every Lua function entry / return and around every C call (vendored
/// lvmexecute.cpp). Each OS thread keeps its own shadow stacks (one per
/// lua_State) and call tree; touched nodes are flushed into the shared
/// tree when an outermost resumption ends.
pub(crate) mod exact {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::{c_char, c_int, c_void};

    /// Bumped by clear_recording: thread-local trees start over.
    pub(super) static EPOCH: AtomicU64 = AtomicU64::new(0);

    pub fn install() {
        // The VM reads it only in single-step mode, turned on after this.
        unsafe { ffi::lua_rs_setcallhook(Some(hook)) };
    }

    pub fn set_singlestep(th: &mlua::Thread, on: bool) {
        // A thread's pointer is its lua_State.
        let l = th.to_pointer() as *mut ffi::lua_State;
        if !l.is_null() {
            unsafe { ffi::lua_singlestep(l, on as c_int) };
        }
    }

    #[derive(Clone)]
    pub(crate) struct XNode {
        pub func: u32,
        pub parent: u32,
        pub first_child: u32,
        pub next: u32,
        pub calls: u64,
        pub total_ns: u64,
        pub self_ns: u64,
        pub max_ns: u64,
    }

    #[derive(Clone)]
    struct LNode {
        func: u32,
        parent: u32,
        first_child: u32,
        next: u32,
        root: Option<(VmKind, Name)>,
        global: u32,
        calls: u64,
        total: u64,
        self_: u64,
        max: u64,
        dirty: bool,
    }

    struct XFrame {
        node: u32,
        start: u64,
        child: u64,
        depth: i32,
    }

    #[derive(Default)]
    struct Stack {
        frames: Vec<XFrame>,
        paused_at: u64,
    }

    #[derive(Default)]
    struct Local {
        epoch: u64,
        cur: usize,
        stacks: HashMap<usize, Stack>,
        nodes: Vec<LNode>,
        roots: HashMap<(VmKind, Name), u32>,
        funcs: HashMap<(usize, usize), u32>,
        dirty: Vec<u32>,
        pending_root: Option<u32>,
    }

    thread_local! {
        static LOCAL: RefCell<Local> = RefCell::new(Local::default());
    }

    fn now_ns() -> u64 {
        epoch().elapsed().as_nanos() as u64
    }

    impl Local {
        fn check_epoch(&mut self) {
            let e = EPOCH.load(Relaxed);
            if self.epoch != e {
                let cur = self.cur;
                *self = Local { epoch: e, cur, ..Local::default() };
            }
        }

        fn switch(&mut self, to: usize, now: u64) {
            if to == self.cur {
                return;
            }
            if let Some(st) = self.stacks.get_mut(&self.cur) {
                st.paused_at = now;
            }
            self.cur = to;
            if let Some(st) = self.stacks.get_mut(&to) {
                if st.paused_at != 0 {
                    let shift = now.saturating_sub(st.paused_at);
                    for f in &mut st.frames {
                        f.start += shift;
                    }
                    st.paused_at = 0;
                }
            }
        }

        fn root(&mut self, kind: VmKind, script: Name) -> u32 {
            if let Some(&r) = self.roots.get(&(kind, script)) {
                return r;
            }
            let func = sampler::intern_func(&mut state(), b"", b"", 0, false, false, Some(script));
            let id = self.nodes.len() as u32;
            self.nodes.push(LNode {
                func,
                parent: NONE,
                first_child: NONE,
                next: NONE,
                root: Some((kind, script)),
                global: NONE,
                calls: 0,
                total: 0,
                self_: 0,
                max: 0,
                dirty: false,
            });
            self.roots.insert((kind, script), id);
            id
        }

        fn child(&mut self, parent: u32, func: u32) -> u32 {
            let mut c = self.nodes[parent as usize].first_child;
            while c != NONE {
                if self.nodes[c as usize].func == func {
                    return c;
                }
                c = self.nodes[c as usize].next;
            }
            let id = self.nodes.len() as u32;
            let next = self.nodes[parent as usize].first_child;
            self.nodes.push(LNode {
                func,
                parent,
                first_child: NONE,
                next,
                root: None,
                global: NONE,
                calls: 0,
                total: 0,
                self_: 0,
                max: 0,
                dirty: false,
            });
            self.nodes[parent as usize].first_child = id;
            id
        }

        /// Ends the top frame of the current stack at `now`.
        fn finish_top(&mut self, now: u64) {
            let Some(st) = self.stacks.get_mut(&self.cur) else { return };
            let Some(f) = st.frames.pop() else { return };
            let total = now.saturating_sub(f.start);
            let self_ = total.saturating_sub(f.child);
            if let Some(p) = st.frames.last_mut() {
                p.child += total;
            }
            let n = &mut self.nodes[f.node as usize];
            n.calls += 1;
            n.total += total;
            n.self_ += self_;
            n.max = n.max.max(total);
            if !n.dirty {
                n.dirty = true;
                self.dirty.push(f.node);
            }
        }

        /// Frames at `depth` or deeper whose return the hook never saw
        /// (errors unwound them): end them now.
        fn unwind_to(&mut self, depth: i32, now: u64) {
            while self.stacks.get(&self.cur).and_then(|s| s.frames.last()).is_some_and(|f| f.depth >= depth) {
                self.finish_top(now);
            }
        }

        fn top_node(&self) -> Option<u32> {
            self.stacks.get(&self.cur).and_then(|s| s.frames.last()).map(|f| f.node)
        }

        fn flush(&mut self) {
            if self.dirty.is_empty() {
                return;
            }
            let mut dirty = std::mem::take(&mut self.dirty);
            dirty.sort_unstable();
            let mut s = state();
            for i in dirty {
                let g = self.map(&mut s, i);
                let n = &mut self.nodes[i as usize];
                let x = &mut s.xnodes[g as usize];
                x.calls += n.calls;
                x.total_ns += n.total;
                x.self_ns += n.self_;
                x.max_ns = x.max_ns.max(n.max);
                n.calls = 0;
                n.total = 0;
                n.self_ = 0;
                n.max = 0;
                n.dirty = false;
            }
        }

        /// The shared node for local node `i` (made on first flush).
        fn map(&mut self, s: &mut State, i: u32) -> u32 {
            let n = self.nodes[i as usize].clone();
            if n.global != NONE {
                return n.global;
            }
            let new = |s: &mut State, func: u32, parent: u32| {
                let id = s.xnodes.len() as u32;
                let next = if parent == NONE { NONE } else { s.xnodes[parent as usize].first_child };
                s.xnodes.push(XNode { func, parent, first_child: NONE, next, calls: 0, total_ns: 0, self_ns: 0, max_ns: 0 });
                if parent != NONE {
                    s.xnodes[parent as usize].first_child = id;
                }
                id
            };
            let g = if let Some(key) = n.root {
                match s.xroots.get(&key) {
                    Some(&g) => g,
                    None => {
                        let g = new(s, n.func, NONE);
                        s.xroots.insert(key, g);
                        g
                    }
                }
            } else {
                let gp = self.map(s, n.parent);
                let mut c = s.xnodes[gp as usize].first_child;
                let mut found = NONE;
                while c != NONE {
                    if s.xnodes[c as usize].func == n.func {
                        found = c;
                        break;
                    }
                    c = s.xnodes[c as usize].next;
                }
                if found == NONE {
                    new(s, n.func, gp)
                } else {
                    found
                }
            };
            self.nodes[i as usize].global = g;
            g
        }
    }

    /// What `enter` replaced (restored by `leave`).
    pub(crate) struct Prev {
        cur: usize,
        pending_root: Option<u32>,
    }

    /// A timed resumption of thread `th` starts.
    pub fn enter(th: usize, kind: VmKind, script: Option<Name>) -> Prev {
        LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            l.check_epoch();
            let prev = Prev { cur: l.cur, pending_root: l.pending_root };
            l.switch(th, now_ns());
            l.pending_root = script.map(|n| l.root(kind, n));
            prev
        })
    }

    /// It ended (`finished`: the thread is dead or errored).
    pub fn leave(th: usize, prev: Prev, finished: bool) {
        LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            let now = now_ns();
            if finished {
                l.cur = th;
                l.unwind_to(i32::MIN, now);
                l.stacks.remove(&th);
            }
            l.switch(prev.cur, now);
            l.pending_root = prev.pending_root;
            if prev.cur == 0 {
                l.cur = 0;
                l.flush();
            }
        })
    }

    /// The function behind a hook event (cached by key and name pointer).
    unsafe fn func(l: &mut Local, state: *mut ffi::lua_State, event: c_int, key: *const c_void, name: *const c_char) -> u32 {
        let cstr = |p: *const c_char| if p.is_null() { &b""[..] } else { std::ffi::CStr::from_ptr(p).to_bytes() };
        // C names can come from a reused buffer and Rust callbacks share one
        // C trampoline: key C functions by their name's text.
        let k = if event == 2 {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cstr(name).hash(&mut h);
            (key as usize, h.finish() as usize)
        } else {
            (key as usize, 0)
        };
        if let Some(&f) = l.funcs.get(&k) {
            return f;
        }
        let f = if event == 2 {
            let n = cstr(name);
            sampler::intern_func(&mut super::state(), b"", if n.is_empty() { b"?" } else { n }, 0, false, true, None)
        } else {
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            if ffi::lua_getinfo(state, 0, c"snN".as_ptr(), &mut ar) == 0 {
                return sampler::intern_func(&mut super::state(), b"?", b"?", 0, false, false, None);
            }
            let native = ar.userdata as usize & 1 != 0;
            sampler::intern_func(&mut super::state(), cstr(ar.source), cstr(ar.name), ar.linedefined.max(0) as u32, native, false, None)
        };
        if l.funcs.len() > 1 << 16 {
            l.funcs.clear();
        }
        l.funcs.insert(k, f);
        f
    }

    unsafe extern "C" fn hook(state: *mut ffi::lua_State, event: c_int, key: *const c_void, name: *const c_char) {
        if FLAGS.load(Relaxed) & EXACT == 0 {
            return;
        }
        let now = now_ns();
        let depth = ffi::lua_stackdepth(state);
        let _ = LOCAL.try_with(|l| {
            let Ok(mut l) = l.try_borrow_mut() else { return };
            let l = &mut *l;
            l.check_epoch();
            let lp = state as usize;
            let mut hint = None;
            if lp != l.cur {
                // A coroutine resumed from Lua: its first frame hangs under
                // the caller's.
                hint = l.top_node();
                l.switch(lp, now);
            }
            match event {
                0 | 2 => {
                    l.unwind_to(depth, now);
                    let f = func(l, state, event, key, name);
                    let parent = l.top_node().or(hint).or(l.pending_root);
                    let Some(parent) = parent else { return };
                    let node = l.child(parent, f);
                    l.stacks.entry(lp).or_default().frames.push(XFrame { node, start: now, child: 0, depth });
                }
                _ => {
                    l.unwind_to(depth + 1, now);
                    if l.stacks.get(&lp).and_then(|s| s.frames.last()).is_some_and(|f| f.depth == depth) {
                        l.finish_top(now);
                    }
                    if l.stacks.get(&lp).is_some_and(|s| s.frames.is_empty()) {
                        // Keep the map small: empty stacks go.
                        l.stacks.remove(&lp);
                    }
                }
            }
        });
    }

    /// Flushes this OS thread's tree (Runtime::step: between resumptions).
    pub fn flush_idle() {
        let _ = LOCAL.try_with(|l| {
            if let Ok(mut l) = l.try_borrow_mut() {
                if l.cur == 0 {
                    l.flush();
                }
            }
        });
    }
}

// ------------------------------------------------------------------ timeline

/// Scripts on the frame profiler's timeline (`rustudio_core::profiler`):
/// each resumption is a `Cat::Script` scope named by its script path, and
/// debug.profilebegin markers nest inside it.
pub(crate) mod timeline {
    use rustudio_core::profiler::{self as p, Cat, Guard, Label};
    use std::cell::RefCell;

    #[inline]
    pub fn on() -> bool {
        p::active()
    }

    pub struct Span(#[allow(dead_code)] Option<Guard>);

    thread_local! {
        /// Script name → core label (interning hashes the string).
        static LABELS: RefCell<Vec<Option<Label>>> = const { RefCell::new(Vec::new()) };
        static OPEN: RefCell<Vec<Guard>> = const { RefCell::new(Vec::new()) };
    }

    pub fn begin(script: Option<super::Name>) -> Span {
        if !on() {
            return Span(None);
        }
        let label = match script {
            Some(n) => LABELS.with(|l| {
                let mut l = l.borrow_mut();
                if l.len() <= n as usize {
                    l.resize(n as usize + 1, None);
                }
                *l[n as usize].get_or_insert_with(|| {
                    // (the name first: never hold the state lock across intern)
                    let name = super::state().names.str(n);
                    p::intern(&name)
                })
            }),
            None => p::intern("(script)"),
        };
        Span(Some(p::scope_dyn(label, Cat::Script)))
    }

    pub fn marker_begin(label: &str) {
        if on() {
            let l = p::intern(label);
            OPEN.with(|o| o.borrow_mut().push(p::scope_dyn(l, Cat::Script)));
        }
    }

    pub fn marker_end() {
        OPEN.with(|o| drop(o.borrow_mut().pop()));
    }
}

// ------------------------------------------------------------------ snapshot

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EntryRow {
    pub kind: EntryKind,
    /// Handler function ("name:line" in its script) when known.
    pub handler: Option<Arc<str>>,
    /// Event name (Heartbeat, Touched, …) when known.
    pub event: Option<Arc<str>>,
    pub calls: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub self_work: u64,
    pub total_ms: f64,
    pub self_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ScriptRow {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub self_work: u64,
    pub vm: VmKind,
    pub path: Arc<str>,
    pub native: bool,
    pub resumes: u64,
    pub total_ms: f64,
    pub self_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    /// Self time per VM frame over the recording.
    pub ms_per_frame: f64,
    /// Share of the last full second spent in this script (%).
    pub activity_pct: f64,
    /// Resumes in the last full second.
    pub rate_hz: u32,
    pub heap_bytes: u64,
    pub threads: u32,
    pub samples: u64,
    pub entries: Vec<EntryRow>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FunctionRow {
    pub vm: VmKind,
    /// Script that defines the function.
    pub script: Arc<str>,
    pub name: Arc<str>,
    /// Line the function is defined on (0 = main chunk / unknown).
    pub line: u32,
    pub native: bool,
    pub engine: bool,
    pub c: bool,
    pub self_samples: u64,
    pub total_samples: u64,
    /// Estimated from sample share × the exact time of the resumed script.
    pub self_ms: f64,
    pub total_ms: f64,
    /// Hottest lines by self samples (line, samples), best first.
    pub hot_lines: Vec<(u32, u64)>,
    /// Exact mode: calls, inclusive / self time (interpreted), slowest call.
    pub calls: u64,
    pub exact_self_ms: f64,
    pub exact_total_ms: f64,
    pub max_ms: f64,
}

/// A call-tree node; `parent` indexes `Snapshot::tree` (None for roots:
/// one root per resumed script and VM).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TreeNode {
    pub parent: Option<u32>,
    pub depth: u32,
    pub vm: VmKind,
    pub script: Arc<str>,
    pub name: Arc<str>,
    pub line: u32,
    pub native: bool,
    pub engine: bool,
    pub c: bool,
    /// Sampled tree: samples; exact tree: 0.
    pub self_samples: u64,
    pub total_samples: u64,
    pub self_ms: f64,
    pub total_ms: f64,
    /// Exact tree: calls and the slowest call; sampled tree: 0.
    pub calls: u64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MarkerRow {
    pub vm: VmKind,
    pub script: Option<Arc<str>>,
    pub label: Arc<str>,
    pub calls: u64,
    pub total_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct VmRow {
    pub vm: VmKind,
    pub frames: u64,
    pub heap_bytes: u64,
    pub heap_engine_bytes: u64,
    pub script_ms: f64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Unit of the optional work counters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit: Option<String>,
    pub live: bool,
    pub recording: bool,
    pub sample_hz: u32,
    /// Seconds recorded (sampling on).
    pub recorded_s: f64,
    pub samples: u64,
    pub missed_samples: u64,
    /// Average cost of one sample (µs).
    pub sample_cost_us: f64,
    pub vms: Vec<VmRow>,
    pub scripts: Vec<ScriptRow>,
    pub functions: Vec<FunctionRow>,
    /// Sampled call tree. Depth-first: children follow their parent,
    /// heaviest first; one root per (VM, resumed script).
    pub tree: Vec<TreeNode>,
    pub exact: bool,
    /// Exact-mode call tree, same layout.
    pub exact_tree: Vec<TreeNode>,
    pub markers: Vec<MarkerRow>,
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// Everything recorded so far (cheap enough to call every UI frame at a
/// few Hz; the tab throttles it).
pub fn snapshot() -> Snapshot {
    // Copy the tables under the lock (every script resume takes it), build
    // outside it: an exact tree of 30k nodes took 7.5 ms to build.
    let s = {
        let g = state();
        let t0 = Instant::now();
        let copy = State {
            names: Names { list: g.names.list.clone(), index: HashMap::new() },
            funcs: g.funcs.clone(),
            func_index: HashMap::new(),
            nodes: g.nodes.clone(),
            roots: g.roots.clone(),
            scripts: g.scripts.clone(),
            markers: g.markers.clone(),
            vms: g.vms.clone(),
            samples: g.samples,
            missed: g.missed,
            rec_started: g.rec_started,
            rec_elapsed: g.rec_elapsed,
            sample_ns: g.sample_ns,
            xnodes: g.xnodes.clone(),
            xroots: g.xroots.clone(),
        };
        SNAPSHOT_LOCK_NS.store(t0.elapsed().as_nanos() as u64, Relaxed);
        copy
    };
    snapshot_of(&s)
}

static SNAPSHOT_LOCK_NS: AtomicU64 = AtomicU64::new(0);

/// How long the last `snapshot()` held the profiler's lock (ns): the time
/// script resumes on the gameplay thread could wait for it.
pub fn last_snapshot_lock_ns() -> u64 {
    SNAPSHOT_LOCK_NS.load(Relaxed)
}

fn snapshot_of(s: &State) -> Snapshot {
    let names = |n: Name| s.names.str(n);
    let opt = |n: Name| (n != NONE).then(|| s.names.str(n));
    let sec = now_sec();
    let recorded = s.rec_elapsed + s.rec_started.map(|t| t.elapsed()).unwrap_or_default();

    // Exact self ns per (vm, script): scales the sampled tree.
    let exact_self: HashMap<(VmKind, Name), u64> = s.scripts.iter().map(|(k, v)| (*k, v.acc.self_ns)).collect();

    let mut scripts: Vec<ScriptRow> = s
        .scripts
        .iter()
        .map(|(&(vm, n), st)| {
            let (activity, rate) = st.window.last_second(sec);
            let frames = s.vms.get(&vm).map(|v| v.frames).unwrap_or(0).max(1);
            let samples = s.roots.get(&(vm, n)).map(|&r| s.nodes[r as usize].total_samples).unwrap_or(0);
            let mut entries: Vec<EntryRow> = st
                .entries
                .iter()
                .map(|(&(kind, h, e), a)| EntryRow {
                    kind,
                    handler: opt(h),
                    event: opt(e),
                    calls: a.calls,
                    self_work: a.self_work,
                    total_ms: ms(a.total_ns),
                    self_ms: ms(a.self_ns),
                    avg_ms: ms(a.total_ns) / a.calls.max(1) as f64,
                    max_ms: ms(a.max_ns),
                })
                .collect();
            entries.sort_by(|a, b| b.self_ms.total_cmp(&a.self_ms));
            ScriptRow {
                self_work: st.acc.self_work,
                vm,
                path: names(n),
                native: st.native,
                resumes: st.acc.calls,
                total_ms: ms(st.acc.total_ns),
                self_ms: ms(st.acc.self_ns),
                avg_ms: ms(st.acc.total_ns) / st.acc.calls.max(1) as f64,
                max_ms: ms(st.acc.max_ns),
                ms_per_frame: ms(st.acc.self_ns) / frames as f64,
                activity_pct: activity,
                rate_hz: rate,
                heap_bytes: st.heap,
                threads: st.threads,
                samples,
                entries,
            }
        })
        .collect();
    scripts.sort_by(|a, b| b.self_ms.total_cmp(&a.self_ms).then(b.heap_bytes.cmp(&a.heap_bytes)));

    // Trees, depth-first, children heaviest first; per-function totals
    // (a recursive function counts once per path in its total).
    #[derive(Default)]
    struct FAcc {
        ss: u64,
        ts: u64,
        sm: f64,
        tm: f64,
        calls: u64,
        xs: f64,
        xt: f64,
        max: f64,
    }
    let mut func_acc: HashMap<(VmKind, u32), FAcc> = HashMap::new();
    struct G {
        func: u32,
        first_child: u32,
        next: u32,
        self_samples: u64,
        total_samples: u64,
        self_ms: f64,
        total_ms: f64,
        calls: u64,
        max_ms: f64,
    }
    let walk = |roots: &HashMap<(VmKind, Name), u32>,
                get: &dyn Fn(u32, f64) -> G,
                scale_of: &dyn Fn(VmKind, Name, u32) -> f64,
                exact: bool,
                func_acc: &mut HashMap<(VmKind, u32), FAcc>|
     -> Vec<TreeNode> {
        let mut tree = Vec::new();
        let mut roots: Vec<((VmKind, Name), u32, f64)> = roots
            .iter()
            .map(|(&k, &r)| {
                let scale = scale_of(k.0, k.1, r);
                let g = get(r, scale);
                (k, r, g.total_ms)
            })
            .collect();
        roots.sort_by(|a, b| b.2.total_cmp(&a.2));
        for ((vm, script), root, _) in roots {
            let scale = scale_of(vm, script, root);
            let mut stack = vec![(root, None::<u32>, 0u32)];
            let mut on_path: HashMap<u32, u32> = HashMap::new();
            let mut path: Vec<u32> = Vec::new();
            while let Some((node, parent, depth)) = stack.pop() {
                while path.len() > depth as usize {
                    if let Some(f) = path.pop() {
                        if let Some(c) = on_path.get_mut(&f) {
                            *c -= 1;
                        }
                    }
                }
                let g = get(node, scale);
                let f = &s.funcs[g.func as usize];
                let idx = tree.len() as u32;
                tree.push(TreeNode {
                    parent,
                    depth,
                    vm,
                    script: names(f.script),
                    name: names(f.name),
                    line: if f.line == u32::MAX { 0 } else { f.line },
                    native: f.native,
                    engine: f.engine,
                    c: f.c,
                    self_samples: g.self_samples,
                    total_samples: g.total_samples,
                    self_ms: g.self_ms,
                    total_ms: g.total_ms,
                    calls: g.calls,
                    max_ms: g.max_ms,
                });
                if parent.is_some() {
                    let first = on_path.get(&g.func).copied().unwrap_or(0) == 0;
                    let e = func_acc.entry((vm, g.func)).or_default();
                    if exact {
                        e.xs += g.self_ms;
                        e.calls += g.calls;
                        e.max = e.max.max(g.max_ms);
                        if first {
                            e.xt += g.total_ms;
                        }
                    } else {
                        e.ss += g.self_samples;
                        e.sm += g.self_ms;
                        if first {
                            e.ts += g.total_samples;
                            e.tm += g.total_ms;
                        }
                    }
                }
                *on_path.entry(g.func).or_default() += 1;
                path.push(g.func);
                let mut kids = Vec::new();
                let mut c = g.first_child;
                while c != NONE {
                    kids.push((c, get(c, scale).total_ms));
                    c = get(c, scale).next;
                }
                kids.sort_by(|a, b| a.1.total_cmp(&b.1));
                for (k, _) in kids {
                    stack.push((k, Some(idx), depth + 1));
                }
            }
        }
        tree
    };
    let sampled_get = |i: u32, scale: f64| {
        let n = s.nodes[i as usize];
        G {
            func: n.func,
            first_child: n.first_child,
            next: n.next,
            self_samples: n.self_samples,
            total_samples: n.total_samples,
            self_ms: n.self_samples as f64 * scale,
            total_ms: n.total_samples as f64 * scale,
            calls: 0,
            max_ms: 0.0,
        }
    };
    let sampled_scale = |vm: VmKind, script: Name, root: u32| {
        let total = s.nodes[root as usize].total_samples.max(1);
        ms(exact_self.get(&(vm, script)).copied().unwrap_or(0)) / total as f64
    };
    let tree = walk(&s.roots, &sampled_get, &sampled_scale, false, &mut func_acc);
    // Exact roots have no frame of their own: their total is their children's.
    let exact_get = |i: u32, _scale: f64| {
        let n = &s.xnodes[i as usize];
        let total = if n.parent == NONE {
            let mut t = 0;
            let mut c = n.first_child;
            while c != NONE {
                t += s.xnodes[c as usize].total_ns;
                c = s.xnodes[c as usize].next;
            }
            t
        } else {
            n.total_ns
        };
        G {
            func: n.func,
            first_child: n.first_child,
            next: n.next,
            self_samples: 0,
            total_samples: 0,
            self_ms: ms(n.self_ns),
            total_ms: ms(total),
            calls: n.calls,
            max_ms: ms(n.max_ns),
        }
    };
    let exact_tree = walk(&s.xroots, &exact_get, &|_, _, _| 1.0, true, &mut func_acc);

    let mut functions: Vec<FunctionRow> = func_acc
        .into_iter()
        .map(|((vm, fid), a)| {
            let f = &s.funcs[fid as usize];
            let mut hot: Vec<(u32, u64)> = f.lines.iter().map(|(&l, &n)| (l, n)).collect();
            hot.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            hot.truncate(8);
            FunctionRow {
                vm,
                script: names(f.script),
                name: names(f.name),
                line: f.line,
                native: f.native,
                engine: f.engine,
                c: f.c,
                self_samples: a.ss,
                total_samples: a.ts,
                self_ms: a.sm,
                total_ms: a.tm,
                hot_lines: hot,
                calls: a.calls,
                exact_self_ms: a.xs,
                exact_total_ms: a.xt,
                max_ms: a.max,
            }
        })
        .collect();
    functions.sort_by(|a, b| {
        (b.self_ms + b.exact_self_ms).total_cmp(&(a.self_ms + a.exact_self_ms)).then(b.self_samples.cmp(&a.self_samples))
    });

    let mut markers: Vec<MarkerRow> = s
        .markers
        .iter()
        .map(|(&(vm, script, label), a)| MarkerRow {
            vm,
            script: opt(script),
            label: names(label),
            calls: a.calls,
            total_ms: ms(a.total_ns),
            avg_ms: ms(a.total_ns) / a.calls.max(1) as f64,
            max_ms: ms(a.max_ns),
        })
        .collect();
    markers.sort_by(|a, b| b.total_ms.total_cmp(&a.total_ms));

    let mut vms: Vec<VmRow> = s
        .vms
        .iter()
        .map(|(&vm, v)| VmRow {
            vm,
            frames: v.frames,
            heap_bytes: v.heap_total,
            heap_engine_bytes: v.heap_other,
            script_ms: s.scripts.iter().filter(|(k, _)| k.0 == vm).map(|(_, st)| ms(st.acc.self_ns)).sum(),
        })
        .collect();
    vms.sort_by_key(|v| v.vm);

    Snapshot {
        work_unit: PROFILE_WORK.load(Relaxed).then(|| {
            if cfg!(windows) { "cycles" } else { "instructions" }.to_owned()
        }),
        live: live_stats(),
        recording: recording(),
        sample_hz: sample_hz(),
        recorded_s: recorded.as_secs_f64(),
        samples: s.samples,
        missed_samples: s.missed,
        sample_cost_us: s.sample_ns as f64 / 1e3 / s.samples.max(1) as f64,
        vms,
        scripts,
        functions,
        tree,
        exact: exact(),
        exact_tree,
        markers,
    }
}

impl Snapshot {
    /// Folded stacks ("vm;script;fn;fn N" per line, N in µs): flamegraph.pl,
    /// speedscope and inferno read it.
    pub fn folded(&self) -> String {
        Self::fold(&self.tree)
    }

    /// Folded stacks of the exact tree (µs, interpreted timing).
    pub fn folded_exact(&self) -> String {
        Self::fold(&self.exact_tree)
    }

    fn fold(tree: &[TreeNode]) -> String {
        let mut out = String::new();
        let mut path: Vec<String> = Vec::new();
        for n in tree {
            path.truncate(n.depth as usize);
            let label = if n.parent.is_none() {
                format!("{} {}", n.vm.label(), n.script)
            } else if n.c {
                format!("[C] {}", if n.name.is_empty() { "?" } else { &n.name })
            } else {
                let name = if n.name.is_empty() { "<anonymous>" } else { &n.name };
                format!("{name} ({}:{})", n.script, n.line)
            };
            path.push(label.replace(';', ","));
            let us = (n.self_ms * 1000.0).round() as u64;
            if us > 0 {
                out.push_str(&path.join(";"));
                out.push_str(&format!(" {us}\n"));
            }
        }
        out
    }

    /// The JSON export (`write_capture`).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    /// Compact summary for bench reports: top scripts and functions.
    pub fn summary(&self, top: usize) -> serde_json::Value {
        serde_json::json!({
            "recorded_s": self.recorded_s,
            "work_unit": self.work_unit,
            "samples": self.samples,
            "missed_samples": self.missed_samples,
            "sample_cost_us": self.sample_cost_us,
            "sample_hz": self.sample_hz,
            "vms": self.vms,
            "scripts": self.scripts.iter().take(top).map(|s| serde_json::json!({
                "vm": s.vm, "path": s.path, "self_ms": s.self_ms, "ms_per_frame": s.ms_per_frame,
                "self_work": s.self_work,
                "resumes": s.resumes, "max_ms": s.max_ms, "heap_kb": s.heap_bytes / 1024,
                "native": s.native, "activity_pct": s.activity_pct, "rate_hz": s.rate_hz,
            })).collect::<Vec<_>>(),
            "functions": self.functions.iter().filter(|f| !f.engine).take(top).map(|f| serde_json::json!({
                "vm": f.vm, "fn": format!("{} ({}:{})", if f.name.is_empty() { "<anonymous>" } else { &f.name }, f.script, f.line),
                "self_ms": f.self_ms, "total_ms": f.total_ms, "self_samples": f.self_samples,
                "native": f.native, "hot_lines": f.hot_lines.iter().take(3).collect::<Vec<_>>(),
                "calls": f.calls, "exact_self_ms": f.exact_self_ms, "exact_total_ms": f.exact_total_ms, "max_ms": f.max_ms,
            })).collect::<Vec<_>>(),
        })
    }
}

/// The bench report's "scripts" entry (None when nothing was profiled);
/// writes RUSTUDIO_SCRIPT_PROFILE_OUT when set.
pub fn bench_report() -> Option<serde_json::Value> {
    if FLAGS.load(Relaxed) == 0 {
        return None;
    }
    if let Some(out) = std::env::var_os("RUSTUDIO_SCRIPT_PROFILE_OUT") {
        if let Err(e) = write_capture(std::path::Path::new(&out)) {
            eprintln!("script profile: can't write {}: {e}", out.to_string_lossy());
        }
    }
    Some(snapshot().summary(15))
}

/// Writes `OUT.json` (snapshot) and `OUT.folded` (folded stacks).
pub fn write_capture(out: &std::path::Path) -> std::io::Result<()> {
    let snap = snapshot();
    std::fs::write(out.with_extension("json"), serde_json::to_string_pretty(&snap.to_json()).unwrap_or_default())?;
    std::fs::write(out.with_extension("folded"), snap.folded())?;
    if !snap.exact_tree.is_empty() {
        std::fs::write(out.with_extension("exact.folded"), snap.folded_exact())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
