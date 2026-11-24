//! Workspace interest for the local Play client. The server always keeps its
//! complete DataModel. Only the active client pairs take part in replication.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant};
use rustc_hash::{FxHashMap, FxHashSet};
use rustudio_core::{
    doc::{self, Document},
    terrain::Voxels,
};

use crate::runtime::RepOp;

const CELL: f32 = 64.0;
const INTERVAL: f64 = 0.1;
const REQUEST_HOLD: f64 = 5.0;
const STREAM_OUT_GRACE: f64 = 5.0;
const JUMP_PAUSE: f64 = 4.0 / 60.0;
const JOIN_TARGET_DELAY: f64 = 0.12;

type CellKey = [i32; 3];

pub struct StreamRequest {
    pub player: Ref,
    pub position: Vec3,
    pub complete: Arc<AtomicBool>,
}

pub struct Streaming {
    pub player: Ref,
    pub spawn_focus: Option<Vec3>,
    pub departures: Vec<Ref>,
    pub collider_revision: u64,
    visible: Vec<bool>,
    pub parked: FxHashMap<Ref, Ref>,
    pub parked_client: FxHashMap<Ref, Ref>,
    pub persistence: FxHashMap<Ref, FxHashSet<Ref>>,
    workspace: Ref,
    nodes: Vec<Node>,
    priority: Vec<f32>,
    indices: FxHashMap<Ref, usize>,
    dead_nodes: usize,
    parts: FxHashMap<Ref, Bounds>,
    cells: FxHashMap<CellKey, FxHashSet<Ref>>,
    large: FxHashSet<Ref>,
    assemblies: Vec<Vec<Ref>>,
    selected: FxHashMap<Ref, f32>,
    selection_scratch: FxHashMap<Ref, f32>,
    outside_since: FxHashMap<Ref, f64>,
    joined: bool,
    target_at: Option<f64>,
    target_started: bool,
    topology_dirty: bool,
    has_assembly_joints: bool,
    assemblies_dirty: bool,
    selection_dirty: bool,
    focus_dirty: bool,
    focus: Option<Vec3>,
    evaluated_cell: Option<CellKey>,
    character: Ref,
    next_step: f64,
    memory_check_at: f64,
    low_memory: bool,
    requests: Vec<(f64, StreamRequest)>,
    terrain: Option<Voxels>,
    terrain_dirty: bool,
    terrain_loaded: FxHashSet<CellKey>,
    terrain_outside_since: FxHashMap<CellKey, f64>,
    pub terrain_blob: Option<Vec<u8>>,
    pub paused: bool,
    paused_until: f64,
    pub ready: bool,
}

struct Node {
    id: Ref,
    parent: Option<usize>,
    depth: usize,
    part: bool,
    // None for classes outside the Model streaming rules.
    mode: Option<u32>,
}

#[derive(Clone, Copy)]
struct Bounds {
    center: Vec3,
    half: Vec3,
}

impl Bounds {
    fn region_distance(self, focus: CellKey) -> f32 {
        let (lo, hi) = self.cells();
        cell_distance(focus, std::array::from_fn(|axis| focus[axis].clamp(lo[axis], hi[axis])))
    }

    fn cells(self) -> (CellKey, CellKey) {
        // A part touching a shared region face is eligible from either side.
        let lo = ((self.center - self.half) / CELL).ceil().as_ivec3();
        (lo.to_array().map(|v| v.saturating_sub(1)), cell(self.center + self.half))
    }
}

fn cell(p: Vec3) -> CellKey {
    (p / CELL).floor().as_ivec3().to_array()
}

fn cell_distance(a: CellKey, b: CellKey) -> f32 {
    a[0].abs_diff(b[0]).max(a[1].abs_diff(b[1])).max(a[2].abs_diff(b[2])) as f32 * CELL
}

fn insert_nearer(out: &mut FxHashMap<Ref, f32>, r: Ref, squared: f32) {
    let distance = out.entry(r).or_insert(squared);
    *distance = distance.min(squared);
}

fn cells(lo: CellKey, hi: CellKey) -> impl Iterator<Item = CellKey> {
    (lo[0]..=hi[0]).flat_map(move |x| {
        (lo[1]..=hi[1]).flat_map(move |y| (lo[2]..=hi[2]).map(move |z| [x, y, z]))
    })
}

fn cell_count(lo: CellKey, hi: CellKey) -> i64 {
    (0..3).fold(1i64, |count, i| {
        count.saturating_mul(i64::from(hi[i]) - i64::from(lo[i]) + 1)
    })
}

/// Whether Play streams Workspace regions to the client. `RUSTUDIO_NO_STREAMING=1`
/// sends the whole Workspace as before streaming existed (an escape hatch if a
/// place behaves worse with streaming on).
pub fn enabled(d: &Document) -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *OFF.get_or_init(|| std::env::var_os("RUSTUDIO_NO_STREAMING").is_some_and(|v| v == "1")) {
        return false;
    }
    d.workspace()
        .is_some_and(|w| d.prop(w, "StreamingEnabled") == Some(Variant::Bool(true)))
}

fn enum_property(d: &Document, r: Ref, name: &str) -> u32 {
    match d.prop(r, name) {
        Some(Variant::Enum(v)) => v.to_u32(),
        _ => 0,
    }
}

fn radius(d: &Document, r: Ref, name: &str, fallback: f32) -> f32 {
    match d.prop(r, name) {
        Some(Variant::Int32(v)) => (v as f32).max(0.0),
        _ => fallback,
    }
}

fn ref_property(d: &Document, r: Ref, name: &str) -> Ref {
    match d.prop(r, name) {
        Some(Variant::Ref(r)) => r,
        _ => Ref::none(),
    }
}

fn assembly_joint(class: &str) -> bool {
    matches!(class, "WeldConstraint" | "RigidConstraint") || doc::is_a(class, "JointInstance")
}

fn bounds(d: &Document, r: Ref) -> Option<Bounds> {
    let Variant::CFrame(cf) = d.prop(r, "CFrame")? else {
        return None;
    };
    let center = Vec3::new(cf.position.x, cf.position.y, cf.position.z);
    let half = match d.prop(r, "Size") {
        Some(Variant::Vector3(v)) => {
            let axes = cf.orientation;
            (Vec3::new(axes.x.x, axes.y.x, axes.z.x).abs() * v.x
                + Vec3::new(axes.x.y, axes.y.y, axes.z.y).abs() * v.y
                + Vec3::new(axes.x.z, axes.y.z, axes.z.z).abs() * v.z)
                * 0.5
        }
        _ => Vec3::ZERO,
    };
    (center.is_finite() && half.is_finite()).then_some(Bounds { center, half })
}

impl Streaming {
    pub fn new(d: &Document) -> Self {
        let mut state = Self {
            workspace: d.workspace().unwrap(),
            player: Ref::none(),
            spawn_focus: None,
            departures: Vec::new(),
            collider_revision: 0,
            visible: Vec::new(),
            parked: FxHashMap::default(),
            parked_client: FxHashMap::default(),
            persistence: FxHashMap::default(),
            nodes: Vec::new(),
            priority: Vec::new(),
            indices: FxHashMap::default(),
            dead_nodes: 0,
            parts: FxHashMap::default(),
            cells: FxHashMap::default(),
            large: FxHashSet::default(),
            assemblies: Vec::new(),
            selected: FxHashMap::default(),
            selection_scratch: FxHashMap::default(),
            outside_since: FxHashMap::default(),
            joined: false,
            target_at: None,
            target_started: false,
            topology_dirty: false,
            has_assembly_joints: false,
            assemblies_dirty: false,
            selection_dirty: false,
            focus_dirty: false,
            focus: None,
            evaluated_cell: None,
            character: Ref::none(),
            next_step: 0.0,
            memory_check_at: 0.0,
            low_memory: false,
            requests: Vec::new(),
            terrain: None,
            terrain_dirty: true,
            terrain_loaded: FxHashSet::default(),
            terrain_outside_since: FxHashMap::default(),
            terrain_blob: None,
            paused: false,
            paused_until: 0.0,
            ready: false,
        };
        let previous = state.index(d);
        state.select(d, previous);
        state
    }

    fn index(&mut self, d: &Document) -> Vec<Ref> {
        let _scope = rustudio_core::bench::scope("rep_stream_index");
        let previous = self.nodes.iter().zip(self.visible.drain(..))
            .filter_map(|(node, visible)| (visible && node.id.is_some()).then_some(node.id))
            .collect();
        self.collider_revision += 1;
        self.nodes.clear();
        self.indices.clear();
        self.dead_nodes = 0;
        self.parts.clear();
        self.cells.clear();
        self.large.clear();
        self.has_assembly_joints = false;
        let mut classes = FxHashMap::default();
        self.topology_dirty = false;
        let mut stack = vec![(self.workspace, None, 0)];
        while let Some((id, parent, depth)) = stack.pop() {
            let Some(i) = d.dom.get_by_ref(id) else {
                continue;
            };
            let (part, model, joint) = *classes.entry(i.class).or_insert_with(|| {
                let class = i.class.as_str();
                (
                    class != "Terrain" && doc::is_a(class, "BasePart"),
                    doc::is_a(class, "Model"),
                    assembly_joint(class),
                )
            });
            self.has_assembly_joints |= joint;
            let index = self.nodes.len();
            self.indices.insert(id, index);
            let mode = (id != self.workspace && model).then(|| enum_property(d, id, "ModelStreamingMode"));
            self.nodes.push(Node {
                id,
                parent,
                depth,
                part,
                mode,
            });
            stack.extend(i.children().iter().rev().map(|&child| (child, Some(index), depth + 1)));
            if part {
                self.update_part(d, id);
            }
        }
        self.index_assemblies(d);
        self.focus_dirty = true;
        previous
    }

    fn index_assemblies(&mut self, d: &Document) {
        self.assemblies_dirty = false;
        self.assemblies.clear();
        if self.has_assembly_joints {
            crate::runtime::domains::world::part::with_graph(d, |graph| {
                self.assemblies.extend(graph.ungrounded_assemblies().map(<[Ref]>::to_vec));
            });
        }
    }

    fn remove_tree(&mut self, r: Ref) {
        let Some(&start) = self.indices.get(&r) else { return };
        let depth = self.nodes[start].depth;
        let mut end = start + 1;
        while end < self.nodes.len() && self.nodes[end].depth > depth {
            end += 1;
        }
        // Keep the surviving nodes in preorder and their parent indices
        // stable. Compact on the next full rebuild or when holes accumulate.
        for index in start..end {
            let id = std::mem::replace(&mut self.nodes[index].id, Ref::none());
            if id.is_none() {
                continue;
            }
            self.indices.remove(&id);
            if let Some(bounds) = self.parts.remove(&id) {
                self.remove_part_cells(id, bounds.cells());
            }
            self.dead_nodes += 1;
        }
        self.topology_dirty |= self.dead_nodes > self.nodes.len() / 4;
        self.selection_dirty = true;
        self.assemblies_dirty |= self.has_assembly_joints;
        self.collider_revision += 1;
    }

    fn remove_part_cells(&mut self, r: Ref, (lo, hi): (CellKey, CellKey)) {
        if self.large.remove(&r) {
            return;
        }
        for key in cells(lo, hi) {
            if let Some(bucket) = self.cells.get_mut(&key) {
                bucket.remove(&r);
                if bucket.is_empty() {
                    self.cells.remove(&key);
                }
            }
        }
    }

    fn update_part(&mut self, d: &Document, r: Ref) {
        let Some(next) = bounds(d, r) else { return };
        let range = next.cells();
        if let Some(old) = self.parts.insert(r, next) {
            let (lo, hi) = old.cells();
            if (lo, hi) == range {
                return;
            }
            self.remove_part_cells(r, (lo, hi));
        }
        let (lo, hi) = range;
        if cell_count(lo, hi) > 64 {
            self.large.insert(r);
        } else {
            for key in cells(lo, hi) {
                self.cells.entry(key).or_default().insert(r);
            }
        }
        self.focus_dirty = true;
    }

    fn in_workspace(&self, d: &Document, mut r: Ref) -> bool {
        while let Some(i) = d.dom.get_by_ref(r) {
            if r == self.workspace {
                return true;
            }
            r = i.parent();
        }
        false
    }

    pub fn part_refs(&self) -> impl Iterator<Item = Ref> + '_ {
        self.parts.keys().copied()
    }

    pub fn part_mapping_changed(&mut self, r: Ref) {
        if self.parts.contains_key(&r) {
            self.collider_revision += 1;
        }
    }

    pub fn allowed(&self, r: Ref) -> bool {
        self.indices.get(&r).is_none_or(|&index| self.visible[index])
    }

    pub fn changed(&mut self, d: &Document, ops: &[RepOp]) {
        for op in ops {
            match op {
                RepOp::Parent(r) => {
                    if let Some(&index) = self.indices.get(r) {
                        if let Some(parent) = self.nodes[index].parent {
                            let new_parent = d.dom.get_by_ref(*r).map_or(Ref::none(), |i| i.parent());
                            if self.nodes[parent].id != new_parent {
                                if self.in_workspace(d, *r) {
                                    self.topology_dirty = true;
                                } else {
                                    self.remove_tree(*r);
                                }
                            }
                        }
                    } else {
                        self.topology_dirty |= self.in_workspace(d, *r);
                    }
                }
                RepOp::Destroy(r) => self.remove_tree(*r),
                RepOp::Prop(r, name)
                    if *r == self.workspace
                        && matches!(
                            name.as_str(),
                            "ModelStreamingBehavior"
                                | "StreamingMinRadius"
                                | "StreamingTargetRadius"
                                | "StreamOutBehavior"
                        ) =>
                {
                    self.selection_dirty = true;
                }
                RepOp::Prop(r, name) if name == "ModelStreamingMode" => {
                    if let Some(&index) = self.indices.get(r) {
                        let node = &mut self.nodes[index];
                        if node.mode.is_some() {
                            node.mode = Some(enum_property(d, *r, "ModelStreamingMode"));
                            self.selection_dirty = true;
                        }
                    }
                }
                RepOp::Prop(r, name)
                    if self.has_assembly_joints
                        && matches!(name.as_str(), "Anchored" | "Part0" | "Part1" | "Enabled" | "Attachment0" | "Attachment1") =>
                {
                    self.assemblies_dirty |= self.indices.get(r).is_some_and(|&index| {
                        if name == "Anchored" {
                            self.nodes[index].part
                        } else {
                            d.dom.get_by_ref(*r).is_some_and(|i| assembly_joint(i.class.as_str()))
                        }
                    });
                }
                RepOp::Prop(_, name) if name == "SmoothGrid" => self.terrain_dirty = true,
                RepOp::Prop(r, name)
                    if (name == "CFrame" || name == "Size" || name == "size")
                        && self.parts.contains_key(r) =>
                {
                    self.update_part(d, *r)
                }
                RepOp::Moved(r) if self.parts.contains_key(r) => self.update_part(d, *r),
                RepOp::Persistence(model, player, add) => {
                    let set = self.persistence.entry(*model).or_default();
                    if *add {
                        set.insert(*player);
                    } else {
                        set.remove(player);
                    }
                    self.selection_dirty = true;
                }
                _ => {}
            }
        }
    }

    fn nearby(&self, p: Vec3, radius: f32, out: &mut FxHashMap<Ref, f32>) {
        // The Studio grid probe loads a cube of 64-stud regions around the
        // region containing the focus. The outer face is one region beyond
        // a part measured at the same distance in the opposite direction.
        let center = cell(p);
        let rings = (radius / CELL).floor() as i32;
        let lo = center.map(|v| v.saturating_sub(rings));
        let hi = center.map(|v| v.saturating_add(rings));
        let mut visit = |key: CellKey, bucket: &FxHashSet<Ref>| {
            // A part's distance is the minimum over its occupied cells.
            // All parts in this bucket share the same candidate distance.
            let dist = cell_distance(center, key);
            let squared = dist * dist;
            for &r in bucket {
                insert_nearer(out, r, squared);
            }
        };
        if cell_count(lo, hi) > self.cells.len() as i64 {
            for (&key, bucket) in &self.cells {
                if (0..3).all(|axis| key[axis] >= lo[axis] && key[axis] <= hi[axis]) {
                    visit(key, bucket);
                }
            }
        } else {
            for key in cells(lo, hi) {
                if let Some(bucket) = self.cells.get(&key) {
                    visit(key, bucket);
                }
            }
        }
        for &r in &self.large {
            let dist = self.parts[&r].region_distance(center);
            if dist <= rings as f32 * CELL {
                insert_nearer(out, r, dist * dist);
            }
        }
    }

    fn select(&mut self, d: &Document, previous: Vec<Ref>) {
        let improved = enum_property(d, self.workspace, "ModelStreamingBehavior") == 2;
        let mut spatial = vec![false; self.nodes.len()];
        let mut distance = vec![f32::INFINITY; self.nodes.len()];
        let mut persistent = vec![false; self.nodes.len()];
        for (&r, &dist) in &self.selected {
            if let Some(&index) = self.indices.get(&r) {
                distance[index] = dist;
            }
        }
        for (idx, node) in self.nodes.iter().enumerate().rev() {
            if node.id.is_none() {
                continue;
            }
            spatial[idx] |= node.part;
            persistent[idx] |= node.id == self.character
                || (self.joined && node.mode == Some(2))
                || (node.mode == Some(3)
                    && self
                        .persistence
                        .get(&node.id)
                        .is_some_and(|players| players.contains(&self.player)));
            if let Some(p) = node.parent {
                spatial[p] |= spatial[idx];
                persistent[p] |= persistent[idx];
                distance[p] = distance[p].min(distance[idx]);
            }
        }
        self.departures.clear();
        let mut included = vec![false; self.nodes.len()];
        let mut whole = vec![false; self.nodes.len()];
        for (idx, node) in self.nodes.iter().enumerate() {
            if node.id.is_none() {
                continue;
            }
            let (parent, all) = node
                .parent
                .map_or((true, false), |p| (included[p], whole[p]));
            let atomic = matches!(node.mode, Some(1..=3));
            let own = node.id == self.character
                || (self.joined && node.mode == Some(2))
                || (node.mode == Some(3) && persistent[idx]);
            let hit = distance[idx].is_finite();
            let gated =
                node.part || (spatial[idx] && (atomic || (improved && node.mode.is_some())));
            included[idx] = parent && (all || !gated || hit || persistent[idx]);
            whole[idx] = included[idx] && (all || own || (atomic && (hit || persistent[idx])));
            if !included[idx] && self.visible.get(idx).copied().unwrap_or(false) {
                self.departures.push(node.id);
            }
        }
        self.priority = distance;
        self.visible = included;
        self.departures.extend(previous.into_iter().filter(|r| {
            self.indices.get(r).is_some_and(|&index| !self.visible[index])
        }));
    }

    pub fn step(&mut self, d: &Document, time: f64, requests: Vec<StreamRequest>) -> bool {
        self.paused = time < self.paused_until;
        let rebuilt = self.topology_dirty;
        let assemblies_changed = self.assemblies_dirty;
        let selection_changed = std::mem::take(&mut self.selection_dirty);
        let previous = if rebuilt {
            self.index(d)
        } else {
            if assemblies_changed {
                self.index_assemblies(d);
            }
            Vec::new()
        };
        let character = ref_property(d, self.player, "Character");
        let focus = ref_property(d, self.player, "ReplicationFocus");
        let primary = ref_property(d, character, "PrimaryPart");
        let root = d
            .find_named(character, "HumanoidRootPart")
            .unwrap_or(primary);
        let position = bounds(d, focus)
            .or_else(|| bounds(d, primary))
            .or_else(|| bounds(d, root))
            .map(|b| b.center)
            .or_else(|| (time > 0.0).then_some(self.spawn_focus).flatten());
        let region = position.map(cell);
        let moved = self.evaluated_cell != region;
        let jumped = self
            .focus
            .zip(position)
            .is_none_or(|(a, b)| a.distance_squared(b) > CELL * CELL);
        self.focus = position;
        let changed_character = self.character != character;
        self.character = character;
        let joining = !self.joined && self.player.is_some() && time > 0.0;
        self.joined |= joining;
        if position.is_some() && self.target_at.is_none() {
            self.target_at = Some(time + JOIN_TARGET_DELAY);
        }
        let target_started = self.target_at.is_some_and(|at| time >= at);
        let new_phase = target_started != self.target_started;
        self.target_started = target_started;
        let mut new_request = false;
        for request in requests {
            if request.player == self.player && request.position.is_finite() {
                new_request = true;
                self.requests.push((time + REQUEST_HOLD, request));
            } else {
                request.complete.store(true, Ordering::Relaxed);
            }
        }
        let old_requests = self.requests.len();
        self.requests.retain(|(until, request)| {
            if *until <= time {
                request.complete.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        });
        let request_changed = new_request || old_requests != self.requests.len();
        let memory_changed = if time >= self.memory_check_at {
            self.memory_check_at = time + 1.0;
            let low = rustudio_core::platform::low_memory();
            std::mem::replace(&mut self.low_memory, low) != low
        } else {
            false
        };
        let invalidated = rebuilt
            || assemblies_changed
            || selection_changed
            || memory_changed
            || changed_character
            || joining
            || new_phase
            || self.terrain_dirty
            || request_changed;
        if !invalidated && !(moved && jumped) {
            if time < self.next_step {
                return false;
            }
            if !moved && !self.focus_dirty {
                if self.outside_since.is_empty() && self.terrain_outside_since.is_empty() {
                    return false;
                }
                // With stable interest, only an expired grace period can
                // change the selection. Keep the same rescan cadence.
                if self
                    .outside_since
                    .values()
                    .chain(self.terrain_outside_since.values())
                    .all(|since| time - since < STREAM_OUT_GRACE)
                {
                    self.next_step = time + INTERVAL;
                    return false;
                }
            }
        }
        let _scope = rustudio_core::bench::scope("rep_stream_interest");
        self.next_step = time + INTERVAL;
        self.evaluated_cell = region;
        self.focus_dirty = false;
        let min = radius(d, self.workspace, "StreamingMinRadius", 64.0);
        let target = if target_started && !self.low_memory {
            radius(d, self.workspace, "StreamingTargetRadius", 1024.0).max(min)
        } else {
            min
        };
        let retain = !self.low_memory && enum_property(d, self.workspace, "StreamOutBehavior") != 2;
        let mut next = std::mem::take(&mut self.selection_scratch);
        next.clear();
        if let Some(p) = position {
            self.nearby(p, target, &mut next);
        }
        for (_, request) in &self.requests {
            self.nearby(request.position, min, &mut next);
        }
        for members in &self.assemblies {
            let nearest = members
                .iter()
                .filter_map(|r| next.get(r).copied())
                .min_by(f32::total_cmp);
            if let Some(distance) = nearest {
                for &r in members {
                    if self.parts.contains_key(&r) {
                        next.insert(r, distance);
                    }
                }
            }
        }
        let entering = next.keys().any(|r| !self.selected.contains_key(r));
        self.outside_since
            .retain(|r, _| self.parts.contains_key(r) && !next.contains_key(r));
        let entering_min = next
            .iter()
            .any(|(r, d)| *d <= min * min && !self.selected.contains_key(r));
        for (&r, &dist) in &self.selected {
            if !self.parts.contains_key(&r) || next.contains_key(&r) {
                continue;
            }
            if retain
                || (!self.low_memory
                    && time - *self.outside_since.entry(r).or_insert(time) < STREAM_OUT_GRACE)
            {
                next.insert(r, dist);
            } else {
                self.outside_since.remove(&r);
            }
        }
        let dirty = rebuilt
            || selection_changed
            || changed_character
            || joining
            || entering
            || next.len() != self.selected.len();
        self.selection_scratch = std::mem::replace(&mut self.selected, next);
        if dirty {
            self.select(d, previous);
        }
        let integrity = enum_property(d, self.workspace, "StreamingIntegrityMode")
            .max(enum_property(d, self.workspace, "StreamingPauseMode"));
        if entering_min
            && moved
            && jumped
            && self.ready
            && !changed_character
            && (integrity == 2 || (integrity == 3 && focus.is_none()))
        {
            self.paused_until = time + JUMP_PAUSE;
            self.paused = true;
        }
        self.update_terrain(d, target, min, retain, time);
        dirty
    }

    fn update_terrain(&mut self, d: &Document, target: f32, min: f32, retain: bool, time: f64) {
        let dirty = std::mem::take(&mut self.terrain_dirty);
        if dirty {
            self.terrain = Voxels::from_dom(&d.dom).and_then(Result::ok);
        }
        let old = &self.terrain_loaded;
        let Some(terrain) = &self.terrain else {
            if dirty {
                self.terrain_loaded.clear();
                self.terrain_blob = Some(Voxels::empty().encode());
            }
            return;
        };
        let mut next: FxHashSet<_> = terrain
            .chunk_keys()
            .filter(|&key| {
                let bounds = Bounds {
                    center: Vec3::from_array(key.map(|x| x as f32 * 128.0 + 64.0)),
                    // SmoothGrid chunks contain eight 64-stud regions.
                    // The far face belongs to the adjacent chunk.
                    half: Vec3::splat(63.875),
                };
                self.focus
                    .is_some_and(|p| bounds.region_distance(cell(p)) <= target)
                    || self
                        .requests
                        .iter()
                        .any(|(_, r)| bounds.region_distance(cell(r.position)) <= min)
            })
            .collect();
        self.terrain_outside_since
            .retain(|key, _| !next.contains(key) && terrain.chunk_has_voxels(*key));
        for &key in old {
            if next.contains(&key) || !terrain.chunk_has_voxels(key) {
                continue;
            }
            if retain
                || (!self.low_memory
                    && time - *self.terrain_outside_since.entry(key).or_insert(time)
                        < STREAM_OUT_GRACE)
            {
                next.insert(key);
            } else {
                self.terrain_outside_since.remove(&key);
            }
        }
        if dirty || *old != next {
            let _scope = rustudio_core::bench::scope("rep_stream_terrain_encode");
            self.terrain_blob = Some(terrain.encode_where(|key| next.contains(&key)));
            self.terrain_loaded = next;
        }
    }

    pub fn initial_stream_delivered(&self) -> bool {
        self.joined && self.target_started
    }

    pub fn complete_requests(&self) {
        for (_, request) in &self.requests {
            request.complete.store(true, Ordering::Relaxed);
        }
    }

    pub fn atomic(&self, r: Ref) -> bool {
        r == self.character
            || self.indices.get(&r).is_some_and(|&index| {
                let node = &self.nodes[index];
                node.part || matches!(node.mode, Some(1..=3))
            })
    }

    pub fn arrivals(&self, mapped: &rustudio_core::RefMap) -> Vec<Ref> {
        let mut order = Vec::new();
        for (index, node) in self.nodes.iter().enumerate() {
            if !self.visible[index] || mapped.contains_key(&node.id) {
                continue;
            }
            let Some(parent) = node.parent else { continue };
            if mapped.contains_key(&self.nodes[parent].id) {
                order.push((self.priority[index], node.id));
            }
        }
        order.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        order.into_iter().map(|(_, r)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::types::{CFrame, Enum, Vector3};
    use rbx_dom_weak::{InstanceBuilder, ustr};

    #[test]
    fn bounds_touching_a_region_face_are_eligible_on_either_side() {
        let region = cell(Vec3::ZERO);
        for x in [-129.0, 193.0] {
            let bounds = Bounds { center: Vec3::new(x, 0.0, 0.0), half: Vec3::ONE };
            assert_eq!(bounds.region_distance(region), 128.0);
            let separated = Bounds { center: bounds.center + Vec3::X * x.signum() * 0.01, ..bounds };
            assert_eq!(separated.region_distance(region), 192.0);
        }
    }

    #[test]
    fn removing_a_subtree_keeps_surviving_indices() {
        for destroy in [false, true] {
            let mut doc = Document::new_place();
            let workspace = doc.workspace().unwrap();
            let folder = doc.dom.insert(workspace, InstanceBuilder::new("Folder"));
            let part = doc.dom.insert(
                folder,
                InstanceBuilder::new("Part")
                    .with_property("CFrame", Variant::CFrame(CFrame::identity())),
            );
            // Keep the removal below the index compaction threshold.
            for _ in 0..4 {
                doc.dom.insert(workspace, InstanceBuilder::new("Folder"));
            }
            let sibling = doc.dom.insert(workspace, InstanceBuilder::new("Folder"));
            let mut state = Streaming::new(&doc);
            let sibling_index = state.indices[&sibling];

            if destroy {
                doc.dom.destroy(folder);
                state.changed(&doc, &[RepOp::Destroy(folder), RepOp::Parent(folder)]);
            } else {
                doc.dom.transfer_within(folder, Ref::none());
                state.changed(&doc, &[RepOp::Parent(folder)]);
            }
            state.step(&doc, 1.0, Vec::new());

            assert_eq!(state.indices[&sibling], sibling_index);
            assert!(!state.indices.contains_key(&folder));
            assert!(!state.indices.contains_key(&part));
            assert!(!state.parts.contains_key(&part));
            assert!(state.cells.values().all(|bucket| !bucket.contains(&part)));

            if !destroy {
                doc.dom.transfer_within(folder, workspace);
                state.changed(&doc, &[RepOp::Parent(folder)]);
                state.step(&doc, 1.25, Vec::new());
                assert!(state.parts.contains_key(&part));
            }
        }
    }

    #[test]
    fn default_stream_out_uses_minimum_radius_under_memory_pressure() {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let part_at = |x| {
            InstanceBuilder::new("Part").with_property(
                "CFrame",
                Variant::CFrame(CFrame::new(
                    Vector3::new(x, 0.0, 0.0),
                    CFrame::identity().orientation,
                )),
            )
        };
        let near = doc.dom.insert(ws, part_at(32.0));
        let target = doc.dom.insert(ws, part_at(320.0));
        let ws_props = &mut doc.dom.get_by_ref_mut(ws).unwrap().properties;
        ws_props.insert(ustr("StreamingMinRadius"), Variant::Int32(64));
        ws_props.insert(ustr("StreamingTargetRadius"), Variant::Int32(512));
        ws_props.insert(ustr("StreamOutBehavior"), Variant::Enum(Enum::from_u32(0)));

        let mut state = Streaming::new(&doc);
        state.spawn_focus = Some(Vec3::ZERO);
        state.memory_check_at = f64::INFINITY;
        state.step(&doc, 1.0, Vec::new());
        state.step(&doc, 1.25, Vec::new());
        assert!(state.allowed(near) && state.allowed(target));

        state.low_memory = true;
        state.focus_dirty = true;
        state.step(&doc, 2.0, Vec::new());
        assert!(state.allowed(near));
        assert!(!state.allowed(target));

        state.low_memory = false;
        state.focus_dirty = true;
        state.step(&doc, 3.0, Vec::new());
        assert!(state.allowed(target));
    }
}
