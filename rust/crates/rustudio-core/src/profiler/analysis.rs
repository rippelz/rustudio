//! Reading a capture: frame statistics, per-frame category breakdowns, call
//! trees and flat hot lists over a time range. Pure functions of a
//! `Capture`, so a file opened from disk reads the same as the live one.

use super::capture::{Capture, FrameRec, SpanRec, Track};
use super::{Cat, FrameKind};

/// Frame time statistics over a set of frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameStats {
    pub count: usize,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Frames per second over the wall time the frames cover.
    pub fps: f64,
}

/// The `p` quantile (0..=1) of sorted values (nearest rank).
pub fn quantile_sorted(v: &[u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let i = ((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1;
    v[i]
}

pub fn frame_stats<'a>(frames: impl IntoIterator<Item = &'a FrameRec>) -> FrameStats {
    let mut d: Vec<u64> = Vec::new();
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    for f in frames {
        d.push(f.dur());
        lo = lo.min(f.start);
        hi = hi.max(f.end);
    }
    if d.is_empty() {
        return FrameStats::default();
    }
    d.sort_unstable();
    let ms = |ns: u64| ns as f64 / 1e6;
    let sum: u64 = d.iter().sum();
    FrameStats {
        count: d.len(),
        avg_ms: ms(sum) / d.len() as f64,
        min_ms: ms(d[0]),
        p50_ms: ms(quantile_sorted(&d, 0.5)),
        p95_ms: ms(quantile_sorted(&d, 0.95)),
        p99_ms: ms(quantile_sorted(&d, 0.99)),
        max_ms: ms(*d.last().unwrap_or(&0)),
        fps: if hi > lo { d.len() as f64 / ((hi - lo) as f64 / 1e9) } else { 0.0 },
    }
}

/// The frames of `kind` overlapping [t0, t1].
pub fn frames_in(cap: &Capture, kind: FrameKind, t0: u64, t1: u64) -> impl Iterator<Item = &FrameRec> {
    let f = cap.frames_of(kind);
    let first = f.partition_point(|x| x.end <= t0);
    f.range(first..).take_while(move |x| x.start < t1)
}

/// The lane that drew the display frames (the editor's or player's main
/// thread), or the first lane.
pub fn main_track(cap: &Capture) -> Option<&Track> {
    cap.render_track
        .and_then(|id| cap.tracks.iter().find(|t| t.id == id))
        .or_else(|| cap.tracks.iter().find(|t| t.name == "main"))
        .or_else(|| cap.tracks.first())
}

/// Self time per span (its duration less its direct children's), for the
/// spans of one track in end order. Returns (span, self ns) pairs.
pub fn with_self_time<'a>(spans: impl Iterator<Item = &'a SpanRec>) -> Vec<(&'a SpanRec, u64)> {
    // In end order a span's children come right before it, after its previous
    // sibling: child time accumulates per depth and is taken by the parent.
    let mut acc = [0u64; 257];
    let mut out = Vec::new();
    for s in spans {
        let d = s.depth as usize;
        let child = std::mem::take(&mut acc[d + 1]);
        let own = s.dur().saturating_sub(child);
        acc[d] += s.dur();
        out.push((s, own));
    }
    out
}

/// Nanoseconds of self time per category in [t0, t1] on `track`, plus
/// the time no span covered (reported as `Cat::Wait`).
pub fn category_breakdown(track: &Track, t0: u64, t1: u64) -> [u64; Cat::ALL.len()] {
    let mut by = [0u64; Cat::ALL.len()];
    let mut covered = 0u64;
    for (s, own) in with_self_time(track.spans_in(t0, t1)) {
        let a = s.start.max(t0);
        let b = s.end.min(t1);
        if b <= a || s.dur() == 0 {
            continue;
        }
        // (a span cut by the range counts the share inside it)
        let inside = (own as u128 * (b - a) as u128 / s.dur() as u128) as u64;
        by[s.cat as usize % by.len()] += inside;
        if s.depth == 0 {
            covered += b - a;
        }
    }
    let total = t1.saturating_sub(t0);
    by[Cat::Wait as usize] += total.saturating_sub(covered);
    by
}

/// Statistics of one call-tree node or hot-list entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Time inside the scope, children included (ns).
    pub incl_ns: u64,
    /// Time in the scope itself (ns).
    pub self_ns: u64,
    pub calls: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub p95_ns: u64,
    /// Instructions (instruction mode), children included / self.
    pub incl_instr: u64,
    pub self_instr: u64,
    /// Display frames it ran in, and its inclusive time per frame: the 95th
    /// percentile and the worst frame (over the frames it ran in).
    pub frames_present: u32,
    pub p95_frame_ns: u64,
    pub max_frame_ns: u64,
    /// Bytes allocated inside it (children included), when counted.
    pub alloc_bytes: u64,
}

impl Stats {
    pub fn avg_ns(&self) -> u64 {
        self.incl_ns.checked_div(self.calls).unwrap_or(0)
    }
}

/// A node of the merged call tree: every call of the same name under the
/// same parent path is one node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Index in `Capture::names`; `None` for a thread's root node.
    pub name: Option<u32>,
    /// The thread's name for root nodes.
    pub label: String,
    pub cat: Cat,
    pub track: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub stats: Stats,
    /// (display frame index in the range, inclusive ns in that frame), in
    /// frame order: the sidebar's per-frame bars.
    pub per_frame: Vec<(u32, u64)>,
}

/// The call tree over [t0, t1]: node 0 is the root, its children the
/// threads, theirs the outermost scopes.
#[derive(Clone, Debug, Default)]
pub struct CallTree {
    pub nodes: Vec<Node>,
    /// Display frames in the range (for "per frame" and "% of frame").
    pub frames: usize,
    /// Their average length (ns).
    pub avg_frame_ns: u64,
}

impl CallTree {
    pub fn name<'a>(&'a self, cap: &'a Capture, i: usize) -> &'a str {
        match self.nodes[i].name {
            Some(n) => cap.name(n),
            None => &self.nodes[i].label,
        }
    }
}

struct Building {
    node: Node,
    durs: Vec<u64>,
    child_ix: rustc_hash::FxHashMap<u32, usize>,
}

fn building(node: Node) -> Building {
    Building { node, durs: vec![], child_ix: Default::default() }
}

fn node(name: Option<u32>, label: String, cat: Cat, track: usize, parent: Option<usize>) -> Node {
    Node { name, label, cat, track, parent, children: vec![], stats: Stats::default(), per_frame: vec![] }
}

/// The display frame (index into `frames`) a time falls in, if any.
fn frame_of(frames: &[FrameRec], t: u64) -> Option<u32> {
    let i = frames.partition_point(|f| f.start <= t);
    (i > 0 && t < frames[i - 1].end).then(|| (i - 1) as u32)
}

/// Builds the call tree of the tracks `keep` selects over [t0, t1]. Spans
/// count whole if they overlap the range (a frame's scopes, not cut ones).
pub fn call_tree(cap: &Capture, t0: u64, t1: u64, keep: impl Fn(&Track) -> bool) -> CallTree {
    let frames: Vec<FrameRec> = frames_in(cap, FrameKind::Render, t0, t1).copied().collect();
    let mut b = vec![building(node(None, "All threads".into(), Cat::Frame, usize::MAX, None))];
    for (ti, track) in cap.tracks.iter().enumerate() {
        if !keep(track) {
            continue;
        }
        let mut spans: Vec<(&SpanRec, u64)> = with_self_time(track.spans_in(t0, t1));
        if spans.is_empty() {
            continue;
        }
        spans.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.depth.cmp(&b.0.depth)).then(b.0.end.cmp(&a.0.end)));
        let tn = b.len();
        b.push(building(node(None, track.name.clone(), Cat::Frame, ti, Some(0))));
        b[0].node.children.push(tn);
        // stack of (span, node)
        let mut stack: Vec<(&SpanRec, usize)> = Vec::new();
        for (s, own) in spans {
            while let Some(&(top, _)) = stack.last() {
                if top.end < s.end || top.depth >= s.depth || top.start > s.start {
                    stack.pop();
                } else {
                    break;
                }
            }
            let parent = stack.last().map(|x| x.1).unwrap_or(tn);
            let ni = match b[parent].child_ix.get(&s.name) {
                Some(&i) => i,
                None => {
                    let i = b.len();
                    b.push(building(node(Some(s.name), String::new(), s.cat, ti, Some(parent))));
                    b[parent].child_ix.insert(s.name, i);
                    b[parent].node.children.push(i);
                    i
                }
            };
            let st = &mut b[ni].node.stats;
            st.incl_ns += s.dur();
            st.self_ns += own;
            st.calls += 1;
            st.incl_instr += s.instr;
            st.alloc_bytes += s.alloc;
            b[ni].durs.push(s.dur());
            if let Some(f) = frame_of(&frames, s.start) {
                let pf = &mut b[ni].node.per_frame;
                match pf.last_mut() {
                    Some(last) if last.0 == f => last.1 += s.dur(),
                    _ => pf.push((f, s.dur())),
                }
            }
            if stack.is_empty() {
                b[tn].node.stats.incl_ns += s.dur();
                b[tn].node.stats.incl_instr += s.instr;
            }
            stack.push((s, ni));
        }
    }
    // self instructions = incl − children's incl; thread totals up to the root
    let n = b.len();
    for i in (1..n).rev() {
        let kids: u64 = b[i].node.children.iter().map(|&c| b[c].node.stats.incl_instr).sum();
        b[i].node.stats.self_instr = b[i].node.stats.incl_instr.saturating_sub(kids);
    }
    let threads: Vec<usize> = b[0].node.children.clone();
    for t in threads {
        let (ns, ins) = (b[t].node.stats.incl_ns, b[t].node.stats.incl_instr);
        b[0].node.stats.incl_ns += ns;
        b[0].node.stats.incl_instr += ins;
        b[t].node.stats.calls = 1;
    }
    let nodes = b
        .into_iter()
        .map(|mut x| {
            if !x.durs.is_empty() {
                x.durs.sort_unstable();
                x.node.stats.min_ns = x.durs[0];
                x.node.stats.max_ns = *x.durs.last().unwrap_or(&0);
                x.node.stats.p95_ns = quantile_sorted(&x.durs, 0.95);
            }
            // (a node fed by several lanes: merge their entries per frame)
            let pf = &mut x.node.per_frame;
            if pf.windows(2).any(|w| w[0].0 >= w[1].0) {
                pf.sort_unstable_by_key(|e| e.0);
                pf.dedup_by(|a, b| {
                    if a.0 == b.0 {
                        b.1 += a.1;
                        true
                    } else {
                        false
                    }
                });
            }
            if !pf.is_empty() {
                let mut v: Vec<u64> = pf.iter().map(|e| e.1).collect();
                v.sort_unstable();
                x.node.stats.frames_present = v.len() as u32;
                x.node.stats.p95_frame_ns = quantile_sorted(&v, 0.95);
                x.node.stats.max_frame_ns = *v.last().unwrap_or(&0);
            }
            x.node
        })
        .collect();
    let avg = if frames.is_empty() { 0 } else { frames.iter().map(|f| f.dur()).sum::<u64>() / frames.len() as u64 };
    CallTree { nodes, frames: frames.len(), avg_frame_ns: avg }
}

/// One row of the flat hot list: every call of a name, wherever it was.
#[derive(Clone, Debug, PartialEq)]
pub struct HotEntry {
    pub name: u32,
    pub cat: Cat,
    /// Lanes it ran on.
    pub tracks: Vec<usize>,
    pub stats: Stats,
}

/// Every scope name over [t0, t1] with its totals, hottest self time first.
/// A recursive call counts its inclusive time once (at the outermost).
pub fn hot_list(cap: &Capture, t0: u64, t1: u64, keep: impl Fn(&Track) -> bool) -> Vec<HotEntry> {
    let mut map: rustc_hash::FxHashMap<u32, (HotEntry, Vec<u64>)> = Default::default();
    for (ti, track) in cap.tracks.iter().enumerate() {
        if !keep(track) {
            continue;
        }
        let mut spans = with_self_time(track.spans_in(t0, t1));
        spans.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.depth.cmp(&b.0.depth)));
        let mut stack: Vec<&SpanRec> = Vec::new();
        for (s, own) in spans {
            while stack.last().is_some_and(|t| t.end < s.end || t.depth >= s.depth) {
                stack.pop();
            }
            let recursive = stack.iter().any(|t| t.name == s.name);
            let e = map.entry(s.name).or_insert_with(|| {
                (HotEntry { name: s.name, cat: s.cat, tracks: vec![], stats: Stats::default() }, vec![])
            });
            if !e.0.tracks.contains(&ti) {
                e.0.tracks.push(ti);
            }
            e.0.stats.self_ns += own;
            e.0.stats.calls += 1;
            e.1.push(s.dur());
            if !recursive {
                e.0.stats.incl_ns += s.dur();
                e.0.stats.incl_instr += s.instr;
                e.0.stats.alloc_bytes += s.alloc;
            }
            stack.push(s);
        }
    }
    let mut out: Vec<HotEntry> = map
        .into_values()
        .map(|(mut e, mut d)| {
            d.sort_unstable();
            e.stats.min_ns = d.first().copied().unwrap_or(0);
            e.stats.max_ns = d.last().copied().unwrap_or(0);
            e.stats.p95_ns = quantile_sorted(&d, 0.95);
            e
        })
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.stats.self_ns));
    out
}

/// Whether the display frames over [t0, t1] waited on the GPU or the CPU:
/// (main thread busy ms, GPU busy ms) per frame, averaged.
pub fn cpu_gpu_balance(cap: &Capture, t0: u64, t1: u64) -> Option<(f64, f64)> {
    let main = main_track(cap)?;
    let frames: Vec<&FrameRec> = frames_in(cap, FrameKind::Render, t0, t1).collect();
    if frames.is_empty() {
        return None;
    }
    let mut cpu = 0u64;
    for f in &frames {
        let by = category_breakdown(main, f.start, f.end);
        cpu += by.iter().sum::<u64>() - by[Cat::Wait as usize];
    }
    let gpu_frames: Vec<&FrameRec> = frames_in(cap, FrameKind::Gpu, t0, t1).collect();
    let gpu = if gpu_frames.is_empty() {
        0.0
    } else {
        // a GPU frame's busy time = the sum of its spans on the GPU lane
        let lane = cap.tracks.iter().find(|t| t.kind == super::capture::TrackKind::Gpu);
        let busy: u64 = lane
            .map(|l| l.spans_in(t0, t1).filter(|s| s.depth == 0).map(|s| s.dur()).sum())
            .unwrap_or(0);
        busy as f64 / 1e6 / gpu_frames.len() as f64
    };
    Some((cpu as f64 / 1e6 / frames.len() as f64, gpu))
}

/// Self time per category over [t0, t1] on the tracks `keep` selects
/// (the table's "By category" view), with call counts.
pub fn category_totals(cap: &Capture, t0: u64, t1: u64, keep: impl Fn(&Track) -> bool) -> Vec<(Cat, u64, u64)> {
    let mut by = [(0u64, 0u64); Cat::ALL.len()];
    for track in cap.tracks.iter().filter(|t| keep(t)) {
        for (s, own) in with_self_time(track.spans_in(t0, t1)) {
            let e = &mut by[s.cat as usize % by.len()];
            e.0 += own;
            e.1 += 1;
        }
    }
    let mut out: Vec<(Cat, u64, u64)> =
        Cat::ALL.iter().map(|&c| (c, by[c as usize].0, by[c as usize].1)).filter(|e| e.2 > 0).collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.1));
    out
}

/// One GPU pass over a range of GPU frames.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuPass {
    /// Index in `Capture::names` ("gpu: scene (opaque, …)").
    pub name: u32,
    /// Mean per GPU frame (frames it didn't run in count as 0) and worst (ns).
    pub avg_ns: u64,
    pub max_ns: u64,
    /// Share of all timed GPU work over the range (0..=1).
    pub share: f64,
    /// GPU frames it ran in.
    pub frames_present: usize,
    /// Its time per GPU frame, oldest first (the history sparkline).
    pub history: Vec<u64>,
}

/// Per-pass GPU times over [t0, t1] (the GPU tab), in the order the passes
/// first ran, and the mean timed GPU work per frame (ns).
pub fn gpu_passes(cap: &Capture, t0: u64, t1: u64) -> (Vec<GpuPass>, u64) {
    let Some(lane) = cap.tracks.iter().find(|t| t.kind == super::capture::TrackKind::Gpu) else {
        return (Vec::new(), 0);
    };
    let frames: Vec<FrameRec> = frames_in(cap, FrameKind::Gpu, t0, t1).copied().collect();
    if frames.is_empty() {
        return (Vec::new(), 0);
    }
    let mut order: Vec<u32> = Vec::new();
    let mut per: rustc_hash::FxHashMap<u32, Vec<u64>> = Default::default();
    let mut spans: Vec<&SpanRec> = lane.spans_in(t0, t1).collect();
    spans.sort_by_key(|s| s.start);
    for s in spans {
        let Some(f) = frame_of(&frames, s.start) else { continue };
        let v = per.entry(s.name).or_insert_with(|| {
            order.push(s.name);
            vec![0; frames.len()]
        });
        v[f as usize] += s.dur();
    }
    let total: u64 = per.values().flatten().sum();
    let n = frames.len() as u64;
    let passes = order
        .into_iter()
        .map(|name| {
            let h = per.remove(&name).unwrap_or_default();
            let sum: u64 = h.iter().sum();
            GpuPass {
                name,
                avg_ns: sum / n,
                max_ns: h.iter().copied().max().unwrap_or(0),
                share: if total > 0 { sum as f64 / total as f64 } else { 0.0 },
                frames_present: h.iter().filter(|&&v| v > 0).count(),
                history: h,
            }
        })
        .collect();
    (passes, total / n)
}

/// How the window names and orders a lane: (title, subtitle, sort key).
/// Numbered pools ("physics-par-1") keep their number.
pub fn lane_info(name: &str) -> (String, &'static str, u32) {
    let num = |prefix: &str| name.strip_prefix(prefix).map(|n| n.parse::<u32>().map_or(0, |n| n + 1));
    const POOLS: &[(&str, &str, &str, u32)] = &[
        ("physics-par-", "Physics", "solver workers", 30),
        ("rustudio-character-", "Characters", "humanoid workers", 40),
        ("rustudio-path-", "Pathfinding", "path search pool", 50),
        ("asset-download-", "Asset download", "web loader", 60),
        ("tex-decode-", "Texture decode", "image decode pool", 70),
        ("toolbox-", "Toolbox", "search and thumbnails", 90),
    ];
    for (prefix, title, sub, order) in POOLS {
        if let Some(n) = num(prefix) {
            return (format!("{title} {n}"), sub, order * 100 + n);
        }
    }
    let (title, sub, order) = match name {
        "main" => ("Main", "editor + Play client", 0),
        "rustudio-gameplay" => ("Play worker", "server · scripts + sim", 10),
        "GPU" => ("GPU", "timestamp queries", 20),
        "collision" => ("Collision", "broadphase", 35),
        "rustudio-http" => ("HTTP", "HttpService requests", 80),
        "rustudio-team-create" => ("Team Create", "live session", 85),
        "rustudio-webstream" => ("Web stream", "streamed requests", 86),
        "layered clothing" => ("Layered clothing", "wrap solve", 75),
        "material packs" => ("Material packs", "loader", 76),
        "hsr" => ("Surface removal", "hidden surfaces", 77),
        "studio-content" => ("Studio content", "downloads", 95),
        _ => return (name.to_string(), "", 10_000),
    };
    (title.to_string(), sub, order * 100)
}

/// Formats nanoseconds at a readable scale: "812 ns", "43.2 µs", "3.41 ms", "1.20 s".
pub fn fmt_ns(ns: u64) -> String {
    let v = ns as f64;
    if ns < 1_000 {
        format!("{ns} ns")
    } else if ns < 1_000_000 {
        format!("{:.1} µs", v / 1e3)
    } else if ns < 1_000_000_000 {
        format!("{:.2} ms", v / 1e6)
    } else {
        format!("{:.2} s", v / 1e9)
    }
}

/// Instructions at a readable scale: "812", "43.2 K", "3.41 M", "1.20 G".
pub fn fmt_instr(n: u64) -> String {
    let v = n as f64;
    if n < 10_000 {
        format!("{n}")
    } else if n < 10_000_000 {
        format!("{:.1} K", v / 1e3)
    } else if n < 10_000_000_000 {
        format!("{:.2} M", v / 1e6)
    } else {
        format!("{:.2} G", v / 1e9)
    }
}
