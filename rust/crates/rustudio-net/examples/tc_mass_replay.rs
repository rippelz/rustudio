//! Offline replay of recorded Team Create streams: feeds every
//! message through the replicator `Decoder` and lists each message it could
//! not fully read (an unknown item kind, an unknown value type or any other
//! decode error) with byte offsets and hex, per battery step.
//!
//!   cargo run -p rustudio-net --example tc_mass_replay -- \
//!       [--json OUT.json] [--corpus DIR] [--step-attr NAME] [--lanes DIR]
//!       [--trace NAME_PREFIX]... STREAM...
//!
//! - `--trace P`: prints (stderr) every op on instances named P* and their
//!   descendants: creation with its property names, changes, moves, deletes.
//! - `--show A-B`: prints messages A..=B (index in each stream): packet,
//!   length, decoded ops, the item kinds inside, the first bytes.
//!
//! - STREAM: a primary-lane capture (`RUSTUDIO_TC_STREAM_OUT`), a
//!   `RUSTUDIO_TC_BAD_FRAME` file or a saved `tc-unknown/*.stream` (schema
//!   message + the message). Each file is its own session (own decoder).
//! - Battery steps: the harness sets the attribute `tcmass_step` (a string)
//!   before each step, so every message after it belongs to that step.
//! - Offsets: `file_offset` is where the message's 4-byte length prefix
//!   starts in the file; `item_offset` is the unreadable item's offset inside
//!   the message (byte 0 = the packet id). The item boundary is found by
//!   decoding one item at a time (the shortest prefix that decodes cleanly is
//!   one item), so it is exact for well-formed items before it.
//! - `--lanes DIR` (a `RUSTUDIO_TC_LANES_OUT` folder): its other channel-4
//!   lanes and `datagrams.bin` are decoded too, with the first STREAM's schema.
//! - `--corpus DIR`: writes each problem message as a fixture in the
//!   `RUSTUDIO_TC_BAD_FRAME` format (header, schema message, the message).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rustudio_net::replicator::{packet_id, split_stream, Decoder, RepOp};
use serde_json::{json, Value as J};

/// The longest single item the boundary probe looks for.
const ITEM_WINDOW: usize = 16 * 1024;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn op_name(op: &RepOp) -> &'static str {
    match op {
        RepOp::JoinDataBegin => "join_begin",
        RepOp::NewInstance { .. } => "new",
        RepOp::ChangeProperty { .. } => "prop",
        RepOp::ChangeAttribute { .. } => "attr",
        RepOp::DeleteInstance { .. } => "delete",
        RepOp::Reparent { .. } => "reparent",
        RepOp::Presence { .. } => "presence",
        RepOp::Event { .. } => "event",
        RepOp::Unhandled { .. } => "unhandled",
        RepOp::NetAsset { .. } => "net_asset",
        RepOp::Terrain { .. } => "terrain",
        RepOp::Services(_) => "services",
    }
}

/// A string attribute body (type 0x02, u32 LE length, bytes).
fn attr_string(v: &[u8]) -> Option<String> {
    if v.len() >= 5 && v[0] == 0x02 {
        let n = u32::from_le_bytes(v[1..5].try_into().ok()?) as usize;
        return v.get(5..5 + n).map(|s| String::from_utf8_lossy(s).into_owned());
    }
    None
}

#[derive(Default)]
struct StepStats {
    messages: u64,
    bytes: u64,
    ops: BTreeMap<&'static str, u64>,
    errors: u64,
    skipped: u64,
    unread_tails: u64,
    unknown_refs: u64,
    packets: BTreeMap<String, u64>,
}

impl StepStats {
    fn json(&self) -> J {
        json!({
            "messages": self.messages, "bytes": self.bytes, "ops": self.ops,
            "decode_errors": self.errors, "skipped_items": self.skipped,
            "unread_tails": self.unread_tails, "unknown_refs": self.unknown_refs, "packets": self.packets,
        })
    }
}

struct Ctx {
    attr: String,
    /// `--trace PREFIX`: instances whose Name starts with it, and every op on them.
    trace: Vec<String>,
    traced: std::collections::HashMap<u64, String>,
    /// `--show A-B`: print messages A..=B of each primary stream.
    show: Option<(usize, usize)>,
    /// `--terrain-region`: track Terrain voxels in the battery's 24x16x24 region
    /// and report material counts after the `terrain` step and at stream end.
    terrain_region: bool,
    terrain_grid: std::collections::HashMap<[i32; 3], (u8, u8, u16)>,
    terrain_lods: std::collections::HashMap<[i32; 3], u8>,
    terrain_after_step: Option<J>,
    /// Every class a NewInstance carried (what the server replicates).
    classes: std::collections::BTreeSet<String>,
    corpus: Option<PathBuf>,
    problems: Vec<J>,
    steps: BTreeMap<String, StepStats>,
    step_order: Vec<String>,
}

impl Ctx {
    fn stats(&mut self, step: &str) -> &mut StepStats {
        if !self.steps.contains_key(step) {
            self.step_order.push(step.to_string());
        }
        self.steps.entry(step.to_string()).or_default()
    }
}

/// Decodes exactly `body` (no terminator: a terminator byte could be read
/// as the tail of a truncated item) as one DATA message on `dec`, and
/// whether that read cleanly. The decoder's error/skip lists are restored.
fn clean(dec: &mut Decoder, body: &[u8]) -> bool {
    let (e, s) = (dec.errors.len(), dec.skipped.len());
    let _ = dec.decode_frame(packet_id::DATA, body);
    let ok = dec.errors.len() == e && dec.skipped.len() == s;
    dec.errors.truncate(e);
    dec.skipped.truncate(s);
    ok
}

/// One DATA body walked item by item with exact-slice probes: an item ends
/// at the shortest prefix that decodes cleanly by itself (no terminator, so
/// a truncated item can't pass). Instance batches (0x0d) are searched from
/// their compressed length on.
struct Walk {
    /// (offset in body, kind byte) of each item read cleanly.
    items: Vec<(usize, u8)>,
    /// Where the walk stopped: the 0x00 terminator, the end, or an item that
    /// does not decode by itself.
    end: usize,
    stuck: bool,
    note: Option<String>,
}

impl Walk {
    /// Non-zero bytes after the terminator: items the decoder never read
    /// (an earlier item was read too short and left a 0 byte behind).
    fn unread_tail<'a>(&self, body: &'a [u8]) -> Option<&'a [u8]> {
        if self.stuck || self.end >= body.len() || body[self.end] != 0 {
            return None;
        }
        let tail = &body[self.end + 1..];
        tail.iter().any(|&b| b != 0).then_some(tail)
    }
}

fn walk(dec: &mut Decoder, body: &[u8]) -> Walk {
    let mut w = Walk { items: Vec::new(), end: 0, stuck: false, note: None };
    let mut p = 0;
    while p < body.len() && body[p] != 0 {
        let kind = body[p];
        if kind == 0x16 {
            // Initial-stream chunks feed per-stage buffers: probing would
            // disturb them.
            w.note = Some("initial-stream chunk (0x16): not probed".into());
            w.end = body.len();
            return w;
        }
        let u32_at = |o: usize| body.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        let range = if kind == 0x0d {
            let lower = match u32_at(p + 1) {
                Some(0) => p + 5,
                Some(_) => p + 13 + u32_at(p + 5).unwrap_or(0),
                None => p + 1,
            };
            lower.min(body.len())..=body.len().min(lower + 512)
        } else {
            p + 1..=body.len().min(p + ITEM_WINDOW)
        };
        match range.into_iter().find(|&q| q > p && clean(dec, &body[p..q])) {
            Some(q) => {
                w.items.push((p, kind));
                p = q;
            }
            None => {
                w.end = p;
                w.stuck = true;
                return w;
            }
        }
    }
    w.end = p;
    w
}

fn write_fixture(dir: &Path, name: &str, schema: &[u8], msg: &[u8]) -> Option<String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).ok()?;
    f.write_all(&[6, 1, 4, 0, 0, 0, 1]).ok()?;
    for m in [schema, msg] {
        f.write_all(&(0x8000_0000u32 | m.len() as u32).to_be_bytes()).ok()?;
        f.write_all(m).ok()?;
    }
    Some(path.display().to_string())
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Replays one session's messages. `start_step` is the step for messages
/// before the first marker. Returns the schema message seen (for lanes).
#[allow(clippy::too_many_arguments)]
fn replay(
    ctx: &mut Ctx,
    source: &str,
    msgs: &[(usize, &[u8])],
    dec: &mut Decoder,
    schema_in: Option<Vec<u8>>,
    start_step: &str,
    lane: &str,
) -> Option<Vec<u8>> {
    let mut schema = schema_in;
    let mut step = start_step.to_string();
    let mut known: std::collections::HashSet<u64> = Default::default();
    for (i, (file_offset, m)) in msgs.iter().enumerate() {
        if m.is_empty() {
            continue;
        }
        let pid = m[0];
        if pid == packet_id::NEW_SCHEMA {
            schema = Some(m.to_vec());
        }
        let (e0, s0) = (dec.errors.len(), dec.skipped.len());
        let ops = dec.decode_frame(pid, &m[1..]);
        for op in &ops {
            if let RepOp::ChangeAttribute { name, value, .. } = op {
                if *name == ctx.attr {
                    if let Some(s) = attr_string(value) {
                        if ctx.terrain_region && step == "terrain" && s != "terrain" && ctx.terrain_after_step.is_none() {
                            ctx.terrain_after_step = Some(summarize_terrain(&ctx.terrain_grid));
                        }
                        step = s;
                    }
                }
            }
            if ctx.terrain_region {
                if let RepOp::Terrain { chunks, .. } = op {
                    apply_terrain_chunks(&mut ctx.terrain_grid, &mut ctx.terrain_lods, chunks);
                }
            }
        }
        if !ctx.trace.is_empty() {
            trace_ops(ctx, &step, i, &ops);
        }
        if ctx.show.is_some_and(|(a, b)| (a..=b).contains(&i)) {
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            for op in &ops {
                *kinds.entry(op_name(op)).or_default() += 1;
            }
            let items = if pid == packet_id::DATA {
                let w = walk(dec, &m[1..]);
                let mut t = w.items.iter().map(|(o, k)| format!("{k:02x}@{}", o + 1)).collect::<Vec<_>>().join(" ");
                if w.stuck {
                    t += &format!(" STUCK@{}", w.end + 1);
                }
                if let Some(tail) = w.unread_tail(&m[1..]) {
                    t += &format!(" UNREAD-TAIL@{} ({} bytes)", w.end + 2, tail.len());
                }
                t
            } else {
                String::new()
            };
            eprintln!("show [{step}] m{i} @{file_offset} packet {pid:#04x} len {} ops {kinds:?} items [{items}] head {}", m.len(), hex(&m[..m.len().min(24)]));
        }
        let errors: Vec<String> = dec.errors[e0..].to_vec();
        let skipped: Vec<u8> = dec.skipped[s0..].to_vec();
        dec.errors.truncate(e0);
        dec.skipped.truncate(s0);
        {
            let st = ctx.stats(&step);
            st.messages += 1;
            st.bytes += m.len() as u64;
            *st.packets.entry(format!("{pid:#04x}")).or_default() += 1;
            for op in &ops {
                *st.ops.entry(op_name(op)).or_default() += 1;
            }
            st.errors += errors.len() as u64;
            st.skipped += skipped.len() as u64;
        }
        // Ops on instances this session never created: the decoder read
        // something that is not what it thinks (e.g. an item read too short
        // and the next bytes taken as a delete).
        let mut unknown_refs = Vec::new();
        for op in &ops {
            match op {
                RepOp::NewInstance { id, class, .. } => {
                    known.insert(*id);
                    if !ctx.classes.contains(class) {
                        ctx.classes.insert(class.clone());
                    }
                }
                RepOp::Services(list) => {
                    known.extend(list.iter().map(|(id, _)| *id));
                    ctx.classes.extend(list.iter().map(|(_, c)| c.clone()));
                }
                RepOp::DeleteInstance { id } | RepOp::ChangeProperty { id, .. } | RepOp::ChangeAttribute { id, .. }
                    if !known.contains(id) =>
                {
                    unknown_refs.push(format!("{} #{id:x}", op_name(op)));
                }
                RepOp::Reparent { id, parent } if !known.contains(id) || !known.contains(parent) => {
                    unknown_refs.push(format!("reparent #{id:x} -> #{parent:x}"));
                }
                _ => {}
            }
        }
        for op in &ops {
            if let RepOp::DeleteInstance { id } = op {
                known.remove(id);
            }
        }
        let w = (pid == packet_id::DATA).then(|| walk(dec, &m[1..]));
        let tail = w.as_ref().and_then(|w| w.unread_tail(&m[1..]));
        let what = if !skipped.is_empty() {
            "unknown_item"
        } else if !errors.is_empty() {
            "decode_error"
        } else if tail.is_some() {
            "unread_tail"
        } else if !unknown_refs.is_empty() {
            "unknown_ref"
        } else {
            continue;
        };
        {
            let st = ctx.stats(&step);
            match what {
                "unread_tail" => st.unread_tails += 1,
                "unknown_ref" => st.unknown_refs += 1,
                _ => {}
            }
        }
        // The item to look at: where the walk got stuck; for an unread tail,
        // the last item read (it was read too short).
        let (item_offset, note) = match &w {
            Some(w) if w.stuck => (Some(w.end + 1), w.note.clone()),
            Some(w) if tail.is_some() => (w.items.last().map(|(o, _)| o + 1), Some(format!(
                "decoding stopped at the 0x00 at offset {}; {} unread bytes follow (first item kind after it: {:#04x})",
                w.end + 1, tail.unwrap().len(), tail.unwrap().iter().find(|&&b| b != 0).copied().unwrap_or(0)))),
            Some(w) => (w.items.last().map(|(o, _)| o + 1), w.note.clone()),
            None => (None, Some(format!("packet {pid:#04x} is not item-framed"))),
        };
        let items_before = w.as_ref().map_or(0, |w| w.items.len());
        let at = item_offset.unwrap_or(0);
        let kind = item_offset.map(|o| format!("{:#04x}", m[o]));
        let fixture = match (&ctx.corpus, &schema) {
            (Some(dir), Some(s)) => {
                let name = format!(
                    "{}-{}-{}-m{i}-{what}{}.stream",
                    safe(&step),
                    safe(Path::new(source).file_stem().and_then(|s| s.to_str()).unwrap_or("stream")),
                    safe(lane),
                    kind.as_deref().map(|k| format!("-{k}")).unwrap_or_default()
                );
                write_fixture(dir, &name, s, m)
            }
            _ => None,
        };
        ctx.problems.push(json!({
            "source": source, "lane": lane, "message_index": i, "file_offset": file_offset,
            "packet": format!("{pid:#04x}"), "message_len": m.len(), "step": step,
            "what": what,
            "skipped_kinds": skipped.iter().map(|k| format!("{k:#04x}")).collect::<Vec<_>>(),
            "errors": errors,
            "unknown_refs": unknown_refs,
            "items": w.as_ref().map(|w| w.items.iter().map(|(o, k)| format!("{k:02x}@{}", o + 1)).collect::<Vec<_>>()),
            "item_offset": item_offset, "item_kind": kind, "clean_items_before": items_before,
            "note": note,
            "hex_at_item": hex(&m[at..m.len().min(at + 96)]),
            "hex_message_head": hex(&m[..m.len().min(48)]),
            "fixture": fixture,
        }));
    }
    schema
}

fn trace_ops(ctx: &mut Ctx, step: &str, msg: usize, ops: &[RepOp]) {
    for op in ops {
        let line = match op {
            RepOp::NewInstance { id, class, parent, props } => {
                let name = props.iter().find_map(|(n, v)| match (n.as_str(), v) {
                    ("Name", rustudio_net::replicator::PropValue::String(s)) => Some(s.clone()),
                    _ => None,
                });
                let parent_name = parent.and_then(|p| ctx.traced.get(&p).cloned());
                match name {
                    Some(n) if ctx.trace.iter().any(|t| n.starts_with(t.as_str())) || parent_name.is_some() => {
                        ctx.traced.insert(*id, n.clone());
                        format!("new {class} {n:?} #{id:x} parent {parent:x?} ({} props: {})", props.len(),
                            props.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(","))
                    }
                    _ => continue,
                }
            }
            RepOp::ChangeProperty { id, name, value } if ctx.traced.contains_key(id) => {
                let mut v = format!("{value:?}");
                v.truncate(100);
                format!("prop {:?}.{name} = {v}", ctx.traced[id])
            }
            RepOp::ChangeAttribute { id, name, value } if ctx.traced.contains_key(id) => format!("attr {:?}.{name} = {} bytes", ctx.traced[id], value.len()),
            RepOp::DeleteInstance { id } if ctx.traced.contains_key(id) => format!("delete {:?}", ctx.traced[id]),
            RepOp::Reparent { id, parent } if ctx.traced.contains_key(id) => format!("reparent {:?} -> #{parent:x}", ctx.traced[id]),
            _ => continue,
        };
        eprintln!("trace [{step}] m{msg}: {line}");
    }
}

/// Messages of an rtcio stream file with each one's file offset.
fn stream_msgs(bytes: &[u8]) -> Option<(u8, [u8; 4], Vec<(usize, &[u8])>)> {
    let (ch, tag, msgs) = split_stream(bytes)?;
    let mut o = 7;
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        out.push((o, m));
        o += 4 + m.len();
    }
    Some((ch, tag, out))
}

/// `datagrams.bin`: frames without a lane header; each is `02 <varint> 00`
/// then one replicator message.
fn datagram_msgs(bytes: &[u8]) -> Vec<(usize, &[u8])> {
    let mut out = Vec::new();
    let mut o = 0;
    while o + 4 <= bytes.len() {
        let w = u32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
        let n = (w & 0x7fff_ffff) as usize;
        if o + 4 + n > bytes.len() {
            break;
        }
        let d = &bytes[o + 4..o + 4 + n];
        let mut p = 1;
        while p < d.len() && d[p] & 0x80 != 0 {
            p += 1;
        }
        p += 2; // last varint byte + the 0 byte
        if d.first() == Some(&2) && p <= d.len() {
            out.push((o, &d[p..]));
        }
        o += 4 + n;
    }
    out
}

const TERRAIN_MATERIALS: [&str; 23] = [
    "Air", "Water", "Grass", "Slate", "Concrete", "Brick", "Sand", "WoodPlanks", "Rock",
    "Glacier", "Snow", "Sandstone", "Mud", "Basalt", "Ground", "CrackedLava", "Asphalt",
    "Cobblestone", "Ice", "LeafyGrass", "Salt", "Limestone", "Pavement",
];

fn apply_terrain_chunks(
    grid: &mut std::collections::HashMap<[i32; 3], (u8, u8, u16)>,
    lods: &mut std::collections::HashMap<[i32; 3], u8>,
    chunks: &[rustudio_net::replicator::TerrainChunk],
) {
    for chunk in chunks {
        let authoritative = chunk.runs.is_empty() || matches!(chunk.mode, None | Some(2));
        if !authoritative && lods.get(&chunk.origin).is_some_and(|&lod| chunk.lod > lod) {
            continue;
        }
        lods.insert(chunk.origin, chunk.lod);
        let empty_run = [rustudio_net::replicator::TerrainRun {
            length: 1 << (3 * chunk.size_log2),
            material: 0,
            occupancy: 0,
            water: 0,
        }];
        let runs: &[rustudio_net::replicator::TerrainRun] =
            if chunk.runs.is_empty() { &empty_run } else { &chunk.runs };
        let edge = 1i32 << chunk.size_log2;
        let scale = 1i32 << chunk.lod;
        let mut index = 0i32;
        for run in runs {
            let water = if run.material > 1 && run.occupancy != 255 { run.water } else { 0 };
            for cell in index..index + i32::from(run.length) {
                let bx = (cell % edge) * scale;
                let by = (cell / (edge * edge)) * scale;
                let bz = (cell / edge % edge) * scale;
                for dx in 0..scale {
                    for dy in 0..scale {
                        for dz in 0..scale {
                            let local = [bx + dx, by + dy, bz + dz];
                            if chunk.boxes.is_empty()
                                || chunk.boxes.iter().any(|(offset, extent)| {
                                    (0..3).all(|a| {
                                        let s = i32::from(offset[a]);
                                        (s..s + i32::from(extent[a])).contains(&local[a])
                                    })
                                })
                            {
                                let pos = [
                                    chunk.origin[0] + local[0],
                                    chunk.origin[1] + local[1],
                                    chunk.origin[2] + local[2],
                                ];
                                if run.material == 0 {
                                    grid.remove(&pos);
                                } else {
                                    grid.insert(pos, (run.material, run.occupancy, water));
                                }
                            }
                        }
                    }
                }
            }
            index += i32::from(run.length);
        }
    }
}

fn summarize_terrain(grid: &std::collections::HashMap<[i32; 3], (u8, u8, u16)>) -> J {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut occ_sum = 0usize;
    for x in 88..112 {
        for y in 0..16 {
            for z in 88..112 {
                let (mat, occ_byte) = match grid.get(&[x, y, z]) {
                    Some(&(m, o, _)) if m != 0 => (m, o),
                    _ => (0, 0),
                };
                let name = TERRAIN_MATERIALS.get(mat as usize).copied().unwrap_or("Unknown");
                *counts.entry(name).or_default() += 1;
                if mat != 0 {
                    let occ = (f64::from(occ_byte) + 1.0) / 256.0;
                    occ_sum += (occ * 100.0 + 0.5).floor() as usize;
                }
            }
        }
    }
    json!({ "size": [24, 16, 24], "counts": counts, "occ": occ_sum })
}

fn main() {
    let mut json_out = None;
    let mut corpus = None;
    let mut attr = "tcmass_step".to_string();
    let mut lanes = None;
    let mut trace = Vec::new();
    let mut show = None;
    let mut terrain_region = false;
    let mut streams = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json_out = args.next(),
            "--corpus" => corpus = args.next().map(PathBuf::from),
            "--step-attr" => attr = args.next().expect("--step-attr NAME"),
            "--lanes" => lanes = args.next().map(PathBuf::from),
            "--trace" => trace.push(args.next().expect("--trace NAME_PREFIX")),
            "--terrain-region" => terrain_region = true,
            "--show" => {
                let v = args.next().expect("--show A-B");
                let (a, b) = v.split_once('-').unwrap_or((&v, &v));
                show = Some((a.parse().unwrap(), b.parse().unwrap()));
            }
            "-h" | "--help" => {
                eprintln!("usage: tc_mass_replay [--json OUT] [--corpus DIR] [--step-attr NAME] [--lanes DIR] [--trace NAME_PREFIX]... [--terrain-region] STREAM...");
                return;
            }
            _ => streams.push(a),
        }
    }
    let mut ctx = Ctx { attr, trace, traced: Default::default(), show, terrain_region, terrain_grid: Default::default(), terrain_lods: Default::default(), terrain_after_step: None, classes: Default::default(), corpus, problems: Vec::new(), steps: BTreeMap::new(), step_order: Vec::new() };
    let mut sessions = Vec::new();
    let mut first_schema = None;
    let mut total_msgs = 0usize;
    for s in &streams {
        let bytes = match std::fs::read(s) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{s}: {e}");
                continue;
            }
        };
        let Some((ch, tag, msgs)) = stream_msgs(&bytes) else {
            eprintln!("{s}: not an rbx-rtcio stream");
            continue;
        };
        total_msgs += msgs.len();
        let mut dec = Decoder::new();
        let (p0, e0) = (ctx.problems.len(), 0);
        let schema = replay(&mut ctx, s, &msgs, &mut dec, None, "join", &format!("ch{ch}-{}", hex(&tag).replace(' ', "")));
        if first_schema.is_none() {
            first_schema = schema;
        }
        let _ = e0;
        sessions.push(json!({"source": s, "messages": msgs.len(), "problems": ctx.problems.len() - p0}));
    }
    let mut lane_reports = Vec::new();
    if let (Some(dir), Some(schema)) = (&lanes, &first_schema) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        for path in entries {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let msgs: Vec<(usize, &[u8])> = if name == "datagrams.bin" {
                datagram_msgs(&bytes)
            } else {
                match stream_msgs(&bytes) {
                    Some((4, _, m)) => m,
                    _ => continue, // control / ping lanes carry no replicator data
                }
            };
            if msgs.is_empty() {
                continue;
            }
            let mut dec = Decoder::new();
            let _ = dec.decode_frame(schema[0], &schema[1..]);
            let p0 = ctx.problems.len();
            replay(&mut ctx, &path.display().to_string(), &msgs, &mut dec, Some(schema.clone()), "lane", &name);
            lane_reports.push(json!({"lane": name, "messages": msgs.len(), "problems": ctx.problems.len() - p0}));
        }
    }
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    for p in &ctx.problems {
        for k in p["skipped_kinds"].as_array().into_iter().flatten() {
            *kinds.entry(format!("item {}", k.as_str().unwrap_or("?"))).or_default() += 1;
        }
        for e in p["errors"].as_array().into_iter().flatten() {
            let e = e.as_str().unwrap_or("?");
            let key = e.find("value type").map(|i| e[i..].split(|c: char| c == '"' || c == ')').next().unwrap_or(e).to_string()).unwrap_or_else(|| e.chars().take(80).collect());
            *kinds.entry(key).or_default() += 1;
        }
    }
    let steps: Vec<J> = ctx.step_order.iter().map(|s| json!({"step": s, "stats": ctx.steps[s].json()})).collect();
    let decode_errors: u64 = ctx.steps.values().map(|s| s.errors).sum();
    let skipped: u64 = ctx.steps.values().map(|s| s.skipped).sum();
    let unread_tails: u64 = ctx.steps.values().map(|s| s.unread_tails).sum();
    let unknown_refs: u64 = ctx.steps.values().map(|s| s.unknown_refs).sum();
    if ctx.terrain_region {
        let after_terrain = ctx.terrain_after_step.clone().unwrap_or_else(|| summarize_terrain(&ctx.terrain_grid));
        let after_end = summarize_terrain(&ctx.terrain_grid);
        eprintln!("terrain region after `terrain` step: {after_terrain}");
        eprintln!("terrain region at stream end:        {after_end}");
    }
    let out = json!({
        "streams": sessions, "lanes": lane_reports, "messages": total_msgs,
        "decode_errors": decode_errors, "skipped_items": skipped,
        "unread_tails": unread_tails, "unknown_refs": unknown_refs,
        "unknown_kinds": kinds, "classes_seen": ctx.classes, "steps": steps, "problems": ctx.problems,
    });
    eprintln!("{total_msgs} primary messages; {decode_errors} decode errors; {skipped} skipped items; {unread_tails} messages with unread bytes; {unknown_refs} with ops on unknown instances; {} problem messages", ctx.problems.len());
    for (k, n) in &kinds {
        eprintln!("  {k}: {n}");
    }
    for p in &ctx.problems {
        eprintln!(
            "  [{}] {} msg {} @file {} item @{} kind {} {} {}",
            p["step"].as_str().unwrap_or(""),
            p["what"].as_str().unwrap_or(""),
            p["message_index"],
            p["file_offset"],
            p["item_offset"],
            p["item_kind"].as_str().unwrap_or("?"),
            p["errors"].as_array().and_then(|e| e.first()).and_then(|e| e.as_str()).unwrap_or(""),
            p["note"].as_str().unwrap_or("")
        );
    }
    let text = serde_json::to_string_pretty(&out).unwrap();
    match json_out {
        Some(p) => std::fs::write(&p, text).expect("write json"),
        None => println!("{text}"),
    }
}
