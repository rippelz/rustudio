//! Server → client replication for Play, following Roblox's documented
//! model: everything except ServerStorage and ServerScriptService contents
//! replicates to the client; property changes, creations, reparents and
//! destroys stream over; client changes stay on the client.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder, Ustr, WeakDom};

use rustudio_core::doc::Document;
use crate::runtime::{RemoteMsg, RepOp, Runtime};
use crate::replicate_streaming::{self, Streaming};

pub use crate::replicate_streaming::StreamRequest;

/// Services whose contents never reach the client.
pub const SERVER_ONLY: &[&str] = &["ServerStorage", "ServerScriptService"];

/// Deduplication key for queued per-instance updates (properties, attributes
/// and tags share separate namespaces so an attribute named "Name" never
/// collides with the property `Name`).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum RepKey {
    Prop(Ustr),
    Attr(Ustr),
    Tag(Ustr),
}

/// Server ↔ client instance pairs, and the changes on their way over.
#[derive(Default)]
pub struct RepMap {
    pub s2c: rustudio_core::RefMap,
    pub c2s: rustudio_core::RefMap,
    streaming: Option<Streaming>,
    /// Server changes not yet applied to the client, oldest first.
    queue: VecDeque<Pending>,
    /// Ops enqueued so far (each op's sequence number).
    seq: u64,
    /// Prop / Attr / Tag / CFrame ops waiting in the queue (a newer one adds
    /// nothing: the value is read from the server when the op is applied).
    queued_props: rustc_hash::FxHashSet<(Ref, RepKey)>,
    /// Server → client remote calls waiting for the replication sent
    /// before them (Roblox keeps remotes and replication in one order).
    held: VecDeque<(u64, RemoteMsg)>,
    /// Client Ref properties whose server target hasn't reached the client
    /// yet: server target -> (client instance, property).
    pending_refs: HashMap<Ref, Vec<(Ref, String)>>,
    /// Targets that arrived since the last flush of `pending_refs`.
    arrived: Vec<Ref>,
    /// Client Animator track slots that changed (crate::anim_replicate).
    anim_dirty: Vec<(Ref, usize)>,
    /// Server parts whose client copy the hand-over buffer moves (play.rs
    /// `handover_frame`, lane DEATH): their physics moves are not copied over.
    pub buffered: HashSet<Ref>,
    /// Frame time average (µs) and this frame's replication time.
    frame_ema_us: f64,
    frame_used: Duration,
    frame_items: u32,
    frame_budget: Option<Duration>,
    /// Bumped by every link / unlink (readers caching a mapped set).
    revision: u64,
}

/// One unit of replication work.
struct Pending {
    /// The op this came from (an attach carries its op's number).
    seq: u64,
    at: Instant,
    kind: PendingKind,
}

enum PendingKind {
    Op(RepOp),
    /// A big new subtree arriving in pieces: the server children of `s`
    /// from index `next` on are still to come (Studio streams new
    /// instances over several packets, parents first).
    Attach { s: Ref, next: usize },
}
/// Studio's client packet budget (ProcessPacketsJob step Studio, its
/// deadline Studio; DFInt defaults read from the exe):
/// ClientPacketHealthyAllocationPercent 15 % of the frame-time average,
/// at least ClientPacketMinMicroseconds 1600, frames counted up to
/// ClientPacketMaxFrameMicroseconds 48000. Once the oldest waiting data is
/// older than ClientPacketMaxDelayMs 100, it catches up: min(packets
/// waiting, MaxProcessPacketsStepsPerCyclic 5) x max(frame / (1/60 s), 1),
/// at most MaxProcessPacketsJobScaling 10, steps of 1/120 s.
const HEALTHY_PERCENT: f64 = 15.0;
const MIN_US: f64 = 1600.0;
const MAX_FRAME_US: f64 = 48_000.0;
const MAX_DELAY: Duration = Duration::from_millis(100);
const STEPS_PER_CYCLIC: f64 = 5.0;
const JOB_SCALING: f64 = 10.0;
const STEP_S: f64 = 1.0 / 120.0;
/// Instances per piece when a big new subtree streams over (about what a
/// few of Studio's packets carry); smaller subtrees arrive whole.
const PIECE: usize = 128;
/// Physics updates per borrow and replication deadline check.
const MOVE_BATCH: usize = 32;

impl RepMap {
    pub fn streaming(&self) -> bool {
        self.streaming.is_some()
    }

    pub fn set_streaming_spawn(&mut self, position: glam::Vec3) {
        if let Some(state) = &mut self.streaming {
            state.spawn_focus = Some(position);
        }
    }

    pub fn streaming_collider_revision(&self) -> Option<u64> {
        self.streaming.as_ref().map(|s| s.collider_revision)
    }

    pub fn unstreamed_parts(&self) -> impl Iterator<Item = Ref> + '_ {
        self.streaming.iter().flat_map(Streaming::part_refs)
            .filter(|r| !self.s2c.contains_key(r))
    }

    pub fn gameplay_paused(&self) -> bool {
        self.streaming.as_ref().is_some_and(|s| s.paused)
    }

    pub fn forget_collected_client(&mut self, freed: &[Ref]) {
        if let Some(state) = &mut self.streaming {
            for c in freed {
                if let Some(s) = state.parked_client.remove(c) {
                    state.parked.remove(&s);
                }
            }
        }
    }

    fn allowed(&self, r: Ref) -> bool {
        self.streaming.as_ref().is_none_or(|s| s.allowed(r))
    }

    /// Changes each time a server ↔ client pair is made or dropped.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn link(&mut self, s: Ref, c: Ref) {
        self.revision += 1;
        self.s2c.insert(s, c);
        self.c2s.insert(c, s);
        if let Some(state) = &mut self.streaming {
            state.part_mapping_changed(s);
        }
        if self.pending_refs.contains_key(&s) {
            self.arrived.push(s);
        }
    }

    fn unlink_client(&mut self, c: Ref) {
        self.revision += 1;
        if let Some(s) = self.c2s.remove(&c) {
            self.s2c.remove(&s);
            if let Some(state) = &mut self.streaming {
                state.part_mapping_changed(s);
            }
        }
    }

    /// Starts a frame's replication budget (`dt`: the frame's length).
    pub fn begin_frame(&mut self, dt: f64) {
        let us = (dt * 1e6).clamp(0.0, MAX_FRAME_US);
        self.frame_ema_us = if self.frame_ema_us == 0.0 {
            us
        } else {
            0.9 * self.frame_ema_us + 0.1 * us
        };
        self.frame_used = Duration::ZERO;
        self.frame_items = 0;
        let late = self
            .queue
            .front()
            .is_some_and(|p| p.at.elapsed() > MAX_DELAY);
        let secs = if late {
            let waiting = (self.queue.len() as f64).min(STEPS_PER_CYCLIC);
            (waiting * (dt * 60.0).max(1.0)).min(JOB_SCALING) * STEP_S
        } else {
            (HEALTHY_PERCENT / 100.0 * self.frame_ema_us).max(MIN_US) * 1e-6
        };
        self.frame_budget = Some(Duration::from_secs_f64(secs));
    }

    /// Whether server changes are still waiting to reach the client.
    pub fn backlog(&self) -> usize {
        self.queue.len()
    }

    fn enqueue(&mut self, ops: Vec<RepOp>) {
        let now = Instant::now();
        let cframe = ustr("CFrame");
        for op in ops {
            let key = match &op {
                RepOp::Prop(r, n) => Some((*r, RepKey::Prop(ustr(n)))),
                RepOp::Attr(r, n) => Some((*r, RepKey::Attr(ustr(n)))),
                RepOp::Tag(r, t) => Some((*r, RepKey::Tag(ustr(t)))),
                RepOp::Moved(r) => Some((*r, RepKey::Prop(cframe))),
                _ => None,
            };
            if let Some(k) = key {
                if !self.queued_props.insert(k) {
                    continue;
                }
            }
            self.seq += 1;
            self.queue.push_back(Pending { seq: self.seq, at: now, kind: PendingKind::Op(op) });
        }
    }

    /// The server → client remote calls that may go now: each waits until
    /// the replication sent before it has been applied.
    pub fn gate_remotes(&mut self, msgs: Vec<RemoteMsg>) -> Vec<RemoteMsg> {
        if self.queue.is_empty() && self.held.is_empty() {
            return msgs;
        }
        for m in msgs {
            self.held.push_back((self.seq, m));
        }
        let first_waiting = self.queue.front().map(|p| p.seq);
        let mut out = Vec::new();
        while let Some((seq, _)) = self.held.front() {
            if first_waiting.is_some_and(|w| w <= *seq) {
                break;
            }
            out.push(self.held.pop_front().unwrap().1);
        }
        out
    }
}

/// Whether a server instance is in a replicated part of the tree.
pub fn replicated(server: &Document, r: Ref) -> bool {
    let root = server.dom.root_ref();
    if r == root {
        return true;
    }
    let mut cur = r;
    loop {
        let Some(i) = server.dom.get_by_ref(cur) else {
            return false;
        };
        let parent = i.parent();
        if parent.is_none() {
            return false;
        }
        if parent == root {
            return !SERVER_ONLY.contains(&i.class.as_str());
        }
        cur = parent;
    }
}

fn builder_of(dom: &WeakDom, r: Ref) -> Option<InstanceBuilder> {
    let i = dom.get_by_ref(r)?;
    // (the map itself: the copy keeps sharing its class's common values)
    Some(InstanceBuilder::new(i.class).with_name(i.name.clone()).with_prop_map(i.properties.clone()))
}

/// Copies a server subtree into the client DOM (parent `parent`, may be
/// none), recording the pairs. Returns the client root of the copy.
fn copy_subtree(
    src: &WeakDom,
    r: Ref,
    dst: &mut WeakDom,
    parent: Ref,
    map: &mut RepMap,
    created: &mut Vec<Ref>,
) -> Option<Ref> {
    let c = copy_one(src, r, dst, parent, map, created)?;
    // A stack, children pushed last-first: each parent gets its children in
    // the server's order (Roblox's GetChildren is the file's order; Place B
    // Newer's Fast Travel list, LayoutOrder ties, read upside down when the
    // client got them reversed).
    let mut queue: Vec<(Ref, Ref)> = src
        .get_by_ref(r)?
        .children()
        .iter()
        .rev()
        .map(|k| (*k, c))
        .collect();
    while let Some((s, cp)) = queue.pop() {
        let Some(cc) = copy_one(src, s, dst, cp, map, created) else {
            continue;
        };
        if let Some(i) = src.get_by_ref(s) {
            queue.extend(i.children().iter().rev().map(|k| (*k, cc)));
        }
    }
    Some(c)
}

fn copy_one(
    src: &WeakDom, r: Ref, dst: &mut WeakDom, parent: Ref, map: &mut RepMap,
    created: &mut Vec<Ref>,
) -> Option<Ref> {
    if map.streaming() && (!map.allowed(r) || map.s2c.contains_key(&r)) {
        return None;
    }
    let i = src.get_by_ref(r)?;
    let retained = map.streaming.as_mut().and_then(|s| {
        let c = s.parked.remove(&r)?;
        s.parked_client.remove(&c);
        Some(c)
    });
    let c = if let Some(c) = retained.filter(|c| dst.get_by_ref(*c).is_some()) {
        let children = dst.get_by_ref(c)?.children().to_vec();
        for child in children {
            // Parked server children that are no longer eligible may
            // need their own independent arrival later.
            if map.streaming.as_ref().is_some_and(|s| s.parked_client.get(&child).is_some_and(|&sr| {
                !s.allowed(sr) || src.get_by_ref(sr).is_none_or(|i| i.parent() != r)
            })) {
                dst.transfer_within(child, Ref::none());
            }
        }
        let copy = dst.get_by_ref_mut(c)?;
        copy.name = i.name.clone();
        copy.properties = i.properties.clone();
        dst.transfer_within(c, parent);
        c
    } else {
        let mut builder = builder_of(src, r)?;
        if i.class == "Terrain" && map.streaming() {
            builder.add_property("SmoothGrid", Variant::BinaryString(rustudio_core::terrain::Voxels::empty().encode().into()));
        }
        dst.insert(parent, builder)
    };
    map.link(r, c);
    created.push(c);
    Some(c)
}

/// Points Ref properties of freshly copied client instances at their client
/// counterparts. A target that is on its way (replicated, not there yet)
/// is nil until it arrives, as in Studio; one that never replicates stays nil.
fn remap_refs(server: Option<&Document>, dst: &mut WeakDom, created: &[Ref], map: &mut RepMap) {
    for c in created {
        let Some(i) = dst.get_by_ref_mut(*c) else {
            continue;
        };
        // (collected, then set: walking the values mutably would copy the
        // class's shared values into every instance)
        let mut changes = Vec::new();
        for (k, v) in i.properties.iter() {
            if let Variant::Ref(target) = v {
                let target = *target;
                let t = map.s2c.get(&target).copied().unwrap_or_else(Ref::none);
                if t.is_none() && target.is_some() && server.is_some_and(|s| replicated(s, target)) {
                    map.pending_refs.entry(target).or_default().push((*c, k.to_string()));
                }
                if t != target {
                    changes.push((*k, t));
                }
            }
        }
        for (k, t) in changes {
            i.properties.insert(k, Variant::Ref(t));
        }
    }
}

fn remap_value(server: &Document, c: Ref, name: &str, v: Variant, map: &mut RepMap) -> Variant {
    match v {
        Variant::Ref(t) => {
            let ct = map.s2c.get(&t).copied().unwrap_or_else(Ref::none);
            if ct.is_none() && t.is_some() && replicated(server, t) {
                map.pending_refs.entry(t).or_default().push((c, name.to_string()));
            }
            Variant::Ref(ct)
        }
        other => other,
    }
}

/// Sets the Ref properties that were waiting for instances that arrived.
fn flush_refs(client: &Runtime, map: &mut RepMap) {
    while let Some(s) = map.arrived.pop() {
        let Some(waiting) = map.pending_refs.remove(&s) else { continue };
        let Some(ct) = map.s2c.get(&s).copied() else { continue };
        for (c, name) in waiting {
            if map.c2s.contains_key(&c) {
                client.apply_prop(c, &name, Variant::Ref(ct));
            }
        }
    }
}

/// The client's starting DataModel: a copy of the server's, with the
/// server-only services present but empty.
pub fn initial_client(server: &Document, title: String) -> (Document, RepMap) {
    let mut map = RepMap::default();
    if replicate_streaming::enabled(server) {
        map.streaming = Some(Streaming::new(server));
    }
    let sroot = server.dom.root();
    let mut root_b = InstanceBuilder::new(sroot.class).with_name(sroot.name.clone());
    for (k, v) in &sroot.properties {
        root_b.add_property(*k, v.clone());
    }
    let mut dom = WeakDom::new(root_b);
    let croot = dom.root_ref();
    map.link(sroot.referent(), croot);
    let mut created = Vec::new();
    for child in sroot.children().to_vec() {
        let class = server
            .dom
            .get_by_ref(child)
            .map(|i| i.class.to_string())
            .unwrap_or_default();
        if SERVER_ONLY.contains(&class.as_str()) {
            if let Some(b) = builder_of(&server.dom, child) {
                let c = dom.insert(croot, b);
                map.link(child, c);
            }
        } else {
            copy_subtree(&server.dom, child, &mut dom, croot, &mut map, &mut created);
        }
    }
    let source = map.streaming().then_some(server);
    remap_refs(source, &mut dom, &created, &mut map);
    let mut doc = Document::from_play_dom(dom, title);
    doc.revision += 1;
    (doc, map)
}

/// Updates Workspace interest before the normal ordered replication drain.
pub fn update_streaming(
    server: &Document, client: &Runtime, map: &mut RepMap, player: Ref,
    ops: &mut Vec<RepOp>, mut requests: Vec<StreamRequest>,
) {
    let Some(state) = &mut map.streaming else { return };
    let Some(mut world) = client.world_mut() else { return };
    requests.extend(world.stream_requests.drain(..).map(|mut request| {
        request.player = map.c2s.get(&request.player).copied().unwrap_or_else(Ref::none);
        request
    }));
    let time = world.time;
    drop(world);
    state.player = player;
    state.changed(server, ops);
    let changed = state.step(server, time, requests);
    let terrain = state.terrain_blob.take();
    let paused = state.paused;
    if changed {
        let mut departures = Vec::new();
        if let Some(mut w) = client.world_mut() {
            let d = w.dm();
            for s in std::mem::take(&mut state.departures) {
                let Some(&c) = map.s2c.get(&s) else { continue };
                let cp = d.dom.get_by_ref(c).map(|i| i.parent()).unwrap_or_else(Ref::none);
                if map.c2s.get(&cp).is_none_or(|p| state.allowed(*p)) {
                    departures.push(c);
                }
            }
        }
        if departures.len() >= 8 && client.stream_out_workspace_parts(&departures) {
            for c in departures {
                park_mapping(map, c);
            }
        } else {
            for c in departures {
                park(client, map, c);
            }
        }
        let arrivals = map.streaming.as_ref().unwrap().arrivals(&map.s2c);
        ops.extend(arrivals.into_iter().map(RepOp::Parent));
    }
    if let Some(blob) = terrain {
        let terrain = server.workspace().and_then(|ws| server.find_child(ws, "Terrain"));
        if let Some(c) = terrain.and_then(|r| map.s2c.get(&r).copied()) {
            client.apply_prop(c, "SmoothGrid", Variant::BinaryString(blob.into()));
        }
    }
    if let Some(&c) = map.s2c.get(&player) {
        let value = Variant::Bool(paused);
        let changed = client.world_mut().is_some_and(|w| w.d().prop(c, "GameplayPaused").unwrap_or(Variant::Bool(false)) != value);
        if changed {
            client.apply_prop(c, "GameplayPaused", value);
        }
    }
}

fn park(client: &Runtime, map: &mut RepMap, root: Ref) {
    let subtree = client.world_mut().map(|mut w| w.dm().subtree(root)).unwrap_or_default();
    client.apply_parent(root, Ref::none());
    for c in subtree {
        park_mapping(map, c);
    }
}

fn park_mapping(map: &mut RepMap, c: Ref) {
    if let Some(&s) = map.c2s.get(&c) {
        map.unlink_client(c);
        let state = map.streaming.as_mut().unwrap();
        state.parked.insert(s, c);
        state.parked_client.insert(c, s);
    }
}

pub fn finish_streaming(server: &Document, client: &Runtime, map: &mut RepMap) {
    let empty = map.queue.is_empty();
    let Some(state) = &mut map.streaming else { return };
    if empty {
        state.complete_requests();
    }
    if empty && !state.ready && state.initial_stream_delivered() && map.s2c.contains_key(&state.player) {
        state.ready = true;
        if let Some(ws) = server.workspace().and_then(|w| map.s2c.get(&w).copied()) {
            client.fire(ws, "PersistentLoaded", Vec::new());
        }
    }
}

/// Applies server changes to the client, all of them now (engine steps
/// that read the client copy right after, like a character spawn). The
/// client runtime must be entered (its World holds the client document) so
/// client scripts see Changed, ChildAdded and friends. Returns the client
/// instances created.
pub fn apply(server: &Document, client: &Runtime, map: &mut RepMap, ops: Vec<RepOp>) -> Vec<Ref> {
    map.enqueue(ops);
    drain(server, client, map, false)
}

/// Applies server changes within this frame's budget (`RepMap::begin_frame`),
/// as Studio's ProcessPacketsJob does: what doesn't fit waits for the next
/// frame, in order. Fixed-step runs (no wall clock) apply everything.
pub fn apply_budgeted(server: &Document, client: &Runtime, map: &mut RepMap, ops: Vec<RepOp>) -> Vec<Ref> {
    map.enqueue(ops);
    let realtime = client.world_mut().is_some_and(|w| w.realtime);
    drain(server, client, map, realtime && map.frame_budget.is_some())
}

fn drain(server: &Document, client: &Runtime, map: &mut RepMap, budgeted: bool) -> Vec<Ref> {
    let mut created = Vec::new();
    if map.queue.is_empty() {
        return created;
    }
    let start = Instant::now();
    let limit = map.frame_budget.unwrap_or(Duration::MAX);
    let used = map.frame_used;
    // At least one piece a frame, as Studio's job always takes a packet.
    let over = |map: &RepMap| budgeted && map.frame_items > 0 && used + start.elapsed() >= limit;
    let mut seen_props = rustc_hash::FxHashSet::default();
    let cframe = ustr("CFrame");
    while let Some(p) = map.queue.front() {
        if over(map) {
            break;
        }
        if matches!(p.kind, PendingKind::Op(RepOp::Moved(_))) {
            let _sc = rustudio_core::bench::scope("rep_moved");
            let mut world = client.world_mut();
            // Consecutive physics updates have no property callbacks. Keep
            // their place in the ordered queue and reuse one document borrow.
            let before = map.frame_items;
            // (Net stats once per batch: a lock and a clock read per part
            // were most of a moved part's cost.)
            let mut net_bytes = 0u64;
            for _ in 0..MOVE_BATCH {
                let Some(Pending { kind: PendingKind::Op(RepOp::Moved(r)), .. }) = map.queue.front() else {
                    break;
                };
                let r = *r;
                map.queue.pop_front();
                map.frame_items += 1;
                map.queued_props.remove(&(r, RepKey::Prop(cframe)));
                if map.buffered.contains(&r) || !seen_props.insert((r, RepKey::Prop(cframe))) {
                    continue;
                }
                let (Some(&target), Some(value)) = (
                    map.s2c.get(&r),
                    server.dom.get_by_ref(r).and_then(|i| i.properties.get(&cframe)),
                ) else {
                    continue;
                };
                net_bytes += 4 + 12 + 6 + 12 + 12;
                if let Some(w) = &mut world {
                    let d = w.dm();
                    if let Some(i) = d.dom.get_by_ref_mut(target) {
                        if i.properties.get(&cframe) != Some(value) {
                            i.properties.insert(cframe, value.clone());
                            d.mark_prop(target, cframe);
                        }
                    }
                }
            }
            crate::runtime::domains::services::perf::net(true, true, net_bytes);
            rustudio_core::bench::add("rep_moved_parts", 0.0, u64::from(map.frame_items - before));
            continue;
        }
        let p = map.queue.pop_front().unwrap();
        map.frame_items += 1;
        let _sc = rustudio_core::bench::scope(match &p.kind {
            PendingKind::Op(RepOp::Parent(_)) => "rep_parent",
            PendingKind::Op(RepOp::Destroy(_)) => "rep_destroy",
            PendingKind::Op(_) => "rep_prop",
            PendingKind::Attach { .. } => "rep_attach",
        });
        match p.kind {
            PendingKind::Op(op) => {
                match &op {
                    RepOp::Prop(r, n) => {
                        map.queued_props.remove(&(*r, RepKey::Prop(ustr(n))));
                    }
                    RepOp::Attr(r, n) => {
                        map.queued_props.remove(&(*r, RepKey::Attr(ustr(n))));
                    }
                    RepOp::Tag(r, t) => {
                        map.queued_props.remove(&(*r, RepKey::Tag(ustr(t))));
                    }
                    _ => {}
                }
                apply_op(server, client, map, op, p.seq, budgeted, &mut seen_props, &mut created);
            }
            PendingKind::Attach { s, next } => {
                attach(server, client, map, s, next, p.seq, &over, &mut created);
            }
        }
        flush_refs(client, map);
    }
    map.frame_used += start.elapsed();
    crate::anim_replicate::sync(&client.lua, std::mem::take(&mut map.anim_dirty));
    created
}

/// Copies a server subtree to the client and parents it there, whole when
/// it is small or `whole`, else its root now and the rest in pieces.
#[allow(clippy::too_many_arguments)]
fn arrive(
    server: &Document,
    client: &Runtime,
    map: &mut RepMap,
    r: Ref,
    cp: Ref,
    whole: bool,
    seq: u64,
    created: &mut Vec<Ref>,
) -> bool {
    let atomic = map.streaming.as_ref().is_some_and(|state| state.atomic(r));
    let parked = map.streaming.as_ref().is_some_and(|state| state.parked.contains_key(&r));
    let big = !whole && !atomic && !parked && server.dom.descendants_of(r).nth(PIECE).is_some();
    let first_created = created.len();
    let (copy, attached) = {
        let Some(mut w) = client.world_mut() else {
            return false;
        };
        // A fresh leaf has no client instance for existing joints or scripts
        // to reference yet. Put it directly under Workspace and queue the
        // same parent signals after remapping its properties.
        let direct = map.streaming()
            && !parked
            && w.d().workspace() == Some(cp)
            && server.dom.get_by_ref(r).is_some_and(|instance| {
                instance.children().is_empty()
                    && instance.class != "Terrain"
                    && rustudio_core::doc::is_a(&instance.class, "BasePart")
            });
        let copy = if big {
            builder_of(&server.dom, r).map(|b| {
                let c = w.dm().dom.insert(Ref::none(), b);
                map.link(r, c);
                created.push(c);
                c
            })
        } else {
            let parent = if direct { cp } else { Ref::none() };
            copy_subtree(&server.dom, r, &mut w.dm().dom, parent, map, created)
        };
        let new_refs = &created[first_created..];
        remap_refs(Some(server), &mut w.dm().dom, new_refs, map);
        w.structure_changed = true;
        if let Some(c) = copy.filter(|_| direct) {
            w.dm().mark_tree(c);
        }
        // A new instance: class, name, parent and every property.
        let bytes: u64 = new_refs
            .iter()
            .filter_map(|c| w.d().dom.get_by_ref(*c))
            .map(|i| 10 + i.name.len() as u64 + i.properties.values().map(|v| 2 + crate::runtime::domains::services::perf::value_bytes(v)).sum::<u64>())
            .sum();
        crate::runtime::domains::services::perf::net(true, false, bytes);
        (copy, direct)
    };
    if let Some(c) = copy {
        if attached {
            client.notify_parent(c, Ref::none(), cp, vec![c]);
        } else {
            client.apply_parent_replicated(c, cp);
        }
    }
    if big {
        map.queue.push_front(Pending { seq, at: Instant::now(), kind: PendingKind::Attach { s: r, next: 0 } });
    }
    true
}

/// Brings over the server children of `s` from `next` on (each a whole
/// small subtree, or a big one's root followed by its own pieces, so the
/// client gets parents before children and siblings in order).
#[allow(clippy::too_many_arguments)]
fn attach(
    server: &Document,
    client: &Runtime,
    map: &mut RepMap,
    s: Ref,
    next: usize,
    seq: u64,
    over: &dyn Fn(&RepMap) -> bool,
    created: &mut Vec<Ref>,
) {
    let Some(cs) = map.s2c.get(&s).copied() else { return };
    let kids: Vec<Ref> = match server.dom.get_by_ref(s) {
        Some(i) if replicated(server, s) => i.children().to_vec(),
        _ => return,
    };
    let mut k = next;
    let mut did = false;
    while k < kids.len() {
        let sc = kids[k];
        k += 1;
        if map.s2c.contains_key(&sc) || !map.allowed(sc) {
            continue;
        }
        if did && over(map) {
            k -= 1;
            break;
        }
        did = true;
        let len = map.queue.len();
        arrive(server, client, map, sc, cs, false, seq, created);
        if map.queue.len() > len {
            // `sc` streams in pieces first; the rest of `s` comes after it.
            let piece = map.queue.pop_front();
            if k < kids.len() {
                map.queue.push_front(Pending { seq, at: Instant::now(), kind: PendingKind::Attach { s, next: k } });
            }
            if let Some(piece) = piece {
                map.queue.push_front(piece);
            }
            return;
        }
    }
    if k < kids.len() {
        map.queue.push_front(Pending { seq, at: Instant::now(), kind: PendingKind::Attach { s, next: k } });
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_op(
    server: &Document,
    client: &Runtime,
    map: &mut RepMap,
    op: RepOp,
    seq: u64,
    budgeted: bool,
    seen_props: &mut rustc_hash::FxHashSet<(Ref, RepKey)>,
    created: &mut Vec<Ref>,
) {
    match op {
        RepOp::Prop(r, name) => {
            if name == "SmoothGrid" && map.streaming() && server.class_of(r).is_some_and(|c| c == "Terrain") {
                return;
            }
            if !seen_props.insert((r, RepKey::Prop(ustr(&name)))) {
                return;
            }
            let Some(c) = map.s2c.get(&r).copied() else {
                return;
            };
            let Some(v) = server
                .dom
                .get_by_ref(r)
                .and_then(|i| i.properties.get(&rbx_dom_weak::ustr(&name)).cloned())
            else {
                return;
            };
            crate::runtime::domains::services::perf::net(true, false, crate::runtime::domains::services::perf::prop_bytes(&v));
            // An Animator's track slot: stored without Changed (hidden,
            // NotScriptable), then the client's own tracks follow it.
            if let Some(slot) = crate::anim_replicate::is_slot_prop(&name) {
                if server.class_of(r).is_some_and(|k| k == "Animator") {
                    if let Some(mut w) = client.world_mut() {
                        if let Some(i) = w.dm().dom.get_by_ref_mut(c) {
                            i.properties.insert(rbx_dom_weak::ustr(&name), v);
                        }
                    }
                    map.anim_dirty.push((c, slot));
                    return;
                }
            }
            let v = remap_value(server, c, &name, v, map);
            client.apply_prop(c, &name, v);
        }
        RepOp::Attr(r, name) => {
            if !seen_props.insert((r, RepKey::Attr(ustr(&name)))) {
                return;
            }
            let Some(c) = map.s2c.get(&r).copied() else {
                return;
            };
            let Some(inst) = server.dom.get_by_ref(r) else {
                return;
            };
            let val = match inst.properties.get(&ustr("Attributes")) {
                Some(Variant::Attributes(a)) => a.get(name.as_str()).cloned(),
                _ => None,
            };
            let bytes = 6 + name.len() as u64 + val.as_ref().map_or(1, crate::runtime::domains::services::perf::value_bytes);
            crate::runtime::domains::services::perf::net(true, false, bytes);
            client.apply_attr(c, &name, val);
        }
        RepOp::Tag(r, tag) => {
            if !seen_props.insert((r, RepKey::Tag(ustr(&tag)))) {
                return;
            }
            let Some(c) = map.s2c.get(&r).copied() else {
                return;
            };
            let Some(inst) = server.dom.get_by_ref(r) else {
                return;
            };
            let has = match inst.properties.get(&ustr("Tags")) {
                Some(Variant::Tags(t)) => t.iter().any(|s| s == tag),
                Some(Variant::BinaryString(b)) => rbx_dom_weak::types::Tags::decode(b.as_ref())
                    .is_ok_and(|t| t.iter().any(|s| s == tag)),
                _ => false,
            };
            crate::runtime::domains::services::perf::net(true, false, 6 + tag.len() as u64 + 1);
            client.apply_tag(c, &tag, has);
        }
        RepOp::Moved(_) => unreachable!("moves are applied in batches"),
        RepOp::Name(r) => {
            if let (Some(c), Some(i)) = (map.s2c.get(&r).copied(), server.dom.get_by_ref(r)) {
                client.apply_name(c, &i.name);
                crate::runtime::domains::services::perf::net(true, false, 6 + i.name.len() as u64 + 2);
            }
        }
        RepOp::Persistence(_, _, _) => {}
        RepOp::Destroy(r) => {
            if let Some(state) = &mut map.streaming {
                state.persistence.remove(&r);
                if let Some(c) = state.parked.remove(&r) {
                    let subtree = client.world_mut().map(|mut w| w.dm().subtree(c)).unwrap_or_default();
                    client.apply_destroy(c);
                    state.parked_client.remove(&c);
                    for child in subtree {
                        if let Some(s) = state.parked_client.remove(&child) {
                            state.parked.remove(&s);
                        }
                    }
                    return;
                }
            }
            // Studio's DestroyInstance item: the client runs Destroy on its
            // copy (one op, however big the subtree).
            let Some(c) = map.s2c.get(&r).copied() else {
                return;
            };
            crate::runtime::domains::services::perf::net(true, false, 10);
            let subtree: Vec<Ref> = client
                .world_mut()
                .map(|mut w| w.dm().subtree(c))
                .unwrap_or_default();
            client.apply_destroy(c);
            for d in subtree {
                map.unlink_client(d);
            }
        }
        RepOp::Parent(r) => {
            let sp = server
                .dom
                .get_by_ref(r)
                .map(|i| i.parent())
                .unwrap_or_else(Ref::none);
            if !map.allowed(r) {
                if let Some(&c) = map.s2c.get(&r) {
                    park(client, map, c);
                }
                return;
            }
            let visible = server.dom.get_by_ref(r).is_some() && replicated(server, r);
            let cp = map.s2c.get(&sp).copied();
            match (visible, map.s2c.get(&r).copied(), cp) {
                (true, Some(c), Some(cp)) => {
                    crate::runtime::domains::services::perf::net(true, false, 10);
                    client.apply_parent_replicated(c, cp)
                }
                (true, None, Some(cp)) => {
                    // New to the client: copy it (unparented), then parent it
                    // so events fire. A big one streams over in pieces.
                    arrive(server, client, map, r, cp, !budgeted, seq, created);
                }
                (_, Some(c), _) => {
                    // Left the replicated tree (unparented, moved server-side only).
                    client.apply_parent(c, Ref::none());
                    // Forget the pairs for the client copy's whole subtree.
                    let subtree: Vec<Ref> = client
                        .world_mut()
                        .map(|mut w| w.dm().subtree(c))
                        .unwrap_or_default();
                    for d in subtree {
                        map.unlink_client(d);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::runtime::{RunMode, Side, World};

    /// A server and a client runtime over a fresh place, as Play pairs them.
    fn pair() -> (Runtime, Runtime, RepMap) {
        let sd = Document::new_place();
        let (cd, map) = initial_client(&sd, "c".into());
        let s = Runtime::new().unwrap();
        let mut sw = World::new(sd, RunMode::Run);
        sw.has_peer = true;
        s.enter(sw);
        s.bind_globals().unwrap();
        let c = Runtime::new().unwrap();
        let mut cw = World::new(cd, RunMode::Run);
        cw.side = Side::Client;
        cw.has_peer = true;
        cw.realtime = true;
        c.enter(cw);
        c.bind_globals().unwrap();
        (s, c, map)
    }

    fn ops(s: &Runtime) -> Vec<RepOp> {
        std::mem::take(&mut s.world_mut().unwrap().rep)
    }

    fn sync(s: &Runtime, c: &Runtime, map: &mut RepMap, budgeted: bool) {
        let o = ops(s);
        let w = s.world_mut().unwrap();
        if budgeted {
            apply_budgeted(w.d(), c, map, o);
        } else {
            apply(w.d(), c, map, o);
        }
        drop(w);
        c.dispatch();
    }

    #[test]
    fn batched_workspace_stream_out_preserves_survivors_and_signal_order() {
        let (_, client, _) = pair();
        client.lua.load(r#"
            for n = 1, 9 do
                local part = Instance.new("Part")
                part.Name = "StreamPart" .. n
                part.Anchored = true
                part.Parent = workspace
            end
            _G.departed = {}
            workspace.ChildRemoved:Connect(function(instance)
                table.insert(_G.departed, instance.Name .. ":" .. tostring(instance.Parent))
            end)
        "#).exec().unwrap();
        let refs = {
            let world = client.world_mut().unwrap();
            let doc = world.d();
            let workspace = doc.workspace().unwrap();
            (1..=9).map(|n| doc.find_named(workspace, &format!("StreamPart{n}")).unwrap()).collect::<Vec<_>>()
        };
        let removed = [refs[3], refs[1], refs[7], refs[4]];
        assert!(client.stream_out_workspace_parts(&removed));
        client.dispatch();
        let actual: String = client.lua.load(r#"
            local survivors = {}
            for _, instance in workspace:GetChildren() do
                if instance.Name:match("^StreamPart") then
                    table.insert(survivors, instance.Name)
                end
            end
            return table.concat(survivors, ",") .. "|" .. table.concat(_G.departed, ",")
        "#).eval().unwrap();
        assert_eq!(
            actual,
            "StreamPart1,StreamPart3,StreamPart6,StreamPart7,StreamPart9|StreamPart4:nil,StreamPart2:nil,StreamPart8:nil,StreamPart5:nil",
        );
    }

    fn streaming_probe(mode: &str) -> (Runtime, Runtime, RepMap, Ref) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../checks/streaming/places").join(format!("{mode}.rbxlx"));
        let mut sd = Document::open(&path).unwrap();
        sd.ensure_services();
        let ws = sd.workspace().unwrap();
        let model = sd.dom.insert(ws, InstanceBuilder::new("Model").with_name("Character"));
        sd.dom.insert(model, InstanceBuilder::new("Part").with_name("HumanoidRootPart"));
        let players = sd.find_child(sd.dom.root_ref(), "Players").unwrap();
        let player = sd.dom.insert(players, InstanceBuilder::new("Player")
            .with_name("Tester").with_property("Character", Variant::Ref(model)));
        let (cd, map) = initial_client(&sd, "client".into());
        assert!(map.streaming());
        let mut sw = World::new(sd, RunMode::Play);
        sw.has_peer = true;
        let s = Runtime::new().unwrap();
        s.enter(sw);
        s.bind_globals().unwrap();
        let mut cw = World::new(cd, RunMode::Play);
        cw.side = Side::Client;
        cw.has_peer = true;
        let c = Runtime::new().unwrap();
        c.enter(cw);
        c.bind_globals().unwrap();
        (s, c, map, player)
    }

    fn stream_at(s: &Runtime, c: &Runtime, map: &mut RepMap, player: Ref, time: f64) {
        c.world_mut().unwrap().time = time;
        let mut ops = ops(s);
        let sw = s.world_mut().unwrap();
        update_streaming(sw.d(), c, map, player, &mut ops, Vec::new());
        apply(sw.d(), c, map, ops);
        finish_streaming(sw.d(), c, map);
        drop(sw);
        c.dispatch();
        c.step(1.0 / 60.0);
    }

    fn drain_weld_map(s: &Runtime, c: &Runtime, map: &mut RepMap, o: &mut Vec<RepOp>) {
        let mut frames = 0;
        loop {
            {
                let w = s.world_mut().unwrap();
                map.begin_frame(1.0 / 60.0);
                map.frame_budget = Some(Duration::from_millis(2));
                apply_budgeted(w.d(), c, map, std::mem::take(o));
            }
            c.dispatch();
            frames += 1;
            assert!(frames < 100_000, "replication backlog failed to drain");
            if map.backlog() == 0 {
                break;
            }
        }
    }

    /// A big welded model (every part welded to an anchored base, all
    /// siblings under one Model) streams over a budgeted client in pieces:
    /// each piece regroups the one growing assembly, which must stay
    /// linear in its size (it was quadratic while the member sort scanned
    /// the Model's children per joint), and ends
    /// as one assembly rooted at the base.
    #[test]
    fn welded_map_streams_in_without_quadratic_regroups() {
        let (s, c, mut map) = pair();
        s.lua.load("local m = Instance.new('Model') m.Name = 'Map'\n\
            local base = Instance.new('Part') base.Name = 'Base' base.Anchored = true base.Parent = m\n\
            for i = 1, 3000 do local p = Instance.new('Part') p.Name = 'P' .. i p.Position = Vector3.new(i, 0, 0)\n\
              local w = Instance.new(if i % 2 == 0 then 'Weld' else 'WeldConstraint')\n\
              if i % 2 == 0 then w.C0 = CFrame.new(i, 0, 0) end\n\
              w.Part0 = base w.Part1 = p w.Parent = p p.Parent = m end\n\
            m.Parent = workspace").exec().unwrap();
        let mut o = ops(&s);
        drain_weld_map(&s, &c, &mut map, &mut o);
        let joined: String = c.lua.load("local m = workspace.Map local n = 0\n\
            for _, p in m:GetChildren() do if p.AssemblyRootPart == m.Base then n += 1 end end\n\
            return n .. ' ' .. #m:GetChildren()").eval().unwrap();
        assert_eq!(joined, "3001 3001");
    }

    /// A map whose parts are welded to each other (a chain from an anchored
    /// base: no part is a leaf) streams in piece by piece, and each piece
    /// is posed against the parts already there, not by walking the map
    /// from its roots (which walked every part already there for each
    /// piece).
    #[test]
    fn chained_welded_map_poses_only_the_arriving_pieces() {
        let (s, c, mut map) = pair();
        s.lua.load("local m = Instance.new('Model') m.Name = 'Map'\n\
            local base = Instance.new('Part') base.Name = 'Base' base.Anchored = true base.Parent = m\n\
            local prev = base\n\
            for i = 1, 3000 do local p = Instance.new('Part') p.Name = 'P' .. i p.Position = Vector3.new(i, 0, 0)\n\
              local w = Instance.new('Weld') w.Part0 = prev w.Part1 = p w.C0 = prev.CFrame:Inverse() * p.CFrame w.Parent = p\n\
              p.Parent = m prev = p end\n\
            m.Parent = workspace").exec().unwrap();
        let mut o = ops(&s);
        rustudio_core::bench::take_sections();
        drain_weld_map(&s, &c, &mut map, &mut o);
        let walked: f64 = rustudio_core::bench::take_sections().iter().filter(|e| e.0 == "count: jp_walked").map(|e| e.1).sum();
        assert!(walked <= 2.0 * 3001.0, "{walked} parts walked to pose a 3001-part map");
        let last: String = c.lua.load("return tostring(workspace.Map.P3000.Position) .. ' ' .. workspace.Map.P3000.AssemblyRootPart.Name").eval().unwrap();
        assert_eq!(last, "3000, 0, 0 Base");
    }

    /// A replicated part welded to a part the client moved itself (its
    /// own character's hand) is posed against the client's copy of it.
    #[test]
    fn arriving_weld_poses_against_the_clients_part() {
        let (s, c, mut map) = pair();
        s.lua.load("local h = Instance.new('Part') h.Name = 'Hand' h.Size = Vector3.new(4, 4, 4) h.Position = Vector3.new(0, 10, 0) h.Parent = workspace").exec().unwrap();
        sync(&s, &c, &mut map, false);
        c.lua.load("workspace.Hand.CFrame = CFrame.new(5, 10, 0)").exec().unwrap();
        s.lua.load("local a = Instance.new('Part') a.Name = 'Hat' a.Position = Vector3.new(0, 12, 0)\n\
            local w = Instance.new('Weld') w.Part0 = workspace.Hand w.Part1 = a w.C0 = CFrame.new(0, 2, 0) w.Parent = a\n\
            a.Parent = workspace").exec().unwrap();
        sync(&s, &c, &mut map, false);
        let at: String = c.lua.load("return tostring(workspace.Hat.Position) .. ' ' .. tostring(workspace.Hand.Position)").eval().unwrap();
        assert_eq!(at, "5, 12, 0 5, 10, 0");
    }

    #[test]
    fn fresh_streamed_leaf_has_parent_and_properties_when_signaled() {
        let (server, client, mut map, player) = streaming_probe("grid");
        client.lua.load(r#"
            _G.leafEvents = {}
            local function observe(event, part)
                if part.Name == "FixtureLeaf" then
                    table.insert(_G.leafEvents, event .. ":" .. tostring(part.Parent == workspace)
                        .. ":" .. tostring(part.Anchored))
                end
            end
            workspace.ChildAdded:Connect(function(part) observe("child", part) end)
            workspace.DescendantAdded:Connect(function(part) observe("descendant", part) end)
        "#).exec().unwrap();
        server.lua.load(r#"
            local part = Instance.new("Part")
            part.Name = "FixtureLeaf"
            part.Anchored = true
            part.Parent = workspace
        "#).exec().unwrap();
        stream_at(&server, &client, &mut map, player, 1.0);
        let events: String = client.lua.load("return table.concat(_G.leafEvents, ',')").eval().unwrap();
        assert_eq!(events, "child:true:true,descendant:true:true");
    }

    #[test]
    fn streaming_focus_events_models_terrain_and_persistence() {
        let (s, c, mut map, player) = streaming_probe("opportunistic");
        c.lua.load(r#"
            assert(workspace:GetStreamingEnabled())
            assert(workspace.Always and not workspace:FindFirstChild("Ring32"))
            assert(not workspace:FindFirstChild("DefaultDistant"))
            assert(not workspace:FindFirstChild("NonatomicDistant"))
            _G.events = {}
            workspace.ChildAdded:Connect(function(i) _G.events[i.Name] = true end)
            workspace.DescendantRemoving:Connect(function(i)
                if i.Name == "Ring32" then _G.removed = true end
            end)
        "#).exec().unwrap();
        s.lua.load(r#"
            workspace.Character.HumanoidRootPart.Position = Vector3.new(0,55,0)
            workspace.Terrain:FillBlock(CFrame.new(16,46,0), Vector3.new(8,8,8), Enum.Material.Grass)
            workspace.Terrain:FillBlock(CFrame.new(1024,46,0), Vector3.new(8,8,8), Enum.Material.Grass)
        "#).exec().unwrap();
        crate::runtime::ext::flush_terrain(&s.lua);
        stream_at(&s, &c, &mut map, player, 0.05);
        stream_at(&s, &c, &mut map, player, 0.25);
        c.lua.load(r#"
            assert(_G.events.Ring32 and workspace.Ring32.Ring32Gui)
            assert(workspace:FindFirstChild("Ring192") and not workspace:FindFirstChild("Ring256"))
            assert(workspace.Atomic.AtomicNear and workspace.Atomic.AtomicFar)
            assert(workspace.PersistentDistant.PersistentRemote)
            assert(not workspace.Default:FindFirstChild("DefaultFar"))
            assert(not workspace.Nonatomic:FindFirstChild("NonatomicFar"))
            _G.original = workspace.Ring32
            _G.hasTerrain = function(x)
                local _, v = workspace.Terrain:ReadVoxels(Region3.new(Vector3.new(x,44,0),Vector3.new(x+4,48,4)),4)
                return v[1][1][1] > 0
            end
            assert(_G.hasTerrain(16) and not _G.hasTerrain(1024))
        "#).exec().unwrap();

        s.lua.load(r#"
            local player = game.Players.Tester
            workspace.PerPlayerDistant:AddPersistentPlayer(player)
            assert(workspace.PerPlayerDistant:GetPersistentPlayers()[1] == player)
            player.ReplicationFocus = workspace.Focus
        "#).exec().unwrap();
        stream_at(&s, &c, &mut map, player, 1.0);
        stream_at(&s, &c, &mut map, player, 6.2);
        c.lua.load(r#"
            assert(_G.removed and _G.original.Parent == nil)
            assert(workspace.FocusNeighbor and workspace.Character.HumanoidRootPart)
            assert(workspace.PerPlayerDistant.PerPlayerRemote)
            assert(not workspace:FindFirstChild("Atomic"))
        "#).exec().unwrap();
        s.lua.load(r#"
            local player = game.Players.Tester
            workspace.PerPlayerDistant:RemovePersistentPlayer(player)
            assert(#workspace.PerPlayerDistant:GetPersistentPlayers() == 0)
            player.ReplicationFocus = nil
        "#).exec().unwrap();
        stream_at(&s, &c, &mut map, player, 7.0);
        c.lua.load(r#"
            assert(not workspace:FindFirstChild("PerPlayerDistant"))
            assert(workspace.Ring32 == _G.original and workspace.Ring32.Ring32Gui)
            assert(workspace.PersistentDistant.PersistentRemote)
        "#).exec().unwrap();
    }

    #[test]
    fn streaming_default_retains_loaded_parts_and_requests_load_minimum_region() {
        let (s, c, mut map, player) = streaming_probe("default");
        s.lua.load("workspace.Character.HumanoidRootPart.Position = Vector3.new(0,55,0)").exec().unwrap();
        stream_at(&s, &c, &mut map, player, 0.1);
        stream_at(&s, &c, &mut map, player, 0.4);
        c.lua.load(r#"
            assert(workspace.DefaultDistant and #workspace.DefaultDistant:GetChildren() == 0)
            assert(workspace.Ring32 and not workspace:FindFirstChild("Ring1024"))
        "#).exec().unwrap();
        s.lua.load(r#"
            task.spawn(function()
                game.Players.Tester:RequestStreamAroundAsync(Vector3.new(1024,55,0), 2)
                _G.request_done = true
            end)
        "#).exec().unwrap();
        let mut changes = ops(&s);
        let mut sw = s.world_mut().unwrap();
        let requests = std::mem::take(&mut sw.stream_requests);
        assert_eq!(requests.len(), 1);
        update_streaming(sw.d(), &c, &mut map, player, &mut changes, requests);
        apply(sw.d(), &c, &mut map, changes);
        finish_streaming(sw.d(), &c, &mut map);
        drop(sw);
        s.step(1.0 / 60.0);
        c.lua.load("assert(workspace.Ring1024 and workspace.DefaultDistant.DefaultRemote)").exec().unwrap();
        assert!(s.lua.load("return _G.request_done == true").eval::<bool>().unwrap());
        s.lua.load("game.Players.Tester.ReplicationFocus = workspace.Focus").exec().unwrap();
        stream_at(&s, &c, &mut map, player, 20.0);
        c.lua.load("assert(workspace.Ring32 and workspace.Ring1024 and workspace.FocusNeighbor)").exec().unwrap();
    }

    #[test]
    fn client_stream_request_and_unanchored_assembly() {
        let (s, c, mut map, player) = streaming_probe("opportunistic");
        s.lua.load(r#"
            workspace.Character.HumanoidRootPart.Position = Vector3.new(0,55,0)
            local a = Instance.new("Part")
            a.Name = "WeldedNear"
            a.Position = Vector3.new(12,55,0)
            a.Parent = workspace
            local b = Instance.new("Part")
            b.Name = "WeldedFar"
            b.Position = Vector3.new(2000,55,0)
            b.Parent = workspace
            local weld = Instance.new("WeldConstraint")
            weld.Part0, weld.Part1 = a, b
            weld.Parent = a
        "#).exec().unwrap();
        stream_at(&s, &c, &mut map, player, 0.05);
        stream_at(&s, &c, &mut map, player, 0.25);
        c.lua.load(r#"
            assert(workspace.WeldedNear and workspace.WeldedFar)
            assert(workspace.WeldedNear.WeldConstraint.Part1 == workspace.WeldedFar)
            assert(not workspace:FindFirstChild("Ring1024"))
            task.spawn(function()
                game.Players.Tester:RequestStreamAroundAsync(Vector3.new(1024,55,0), 2)
                _G.client_request_done = true
            end)
        "#).exec().unwrap();
        stream_at(&s, &c, &mut map, player, 0.4);
        c.lua.load("assert(_G.client_request_done and workspace.Ring1024)").exec().unwrap();
    }

    fn names(d: &Document, r: Ref) -> Vec<String> {
        d.dom.descendants_of(r).skip(1).map(|i| i.name.clone()).collect()
    }

    /// A server Destroy reaches the client as one op (Studio's
    /// DestroyInstance item), and the client destroys its copy: Destroying
    /// fires there, every descendant is cut loose and locked.
    #[test]
    fn destroy_replicates_as_one_op() {
        let (s, c, mut map) = pair();
        s.lua.load("local m = Instance.new('Model') m.Name = 'Map'\n\
            for i = 1, 300 do local p = Instance.new('Part') p.Name = 'P' .. i\n\
            local a = Instance.new('Attachment') a.Parent = p p.Parent = m end\n\
            m.Parent = workspace").exec().unwrap();
        sync(&s, &c, &mut map, false);
        c.lua.load("_G.log = {} local m = workspace:FindFirstChild('Map')\n\
            m.Destroying:Connect(function() table.insert(_G.log, 'destroying') end)\n\
            _G.m = m _G.p = m:FindFirstChild('P7')").exec().unwrap();
        s.lua.load("workspace.Map:Destroy()").exec().unwrap();
        let o = ops(&s);
        assert_eq!(o.iter().filter(|x| matches!(x, RepOp::Destroy(_))).count(), 1, "{o:?}");
        assert!(o.len() <= 2, "one Destroy and the root's Parent, not one per descendant: {}", o.len());
        {
            let w = s.world_mut().unwrap();
            apply(w.d(), &c, &mut map, o);
        }
        c.dispatch();
        c.step(1.0 / 60.0);
        let out: Vec<String> = c.lua.load("local p = _G.p\n\
            return table.concat(_G.log, ',') .. '|' .. tostring(_G.m.Parent) .. '|' .. tostring(p.Parent) ..\n\
            '|' .. #_G.m:GetChildren() .. '|' .. #p:GetChildren() .. '|' ..\n\
            tostring(pcall(function() p.Parent = workspace end))").eval::<String>().map(|x| vec![x]).unwrap();
        assert_eq!(out[0], "destroying|nil|nil|0|0|false");
        let w = c.world_mut().unwrap();
        assert!(w.d().find_named(w.d().workspace().unwrap(), "Map").is_none());
        assert_eq!(map.c2s.len(), map.s2c.len());
    }

    /// A big new subtree streams over a budgeted client in pieces, parents
    /// before children and siblings in order, ending identical to the
    /// server's; a Ref to a part that comes later is set when it arrives;
    /// a remote fired after it waits until it is all there.
    #[test]
    fn big_subtree_arrives_in_order_over_frames() {
        let (s, c, mut map) = pair();
        c.lua.load("_G.seen = {} workspace.DescendantAdded:Connect(function(d) table.insert(_G.seen, d.Name) end)").exec().unwrap();
        s.lua.load("local m = Instance.new('Model') m.Name = 'Map'\n\
            for i = 1, 60 do local f = Instance.new('Folder') f.Name = 'F' .. i\n\
              for j = 1, 40 do local p = Instance.new('Part') p.Name = 'P' .. i .. '_' .. j p.Parent = f end\n\
              f.Parent = m end\n\
            m.PrimaryPart = m.F60.P60_40\n\
            m.Parent = workspace").exec().unwrap();
        let o = ops(&s);
        let mut frames = 0;
        {
            let w = s.world_mut().unwrap();
            map.begin_frame(1.0 / 60.0);
            // (A small budget, so the test doesn't depend on machine speed.)
            map.frame_budget = Some(Duration::from_micros(300));
            apply_budgeted(w.d(), &c, &mut map, o);
        }
        c.dispatch();
        assert!(map.backlog() > 0, "2461 instances don't fit a 0.3 ms budget");
        let msg = RemoteMsg { inst: Ref::none(), kind: crate::runtime::RemoteKind::Event, args: vec![] };
        assert!(map.gate_remotes(vec![msg]).is_empty(), "a remote waits for the replication before it");
        while map.backlog() > 0 && frames < 10_000 {
            frames += 1;
            let w = s.world_mut().unwrap();
            map.begin_frame(1.0 / 60.0);
            map.frame_budget = Some(Duration::from_micros(300));
            apply_budgeted(w.d(), &c, &mut map, vec![]);
            drop(w);
            c.dispatch();
        }
        assert!(frames > 1, "{frames}");
        assert_eq!(map.gate_remotes(vec![]).len(), 1, "the remote goes once all of it arrived");
        let (sn, sm) = {
            let w = s.world_mut().unwrap();
            let m = w.d().find_named(w.d().workspace().unwrap(), "Map").unwrap();
            (names(w.d(), m), m)
        };
        let cm = map.s2c[&sm];
        let w = c.world_mut().unwrap();
        let mut cn = names(w.d(), cm);
        let mut sn2 = sn.clone();
        assert_eq!(w.d().dom.get_by_ref(cm).unwrap().children().len(), 60);
        // Same tree (breadth-first names), same child order everywhere.
        assert_eq!(cn.len(), sn2.len());
        assert_eq!(cn, sn2);
        cn.sort();
        sn2.sort();
        assert_eq!(cn, sn2);
        let pp = match w.d().prop(cm, "PrimaryPart") {
            Some(Variant::Ref(r)) => r,
            other => panic!("PrimaryPart {other:?}"),
        };
        assert_eq!(w.d().dom.get_by_ref(pp).map(|i| i.name.as_str()), Some("P60_40"));
        drop(w);
        // DescendantAdded saw each parent before its children.
        let order: String = c.lua.load("local pos = {} for i, n in _G.seen do pos[n] = i end\n\
            for i = 1, 60 do local f = pos['F' .. i] if not f then return 'missing F' .. i end\n\
              for j = 1, 40 do local p = pos['P' .. i .. '_' .. j] if not p or p < f then return 'bad P' .. i .. '_' .. j end end end\n\
            return 'ok ' .. #_G.seen").eval().unwrap();
        assert_eq!(order, "ok 2461");
        let _ = sn;
    }

    #[test]
    fn motion_batch_respects_handover_budget_and_remote_order() {
        use rbx_dom_weak::types::{CFrame, Matrix3, Vector3};

        let (s, c, mut map) = pair();
        let mut server = s.world_mut().unwrap();
        let d = server.dm();
        let ws = d.workspace().unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model"));
        let value = |x| Variant::CFrame(CFrame::new(Vector3::new(x, 0.0, 0.0), Matrix3::identity()));
        let parts: Vec<_> = (0..40).map(|_| {
            d.dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", value(0.0)))
        }).collect();
        apply(d, &c, &mut map, vec![RepOp::Parent(model)]);
        for &part in &parts {
            d.dom.get_by_ref_mut(part).unwrap().properties.insert(ustr("CFrame"), value(10.0));
        }
        map.buffered.insert(parts[0]);
        let moves = parts.iter().copied().chain([parts[35]]).map(RepOp::Moved).collect();
        map.begin_frame(1.0 / 60.0);
        map.frame_budget = Some(Duration::ZERO);
        apply_budgeted(d, &c, &mut map, moves);
        assert_eq!(map.backlog(), parts.len() - MOVE_BATCH);
        {
            let client = c.world_mut().unwrap();
            let read = |i: usize| client.d().prop(map.s2c[&parts[i]], "CFrame");
            assert_eq!(read(0), Some(value(0.0)));
            assert_eq!(read(1), Some(value(10.0)));
            assert_eq!(read(35), Some(value(0.0)));
        }
        let remote = RemoteMsg { inst: Ref::none(), kind: crate::runtime::RemoteKind::Event, args: vec![] };
        assert!(map.gate_remotes(vec![remote]).is_empty());

        d.dom.get_by_ref_mut(parts[35]).unwrap().properties.insert(ustr("CFrame"), value(20.0));
        apply(d, &c, &mut map, vec![]);
        assert_eq!(map.backlog(), 0);
        assert_eq!(map.gate_remotes(vec![]).len(), 1);
        let client = c.world_mut().unwrap();
        assert_eq!(client.d().prop(map.s2c[&parts[35]], "CFrame"), Some(value(20.0)));
    }

    #[test]
    fn server_only_services_stay_empty() {
        let mut s = Document::new_place();
        let root = s.dom.root_ref();
        let ss = s.find_child(root, "ServerStorage").unwrap();
        s.dom
            .insert(ss, InstanceBuilder::new("Folder").with_name("Secret"));
        let (c, map) = initial_client(&s, "c".into());
        let css = c.find_child(c.dom.root_ref(), "ServerStorage").unwrap();
        assert!(c.dom.get_by_ref(css).unwrap().children().is_empty());
        assert_eq!(c.count_class("BasePart"), s.count_class("BasePart"));
        assert!(map.s2c.contains_key(&s.workspace().unwrap()));
    }

    /// The client's children come in the server's order, at every depth
    /// (round 26: Place B Newer's Fast Travel list was upside down).
    #[test]
    fn client_keeps_child_order() {
        let mut s = Document::new_place();
        let ws = s.workspace().unwrap();
        let names = ["Desert", "Forest", "Snow", "Spawn", "Mushroom"];
        let mut f = InstanceBuilder::new("Folder").with_name("List");
        for n in names {
            f = f.with_child(
                InstanceBuilder::new("Folder")
                    .with_name(n)
                    .with_child(InstanceBuilder::new("Folder").with_name(format!("{n}1")))
                    .with_child(InstanceBuilder::new("Folder").with_name(format!("{n}2"))),
            );
        }
        let list = s.dom.insert(ws, f);
        let (c, map) = initial_client(&s, "c".into());
        let kids = |r: Ref| -> Vec<String> {
            c.dom.get_by_ref(r).unwrap().children().iter()
                .map(|k| c.dom.get_by_ref(*k).unwrap().name.clone())
                .collect()
        };
        let clist = map.s2c[&list];
        assert_eq!(kids(clist), names);
        let first = c.dom.get_by_ref(clist).unwrap().children()[0];
        assert_eq!(kids(first), ["Desert1", "Desert2"]);
    }

    /// Attributes replicate one name at a time: client-local attributes
    /// survive server sets and clears, a server write to the same name wins,
    /// and a newly replicated instance still arrives with all its attributes.
    #[test]
    fn attributes_replicate_per_name_without_wiping_client_locals() {
        let (s, c, mut map) = pair();
        s.lua.load(
            "local p = Instance.new('Part'); p.Name = 'Target'; p.Parent = workspace",
        ).exec().unwrap();
        sync(&s, &c, &mut map, false);

        // Client sets local attributes and listens for AttributeChanged.
        c.lua.load(r#"
            local p = workspace:FindFirstChild("Target")
            p:SetAttribute("LocalFly", true)
            p:SetAttribute("Shared", "client")
            _G.changed = {}
            _G.fly_hits = 0
            p.AttributeChanged:Connect(function(k) table.insert(_G.changed, k) end)
            p:GetAttributeChangedSignal("LocalFly"):Connect(function() _G.fly_hits += 1 end)
        "#).exec().unwrap();

        // 1. Server sets different attributes: client-local ones survive.
        s.lua.load(r#"
            local p = workspace.Target
            p:SetAttribute("AimPitch", 1)
            p:SetAttribute("Keep", "srv")
        "#).exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local p = workspace.Target
            table.sort(_G.changed)
            local out = table.concat({
                tostring(p:GetAttribute("LocalFly")),
                tostring(p:GetAttribute("Shared")),
                tostring(p:GetAttribute("AimPitch")),
                tostring(p:GetAttribute("Keep")),
                tostring(_G.fly_hits),
                table.concat(_G.changed, ","),
            }, "|")
            _G.changed = {}
            return out
        "#).eval().unwrap();
        assert_eq!(res, "true|client|1|srv|0|AimPitch,Keep");

        // 2. Server clears one attribute to nil: only that name is removed.
        s.lua.load("workspace.Target:SetAttribute('AimPitch', nil)").exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local p = workspace.Target
            local out = table.concat({
                tostring(p:GetAttribute("LocalFly")),
                tostring(p:GetAttribute("Shared")),
                tostring(p:GetAttribute("AimPitch")),
                tostring(p:GetAttribute("Keep")),
                tostring(_G.fly_hits),
                table.concat(_G.changed, ","),
            }, "|")
            _G.changed = {}
            return out
        "#).eval().unwrap();
        assert_eq!(res, "true|client|nil|srv|0|AimPitch");

        // 3. Server writes a name the client also set locally: server wins.
        s.lua.load("workspace.Target:SetAttribute('Shared', 'server')").exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local p = workspace.Target
            local out = table.concat({
                tostring(p:GetAttribute("LocalFly")),
                tostring(p:GetAttribute("Shared")),
                tostring(p:GetAttribute("Keep")),
                tostring(_G.fly_hits),
                table.concat(_G.changed, ","),
            }, "|")
            _G.changed = {}
            return out
        "#).eval().unwrap();
        assert_eq!(res, "true|server|srv|0|Shared");

        // 4. Server sets an attribute named after a real property in the same
        //    frame as that property: neither deduplicates the other.
        s.lua.load(r#"
            local p = workspace.Target
            p.Transparency = 0.5
            p:SetAttribute("Transparency", 99)
        "#).exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local p = workspace.Target
            return tostring(p.Transparency) .. "|" .. tostring(p:GetAttribute("Transparency")) .. "|" .. tostring(p:GetAttribute("LocalFly"))
        "#).eval().unwrap();
        assert_eq!(res, "0.5|99|true");

        // 5. Initial replication of a new instance carries all of its attributes.
        s.lua.load(r#"
            local fresh = Instance.new("Folder")
            fresh.Name = "Fresh"
            fresh:SetAttribute("A", 1)
            fresh:SetAttribute("B", "two")
            fresh:SetAttribute("C", true)
            fresh.Parent = workspace
        "#).exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local f = workspace:FindFirstChild("Fresh")
            return table.concat({
                tostring(f:GetAttribute("A")),
                tostring(f:GetAttribute("B")),
                tostring(f:GetAttribute("C")),
            }, "|")
        "#).eval().unwrap();
        assert_eq!(res, "1|two|true");
    }

    /// CollectionService tags replicate per tag: client-local tags survive
    /// server AddTag / RemoveTag, and signals + GetTagged stay in sync.
    #[test]
    fn tags_replicate_per_tag_without_wiping_client_locals() {
        let (s, c, mut map) = pair();
        s.lua.load("local p = Instance.new('Part'); p.Name = 'Tagged'; p.Parent = workspace").exec().unwrap();
        sync(&s, &c, &mut map, false);

        c.lua.load(r#"
            local CS = game:GetService("CollectionService")
            local p = workspace:FindFirstChild("Tagged")
            p:AddTag("ClientOnly")
            _G.added = 0
            _G.removed = 0
            CS:GetInstanceAddedSignal("ServerTag"):Connect(function() _G.added += 1 end)
            CS:GetInstanceRemovedSignal("ServerTag"):Connect(function() _G.removed += 1 end)
        "#).exec().unwrap();

        s.lua.load("workspace.Tagged:AddTag('ServerTag'); workspace.Tagged:AddTag('KeepTag')").exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local CS = game:GetService("CollectionService")
            local p = workspace.Tagged
            return table.concat({
                tostring(p:HasTag("ClientOnly")),
                tostring(p:HasTag("ServerTag")),
                tostring(p:HasTag("KeepTag")),
                tostring(#CS:GetTagged("ServerTag")),
                tostring(_G.added),
                tostring(_G.removed),
            }, "|")
        "#).eval().unwrap();
        assert_eq!(res, "true|true|true|1|1|0");

        s.lua.load("workspace.Tagged:RemoveTag('ServerTag')").exec().unwrap();
        sync(&s, &c, &mut map, false);
        let res: String = c.lua.load(r#"
            local CS = game:GetService("CollectionService")
            local p = workspace.Tagged
            return table.concat({
                tostring(p:HasTag("ClientOnly")),
                tostring(p:HasTag("ServerTag")),
                tostring(p:HasTag("KeepTag")),
                tostring(#CS:GetTagged("ServerTag")),
                tostring(_G.added),
                tostring(_G.removed),
            }, "|")
        "#).eval().unwrap();
        assert_eq!(res, "true|false|true|0|1|1");
    }
}
