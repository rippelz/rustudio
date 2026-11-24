//! The hot path: per-thread buffers that scopes, counters and marks append
//! to. A thread's buffer has its own lock, taken by that thread for each
//! record and by the gatherer once per display frame, so recording threads
//! never wait on each other.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

use super::capture::Unit;
use super::{Cat, FrameKind};

/// On while something reads the profiler (the window, a capture, the CLI)
/// and it isn't paused.
pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Scopes also count the thread's retired instructions.
pub(crate) static INSTR: AtomicBool = AtomicBool::new(false);
/// Recording and the GPU lane wanted (timestamp queries on).
pub(crate) static GPU: AtomicBool = AtomicBool::new(false);

/// Whether the renderer should time its passes for the profiler.
#[inline]
pub fn gpu_wanted() -> bool {
    GPU.load(Ordering::Relaxed)
}

/// Whether recording is on: one relaxed load. Callers that build something
/// costly for a scope (a label string) check this first.
#[inline(always)]
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Nanoseconds since the profiler's epoch (the first call in the process).
#[inline]
pub fn now_ns() -> u64 {
    epoch().elapsed().as_nanos() as u64
}

/// Wall-clock milliseconds since the Unix epoch at the profiler's epoch.
pub(crate) fn epoch_unix_ms() -> f64 {
    static AT: OnceLock<f64> = OnceLock::new();
    *AT.get_or_init(|| {
        let since = epoch().elapsed();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        (now.saturating_sub(since)).as_secs_f64() * 1000.0
    })
}

// ------------------------------------------------------------ labels

/// An interned runtime name (a script path, an event name): copyable, and
/// cheap to record. `intern` the string once and keep the label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub(crate) u32);

#[derive(Default)]
struct Interner {
    map: rustc_hash::FxHashMap<Arc<str>, u32>,
    names: Vec<Arc<str>>,
}

fn interner() -> &'static RwLock<Interner> {
    static I: OnceLock<RwLock<Interner>> = OnceLock::new();
    I.get_or_init(Default::default)
}

/// The label for `name`, the same one every time for the same text.
pub fn intern(name: &str) -> Label {
    if let Some(&id) = interner().read().unwrap_or_else(|e| e.into_inner()).map.get(name) {
        return Label(id);
    }
    let mut g = interner().write().unwrap_or_else(|e| e.into_inner());
    if let Some(&id) = g.map.get(name) {
        return Label(id);
    }
    let id = g.names.len() as u32;
    let s: Arc<str> = Arc::from(name);
    g.names.push(s.clone());
    g.map.insert(s, id);
    Label(id)
}

/// The text a label was interned from.
pub fn label_name(label: Label) -> Arc<str> {
    interner()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .names
        .get(label.0 as usize)
        .cloned()
        .unwrap_or_else(|| Arc::from("?"))
}

/// A recorded name: a string literal or an interned label.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Name {
    Static(&'static str),
    Dyn(Label),
}

// ------------------------------------------------------------ buffers

#[derive(Clone, Copy)]
pub(crate) struct RawSpan {
    pub start: u64,
    pub end: u64,
    pub instr: u64,
    pub alloc: u64,
    /// Where the scope was opened (`#[track_caller]`).
    pub loc: Option<&'static std::panic::Location<'static>>,
    pub name: Name,
    pub cat: Cat,
    pub depth: u8,
}

#[derive(Clone, Copy)]
pub(crate) struct RawCounter {
    pub t: u64,
    pub name: &'static str,
    pub cat: Cat,
    pub unit: Unit,
    pub value: f64,
}

#[derive(Clone, Copy)]
pub(crate) struct RawMark {
    pub t: u64,
    pub name: Name,
    pub cat: Cat,
}

#[derive(Default)]
pub(crate) struct Pending {
    pub spans: Vec<RawSpan>,
    pub counters: Vec<RawCounter>,
    pub marks: Vec<RawMark>,
    pub frames: Vec<(u64, FrameKind)>,
    /// Records thrown away because nothing gathered them in time.
    pub dropped: u64,
}

/// A thread that nothing gathers from (no display frames while it records)
/// keeps at most this many spans; later ones are counted as dropped.
const MAX_PENDING: usize = 1 << 19;

pub(crate) struct ThreadBuf {
    pub id: u32,
    pub name: Mutex<String>,
    pub pending: Mutex<Pending>,
}

pub(crate) fn threads() -> &'static Mutex<Vec<Arc<ThreadBuf>>> {
    static T: Mutex<Vec<Arc<ThreadBuf>>> = Mutex::new(Vec::new());
    &T
}

struct Local {
    buf: Arc<ThreadBuf>,
    depth: Cell<u8>,
}

thread_local! {
    static LOCAL: Local = {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = std::thread::current().name().map(str::to_string).unwrap_or_else(|| format!("thread {id}"));
        let buf = Arc::new(ThreadBuf { id, name: Mutex::new(name), pending: Mutex::new(Pending::default()) });
        threads().lock().unwrap_or_else(|e| e.into_inner()).push(buf.clone());
        Local { buf, depth: Cell::new(0) }
    };
}

#[inline]
fn with_pending(f: impl FnOnce(&mut Pending)) {
    let _ = LOCAL.try_with(|l| f(&mut l.buf.pending.lock().unwrap_or_else(|e| e.into_inner())));
}

/// Names this thread's lane in the timeline (otherwise its std name).
pub fn thread_name(name: &str) {
    let _ = LOCAL.try_with(|l| *l.buf.name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string());
}

// ------------------------------------------------------------ scopes

/// Times its scope: records start, end, nesting depth (and the thread's
/// instructions in instruction mode) when dropped. Inert while the profiler
/// is off.
#[must_use = "the scope ends when the guard drops"]
pub struct Guard {
    start: u64,
    instr: u64,
    alloc: u64,
    loc: &'static std::panic::Location<'static>,
    name: Name,
    cat: Cat,
    depth: u8,
    live: bool,
}

#[inline]
fn begin(name: Name, cat: Cat, loc: &'static std::panic::Location<'static>) -> Guard {
    if !active() {
        return Guard { start: 0, instr: 0, alloc: 0, loc, name, cat, depth: 0, live: false };
    }
    begin_live(name, cat, loc)
}

#[inline(never)]
fn begin_live(name: Name, cat: Cat, loc: &'static std::panic::Location<'static>) -> Guard {
    let depth = LOCAL
        .try_with(|l| {
            let d = l.depth.get();
            l.depth.set(d.saturating_add(1));
            d
        })
        .unwrap_or(0);
    let instr = if INSTR.load(Ordering::Relaxed) { crate::bench::instructions() } else { 0 };
    // (+1: 0 means "not counted" at the end even if nothing was allocated yet)
    let alloc = if super::alloc::ON.load(Ordering::Relaxed) { super::alloc::thread_allocated().wrapping_add(1) } else { 0 };
    Guard { start: now_ns(), instr, alloc, loc, name, cat, depth, live: true }
}

/// Times until the guard drops under a fixed name. The caller's file and
/// line go with it (`#[track_caller]`: a constant, nothing at run time).
#[inline]
#[track_caller]
pub fn scope(name: &'static str, cat: Cat) -> Guard {
    begin(Name::Static(name), cat, std::panic::Location::caller())
}

/// Times until the guard drops under an interned runtime name.
#[inline]
#[track_caller]
pub fn scope_dyn(label: Label, cat: Cat) -> Guard {
    begin(Name::Dyn(label), cat, std::panic::Location::caller())
}

impl Drop for Guard {
    #[inline]
    fn drop(&mut self) {
        if self.live {
            self.finish();
        }
    }
}

impl Guard {
    #[inline(never)]
    fn finish(&mut self) {
        let end = now_ns();
        let instr = if self.instr != 0 { crate::bench::instructions().saturating_sub(self.instr) } else { 0 };
        let alloc = if self.alloc != 0 { (super::alloc::thread_allocated().wrapping_add(1)).wrapping_sub(self.alloc) } else { 0 };
        let span = RawSpan { start: self.start, end, instr, alloc, loc: Some(self.loc), name: self.name, cat: self.cat, depth: self.depth };
        let _ = LOCAL.try_with(|l| {
            l.depth.set(l.depth.get().saturating_sub(1));
            let mut p = l.buf.pending.lock().unwrap_or_else(|e| e.into_inner());
            if p.spans.len() < MAX_PENDING {
                p.spans.push(span);
            } else {
                p.dropped += 1;
            }
        });
    }
}

// ------------------------------------------------------------ counters, marks, frames

/// A sampled value (counts): the last sample in each display frame is kept.
#[inline]
pub fn counter(name: &'static str, cat: Cat, value: f64) {
    counter_unit(name, cat, Unit::Count, value);
}

/// A sampled value with its unit.
#[inline]
pub fn counter_unit(name: &'static str, cat: Cat, unit: Unit, value: f64) {
    if !active() {
        return;
    }
    let t = now_ns();
    with_pending(|p| {
        if p.counters.len() < MAX_PENDING {
            p.counters.push(RawCounter { t, name, cat, unit, value });
        }
    });
}

/// Adds `amount` to a rate counter (bytes sent, packets…): the capture
/// shows it per second over the trailing second, sampled each display frame.
pub fn rate(name: &'static str, cat: Cat, unit: Unit, amount: f64) {
    if !active() {
        return;
    }
    let mut r = rates().lock().unwrap_or_else(|e| e.into_inner());
    match r.iter_mut().find(|x| std::ptr::eq(x.name, name) || x.name == name) {
        Some(x) => x.pending += amount,
        None => r.push(RateAcc { name, cat, unit, pending: amount, window: std::collections::VecDeque::new() }),
    }
}

pub(crate) struct RateAcc {
    pub name: &'static str,
    pub cat: Cat,
    /// The per-second unit (`BytesPerSec`, `Hz`…).
    pub unit: Unit,
    pub pending: f64,
    /// (time, amount) over the trailing second.
    pub window: std::collections::VecDeque<(u64, f64)>,
}

pub(crate) fn rates() -> &'static Mutex<Vec<RateAcc>> {
    static R: Mutex<Vec<RateAcc>> = Mutex::new(Vec::new());
    &R
}

/// An instant event on this thread.
#[inline]
pub fn mark(name: &'static str, cat: Cat) {
    if active() {
        let t = now_ns();
        with_pending(|p| p.marks.push(RawMark { t, name: Name::Static(name), cat }));
    }
}

/// An instant event under a runtime name.
#[inline]
pub fn mark_dyn(label: Label, cat: Cat) {
    if active() {
        let t = now_ns();
        with_pending(|p| p.marks.push(RawMark { t, name: Name::Dyn(label), cat }));
    }
}

/// A frame boundary of `kind`: ends the previous frame of that kind and
/// starts the next. The display loop's `FrameKind::Render` boundary also
/// gathers every thread's records into the capture (and runs spike and
/// capture-N bookkeeping), so call it once per display frame on the thread
/// that draws.
pub fn frame(kind: FrameKind) {
    if !active() {
        // A pending "capture next N" or an armed spike capture still needs
        // the gatherer to notice when it was paused; nothing else to do.
        return;
    }
    let t = now_ns();
    with_pending(|p| p.frames.push((t, kind)));
    if kind == FrameKind::Render {
        super::control::gather(t);
    }
}

/// Book a span recorded elsewhere (GPU timestamps, another process) on this
/// thread's buffer under an explicit lane: used by the GPU timing reader.
pub(crate) fn push_external(lane: &Arc<ThreadBuf>, span: RawSpan) {
    let mut p = lane.pending.lock().unwrap_or_else(|e| e.into_inner());
    if p.spans.len() < MAX_PENDING {
        p.spans.push(span);
    } else {
        p.dropped += 1;
    }
}

/// A lane that no OS thread owns (the GPU), registered once by name.
pub(crate) fn virtual_lane(name: &str) -> Arc<ThreadBuf> {
    let mut g = threads().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = g.iter().find(|b| b.id >= VIRTUAL_BASE && *b.name.lock().unwrap_or_else(|e| e.into_inner()) == name) {
        return b.clone();
    }
    static NEXT: AtomicU32 = AtomicU32::new(VIRTUAL_BASE);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let buf = Arc::new(ThreadBuf { id, name: Mutex::new(name.to_string()), pending: Mutex::new(Pending::default()) });
    g.push(buf.clone());
    buf
}

/// Lane ids from here up belong to virtual lanes (GPU queues).
pub(crate) const VIRTUAL_BASE: u32 = 1 << 30;
