//! Reports for agents (and people at a terminal): `--profile-report` and
//! `--profile-diff`, as text or JSON (schemas in docs/PROFILING.md).
//!
//! Everything is computed per display frame: each scope's self and
//! inclusive cost in every frame (0 where it didn't run), so means, spreads
//! and percentiles describe frames, and a diff can tell a real change from
//! run-to-run noise.

use std::fmt::Write;

use rustc_hash::FxHashMap;
use serde_json::{json, Value};

use super::analysis;
use super::capture::{Capture, FrameRec, TrackKind};
use super::{Cat, FrameKind};

pub const REPORT_SCHEMA: &str = "rustudio-profile-report/1";
pub const DIFF_SCHEMA: &str = "rustudio-profile-diff/1";

/// One scope name across the capture (all lanes it ran on).
pub struct ScopeAgg {
    pub name: String,
    pub cat: Cat,
    /// rustudio source location of the scope ("crates/…/x.rs:12"), the
    /// first one seen; `sources` counts the distinct sites with this name.
    pub source: Option<String>,
    pub sources: usize,
    pub lanes: Vec<String>,
    pub calls: u64,
    /// Per display frame (index = frame): self and inclusive milliseconds.
    pub self_ms: Vec<f64>,
    pub incl_ms: Vec<f64>,
    /// The same in millions of instructions (empty unless counted).
    pub self_minstr: Vec<f64>,
    pub incl_minstr: Vec<f64>,
    /// Each call's length (ms), sorted.
    pub call_ms: Vec<f64>,
    pub alloc_bytes: u64,
}

/// A capture reduced to per-frame numbers.
pub struct Data {
    pub frames: Vec<FrameRec>,
    pub scopes: Vec<ScopeAgg>,
    /// The display lane's busy time per frame (ms; and M instructions when counted).
    pub main_ms: Vec<f64>,
    pub main_minstr: Vec<f64>,
    /// Instructions were counted (the primary unit then).
    pub instr: bool,
}

fn frame_of(frames: &[FrameRec], t: u64) -> Option<usize> {
    let i = frames.partition_point(|f| f.start <= t);
    (i > 0 && t < frames[i - 1].end).then(|| i - 1)
}

/// Reduces a capture to per-frame scope numbers.
pub fn collect(cap: &Capture) -> Data {
    let frames: Vec<FrameRec> = cap.render_frames().iter().copied().collect();
    let n = frames.len();
    let instr = cap.meta.instr_mode && cap.tracks.iter().any(|t| t.spans.iter().any(|s| s.instr > 0));
    let mut by: FxHashMap<&str, ScopeAgg> = FxHashMap::default();
    let main_id = analysis::main_track(cap).map(|t| t.id);
    let mut main_ms = vec![0.0; n];
    let mut main_minstr = vec![0.0; n];
    for t in cap.tracks.iter().filter(|t| t.kind == TrackKind::Thread || t.kind == TrackKind::Gpu) {
        // self time and self instructions, end order (children before parents)
        let mut acc_ns = [0u64; 257];
        let mut acc_in = [0u64; 257];
        for s in &t.spans {
            let d = s.depth as usize;
            let child_ns = std::mem::take(&mut acc_ns[d + 1]);
            let child_in = std::mem::take(&mut acc_in[d + 1]);
            acc_ns[d] += s.dur();
            acc_in[d] += s.instr;
            let own_ns = s.dur().saturating_sub(child_ns);
            let own_in = s.instr.saturating_sub(child_in);
            let Some(f) = frame_of(&frames, s.start) else { continue };
            let name = cap.name(s.name);
            let e = by.entry(name).or_insert_with(|| ScopeAgg {
                name: name.to_string(),
                cat: s.cat,
                source: None,
                sources: 0,
                lanes: Vec::new(),
                calls: 0,
                self_ms: vec![0.0; n],
                incl_ms: vec![0.0; n],
                self_minstr: if instr { vec![0.0; n] } else { Vec::new() },
                incl_minstr: if instr { vec![0.0; n] } else { Vec::new() },
                call_ms: Vec::new(),
                alloc_bytes: 0,
            });
            if let Some(l) = cap.location(s.loc) {
                match &e.source {
                    None => {
                        e.source = Some(l.to_string());
                        e.sources = 1;
                    }
                    Some(x) if x != l && e.sources < 2 => e.sources = 2,
                    _ => {}
                }
            }
            if !e.lanes.contains(&t.name) {
                e.lanes.push(t.name.clone());
            }
            e.calls += 1;
            e.self_ms[f] += own_ns as f64 / 1e6;
            e.incl_ms[f] += s.dur() as f64 / 1e6;
            if instr {
                e.self_minstr[f] += own_in as f64 / 1e6;
                e.incl_minstr[f] += s.instr as f64 / 1e6;
            }
            e.call_ms.push(s.dur() as f64 / 1e6);
            e.alloc_bytes += s.alloc;
            if Some(t.id) == main_id && s.depth == 0 {
                main_ms[f] += s.dur() as f64 / 1e6;
                main_minstr[f] += s.instr as f64 / 1e6;
            }
        }
    }
    let mut scopes: Vec<ScopeAgg> = by.into_values().collect();
    for s in &mut scopes {
        s.call_ms.sort_by(f64::total_cmp);
    }
    scopes.sort_by(|a, b| mean(primary_self(b, instr)).total_cmp(&mean(primary_self(a, instr))));
    Data { frames, scopes, main_ms, main_minstr: if instr { main_minstr } else { Vec::new() }, instr }
}

fn primary_self(s: &ScopeAgg, instr: bool) -> &[f64] {
    if instr { &s.self_minstr } else { &s.self_ms }
}

pub fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 }
}

/// Sample variance.
pub fn var(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64
}

fn q(v: &[f64], p: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    qs(&s, p)
}

fn qs(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i]
}

fn r3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn frame_kind_json(cap: &Capture, kind: FrameKind) -> Option<Value> {
    let f = cap.frames_of(kind);
    if f.is_empty() {
        return None;
    }
    let ms: Vec<f64> = f.iter().map(|x| x.ms()).collect();
    let st = analysis::frame_stats(f);
    Some(json!({
        "count": st.count,
        "per_second": r3(st.fps),
        "mean_ms": r3(st.avg_ms),
        "stddev_ms": r3(var(&ms).sqrt()),
        "p50_ms": r3(st.p50_ms),
        "p95_ms": r3(st.p95_ms),
        "p99_ms": r3(st.p99_ms),
        "max_ms": r3(st.max_ms),
    }))
}

fn kind_key(k: FrameKind) -> &'static str {
    match k {
        FrameKind::Render => "render",
        FrameKind::Physics => "physics",
        FrameKind::Heartbeat => "heartbeat",
        FrameKind::Gpu => "gpu",
    }
}

/// The worst `n` display frames and, for each, the scopes over their own
/// 95th percentile that frame (by how much).
pub fn spikes(d: &Data, n: usize) -> Vec<Value> {
    let p95: Vec<f64> = d.scopes.iter().map(|s| q(&s.self_ms, 0.95)).collect();
    let mut order: Vec<usize> = (0..d.frames.len()).collect();
    order.sort_by(|&a, &b| d.frames[b].dur().cmp(&d.frames[a].dur()));
    let t0 = d.frames.first().map(|f| f.start).unwrap_or(0);
    order
        .into_iter()
        .take(n)
        .map(|fi| {
            let mut culprits: Vec<(f64, usize)> = d
                .scopes
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    let v = s.self_ms[fi];
                    let excess = v - p95[i];
                    (excess > 0.05 && v > 0.1).then_some((excess, i))
                })
                .collect();
            culprits.sort_by(|a, b| b.0.total_cmp(&a.0));
            json!({
                "frame": fi,
                "t_s": r3((d.frames[fi].start - t0) as f64 / 1e9),
                "ms": r3(d.frames[fi].ms()),
                "main_busy_ms": r3(d.main_ms[fi]),
                "culprits": culprits.iter().take(8).map(|&(excess, i)| {
                    let s = &d.scopes[i];
                    json!({
                        "name": s.name,
                        "category": s.cat.name(),
                        "source": s.source,
                        "self_ms": r3(s.self_ms[fi]),
                        "incl_ms": r3(s.incl_ms[fi]),
                        "frame_p95_ms": r3(p95[i]),
                        "excess_ms": r3(excess),
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The script lane's table (capture extension "scripts"), summarised:
/// the top scripts and functions by self time.
fn scripts_json(cap: &Capture, top: usize) -> Value {
    let Some(v) = cap.extensions.get("scripts") else { return Value::Null };
    let take = |key: &str| -> Value {
        let mut rows: Vec<Value> = v.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
        rows.sort_by(|a, b| {
            let f = |x: &Value| x.get("self_ms").and_then(Value::as_f64).unwrap_or(0.0);
            f(b).total_cmp(&f(a))
        });
        Value::Array(rows.into_iter().take(top).collect())
    };
    json!({
        "recorded_s": v.get("recorded_s"),
        "samples": v.get("samples"),
        "exact": v.get("exact"),
        "scripts": take("scripts"),
        "functions": take("functions"),
    })
}

/// The JSON report (schema `rustudio-profile-report/1`).
pub fn report_json(cap: &Capture, top: usize, spikes_n: usize) -> Value {
    let d = collect(cap);
    let n = d.frames.len().max(1) as f64;
    let (t0, t1) = cap.time_range().unwrap_or((0, 0));
    let m = &cap.meta;
    let mut frames = serde_json::Map::new();
    for k in FrameKind::ALL {
        if let Some(v) = frame_kind_json(cap, k) {
            frames.insert(kind_key(k).into(), v);
        }
    }
    if let Some(r) = frames.get_mut("render") {
        r["main_busy_ms_mean"] = json!(r3(mean(&d.main_ms)));
        if d.instr {
            r["main_busy_minstr_mean"] = json!(r3(mean(&d.main_minstr)));
            r["main_busy_minstr_stddev"] = json!(r3(var(&d.main_minstr).sqrt()));
        }
    }
    let lanes: Vec<Value> = cap
        .tracks
        .iter()
        .map(|t| {
            let busy: u64 = t.spans.iter().filter(|s| s.depth == 0).map(|s| s.dur()).sum();
            json!({
                "name": t.name,
                "title": analysis::lane_info(&t.name).0,
                "kind": if t.kind == TrackKind::Gpu { "gpu" } else { "thread" },
                "spans": t.spans.len(),
                "busy_pct": r3(busy as f64 * 100.0 / (t1 - t0).max(1) as f64),
                "dropped": t.dropped,
                "display_frames": Some(t.id) == cap.render_track,
            })
        })
        .collect();
    let scopes: Vec<Value> = d
        .scopes
        .iter()
        .take(top)
        .map(|s| {
            let mut v = json!({
                "name": s.name,
                "category": s.cat.name(),
                "source": s.source,
                "source_sites": s.sources,
                "lanes": s.lanes,
                "calls": s.calls,
                "calls_per_frame": r3(s.calls as f64 / n),
                "frames_present": s.self_ms.iter().filter(|v| **v > 0.0).count(),
                "self_ms_per_frame": r3(mean(&s.self_ms)),
                "self_ms_stddev": r3(var(&s.self_ms).sqrt()),
                "self_ms_frame_p95": r3(q(&s.self_ms, 0.95)),
                "incl_ms_per_frame": r3(mean(&s.incl_ms)),
                "call_ms_p50": r3(qs(&s.call_ms, 0.5)),
                "call_ms_p95": r3(qs(&s.call_ms, 0.95)),
                "call_ms_max": r3(s.call_ms.last().copied().unwrap_or(0.0)),
                "alloc_bytes_per_frame": (s.alloc_bytes as f64 / n).round(),
            });
            if d.instr {
                v["self_minstr_per_frame"] = json!(r3(mean(&s.self_minstr)));
                v["self_minstr_stddev"] = json!(r3(var(&s.self_minstr).sqrt()));
                v["incl_minstr_per_frame"] = json!(r3(mean(&s.incl_minstr)));
            }
            v
        })
        .collect();
    let counters: Vec<Value> = cap
        .counters
        .iter()
        .filter_map(|c| {
            let (lo, hi, mean) = c.range_stats(0, u64::MAX)?;
            Some(json!({
                "name": c.name,
                "category": c.cat.name(),
                "unit": format!("{:?}", c.unit).to_lowercase(),
                "min": r3(lo), "mean": r3(mean), "max": r3(hi),
                "last": c.last().map(r3),
            }))
        })
        .collect();
    json!({
        "schema": REPORT_SCHEMA,
        "capture": {
            "file": m.source,
            "app": m.app,
            "build": m.build,
            "host": m.host,
            "platform": m.platform,
            "cpus": m.cpus,
            "gpu": m.gpu,
            "context": m.context,
            "load_avg": m.load_avg,
            "unit": if d.instr { "instr" } else { "ms" },
            "instr_unit": m.instr_unit,
            "alloc_tracked": m.alloc_tracked,
            "duration_s": r3((t1 - t0) as f64 / 1e9),
            "spans": cap.span_count(),
            "display_frames": d.frames.len(),
        },
        "frames": frames,
        "lanes": lanes,
        "scopes": scopes,
        "spikes": spikes(&d, spikes_n),
        "counters": counters,
        "scripts": scripts_json(cap, top.min(25)),
    })
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

/// The text report: what `--profile-report FILE` prints.
pub fn report_text(cap: &Capture, top: usize, spikes_n: usize) -> String {
    let v = report_json(cap, top, spikes_n);
    let mut o = String::new();
    let c = &v["capture"];
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let _ = writeln!(o, "{}  build {}  {}  {}  {} cpus{}", s(&c["app"]), if s(&c["build"]).is_empty() { "?".into() } else { s(&c["build"]) }, s(&c["platform"]), s(&c["host"]), c["cpus"], match c["load_avg"].as_array() {
        Some(a) => format!("  load {}", a.iter().map(|x| format!("{:.1}", x.as_f64().unwrap_or(0.0))).collect::<Vec<_>>().join(" ")),
        None => String::new(),
    });
    if !s(&c["context"]).is_empty() || !s(&c["gpu"]).is_empty() {
        let _ = writeln!(o, "{}  {}", s(&c["context"]), s(&c["gpu"]));
    }
    let instr = c["unit"] == "instr";
    let _ = writeln!(
        o,
        "{:.2} s, {} spans, {} display frames, unit {}{}",
        c["duration_s"].as_f64().unwrap_or(0.0),
        c["spans"],
        c["display_frames"],
        if instr { "instructions (M)" } else { "ms" },
        if instr && s(&c["instr_unit"]) == "cycles" { " [Windows: CPU cycles]" } else { "" }
    );
    let _ = writeln!(o, "\nframes            count     /s    mean ± sd     p50     p95     p99     max  (ms)");
    if let Some(f) = v["frames"].as_object() {
        for (k, x) in f {
            let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
            let _ = writeln!(o, "  {:<12} {:>8} {:>6.1} {:>7.2} ± {:<5.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2}", k, x["count"], g("per_second"), g("mean_ms"), g("stddev_ms"), g("p50_ms"), g("p95_ms"), g("p99_ms"), g("max_ms"));
        }
        if let Some(r) = f.get("render") {
            if let Some(mi) = r.get("main_busy_minstr_mean") {
                let _ = writeln!(o, "  main lane busy per frame: {:.2} M instr ± {:.2}", mi.as_f64().unwrap_or(0.0), r["main_busy_minstr_stddev"].as_f64().unwrap_or(0.0));
            }
            let _ = writeln!(o, "  main lane busy per frame: {:.2} ms", r["main_busy_ms_mean"].as_f64().unwrap_or(0.0));
        }
    }
    let _ = writeln!(o, "\nlanes");
    for l in v["lanes"].as_array().into_iter().flatten() {
        let _ = writeln!(o, "  {:<26} {:>8} spans  busy {:>5.1} %{}{}", trunc(&s(&l["title"]), 26), l["spans"], l["busy_pct"].as_f64().unwrap_or(0.0), if l["display_frames"] == true { "  (display frames)" } else { "" }, if l["dropped"].as_u64().unwrap_or(0) > 0 { format!("  {} dropped", l["dropped"]) } else { String::new() });
    }
    let unit = if instr { "M instr" } else { "ms" };
    let _ = writeln!(o, "\nhottest scopes, per display frame ({unit}; call times in ms)");
    let _ = writeln!(o, "  {:<34} {:>9} {:>7} {:>9} {:>7} {:>8} {:>8} {:>8}  {:<9} source", "scope", "self", "± sd", "incl", "calls", "call p50", "call p95", "call max", "category");
    for x in v["scopes"].as_array().into_iter().flatten() {
        let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
        let (sf, sd, inc) = if instr { (g("self_minstr_per_frame"), g("self_minstr_stddev"), g("incl_minstr_per_frame")) } else { (g("self_ms_per_frame"), g("self_ms_stddev"), g("incl_ms_per_frame")) };
        let src = match (&x["source"], x["source_sites"].as_u64()) {
            (Value::String(p), Some(n)) if n > 1 => format!("{p} (+more)"),
            (Value::String(p), _) => p.clone(),
            _ => String::new(),
        };
        let _ = writeln!(o, "  {:<34} {:>9.3} {:>7.3} {:>9.3} {:>7.1} {:>8.3} {:>8.3} {:>8.3}  {:<9} {}", trunc(&s(&x["name"]), 34), sf, sd, inc, g("calls_per_frame"), g("call_ms_p50"), g("call_ms_p95"), g("call_ms_max"), s(&x["category"]), src);
    }
    if let Some(sp) = v["spikes"].as_array().filter(|a| !a.is_empty()) {
        let _ = writeln!(o, "\nworst frames and what made them slow (scopes over their own p95 self time that frame)");
        for f in sp {
            let _ = writeln!(o, "  frame {:>5} at {:>7.3} s  {:>8.2} ms  (main busy {:.2} ms)", f["frame"], f["t_s"].as_f64().unwrap_or(0.0), f["ms"].as_f64().unwrap_or(0.0), f["main_busy_ms"].as_f64().unwrap_or(0.0));
            for cu in f["culprits"].as_array().into_iter().flatten() {
                let g = |n: &str| cu[n].as_f64().unwrap_or(0.0);
                let _ = writeln!(o, "      {:<34} self {:>7.2} ms  (p95 {:>6.2}, +{:>6.2})  {}  {}", trunc(&s(&cu["name"]), 34), g("self_ms"), g("frame_p95_ms"), g("excess_ms"), s(&cu["category"]), s(&cu["source"]));
            }
        }
    }
    let ctr = v["counters"].as_array().cloned().unwrap_or_default();
    if !ctr.is_empty() {
        let _ = writeln!(o, "\ncounters                          min           mean            max   unit");
        for x in ctr {
            let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
            let _ = writeln!(o, "  {:<28} {:>12.2} {:>14.2} {:>14.2}   {}", trunc(&s(&x["name"]), 28), g("min"), g("mean"), g("max"), s(&x["unit"]));
        }
    }
    if let Some(sc) = v["scripts"].as_object() {
        let _ = writeln!(o, "\nscripts (script profiler, {} s recorded)", sc.get("recorded_s").and_then(Value::as_f64).map(|x| format!("{x:.1}")).unwrap_or("?".into()));
        for x in sc.get("scripts").and_then(Value::as_array).into_iter().flatten() {
            let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
            let _ = writeln!(o, "  {:<48} self {:>8.2} ms  {:>6.3} ms/frame  {}", trunc(&s(&x["path"]), 48), g("self_ms"), g("ms_per_frame"), s(&x["vm"]));
        }
        let funcs = sc.get("functions").and_then(Value::as_array).cloned().unwrap_or_default();
        if !funcs.is_empty() {
            let _ = writeln!(o, "  functions:");
            for x in funcs {
                let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
                let _ = writeln!(o, "    {:<52} self {:>8.2} ms  total {:>8.2} ms", trunc(&format!("{}:{} {}", s(&x["script"]), x["line"], s(&x["name"])), 52), g("self_ms"), g("total_ms"));
            }
        }
    }
    o
}

// ------------------------------------------------------------ diffs

/// The means of `k` contiguous blocks of frames (neighbouring frames are
/// alike, so blocks, not frames, are the independent samples).
fn block_means(v: &[f64], k: usize) -> Vec<f64> {
    let k = k.min(v.len()).max(1);
    (0..k)
        .map(|i| {
            let a = i * v.len() / k;
            let b = ((i + 1) * v.len() / k).max(a + 1).min(v.len());
            mean(&v[a..b])
        })
        .collect()
}

/// How a difference reads: inside the noise, or a real change.
///
/// The rule (docs/PROFILING.md): the means differ by more than 3 standard
/// errors of the difference, the errors taken from 10 blocks of frames per
/// capture, and by more than `min_pct` % and `floor` (unit). Times also
/// move with the machine's load between runs, which no single capture
/// shows: so ms need at least 15 % (and 5 µs); instruction counts 3 %
/// (and 0.02 M).
fn verdict(a: &[f64], b: &[f64], floor: f64, min_pct: f64) -> (f64, f64, &'static str) {
    let (ma, mb) = (mean(a), mean(b));
    let d = mb - ma;
    let (ba, bb) = (block_means(a, 10), block_means(b, 10));
    let se = (var(&ba) / ba.len() as f64 + var(&bb) / bb.len() as f64).sqrt();
    let pct = if ma.abs() > 1e-12 { d / ma * 100.0 } else if d.abs() > 1e-12 { f64::INFINITY } else { 0.0 };
    let tag = if d.abs() <= 3.0 * se || d.abs() < floor || pct.abs() < min_pct {
        "noise"
    } else if d > 0.0 {
        "slower"
    } else {
        "faster"
    };
    (d, pct, tag)
}

/// The diff of two captures (schema `rustudio-profile-diff/1`).
pub fn diff_json(a: &Capture, b: &Capture, top: usize) -> Value {
    let da = collect(a);
    let db = collect(b);
    let instr = da.instr && db.instr;
    let unit = if instr { "instr" } else { "ms" };
    let floor = if instr { 0.02 } else { 0.005 };
    let min_pct = if instr { 3.0 } else { 15.0 };
    let mut notes: Vec<String> = Vec::new();
    if da.instr != db.instr {
        notes.push("one capture counted instructions and the other didn't: compared in ms".into());
    }
    if instr && a.meta.instr_unit != b.meta.instr_unit {
        notes.push(format!("instruction units differ ({} vs {}): don't compare across machines", a.meta.instr_unit, b.meta.instr_unit));
    }
    if !instr {
        notes.push("times in ms move with the machine's load: a change under 15 % reads as noise; confirm smaller ones with --instr captures or repeated runs".into());
        let la = a.meta.load_avg.map(|l| l[0]).unwrap_or(0.0);
        let lb = b.meta.load_avg.map(|l| l[0]).unwrap_or(0.0);
        if (la - lb).abs() > 2.0 {
            notes.push(format!("load average differed ({la:.1} vs {lb:.1}): time changes may be the machine, use --instr captures"));
        }
    }
    let pick = |s: &ScopeAgg| -> Vec<f64> { if instr { s.self_minstr.clone() } else { s.self_ms.clone() } };
    let mut frames = serde_json::Map::new();
    for k in FrameKind::ALL {
        let fa: Vec<f64> = a.frames_of(k).iter().map(|f| f.ms()).collect();
        let fb: Vec<f64> = b.frames_of(k).iter().map(|f| f.ms()).collect();
        if fa.is_empty() || fb.is_empty() {
            continue;
        }
        let (d, pct, tag) = verdict(&fa, &fb, 0.01, 15.0);
        frames.insert(kind_key(k).into(), json!({
            "a_mean_ms": r3(mean(&fa)), "b_mean_ms": r3(mean(&fb)),
            "a_p95_ms": r3(q(&fa, 0.95)), "b_p95_ms": r3(q(&fb, 0.95)),
            "delta_ms": r3(d), "delta_pct": r3(pct), "verdict": tag,
        }));
    }
    let main = if instr {
        let (d, pct, tag) = verdict(&da.main_minstr, &db.main_minstr, floor, min_pct);
        json!({"unit": "M instr", "a": r3(mean(&da.main_minstr)), "b": r3(mean(&db.main_minstr)), "delta": r3(d), "delta_pct": r3(pct), "verdict": tag})
    } else {
        let (d, pct, tag) = verdict(&da.main_ms, &db.main_ms, floor, min_pct);
        json!({"unit": "ms", "a": r3(mean(&da.main_ms)), "b": r3(mean(&db.main_ms)), "delta": r3(d), "delta_pct": r3(pct), "verdict": tag})
    };
    let ia: FxHashMap<&str, &ScopeAgg> = da.scopes.iter().map(|s| (s.name.as_str(), s)).collect();
    let ib: FxHashMap<&str, &ScopeAgg> = db.scopes.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut names: Vec<&str> = ia.keys().chain(ib.keys()).copied().collect();
    names.sort_unstable();
    names.dedup();
    let mut rows: Vec<(f64, Value)> = names
        .into_iter()
        .map(|n| {
            let (sa, sb) = (ia.get(n), ib.get(n));
            let va = sa.map(|s| pick(s)).unwrap_or_else(|| vec![0.0; da.frames.len().max(1)]);
            let vb = sb.map(|s| pick(s)).unwrap_or_else(|| vec![0.0; db.frames.len().max(1)]);
            let (d, pct, mut tag) = verdict(&va, &vb, floor, min_pct);
            if sa.is_none() {
                tag = "new";
            } else if sb.is_none() {
                tag = "gone";
            }
            let any = sa.or(sb).expect("one side has it");
            let calls = |s: Option<&&ScopeAgg>, nf: usize| s.map(|s| r3(s.calls as f64 / nf.max(1) as f64)).unwrap_or(0.0);
            (d.abs(), json!({
                "name": n,
                "category": any.cat.name(),
                "source": any.source,
                "a_self": r3(mean(&va)), "b_self": r3(mean(&vb)),
                "delta": r3(d), "delta_pct": if pct.is_finite() { json!(r3(pct)) } else { Value::Null },
                "a_calls_per_frame": calls(sa, da.frames.len()), "b_calls_per_frame": calls(sb, db.frames.len()),
                "verdict": tag,
            }))
        })
        .collect();
    rows.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut ctr = Vec::new();
    for c in &a.counters {
        let Some(cb) = b.counter(&c.name) else { continue };
        let (Some(x), Some(y)) = (c.range_stats(0, u64::MAX), cb.range_stats(0, u64::MAX)) else { continue };
        if (x.2 - y.2).abs() > 1e-9 {
            ctr.push(json!({"name": c.name, "a_mean": r3(x.2), "b_mean": r3(y.2), "delta": r3(y.2 - x.2), "delta_pct": if x.2.abs() > 1e-12 { json!(r3((y.2 - x.2) / x.2 * 100.0)) } else { Value::Null }}));
        }
    }
    json!({
        "schema": DIFF_SCHEMA,
        "a": {"file": a.meta.source, "build": a.meta.build, "host": a.meta.host, "load_avg": a.meta.load_avg, "display_frames": da.frames.len()},
        "b": {"file": b.meta.source, "build": b.meta.build, "host": b.meta.host, "load_avg": b.meta.load_avg, "display_frames": db.frames.len()},
        "unit": unit,
        "notes": notes,
        "frames": frames,
        "main_lane_busy_per_frame": main,
        "scopes": rows.into_iter().take(top).map(|r| r.1).collect::<Vec<_>>(),
        "counters": ctr,
    })
}

pub fn diff_text(a: &Capture, b: &Capture, top: usize) -> String {
    let v = diff_json(a, b, top);
    let mut o = String::new();
    let s = |x: &Value| x.as_str().unwrap_or("").to_string();
    let _ = writeln!(o, "A: {} (build {}, {} frames)\nB: {} (build {}, {} frames)", s(&v["a"]["file"]), s(&v["a"]["build"]), v["a"]["display_frames"], s(&v["b"]["file"]), s(&v["b"]["build"]), v["b"]["display_frames"]);
    let instr = v["unit"] == "instr";
    let _ = writeln!(o, "unit: {}", if instr { "M instructions per frame" } else { "ms per frame" });
    for n in v["notes"].as_array().into_iter().flatten() {
        let _ = writeln!(o, "note: {}", s(n));
    }
    let _ = writeln!(o, "\nframes (ms)     A mean    B mean     delta       %   verdict");
    for (k, x) in v["frames"].as_object().into_iter().flatten() {
        let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
        let _ = writeln!(o, "  {:<12} {:>8.3} {:>9.3} {:>+9.3} {:>+7.1}   {}", k, g("a_mean_ms"), g("b_mean_ms"), g("delta_ms"), g("delta_pct"), s(&x["verdict"]));
    }
    let m = &v["main_lane_busy_per_frame"];
    let g = |n: &str| m[n].as_f64().unwrap_or(0.0);
    let _ = writeln!(o, "  main busy    {:>8.3} {:>9.3} {:>+9.3} {:>+7.1}   {}  ({})", g("a"), g("b"), g("delta"), g("delta_pct"), s(&m["verdict"]), s(&m["unit"]));
    let _ = writeln!(o, "\nscopes by size of change (self per frame)");
    let _ = writeln!(o, "  {:<34} {:>9} {:>9} {:>9} {:>8}  {:<7} {:<9} source", "scope", "A", "B", "delta", "%", "verdict", "category");
    for x in v["scopes"].as_array().into_iter().flatten() {
        let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
        let pct = x["delta_pct"].as_f64().map(|p| format!("{p:+.1}")).unwrap_or_else(|| "—".into());
        let _ = writeln!(o, "  {:<34} {:>9.3} {:>9.3} {:>+9.3} {:>8}  {:<7} {:<9} {}", trunc(&s(&x["name"]), 34), g("a_self"), g("b_self"), g("delta"), pct, s(&x["verdict"]), s(&x["category"]), s(&x["source"]));
    }
    let ctr = v["counters"].as_array().cloned().unwrap_or_default();
    if !ctr.is_empty() {
        let _ = writeln!(o, "\ncounters (mean)                    A              B          delta");
        for x in ctr {
            let g = |n: &str| x[n].as_f64().unwrap_or(0.0);
            let _ = writeln!(o, "  {:<28} {:>12.2} {:>14.2} {:>+14.2}", trunc(&s(&x["name"]), 28), g("a_mean"), g("b_mean"), g("delta"));
        }
    }
    o
}
