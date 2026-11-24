//! The gathered capture: a self-contained, serializable record of the last
//! few seconds (or a frozen window), which the window draws, the analysis
//! reads, and the export writes.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{Cat, FrameKind};

/// The unit a counter's samples are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Unit {
    #[default]
    Count,
    Ms,
    Bytes,
    BytesPerSec,
    Percent,
    Hz,
    /// A ratio shown as "0.66×" (simulation speed).
    Ratio,
}

impl Unit {
    /// `value` with its unit, at a readable scale ("12.4 MB", "3.20 ms").
    pub fn format(self, value: f64) -> String {
        fn bytes(v: f64) -> String {
            let a = v.abs();
            if a >= 1024.0 * 1024.0 * 1024.0 {
                format!("{:.2} GB", v / (1024.0 * 1024.0 * 1024.0))
            } else if a >= 1024.0 * 1024.0 {
                format!("{:.1} MB", v / (1024.0 * 1024.0))
            } else if a >= 1024.0 {
                format!("{:.1} KB", v / 1024.0)
            } else {
                format!("{v:.0} B")
            }
        }
        match self {
            Unit::Count => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    group_thousands(value as i64)
                } else if value.abs() >= 100.0 && value.abs() < 1e15 {
                    // (means of large counts: one decimal is plenty)
                    let t = (value * 10.0).round() as i64;
                    format!("{}.{}", group_thousands(t / 10), (t % 10).abs())
                } else {
                    format!("{value:.2}")
                }
            }
            Unit::Ms => format!("{value:.2} ms"),
            Unit::Bytes => bytes(value),
            Unit::BytesPerSec => format!("{}/s", bytes(value)),
            Unit::Percent => format!("{value:.1} %"),
            Unit::Hz => format!("{value:.1} Hz"),
            Unit::Ratio => format!("{value:.2}×"),
        }
    }
}

/// 1234567 → "1,234,567".
pub fn group_thousands(v: i64) -> String {
    let s = v.unsigned_abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if v < 0 {
        out.push('-');
    }
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One timed scope. Times are nanoseconds since the capture's epoch.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpanRec {
    #[serde(rename = "s")]
    pub start: u64,
    #[serde(rename = "e")]
    pub end: u64,
    /// Index into `Capture::names`.
    #[serde(rename = "n")]
    pub name: u32,
    #[serde(rename = "c")]
    pub cat: Cat,
    /// Nesting depth on its thread when it began (0 = outermost).
    #[serde(rename = "d")]
    pub depth: u8,
    /// Instructions retired (0 unless instruction mode was on).
    #[serde(rename = "i", default, skip_serializing_if = "is_zero")]
    pub instr: u64,
    /// Bytes allocated inside it, children included (0 unless counted).
    #[serde(rename = "a", default, skip_serializing_if = "is_zero")]
    pub alloc: u64,
    /// Where in rustudio's source the scope was opened: index + 1 into
    /// `Capture::locations` (0 = unknown).
    #[serde(rename = "l", default, skip_serializing_if = "is_zero32")]
    pub loc: u32,
}

fn is_zero32(v: &u32) -> bool {
    *v == 0
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl SpanRec {
    pub fn dur(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkRec {
    pub t: u64,
    pub name: u32,
    pub cat: Cat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TrackKind {
    #[default]
    Thread,
    /// Spans from GPU timestamp queries, placed at the frame's submit time.
    Gpu,
}

/// One lane of the timeline: a thread (or the GPU).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Track {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub kind: TrackKind,
    /// In the order they ended (children before their parent).
    pub spans: VecDeque<SpanRec>,
    #[serde(default)]
    pub marks: VecDeque<MarkRec>,
    /// The longest span kept (bounds a search by start time).
    #[serde(default)]
    pub max_dur: u64,
    /// Records the thread had to throw away (nothing gathered in time).
    #[serde(default)]
    pub dropped: u64,
}

impl Track {
    /// Spans overlapping [t0, t1], in end order.
    pub fn spans_in(&self, t0: u64, t1: u64) -> impl Iterator<Item = &SpanRec> {
        // Spans are sorted by end: skip those ending before t0, stop once a
        // span starts after t1 and every later one must too (their end is
        // later, so their start is past t1 when end - max_dur > t1).
        let first = self.spans.partition_point(|s| s.end < t0);
        let max = self.max_dur;
        self.spans
            .range(first..)
            .take_while(move |s| s.end.saturating_sub(max) <= t1)
            .filter(move |s| s.start <= t1)
    }
}

/// One frame of a loop: [start, end).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRec {
    pub start: u64,
    pub end: u64,
}

impl FrameRec {
    pub fn dur(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    pub fn ms(&self) -> f64 {
        self.dur() as f64 / 1e6
    }
}

/// A counter's samples: one per display frame it was set in.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CounterTrack {
    pub name: String,
    pub cat: Cat,
    pub unit: Unit,
    pub samples: VecDeque<(u64, f64)>,
}

impl CounterTrack {
    pub fn last(&self) -> Option<f64> {
        self.samples.back().map(|s| s.1)
    }

    /// The value at time t (the last sample at or before it).
    pub fn at(&self, t: u64) -> Option<f64> {
        let i = self.samples.partition_point(|s| s.0 <= t);
        if i == 0 {
            None
        } else {
            Some(self.samples[i - 1].1)
        }
    }

    /// (min, max, mean) over samples in [t0, t1].
    pub fn range_stats(&self, t0: u64, t1: u64) -> Option<(f64, f64, f64)> {
        let mut n = 0usize;
        let (mut lo, mut hi, mut sum) = (f64::MAX, f64::MIN, 0.0);
        for &(t, v) in &self.samples {
            if t < t0 || t > t1 {
                continue;
            }
            n += 1;
            lo = lo.min(v);
            hi = hi.max(v);
            sum += v;
        }
        (n > 0).then(|| (lo, hi, sum / n as f64))
    }
}

/// Where and how a capture was made.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    /// "rustudio 0.3.0" etc.
    pub app: String,
    /// The machine / OS ("linux x86_64").
    pub platform: String,
    /// Host name, when known (tells a laptop capture from a desktop one).
    #[serde(default)]
    pub host: String,
    /// Wall clock at the capture's epoch (ms since the Unix epoch).
    pub epoch_unix_ms: f64,
    /// Scopes counted instructions too.
    pub instr_mode: bool,
    /// Scopes counted the bytes they allocated.
    #[serde(default)]
    pub alloc_tracked: bool,
    /// The GPU and how it was driven ("AMD Radeon … · Vulkan").
    #[serde(default)]
    pub gpu: String,
    /// The build that recorded it: git commit (short sha, "+dirty" when the
    /// tree had edits), or "" when unknown (a binary built without it).
    #[serde(default)]
    pub build: String,
    /// What instruction mode counts on this machine: "instructions" (Linux
    /// perf), "cycles" (Windows), "" (off).
    #[serde(default)]
    pub instr_unit: String,
    /// The machine's load average (1, 5, 15 min) when the capture was
    /// taken: a busy machine stretches times, not instruction counts.
    #[serde(default)]
    pub load_avg: Option<[f32; 3]>,
    #[serde(default)]
    pub cpus: u32,
    /// What was open: "editor", "play", "player", "headless", a place name.
    #[serde(default)]
    pub context: String,
    /// Imported from a file (and which), rather than recorded live.
    #[serde(default)]
    pub source: Option<String>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            app: format!("rustudio {}", env!("CARGO_PKG_VERSION")),
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            host: std::env::var("HOSTNAME")
                .ok()
                .or_else(|| std::env::var("COMPUTERNAME").ok())
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
                .unwrap_or_default(),
            epoch_unix_ms: super::record::epoch_unix_ms(),
            instr_mode: false,
            alloc_tracked: false,
            gpu: String::new(),
            build: super::control::build().to_string(),
            instr_unit: String::new(),
            load_avg: load_avg(),
            cpus: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
            context: String::new(),
            source: None,
        }
    }
}

/// The 1/5/15-minute load average (Linux; None elsewhere).
pub fn load_avg() -> Option<[f32; 3]> {
    let t = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut it = t.split_whitespace().filter_map(|v| v.parse::<f32>().ok());
    Some([it.next()?, it.next()?, it.next()?])
}

/// What instruction mode counts here.
pub fn instr_unit_here() -> &'static str {
    if cfg!(target_os = "linux") {
        "instructions"
    } else if cfg!(windows) {
        "cycles"
    } else {
        "none"
    }
}

/// Everything the profiler has gathered.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capture {
    pub version: u32,
    pub meta: Meta,
    /// The name of every span and mark (`SpanRec::name` indexes it).
    pub names: Vec<String>,
    pub tracks: Vec<Track>,
    /// Frame boundaries per loop.
    pub frames: BTreeMap<FrameKindKey, VecDeque<FrameRec>>,
    pub counters: Vec<CounterTrack>,
    /// Source locations ("crates/…/physics.rs:3678") that spans point at.
    #[serde(default)]
    pub locations: Vec<String>,
    /// The lane that closed the display frames (its main thread).
    #[serde(default)]
    pub render_track: Option<u32>,
    /// Display frames dropped off the front of the ring so far: frame
    /// numbers are this plus the index in `render_frames()`.
    #[serde(default)]
    pub frames_trimmed: u64,
    /// Data other parts add to the capture under their own key (the script
    /// profiler's per-script tables live under "scripts").
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// A frame kind as a map key that serializes as a readable string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FrameKindKey {
    Render,
    Physics,
    Heartbeat,
    Gpu,
}

impl From<FrameKind> for FrameKindKey {
    fn from(k: FrameKind) -> Self {
        match k {
            FrameKind::Render => FrameKindKey::Render,
            FrameKind::Physics => FrameKindKey::Physics,
            FrameKind::Heartbeat => FrameKindKey::Heartbeat,
            FrameKind::Gpu => FrameKindKey::Gpu,
        }
    }
}

pub const CAPTURE_VERSION: u32 = 1;

impl Capture {
    pub fn new() -> Capture {
        Capture { version: CAPTURE_VERSION, ..Default::default() }
    }

    /// The source location of a span, if recorded ("path/file.rs:123").
    pub fn location(&self, loc: u32) -> Option<&str> {
        loc.checked_sub(1).and_then(|i| self.locations.get(i as usize)).map(String::as_str)
    }

    pub fn name(&self, i: u32) -> &str {
        self.names.get(i as usize).map(String::as_str).unwrap_or("?")
    }

    /// The display frames (or an empty list).
    pub fn render_frames(&self) -> &VecDeque<FrameRec> {
        static EMPTY: VecDeque<FrameRec> = VecDeque::new();
        self.frames.get(&FrameKindKey::Render).unwrap_or(&EMPTY)
    }

    pub fn frames_of(&self, kind: FrameKind) -> &VecDeque<FrameRec> {
        static EMPTY: VecDeque<FrameRec> = VecDeque::new();
        self.frames.get(&kind.into()).unwrap_or(&EMPTY)
    }

    /// The time span the capture covers.
    pub fn time_range(&self) -> Option<(u64, u64)> {
        let mut lo = u64::MAX;
        let mut hi = 0u64;
        for f in self.frames.values().flatten() {
            lo = lo.min(f.start);
            hi = hi.max(f.end);
        }
        for t in &self.tracks {
            if let (Some(a), Some(b)) = (t.spans.front(), t.spans.back()) {
                lo = lo.min(a.start);
                hi = hi.max(b.end);
            }
        }
        (lo < hi).then_some((lo, hi))
    }

    pub fn track_by_name(&self, name: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.name == name)
    }

    pub fn counter(&self, name: &str) -> Option<&CounterTrack> {
        self.counters.iter().find(|c| c.name == name)
    }

    pub fn span_count(&self) -> usize {
        self.tracks.iter().map(|t| t.spans.len()).sum()
    }

    /// Drops everything that ended before `cutoff`, and counter samples
    /// before `counter_cutoff` (counters keep a longer history: they're small).
    pub fn trim_before(&mut self, cutoff: u64, counter_cutoff: u64) {
        for t in &mut self.tracks {
            while t.spans.front().is_some_and(|s| s.end < cutoff) {
                t.spans.pop_front();
            }
            while t.marks.front().is_some_and(|m| m.t < cutoff) {
                t.marks.pop_front();
            }
        }
        for (k, f) in self.frames.iter_mut() {
            while f.front().is_some_and(|f| f.end < cutoff) {
                f.pop_front();
                if *k == FrameKindKey::Render {
                    self.frames_trimmed += 1;
                }
            }
        }
        for c in &mut self.counters {
            // (keep one sample before the cutoff so the value at the cutoff is known)
            while c.samples.len() > 1 && c.samples[1].0 < counter_cutoff {
                c.samples.pop_front();
            }
        }
    }

    /// A copy holding only [t0, t1] (for "export selection").
    pub fn slice(&self, t0: u64, t1: u64) -> Capture {
        let mut c = Capture {
            version: self.version,
            meta: self.meta.clone(),
            names: self.names.clone(),
            tracks: Vec::new(),
            frames: BTreeMap::new(),
            counters: Vec::new(),
            render_track: self.render_track,
            locations: self.locations.clone(),
            frames_trimmed: self.frames_trimmed,
            extensions: self.extensions.clone(),
        };
        for t in &self.tracks {
            let spans: VecDeque<SpanRec> = t.spans_in(t0, t1).copied().collect();
            let marks = t.marks.iter().filter(|m| m.t >= t0 && m.t <= t1).copied().collect();
            let max_dur = spans.iter().map(|s| s.dur()).max().unwrap_or(0);
            c.tracks.push(Track { id: t.id, name: t.name.clone(), kind: t.kind, spans, marks, max_dur, dropped: 0 });
        }
        for (k, f) in &self.frames {
            c.frames.insert(*k, f.iter().filter(|f| f.end > t0 && f.start < t1).copied().collect());
        }
        for ct in &self.counters {
            let samples = ct.samples.iter().filter(|s| s.0 >= t0 && s.0 <= t1).copied().collect();
            c.counters.push(CounterTrack { name: ct.name.clone(), cat: ct.cat, unit: ct.unit, samples });
        }
        c
    }
}
