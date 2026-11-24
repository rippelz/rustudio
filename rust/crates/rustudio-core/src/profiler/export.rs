//! Capture files.
//!
//! - Native `.rsprof`: the `Capture` as gzip-compressed JSON. Re-opens in the
//!   profiler window with everything (categories, counters, frames, the
//!   script profiler's tables).
//! - Chrome trace JSON (`.json`, or `.json.gz`): opens in Perfetto
//!   (ui.perfetto.dev) and chrome://tracing. Frames are written as spans on a
//!   "Frames" lane so they survive a round trip.
//!
//! `open` reads either (gzip or not), so a capture made on another machine
//! (a low-end laptop) reads on the desktop.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;

use serde_json::{json, Value};

use super::capture::{Capture, CounterTrack, FrameKindKey, FrameRec, MarkRec, SpanRec, Track, TrackKind, Unit, CAPTURE_VERSION};
use super::Cat;

/// Which file format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Native,
    ChromeTrace,
}

impl Format {
    /// From a file name: `.rsprof` is native, anything else Chrome trace.
    pub fn from_path(p: &Path) -> Format {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.ends_with(".rsprof") {
            Format::Native
        } else {
            Format::ChromeTrace
        }
    }
}

pub fn save(cap: &Capture, path: &Path) -> std::io::Result<()> {
    let bytes = match Format::from_path(path) {
        Format::Native => to_native(cap)?,
        Format::ChromeTrace => {
            let v = to_chrome(cap);
            let raw = serde_json::to_vec(&v).map_err(std::io::Error::other)?;
            if path.extension().is_some_and(|e| e == "gz") {
                gzip(&raw)?
            } else {
                raw
            }
        }
    };
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn open(path: &Path) -> Result<Capture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let mut cap = from_bytes(&bytes)?;
    cap.meta.source = Some(path.display().to_string());
    Ok(cap)
}

fn gzip(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(raw)?;
    e.finish()
}

pub fn to_native(cap: &Capture) -> std::io::Result<Vec<u8>> {
    let raw = serde_json::to_vec(cap).map_err(std::io::Error::other)?;
    gzip(&raw)
}

/// Reads a native capture or a Chrome trace, compressed or not.
pub fn from_bytes(bytes: &[u8]) -> Result<Capture, String> {
    let raw: Vec<u8> = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).map_err(|e| format!("not a valid gzip file: {e}"))?;
        out
    } else {
        bytes.to_vec()
    };
    let v: Value = serde_json::from_slice(&raw).map_err(|e| format!("not JSON: {e}"))?;
    if v.get("tracks").is_some() && v.get("names").is_some() {
        let mut cap: Capture = serde_json::from_value(v).map_err(|e| format!("not a rustudio capture: {e}"))?;
        if cap.version > CAPTURE_VERSION {
            return Err(format!("capture version {} is newer than this rustudio reads ({CAPTURE_VERSION})", cap.version));
        }
        for t in &mut cap.tracks {
            t.max_dur = t.spans.iter().map(|s| s.dur()).max().unwrap_or(0);
        }
        return Ok(cap);
    }
    let events = match &v {
        Value::Array(a) => a.as_slice(),
        Value::Object(o) => o.get("traceEvents").and_then(Value::as_array).map(Vec::as_slice).ok_or("no traceEvents")?,
        _ => return Err("not a trace".into()),
    };
    from_chrome(events, v.get("otherData"))
}

// ------------------------------------------------------------ Chrome trace

const PID: u32 = 1;
/// The pseudo-thread frames are written on.
const FRAMES_TID: u32 = 0;

fn frame_cat(k: FrameKindKey) -> &'static str {
    match k {
        FrameKindKey::Render => "frame.render",
        FrameKindKey::Physics => "frame.physics",
        FrameKindKey::Heartbeat => "frame.heartbeat",
        FrameKindKey::Gpu => "frame.gpu",
    }
}

fn cat_slug(c: Cat) -> &'static str {
    match c {
        Cat::Frame => "frame",
        Cat::Render => "render",
        Cat::Gpu => "gpu",
        Cat::Ui => "ui",
        Cat::Physics => "physics",
        Cat::Script => "script",
        Cat::Animation => "animation",
        Cat::Network => "network",
        Cat::Assets => "assets",
        Cat::Audio => "audio",
        Cat::Input => "input",
        Cat::Gui => "gui",
        Cat::Pathfinding => "pathfinding",
        Cat::Wait => "wait",
        Cat::Other | Cat::Auto => "other",
    }
}

fn cat_from_slug(s: &str) -> Cat {
    Cat::ALL.iter().copied().find(|c| cat_slug(*c) == s).unwrap_or_else(|| Cat::from_name(s))
}

fn unit_slug(u: Unit) -> &'static str {
    match u {
        Unit::Count => "count",
        Unit::Ms => "ms",
        Unit::Bytes => "bytes",
        Unit::BytesPerSec => "bytes/s",
        Unit::Percent => "percent",
        Unit::Hz => "hz",
        Unit::Ratio => "ratio",
    }
}

fn unit_from_slug(s: &str) -> Unit {
    [Unit::Count, Unit::Ms, Unit::Bytes, Unit::BytesPerSec, Unit::Percent, Unit::Hz, Unit::Ratio]
        .into_iter()
        .find(|u| unit_slug(*u) == s)
        .unwrap_or(Unit::Count)
}

fn us(ns: u64) -> f64 {
    ns as f64 / 1000.0
}

/// The capture as a Chrome trace (`{"traceEvents": [...]}`).
pub fn to_chrome(cap: &Capture) -> Value {
    let mut ev: Vec<Value> = Vec::with_capacity(cap.span_count() + 64);
    ev.push(json!({"ph": "M", "name": "process_name", "pid": PID, "tid": 0, "args": {"name": cap.meta.app}}));
    ev.push(json!({"ph": "M", "name": "thread_name", "pid": PID, "tid": FRAMES_TID, "args": {"name": "Frames"}}));
    ev.push(json!({"ph": "M", "name": "thread_sort_index", "pid": PID, "tid": FRAMES_TID, "args": {"sort_index": -1}}));
    for (k, frames) in &cap.frames {
        for (i, f) in frames.iter().enumerate() {
            ev.push(json!({
                "ph": "X", "pid": PID, "tid": FRAMES_TID, "cat": frame_cat(*k),
                "name": format!("{} {}", match k { FrameKindKey::Render => "Frame", FrameKindKey::Physics => "Physics step", FrameKindKey::Heartbeat => "Heartbeat", FrameKindKey::Gpu => "GPU frame" }, i),
                "ts": us(f.start), "dur": us(f.dur()),
            }));
        }
    }
    for (order, t) in cap.tracks.iter().enumerate() {
        let tid = t.id;
        ev.push(json!({"ph": "M", "name": "thread_name", "pid": PID, "tid": tid, "args": {"name": t.name}}));
        let sort = if Some(t.id) == cap.render_track { 0 } else { order as i64 + 1 };
        ev.push(json!({"ph": "M", "name": "thread_sort_index", "pid": PID, "tid": tid, "args": {"sort_index": sort}}));
        // (written in start order so viewers nest them without sorting)
        let mut spans: Vec<&SpanRec> = t.spans.iter().collect();
        spans.sort_by(|a, b| a.start.cmp(&b.start).then(a.depth.cmp(&b.depth)));
        for s in spans {
            let mut e = json!({
                "ph": "X", "pid": PID, "tid": tid, "name": cap.name(s.name), "cat": cat_slug(s.cat),
                "ts": us(s.start), "dur": us(s.dur()),
            });
            if s.instr != 0 || s.alloc != 0 || s.loc != 0 {
                let mut args = serde_json::Map::new();
                if let Some(l) = cap.location(s.loc) {
                    args.insert("source".into(), json!(l));
                }
                if s.instr != 0 {
                    args.insert("instructions".into(), json!(s.instr));
                }
                if s.alloc != 0 {
                    args.insert("allocated_bytes".into(), json!(s.alloc));
                }
                e["args"] = Value::Object(args);
            }
            ev.push(e);
        }
        for m in &t.marks {
            ev.push(json!({"ph": "i", "s": "t", "pid": PID, "tid": tid, "name": cap.name(m.name), "cat": cat_slug(m.cat), "ts": us(m.t)}));
        }
    }
    for c in &cap.counters {
        for &(t, v) in &c.samples {
            ev.push(json!({"ph": "C", "pid": PID, "tid": 0, "name": c.name, "cat": cat_slug(c.cat), "ts": us(t), "args": {"value": v}}));
        }
    }
    let units: BTreeMap<&str, &str> = cap.counters.iter().map(|c| (c.name.as_str(), unit_slug(c.unit))).collect();
    json!({
        "traceEvents": ev,
        "displayTimeUnit": "ms",
        "otherData": {
            "app": cap.meta.app,
            "platform": cap.meta.platform,
            "host": cap.meta.host,
            "context": cap.meta.context,
            "epoch_unix_ms": cap.meta.epoch_unix_ms,
            "instr_mode": cap.meta.instr_mode,
            "alloc_tracked": cap.meta.alloc_tracked,
            "gpu": cap.meta.gpu,
            "render_track": cap.render_track,
            "counter_units": units,
            "rustudio_extensions": cap.extensions,
        }
    })
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

fn ns(us: f64) -> u64 {
    (us * 1000.0).max(0.0).round() as u64
}

/// Reads Chrome trace events (ours or another program's): complete (`X`)
/// and begin/end (`B`/`E`) spans, instants, counters, thread names.
fn from_chrome(events: &[Value], other: Option<&Value>) -> Result<Capture, String> {
    let mut cap = Capture::new();
    let mut names: HashMap<String, u32> = HashMap::new();
    let mut name_of = |cap: &mut Capture, n: &str| -> u32 {
        *names.entry(n.to_string()).or_insert_with(|| {
            cap.names.push(n.to_string());
            (cap.names.len() - 1) as u32
        })
    };
    // (pid, tid) → track index
    let mut track_of: HashMap<(i64, i64), usize> = HashMap::new();
    let mut thread_names: HashMap<(i64, i64), String> = HashMap::new();
    // per lane: (start, end, name, category, instructions)
    // (start, end, name, category, instructions, allocated bytes, location)
    type Raw = (u64, u64, u32, Cat, u64, u64, u32);
    let mut loc_ix: HashMap<String, u32> = HashMap::new();
    let mut raw: Vec<Vec<Raw>> = Vec::new();
    type Open = (u64, String, Cat);
    let mut open: HashMap<(i64, i64), Vec<Open>> = HashMap::new();
    let mut counters: BTreeMap<String, CounterTrack> = BTreeMap::new();
    let mut frames: BTreeMap<FrameKindKey, Vec<FrameRec>> = BTreeMap::new();
    let mut marks: HashMap<(i64, i64), Vec<MarkRec>> = HashMap::new();
    let units: HashMap<String, Unit> = other
        .and_then(|o| o.get("counter_units"))
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), unit_from_slug(v.as_str().unwrap_or("")))).collect())
        .unwrap_or_default();
    let mut min_ts = u64::MAX;

    for e in events {
        let ph = e.get("ph").and_then(Value::as_str).unwrap_or("");
        let pid = e.get("pid").and_then(Value::as_i64).unwrap_or(0);
        let tid = e.get("tid").and_then(Value::as_i64).unwrap_or(0);
        let key = (pid, tid);
        let name = e.get("name").and_then(Value::as_str).unwrap_or("?");
        let cat_s = e.get("cat").and_then(Value::as_str).unwrap_or("");
        let ts = num(e, "ts").map(ns);
        if let Some(t) = ts {
            min_ts = min_ts.min(t);
        }
        match ph {
            "M" if name == "thread_name" => {
                if let Some(n) = e.pointer("/args/name").and_then(Value::as_str) {
                    thread_names.insert(key, n.to_string());
                }
            }
            "X" => {
                let (Some(t), d) = (ts, num(e, "dur").map(ns).unwrap_or(0)) else { continue };
                if let Some(k) = cat_s.strip_prefix("frame.") {
                    let k = match k {
                        "physics" => FrameKindKey::Physics,
                        "heartbeat" => FrameKindKey::Heartbeat,
                        "gpu" => FrameKindKey::Gpu,
                        _ => FrameKindKey::Render,
                    };
                    frames.entry(k).or_default().push(FrameRec { start: t, end: t + d });
                    continue;
                }
                let n = name_of(&mut cap, name);
                let instr = e.pointer("/args/instructions").and_then(Value::as_u64).unwrap_or(0);
                let alloc = e.pointer("/args/allocated_bytes").and_then(Value::as_u64).unwrap_or(0);
                let loc = match e.pointer("/args/source").and_then(Value::as_str) {
                    Some(l) => *loc_ix.entry(l.to_string()).or_insert_with(|| {
                        cap.locations.push(l.to_string());
                        cap.locations.len() as u32
                    }),
                    None => 0,
                };
                let ti = *track_of.entry(key).or_insert_with(|| {
                    raw.push(Vec::new());
                    raw.len() - 1
                });
                raw[ti].push((t, t + d, n, cat_from_slug(cat_s), instr, alloc, loc));
            }
            "B" => {
                if let Some(t) = ts {
                    open.entry(key).or_default().push((t, name.to_string(), cat_from_slug(cat_s)));
                }
            }
            "E" => {
                let (Some(t), Some((t0, n, c))) = (ts, open.get_mut(&key).and_then(Vec::pop)) else { continue };
                let n = name_of(&mut cap, &n);
                let ti = *track_of.entry(key).or_insert_with(|| {
                    raw.push(Vec::new());
                    raw.len() - 1
                });
                raw[ti].push((t0, t, n, c, 0, 0, 0));
            }
            "i" | "I" | "R" => {
                if let Some(t) = ts {
                    let n = name_of(&mut cap, name);
                    marks.entry(key).or_default().push(MarkRec { t, name: n, cat: cat_from_slug(cat_s) });
                }
            }
            "C" => {
                let Some(t) = ts else { continue };
                let Some(args) = e.get("args").and_then(Value::as_object) else { continue };
                for (k, v) in args {
                    let Some(v) = v.as_f64() else { continue };
                    let cname = if args.len() == 1 && k == "value" { name.to_string() } else { format!("{name}.{k}") };
                    let unit = units.get(&cname).copied().unwrap_or_default();
                    counters
                        .entry(cname.clone())
                        .or_insert_with(|| CounterTrack { name: cname, cat: cat_from_slug(cat_s), unit, samples: VecDeque::new() })
                        .samples
                        .push_back((t, v));
                }
            }
            _ => {}
        }
    }
    if min_ts == u64::MAX {
        return Err("the trace has no timed events".into());
    }
    // Foreign traces start at arbitrary clocks; ours start near 0. Keep ours
    // as they are so times match the original session's.
    let base = if other.is_some_and(|o| o.get("app").is_some()) { 0 } else { min_ts };

    let mut keys: Vec<((i64, i64), usize)> = track_of.into_iter().collect();
    keys.sort();
    for (key, ti) in keys {
        let mut spans = std::mem::take(&mut raw[ti]);
        // depth from containment, in start order
        spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        let mut stack: Vec<u64> = Vec::new();
        let mut recs: Vec<SpanRec> = Vec::with_capacity(spans.len());
        for (s, e, n, c, i, a, l) in spans {
            while stack.last().is_some_and(|&end| end <= s) {
                stack.pop();
            }
            recs.push(SpanRec { start: s - base, end: e - base, name: n, cat: c, depth: stack.len().min(255) as u8, instr: i, alloc: a, loc: l });
            stack.push(e);
        }
        recs.sort_by_key(|r| r.end);
        let max_dur = recs.iter().map(|r| r.dur()).max().unwrap_or(0);
        let mut m = marks.remove(&key).unwrap_or_default();
        m.sort_by_key(|m| m.t);
        for x in &mut m {
            x.t -= base;
        }
        let name = thread_names.get(&key).cloned().unwrap_or_else(|| format!("thread {}", key.1));
        let kind = if name.starts_with("GPU") { TrackKind::Gpu } else { TrackKind::Thread };
        cap.tracks.push(Track { id: key.1.max(0) as u32, name, kind, spans: recs.into(), marks: m.into(), max_dur, dropped: 0 });
    }
    for (k, mut f) in frames {
        f.sort_by_key(|f| f.start);
        cap.frames.insert(k, f.into_iter().map(|f| FrameRec { start: f.start - base, end: f.end - base }).collect());
    }
    cap.counters = counters
        .into_values()
        .map(|mut c| {
            for s in &mut c.samples {
                s.0 -= base;
            }
            c
        })
        .collect();
    if let Some(o) = other {
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
        if let Some(a) = s("app") {
            cap.meta.app = a;
        }
        cap.meta.platform = s("platform").unwrap_or_default();
        cap.meta.host = s("host").unwrap_or_default();
        cap.meta.context = s("context").unwrap_or_default();
        cap.meta.epoch_unix_ms = num(o, "epoch_unix_ms").unwrap_or(0.0);
        cap.meta.instr_mode = o.get("instr_mode").and_then(Value::as_bool).unwrap_or(false);
        cap.meta.alloc_tracked = o.get("alloc_tracked").and_then(Value::as_bool).unwrap_or(false);
        cap.meta.gpu = s("gpu").unwrap_or_default();
        cap.render_track = o.get("render_track").and_then(Value::as_u64).map(|v| v as u32);
        if let Some(ext) = o.get("rustudio_extensions").and_then(Value::as_object) {
            cap.extensions = ext.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        }
    } else {
        cap.meta.app = "imported trace".into();
        cap.meta.platform.clear();
        cap.meta.host.clear();
    }
    if cap.render_track.is_none() {
        cap.render_track = cap.tracks.iter().find(|t| t.name == "main").map(|t| t.id);
    }
    Ok(cap)
}
