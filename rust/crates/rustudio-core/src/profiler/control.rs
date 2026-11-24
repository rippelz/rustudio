//! The gatherer and the profiler's switches: who wants recording, pause and
//! freeze, "capture the next N frames", spike capture, and the settings that
//! bound the ring.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::capture::{Capture, CounterTrack, FrameKindKey, FrameRec, MarkRec, SpanRec, Track, TrackKind};
use super::record::{self, Name, Pending, ACTIVE, INSTR};
use super::Cat;

/// What bounds and triggers a capture (the window persists these in the
/// rustudio settings and hands them over with `set_config`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Seconds of history the ring keeps.
    pub history_secs: f32,
    /// Seconds of history counters keep (at least `history_secs`).
    pub counter_history_secs: f32,
    /// The frame budget drawn as a line (ms).
    pub budget_ms: f32,
    /// A display frame longer than this is a spike (ms).
    pub spike_ms: f32,
    /// Freeze the capture shortly after the next spike.
    pub spike_capture: bool,
    /// Frames still recorded after a spike before freezing (context after it).
    pub spike_frames_after: u32,
    /// Scopes also count the thread's retired instructions.
    pub instr_mode: bool,
    /// Time GPU passes with timestamp queries while recording.
    pub gpu_timing: bool,
    /// Count the bytes each scope allocates (binaries with `CountingAlloc`).
    pub track_alloc: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            history_secs: 10.0,
            counter_history_secs: 60.0,
            budget_ms: 16.7,
            spike_ms: 33.3,
            spike_capture: false,
            spike_frames_after: 30,
            instr_mode: false,
            gpu_timing: true,
            track_alloc: true,
        }
    }
}

/// Why recording stopped by itself.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Frozen {
    /// A spike at this time (ns), of this many ms.
    Spike { at: u64, ms: f64 },
    /// "Capture next N frames" finished.
    Captured { frames: u32 },
}

/// What the window's toolbar shows.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub enabled: bool,
    pub paused: bool,
    pub frozen: Option<Frozen>,
    /// Frames left to record for "capture next N".
    pub capturing_left: Option<u32>,
    /// Frames left after a spike before freezing.
    pub spike_left: Option<u32>,
    pub spans: usize,
    pub dropped: u64,
    /// Time the gatherer took on the last display frame (its own overhead).
    pub gather_ns: u64,
}

struct Store {
    cfg: Config,
    capture: Capture,
    users: u32,
    paused: bool,
    frozen: Option<Frozen>,
    capture_left: Option<u32>,
    capture_total: u32,
    spike_left: Option<u32>,
    spike: Option<(u64, f64)>,
    /// Static names by address and length → name index.
    static_names: FxHashMap<(usize, usize), u32>,
    /// Interned labels → name index.
    label_names: FxHashMap<u32, u32>,
    /// Source location (address) → index + 1 in `capture.locations`.
    loc_ids: FxHashMap<usize, u32>,
    /// Name index → resolved category, for `Cat::Auto` spans.
    auto_cat: FxHashMap<u32, Cat>,
    /// Lane id → index in `capture.tracks`.
    track_of: FxHashMap<u32, usize>,
    /// Counter name address → index in `capture.counters`.
    counter_of: FxHashMap<(usize, usize), usize>,
    /// The last boundary per frame kind.
    last_boundary: HashMap<FrameKindKey, u64>,
    /// Emptied buffers handed back to each lane (keeps their capacity).
    spare: FxHashMap<u32, Pending>,
    gather_ns: u64,
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Store {
            cfg: Config::default(),
            capture: Capture::new(),
            users: 0,
            paused: false,
            frozen: None,
            capture_left: None,
            capture_total: 0,
            spike_left: None,
            spike: None,
            static_names: Default::default(),
            label_names: Default::default(),
            auto_cat: Default::default(),
            loc_ids: Default::default(),
            track_of: Default::default(),
            counter_of: Default::default(),
            last_boundary: HashMap::new(),
            spare: Default::default(),
            gather_ns: 0,
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Store> {
    store().lock().unwrap_or_else(|e| e.into_inner())
}

fn update_active(s: &Store) {
    let on = s.users > 0 && !s.paused && s.frozen.is_none();
    INSTR.store(on && s.cfg.instr_mode, Ordering::Relaxed);
    record::GPU.store(on && s.cfg.gpu_timing, Ordering::Relaxed);
    super::alloc::ON.store(on && s.cfg.track_alloc && super::alloc::installed(), Ordering::Relaxed);
    ACTIVE.store(on, Ordering::Relaxed);
}

/// Something wants recording (the window opened, a CLI capture began, the
/// player overlay). Pair with `disable`.
pub fn enable() {
    let mut s = lock();
    s.users += 1;
    if s.users == 1 {
        // a fresh start: no frame spans the time recording was off
        s.last_boundary.clear();
        s.capture.meta.instr_mode = s.cfg.instr_mode;
        s.capture.meta.alloc_tracked = s.cfg.track_alloc && super::alloc::installed();
    }
    update_active(&s);
}

/// What the GPU is ("AMD Radeon RX 9070 · Vulkan"): kept in the capture.
pub fn set_gpu_info(info: &str) {
    let mut s = lock();
    if s.capture.meta.gpu != info {
        s.capture.meta.gpu = info.to_string();
    }
}

/// Undoes one `enable`. The capture stays readable.
pub fn disable() {
    let mut s = lock();
    s.users = s.users.saturating_sub(1);
    update_active(&s);
    if s.users == 0 {
        drop(s);
        discard_pending();
    }
}

/// Whether an explicitly requested capture still needs frames.
pub fn capture_requested() -> bool {
    let s = lock();
    !s.paused && s.frozen.is_none()
        && (s.capture_left.is_some() || s.spike_left.is_some() || s.cfg.spike_capture)
}

pub fn is_enabled() -> bool {
    lock().users > 0
}

/// Stops recording but keeps the capture (the window's Pause).
pub fn pause() {
    let mut s = lock();
    s.paused = true;
    update_active(&s);
}

/// Records again after a pause or a freeze (spike / capture-N).
pub fn resume() {
    let mut s = lock();
    s.paused = false;
    s.frozen = None;
    s.capture_left = None;
    s.spike_left = None;
    s.last_boundary.clear();
    update_active(&s);
}

/// Why recording froze itself, if it did.
pub fn frozen() -> Option<Frozen> {
    lock().frozen
}

/// Clears the capture and records exactly the next `frames` display frames,
/// then freezes.
pub fn capture_next(frames: u32) {
    discard_pending();
    let mut s = lock();
    clear_locked(&mut s);
    s.capture_left = Some(frames.max(1));
    s.capture_total = frames.max(1);
    s.paused = false;
    s.frozen = None;
    s.spike_left = None;
    update_active(&s);
}

/// Throws the capture away (keeps recording if it was).
pub fn clear() {
    discard_pending();
    let mut s = lock();
    clear_locked(&mut s);
}

fn clear_locked(s: &mut Store) {
    let meta = std::mem::take(&mut s.capture.meta);
    s.capture = Capture::new();
    s.capture.meta = meta;
    s.capture.meta.instr_mode = s.cfg.instr_mode;
    s.capture.meta.alloc_tracked = s.cfg.track_alloc && super::alloc::installed();
    s.static_names.clear();
    s.label_names.clear();
    s.auto_cat.clear();
    s.loc_ids.clear();
    s.track_of.clear();
    s.counter_of.clear();
    s.last_boundary.clear();
    s.spike = None;
}

fn discard_pending() {
    let lanes: Vec<_> = record::threads().lock().unwrap_or_else(|e| e.into_inner()).clone();
    for l in lanes {
        let mut p = l.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.spans.clear();
        p.counters.clear();
        p.marks.clear();
        p.frames.clear();
    }
}

pub fn config() -> Config {
    lock().cfg.clone()
}

pub fn set_config(cfg: Config) {
    let mut s = lock();
    if s.cfg.instr_mode != cfg.instr_mode {
        // instruction counts and times don't mix in one capture
        clear_locked(&mut s);
        s.capture.meta.instr_mode = cfg.instr_mode;
    }
    if !cfg.spike_capture {
        s.spike_left = None;
    }
    s.capture.meta.alloc_tracked = cfg.track_alloc && super::alloc::installed();
    s.cfg = cfg;
    update_active(&s);
}

static BUILD: OnceLock<String> = OnceLock::new();

/// The binary's build id (git short sha, "+dirty"), set once at startup by
/// a binary that knows it; captures carry it in `Meta::build`.
pub fn set_build(id: &str) {
    let _ = BUILD.set(id.to_string());
    lock().capture.meta.build = id.to_string();
}

pub(crate) fn build() -> &'static str {
    BUILD.get().map(String::as_str).unwrap_or("")
}

/// The settings key the profiler's `Config` is saved under.
pub const SETTINGS_KEY: &str = "profiler";

/// Loads the saved `Config` (rustudio settings, key "profiler"), keeping
/// defaults for anything missing. Runs that don't load the user's settings
/// (tests, benches) get the defaults.
pub fn load_saved_config() {
    if let Some(v) = crate::rustudio_settings::saved_value(SETTINGS_KEY) {
        match serde_json::from_value::<Config>(v) {
            Ok(cfg) => set_config(cfg),
            Err(e) => eprintln!("rustudio: ignoring the saved profiler settings: {e}"),
        }
    }
}

/// Sets the config and saves it with the rustudio settings.
pub fn set_and_save_config(cfg: Config) {
    let v = serde_json::to_value(&cfg).ok();
    set_config(cfg);
    let default = serde_json::to_value(Config::default()).ok();
    crate::rustudio_settings::set(SETTINGS_KEY, v.filter(|v| Some(v) != default.as_ref()));
}

/// A free-form note on what was profiled ("editor: Place.rbxl", "play").
pub fn set_context(context: &str) {
    lock().capture.meta.context = context.to_string();
}

pub fn status() -> Status {
    let s = lock();
    Status {
        enabled: s.users > 0,
        paused: s.paused,
        frozen: s.frozen,
        capturing_left: s.capture_left,
        spike_left: s.spike_left,
        spans: s.capture.span_count(),
        dropped: s.capture.tracks.iter().map(|t| t.dropped).sum(),
        gather_ns: s.gather_ns,
    }
}

/// Reads the live capture without copying it.
pub fn with_capture<R>(f: impl FnOnce(&Capture) -> R) -> R {
    f(&lock().capture)
}

type ExtensionFn = fn() -> Option<serde_json::Value>;

fn extensions() -> &'static Mutex<Vec<(&'static str, ExtensionFn)>> {
    static E: Mutex<Vec<(&'static str, ExtensionFn)>> = Mutex::new(Vec::new());
    &E
}

/// Adds `key` → `f()` to every snapshot (and so every exported capture):
/// how other parts (the script profiler) put their tables in the file.
pub fn register_extension(key: &'static str, f: ExtensionFn) {
    let mut e = extensions().lock().unwrap_or_else(|e| e.into_inner());
    if !e.iter().any(|x| x.0 == key) {
        e.push((key, f));
    }
}

/// A copy of the live capture, with the registered extensions filled in.
pub fn snapshot() -> Capture {
    let mut c = lock().capture.clone();
    c.meta.load_avg = super::capture::load_avg();
    c.meta.instr_unit = if c.meta.instr_mode { super::capture::instr_unit_here().to_string() } else { String::new() };
    let ext: Vec<_> = extensions().lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (key, f) in ext {
        if let Some(v) = f() {
            c.extensions.insert(key.to_string(), v);
        }
    }
    c
}

/// The time and length (ms) of the spike that froze the capture, or the
/// last one seen while spike capture was armed.
pub fn last_spike() -> Option<(u64, f64)> {
    lock().spike
}

// ------------------------------------------------------------ gathering

/// Called by `frame(FrameKind::Render)`: moves every lane's records into the
/// capture, trims the ring, and runs the spike / capture-N bookkeeping.
pub(crate) fn gather(now: u64) {
    let t0 = std::time::Instant::now();
    let lanes: Vec<Arc<record::ThreadBuf>> = record::threads().lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut s = lock();
    let s = &mut *s;
    let mut dead = Vec::new();
    // lanes whose thread is still running (its thread-local holds a third reference)
    let live: Vec<u32> = lanes.iter().filter(|l| l.id >= record::VIRTUAL_BASE || Arc::strong_count(l) > 2).map(|l| l.id).collect();
    // counters sampled since the previous display boundary belong to this frame
    let frame_start = s.last_boundary.get(&FrameKindKey::Render).copied().unwrap_or(0);
    for lane in &lanes {
        let mut batch = s.spare.remove(&lane.id).unwrap_or_default();
        {
            let mut p = lane.pending.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::swap(&mut *p, &mut batch);
        }
        // (a thread that ended and has nothing left: forget it)
        if lane.id < record::VIRTUAL_BASE && Arc::strong_count(lane) <= 2 && batch.spans.is_empty() && batch.frames.is_empty() {
            dead.push(lane.id);
        }
        absorb(s, lane, &mut batch, frame_start, &live);
        batch.spans.clear();
        batch.counters.clear();
        batch.marks.clear();
        batch.frames.clear();
        batch.dropped = 0;
        s.spare.insert(lane.id, batch);
    }
    if !dead.is_empty() {
        let mut g = record::threads().lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|l| !(dead.contains(&l.id) && Arc::strong_count(l) <= 2));
        for id in dead {
            s.spare.remove(&id);
        }
    }

    // rate counters: the trailing second's sum, as a sample on the gathering thread
    {
        let mut rates = record::rates().lock().unwrap_or_else(|e| e.into_inner());
        for r in rates.iter_mut() {
            let amount = std::mem::take(&mut r.pending);
            if amount != 0.0 {
                r.window.push_back((now, amount));
            }
            while r.window.front().is_some_and(|w| now.saturating_sub(w.0) > 1_000_000_000) {
                r.window.pop_front();
            }
            // (+ 0.0: an empty f64 sum is -0.0, which prints as "-0")
            let per_sec: f64 = r.window.iter().map(|w| w.1).sum::<f64>() + 0.0;
            let key = (r.name.as_ptr() as usize, r.name.len());
            let ci = match s.counter_of.get(&key) {
                Some(&i) => i,
                None => {
                    s.capture.counters.push(CounterTrack { name: r.name.to_string(), cat: r.cat, unit: r.unit, samples: VecDeque::new() });
                    let i = s.capture.counters.len() - 1;
                    s.counter_of.insert(key, i);
                    i
                }
            };
            s.capture.counters[ci].samples.push_back((now, per_sec));
        }
    }

    // the ring
    let history = (s.cfg.history_secs.max(0.5) as f64 * 1e9) as u64;
    if s.capture_left.is_none() {
        let counters = ((s.cfg.counter_history_secs.max(s.cfg.history_secs).max(0.5)) as f64 * 1e9) as u64;
        s.capture.trim_before(now.saturating_sub(history), now.saturating_sub(counters));
    }

    // spike capture: freeze a few frames after a slow one
    let last_ms = s.capture.render_frames().back().map(|f| f.ms());
    if let Some(ms) = last_ms {
        if s.cfg.spike_capture && s.spike_left.is_none() && s.frozen.is_none() && ms > s.cfg.spike_ms as f64 {
            let at = s.capture.render_frames().back().map(|f| f.start).unwrap_or(now);
            s.spike = Some((at, ms));
            s.spike_left = Some(s.cfg.spike_frames_after);
        }
    }
    if let Some(left) = s.spike_left {
        if left == 0 {
            s.spike_left = None;
            if let Some((at, ms)) = s.spike {
                s.frozen = Some(Frozen::Spike { at, ms });
            }
        } else {
            s.spike_left = Some(left - 1);
        }
    }
    if let Some(left) = s.capture_left {
        let have = s.capture.render_frames().len() as u32;
        if have >= s.capture_total || left <= 1 {
            s.capture_left = None;
            s.frozen = Some(Frozen::Captured { frames: have });
        } else {
            s.capture_left = Some(s.capture_total - have);
        }
    }
    s.gather_ns = t0.elapsed().as_nanos() as u64;
    update_active(s);
}

fn name_index(s: &mut Store, name: Name) -> u32 {
    match name {
        Name::Static(n) => {
            let key = (n.as_ptr() as usize, n.len());
            if let Some(&i) = s.static_names.get(&key) {
                return i;
            }
            let i = intern_name(&mut s.capture, n);
            s.static_names.insert(key, i);
            i
        }
        Name::Dyn(l) => {
            if let Some(&i) = s.label_names.get(&l.0) {
                return i;
            }
            let i = intern_name(&mut s.capture, &record::label_name(l));
            s.label_names.insert(l.0, i);
            i
        }
    }
}

fn intern_name(c: &mut Capture, n: &str) -> u32 {
    // (two static strings with the same text share an entry)
    if let Some(i) = c.names.iter().position(|x| x == n) {
        return i as u32;
    }
    c.names.push(n.to_string());
    (c.names.len() - 1) as u32
}

fn absorb(s: &mut Store, lane: &record::ThreadBuf, batch: &mut Pending, frame_start: u64, live: &[u32]) {
    if batch.spans.is_empty() && batch.marks.is_empty() && batch.frames.is_empty() && batch.counters.is_empty() && batch.dropped == 0 {
        return;
    }
    let ti = match s.track_of.get(&lane.id) {
        Some(&i) => i,
        None => {
            // A short-lived thread (an HTTP request, a one-off loader) joins the
            // lane of an ended thread of the same name instead of adding one
            // lane per request, when their times don't overlap.
            let name = lane.name.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let first = batch.spans.iter().map(|x| x.start).min().unwrap_or(u64::MAX);
            let reuse = s.capture.tracks.iter().position(|t| {
                t.kind == TrackKind::Thread
                    && t.name == name
                    && t.id != lane.id
                    && !live.contains(&t.id)
                    && t.spans.back().is_none_or(|b| b.end <= first)
            });
            match reuse {
                Some(i) => {
                    s.track_of.insert(lane.id, i);
                    i
                }
                None => {
                    let kind = if lane.id >= record::VIRTUAL_BASE { TrackKind::Gpu } else { TrackKind::Thread };
                    s.capture.tracks.push(Track { id: lane.id, kind, ..Default::default() });
                    let i = s.capture.tracks.len() - 1;
                    s.track_of.insert(lane.id, i);
                    i
                }
            }
        }
    };
    {
        let name = lane.name.lock().unwrap_or_else(|e| e.into_inner());
        if s.capture.tracks[ti].name != *name {
            s.capture.tracks[ti].name = name.clone();
        }
    }
    s.capture.tracks[ti].dropped += batch.dropped;
    for sp in &batch.spans {
        let n = name_index(s, sp.name);
        let cat = if sp.cat == Cat::Auto {
            match s.auto_cat.get(&n) {
                Some(&c) => c,
                None => {
                    let c = Cat::from_name(s.capture.name(n));
                    s.auto_cat.insert(n, c);
                    c
                }
            }
        } else {
            sp.cat
        };
        let loc = match sp.loc {
            None => 0,
            Some(l) => {
                let key = l as *const _ as usize;
                match s.loc_ids.get(&key) {
                    Some(&i) => i,
                    None => {
                        s.capture.locations.push(format!("{}:{}", l.file().replace('\\', "/"), l.line()));
                        let i = s.capture.locations.len() as u32;
                        s.loc_ids.insert(key, i);
                        i
                    }
                }
            }
        };
        let rec = SpanRec { start: sp.start, end: sp.end, name: n, cat, depth: sp.depth, instr: sp.instr, alloc: sp.alloc, loc };
        let t = &mut s.capture.tracks[ti];
        t.max_dur = t.max_dur.max(rec.dur());
        // (an external lane may deliver slightly out of end order: keep the deque sorted)
        if t.spans.back().is_some_and(|b| b.end > rec.end) {
            let at = t.spans.partition_point(|x| x.end <= rec.end);
            t.spans.insert(at, rec);
        } else {
            t.spans.push_back(rec);
        }
    }
    for m in &batch.marks {
        let n = name_index(s, m.name);
        s.capture.tracks[ti].marks.push_back(MarkRec { t: m.t, name: n, cat: m.cat });
    }
    for &(t, kind) in &batch.frames {
        let key: FrameKindKey = kind.into();
        if key == FrameKindKey::Render {
            s.capture.render_track = Some(lane.id);
        }
        if let Some(prev) = s.last_boundary.insert(key, t) {
            if t > prev {
                s.capture.frames.entry(key).or_default().push_back(FrameRec { start: prev, end: t });
            }
        }
    }
    for c in &batch.counters {
        let key = (c.name.as_ptr() as usize, c.name.len());
        let ci = match s.counter_of.get(&key) {
            Some(&i) => i,
            None => {
                let i = match s.capture.counters.iter().position(|x| x.name == c.name) {
                    Some(i) => i,
                    None => {
                        s.capture.counters.push(CounterTrack {
                            name: c.name.to_string(),
                            cat: c.cat,
                            unit: c.unit,
                            samples: VecDeque::new(),
                        });
                        s.capture.counters.len() - 1
                    }
                };
                s.counter_of.insert(key, i);
                i
            }
        };
        let ct = &mut s.capture.counters[ci];
        // one sample per display frame: the latest wins
        match ct.samples.back_mut() {
            Some(last) if last.0 >= frame_start && c.t >= last.0 && frame_start > 0 => *last = (c.t, c.value),
            _ => ct.samples.push_back((c.t, c.value)),
        }
    }
}

/// Books a span measured elsewhere (GPU timestamps) on a named virtual lane.
/// `start`/`end` are profiler nanoseconds (`now_ns` scale).
pub fn lane_span(lane: &str, name: &'static str, cat: Cat, start: u64, end: u64) {
    if !record::active() {
        return;
    }
    thread_local! {
        static CACHE: std::cell::RefCell<Vec<(String, Arc<record::ThreadBuf>)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    let buf = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(b) = c.iter().find(|b| b.0 == lane) {
            return b.1.clone();
        }
        let b = record::virtual_lane(lane);
        c.push((lane.to_string(), b.clone()));
        b
    });
    record::push_external(
        &buf,
        record::RawSpan { start, end: end.max(start), instr: 0, alloc: 0, loc: None, name: Name::Static(name), cat, depth: 0 },
    );
}

/// A frame boundary on a virtual lane's frame kind measured elsewhere (the
/// GPU frame from timestamps), at time `t`.
pub fn frame_at(kind: super::FrameKind, t: u64) {
    if !record::active() {
        return;
    }
    let buf = record::virtual_lane("GPU");
    buf.pending.lock().unwrap_or_else(|e| e.into_inner()).frames.push((t, kind));
}
