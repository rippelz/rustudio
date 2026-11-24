//! Assemblies a client builds for itself (a game's display models: Place A's
//! lobby crew is a server-made avatar cloned into the client's menu scene,
//! its root anchored and moved by script). Studio's client simulates those;
//! here each frame places their parts from their joints, outward from the
//! anchored part: Part1 = Part0 · C0 · Transform · C1⁻¹ (and the inverse
//! when the anchored side is Part1). Parts the server also has are left
//! alone: server physics and replication own them.
//!
//! WeldConstraint and RigidConstraint hold their parts too (one assembly in
//! Studio): a WeldConstraint keeps the offset its parts had when it is first
//! seen here, a RigidConstraint Attachment0 · Attachment1⁻¹. Without them a
//! Laser Tag view model's Trim pieces, welded to its animated Body, stayed in
//! the rest pose while the Body swung through Reload.

use std::collections::{HashMap, HashSet, VecDeque};

use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{Instance, Ustr};
use rustudio_core::ukey;

use rustudio_core::doc::{self, Document};
use rustudio_core::math::Xform;

/// Joint classes that hold two parts by C0 / C1.
const JOINTS: [&str; 7] = ["Motor6D", "Weld", "Motor", "ManualWeld", "Snap", "Glue", "AnimationConstraint"];
/// Rigid constraints without C0 / C1 (see the module note).
const RIGID: [&str; 2] = ["WeldConstraint", "RigidConstraint"];

fn joint_class(class: &str) -> bool {
    JOINTS.contains(&class) || RIGID.contains(&class)
}

/// Edges are linked from each endpoint, in the rig's joint order, so a
/// traversal visits only the edges incident to the part it is posing.
struct RigEdge {
    parts: [Ref; 2],
    frames: [Xform; 2],
    next: [usize; 2],
}

#[derive(Default)]
struct RigPoseScratch {
    joints: Vec<Ref>,
    edges: Vec<RigEdge>,
    heads: rustc_hash::FxHashMap<Ref, usize>,
    placed: rustc_hash::FxHashSet<Ref>,
    queue: VecDeque<(Ref, Xform)>,
    writes: Vec<(Ref, Xform)>,
    /// Per write: from a rig kept as posed (RUSTUDIO_CJ_CHECK only).
    kept: Vec<bool>,
}

/// RUSTUDIO_CJ_CHECK=1: rigs kept as posed are posed anyway and every part
/// that would have moved is counted (`cj_check_moved`) and printed; the
/// kept rigs' writes are still not applied.
fn check_kept() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_CJ_CHECK").is_some())
}

#[derive(Default)]
pub struct ClientJoints {
    doc_id: u64,
    /// The client-only joints under Workspace, kept up to date from the
    /// document's journal (a full scan of Place B's 116k instances on every
    /// tree change was 3 ms a frame).
    joints: HashSet<Ref>,
    /// Root parts of animated rigs (a Humanoid's HumanoidRootPart, an
    /// AnimationController model's PrimaryPart) the server owns, found
    /// every 60 frames.
    rigs: Vec<Ref>,
    frame: u64,
    /// WeldConstraint → (Part0, Part1, Part0⁻¹ · Part1 when first seen).
    welds: HashMap<Ref, (Ref, Ref, Xform)>,
    /// Each posed rig's model → its joints (descendant order), kept until
    /// a tree change inside the model; and each listed joint → its model.
    /// (Walking 230 NPC models' descendants every frame was ~4 M
    /// instructions on the NPC test place.)
    rig_joints: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    joint_rig: rustc_hash::FxHashMap<Ref, Ref>,
    pose_scratch: RigPoseScratch,
    /// Rigs whose parts hold the pose `pose_rigs` last computed for them
    /// (model → root it was posed from): nothing it read has changed since,
    /// so posing them again would only find every part where it is. A large test place
    /// had ~1,400 rig joints re-read every frame for ~240 parts that moved.
    posed: rustc_hash::FxHashMap<Ref, Ref>,
    /// Everything a posed rig's pose was read from (its root, joints, their
    /// parts and attachments) → the rigs read from it; a journal mark on
    /// any of them poses those rigs again.
    read_by: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    reads_of: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    /// `rig_controllers` as last walked, and whether a tree change since
    /// then had a Humanoid / AnimationController in it.
    controllers: Vec<Ref>,
    controllers_dirty: bool,
    /// Per rig: whether its reads are tracked (`posed`), and how often it was
    /// dirty again by its next pose. A rig animating every frame (a test place's
    /// NPCs) gains nothing from tracking and pays for it; after a few dirty
    /// poses in a row it is posed untracked (every frame, as before) for a
    /// while, then tried again.
    tracks: rustc_hash::FxHashMap<Ref, RigTrack>,
}

#[derive(Default, Clone, Copy)]
struct RigTrack {
    tracked: bool,
    dirty_streak: u8,
    untracked_until: u64,
}

fn is_joint(cd: &Document, r: Ref) -> bool {
    cd.dom
        .get_by_ref(r)
        .is_some_and(|i| joint_class(i.class.as_str()))
}

fn compose(a: &Xform, b: &Xform) -> Xform {
    Xform {
        pos: a.apply(b.pos),
        rot: a.rot * b.rot,
    }
}

fn cframe(d: &Document, r: Ref, k: Ustr) -> Option<Xform> {
    cframe_for(d.dom.get_by_ref(r)?, k)
}

fn cframe_for(i: &Instance, k: Ustr) -> Option<Xform> {
    match i.properties.get(&k) {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(c)),
        _ => None,
    }
}

fn part_ref(d: &Document, j: Ref, k: Ustr) -> Option<Ref> {
    match d.dom.get_by_ref(j)?.properties.get(&k) {
        Some(Variant::Ref(r)) if r.is_some() && d.dom.get_by_ref(*r).is_some() => Some(*r),
        _ => None,
    }
}

fn anchored(d: &Document, p: Ref) -> bool {
    matches!(d.prop_key(p, ukey!("Anchored")), Some(Variant::Bool(true)))
}

impl ClientJoints {
    /// Places the client-only parts of `cd` (the client's DataModel) from
    /// their joints. `c2s` maps client instances the server also has.
    pub fn solve(&mut self, cd: &mut Document, c2s: &rustudio_core::RefMap) {
        // (Read first: pose_rigs keeps its rigs' joint lists by it. Its own
        // writes are CFrames, which only matter here as tree changes.)
        let marks = cd.marks_for(doc::Reader::ClientJoints);
        self.note_rig_trees(cd, &marks);
        self.rig_marks(cd, &marks);
        self.pose_rigs_with(cd, |root, _model| c2s.contains_key(&root));
        self.solve_rest(cd, c2s, marks);
        // What this solve wrote is its own output, not news to it: only the
        // CFrames of the parts it placed (no tree changes), all already in
        // the poses it keeps (`posed`).
        cd.discard_marks_for(doc::Reader::ClientJoints);
    }

    /// Server-side rigs that physics / NPC humanoids do not pose themselves
    /// (AnimationController rigs and anchored Humanoid rigs): places their
    /// unanchored limbs from Motor6D / AnimationConstraint Transforms so
    /// server scripts read the animated Part.CFrames just like Studio.
    pub fn solve_server_rigs(
        &mut self,
        doc: &mut Document,
        skip_models: &rustc_hash::FxHashSet<Ref>,
    ) {
        let marks = doc.marks_for(doc::Reader::ServerJoints);
        self.note_rig_trees(doc, &marks);
        self.rig_marks(doc, &marks);
        self.pose_rigs_with(doc, |_root, model| !skip_models.contains(&model));
        self.doc_id = doc.id;
        doc.discard_marks_for(doc::Reader::ServerJoints);
    }

    fn note_rig_trees(&mut self, cd: &Document, marks: &doc::Marks) {
        if self.controllers_dirty {
            return;
        }
        match marks {
            doc::Marks::Some(list) => {
                for m in list {
                    if let doc::Mark::Tree(t) = *m {
                        // (A removed subtree is read from limbo.)
                        if cd.subtree(t).into_iter().any(|r| cd.class_any(r).is_some_and(|c| is_rig_controller(c.as_str()))) {
                            self.controllers_dirty = true;
                            return;
                        }
                    }
                }
            }
            doc::Marks::All => self.controllers_dirty = true,
        }
    }

    /// Drops a posed rig: its next frame poses it again.
    fn unpose(&mut self, model: Ref) {
        self.posed.remove(&model);
        if let Some(reads) = self.reads_of.remove(&model) {
            for r in reads {
                if let Some(v) = self.read_by.get_mut(&r) {
                    v.retain(|m| *m != model);
                    if v.is_empty() {
                        self.read_by.remove(&r);
                    }
                }
            }
        }
    }

    fn solve_rest(&mut self, cd: &mut Document, c2s: &rustudio_core::RefMap, marks: doc::Marks) {
        let ws = cd.find_child(cd.dom.root_ref(), "Workspace");
        // Under Workspace, or under a WorldModel (a ViewportFrame's world:
        // Studio solves joints and plays animations there, not elsewhere
        // in a ViewportFrame).
        let in_world = |cd: &Document, r: Ref| {
            let mut cur = r;
            while let Some(i) = cd.dom.get_by_ref(cur) {
                if Some(cur) == ws || i.class == "WorldModel" {
                    return true;
                }
                cur = i.parent();
            }
            false
        };
        let wanted = |cd: &Document, r: Ref| !c2s.contains_key(&r) && is_joint(cd, r) && in_world(cd, r);
        match marks {
            doc::Marks::Some(marks) if cd.id == self.doc_id => {
                for m in marks {
                    let doc::Mark::Tree(t) = m else { continue };
                    for r in cd.subtree(t) {
                        if wanted(cd, r) {
                            self.joints.insert(r);
                        } else {
                            self.joints.remove(&r);
                        }
                    }
                }
            }
            _ => {
                self.doc_id = cd.id;
                self.joints = cd
                    .dom
                    .descendants_of(cd.dom.root_ref())
                    .filter(|i| joint_class(i.class.as_str()))
                    .map(|i| i.referent())
                    .filter(|r| wanted(cd, *r))
                    .collect();
            }
        }
        if self.welds.len() > self.joints.len() * 2 + 64 {
            self.welds.retain(|j, _| cd.dom.get_by_ref(*j).is_some());
        }
        if self.joints.is_empty() {
            return;
        }
        // (joint, part0, part1, C0 · Transform, C1)
        let mut edges: Vec<(Ref, Ref, Xform, Xform)> = Vec::new();
        let mut by_part: HashMap<Ref, Vec<usize>> = HashMap::new();
        let joints: Vec<Ref> = self.joints.iter().copied().collect();
        rustudio_core::bench::add("cj_joints", 0.0, joints.len() as u64);
        for j in joints {
            let Some(joint) = cd.dom.get_by_ref(j) else { continue };
            let Some((p0, p1, c0, c1)) = self.frames(cd, joint) else {
                continue;
            };
            let is_part = |r: Ref| {
                cd.dom
                    .get_by_ref(r)
                    .is_some_and(|i| doc::is_a(&i.class, "BasePart"))
            };
            if p0 == p1 || !is_part(p0) || !is_part(p1) {
                continue;
            }
            let ident = Xform::from_cframe(&rbx_dom_weak::types::CFrame::identity());
            let t = cframe_for(joint, ukey!("Transform")).unwrap_or(ident);
            let i = edges.len();
            edges.push((p0, p1, compose(&c0, &t), c1));
            by_part.entry(p0).or_default().push(i);
            by_part.entry(p1).or_default().push(i);
        }
        // Outward from each anchored part, and from each part the server
        // has (its place comes from the server's simulation and
        // replication; a client-only part welded to it rides along, as a
        // massless part welded into a server assembly does in Studio: a
        // game's aura clones and FakeHead accessories welded to the
        // character's limbs from a client script). Parts with neither in
        // their assembly would fall in Studio and are left where they are.
        let mut placed: HashMap<Ref, Xform> = HashMap::new();
        let mut queue: VecDeque<Ref> = VecDeque::new();
        for &p in by_part.keys() {
            if anchored(cd, p) || c2s.contains_key(&p) {
                if let Some(xf) = cframe(cd, p, ukey!("CFrame")) {
                    placed.insert(p, xf);
                    queue.push_back(p);
                }
            }
        }
        let mut moved: HashSet<Ref> = HashSet::new();
        while let Some(p) = queue.pop_front() {
            let at = placed[&p];
            for &i in by_part.get(&p).map(|v| v.as_slice()).unwrap_or(&[]) {
                let (p0, p1, c0t, c1) = edges[i];
                let (other, xf) = if p == p0 {
                    (p1, compose(&compose(&at, &c0t), &c1.inverse()))
                } else {
                    (p0, compose(&compose(&at, &c1), &c0t.inverse()))
                };
                if placed.contains_key(&other) || anchored(cd, other) || c2s.contains_key(&other) {
                    continue;
                }
                placed.insert(other, xf);
                queue.push_back(other);
                moved.insert(other);
            }
        }
        for p in moved {
            let xf = placed[&p];
            let old = cframe(cd, p, ukey!("CFrame"));
            let same = old.is_some_and(|o| {
                (o.pos - xf.pos).length() < 1e-4 && o.rot.abs_diff_eq(xf.rot, 1e-5)
            });
            if same {
                continue;
            }
            if let Some(inst) = cd.dom.get_by_ref_mut(p) {
                inst.properties
                    .insert(ukey!("CFrame"), Variant::CFrame(xf.to_cframe()));
            }
            cd.mark_prop(p, ukey!("CFrame"));
        }
    }
}

impl ClientJoints {
    /// A joint's (Part0, Part1, C0, C1), or None when it is off or not
    /// between two parts.
    fn frames(&mut self, cd: &Document, joint: &Instance) -> Option<(Ref, Ref, Xform, Xform)> {
        let class = joint.class;
        let j = joint.referent();
        if !RIGID.contains(&class.as_str()) {
            let enabled = ukey!("Enabled");
            let disabled = match joint.properties.get(&enabled) {
                Some(v) => matches!(v, Variant::Bool(false)),
                None => matches!(cd.prop_key(j, enabled), Some(Variant::Bool(false))),
            };
            if disabled {
                return None;
            }
            return rustudio_core::body::joint_frames_for(cd, joint);
        }
        if !rustudio_core::body::joint_enabled(cd, j) {
            return None;
        }
        let (p0, p1) = rustudio_core::body::joint_parts(cd, j)?;
        let ident = Xform::from_cframe(&rbx_dom_weak::types::CFrame::identity());
        if class == "RigidConstraint" {
            let a = |k: Ustr| part_ref(cd, j, k).and_then(|a| cframe(cd, a, ukey!("CFrame"))).unwrap_or(ident);
            return Some((p0, p1, a(ukey!("Attachment0")), a(ukey!("Attachment1"))));
        }
        match self.welds.get(&j) {
            Some(&(a, b, rel)) if a == p0 && b == p1 => Some((p0, p1, rel, ident)),
            _ => {
                let rel = compose(&cframe(cd, p0, ukey!("CFrame"))?.inverse(), &cframe(cd, p1, ukey!("CFrame"))?);
                self.welds.insert(j, (p0, p1, rel));
                Some((p0, p1, rel, ident))
            }
        }
    }

    /// Drops the joint lists of rigs a tree change reached: something added
    /// or moved under the model (its ancestors), or a listed joint (or an
    /// ancestor of one) taken out or moved.
    fn rig_marks(&mut self, cd: &Document, marks: &doc::Marks) {
        let list = match marks {
            doc::Marks::Some(m) if cd.id == self.doc_id => m,
            _ => {
                self.rig_joints.clear();
                self.joint_rig.clear();
                self.posed.clear();
                self.read_by.clear();
                self.reads_of.clear();
                return;
            }
        };
        if self.rig_joints.is_empty() {
            return;
        }
        if !self.posed.is_empty() {
            for m in list {
                let (doc::Mark::Prop(r, _) | doc::Mark::Tree(r)) = *m;
                if let Some(models) = self.read_by.get(&r) {
                    for model in models.clone() {
                        self.unpose(model);
                    }
                }
            }
        }
        let drop_rig = |me: &mut Self, model: Ref| {
            me.unpose(model);
            if let Some(js) = me.rig_joints.remove(&model) {
                for j in js {
                    me.joint_rig.remove(&j);
                }
            }
        };
        for m in list {
            let doc::Mark::Tree(t) = *m else { continue };
            let mut cur = t;
            while let Some(i) = cd.dom.get_by_ref(cur) {
                if self.rig_joints.contains_key(&cur) {
                    drop_rig(self, cur);
                }
                cur = i.parent();
            }
            for r in cd.subtree(t) {
                if let Some(model) = self.joint_rig.get(&r).copied() {
                    drop_rig(self, model);
                }
            }
        }
        if self.rig_joints.len() > self.rigs.len() * 2 + 64 {
            let live: HashSet<Ref> = self.rigs.iter().filter_map(|r| cd.dom.get_by_ref(*r).map(|i| i.parent())).collect();
            let gone: Vec<Ref> = self.rig_joints.keys().copied().filter(|m| !live.contains(m)).collect();
            for m in gone {
                drop_rig(self, m);
            }
        }
    }

    /// Server-owned rigs as the client sees them: each frame its own
    /// simulation poses them from their Motor6D.Transform (what the
    /// Animator wrote, replicated), outward from the root part the server
    /// moves. Studio animates NPCs on every peer; without this a server
    /// NPC's limbs stayed in the rest pose on the client.
    fn pose_rigs_with(&mut self, cd: &mut Document, include_rig: impl Fn(Ref, Ref) -> bool) {
        if self.controllers_dirty || self.frame % 60 == 0 || cd.id != self.doc_id {
            // The controllers in document order move only by a tree change
            // that has one in it (`note_rig_trees`); otherwise only the roots
            // are looked up again (a full walk of a large test place's client Workspace
            // every 60 frames was ~2% of the Play step).
            if self.controllers_dirty || cd.id != self.doc_id {
                self.controllers = rig_controllers(cd);
                self.controllers_dirty = false;
            } else if check_kept() {
                let live: Vec<Ref> = self.controllers.iter().copied().filter(|c| cd.dom.get_by_ref(*c).is_some()).collect();
                if live != rig_controllers(cd) {
                    rustudio_core::bench::add("cj_check_controllers", 0.0, 1);
                    eprintln!("CJ_CHECK rig controllers differ from a walk at frame {}", self.frame);
                }
            }
            self.rigs = rig_roots(cd, &self.controllers);
            if self.tracks.len() > self.rigs.len() * 2 + 64 {
                self.tracks.clear();
            }
        }
        self.frame += 1;
        let ident = Xform::from_cframe(&rbx_dom_weak::types::CFrame::identity());
        let mut scratch = std::mem::take(&mut self.pose_scratch);
        let rigs = std::mem::take(&mut self.rigs);
        let (mut kept_count, mut joint_count) = (0u64, 0u64);
        let check = check_kept();
        for &root in &rigs {
            let Some(model) = cd.dom.get_by_ref(root).map(|i| i.parent()) else {
                continue;
            };
            if !include_rig(root, model) {
                continue;
            }
            // (A root left unparented when its character was destroyed:
            // the rig list is refreshed only every 60 frames.)
            if cd.dom.get_by_ref(model).is_none() {
                continue;
            }
            let Some(at) = cframe(cd, root, ukey!("CFrame")) else { continue };
            let kept = self.posed.get(&model) == Some(&root);
            if kept {
                kept_count += 1;
                if let Some(t) = self.tracks.get_mut(&model) {
                    t.dirty_streak = 0;
                }
                if !check {
                    continue;
                }
            } else {
                self.unpose(model);
            }
            let track = !kept && {
                let frame = self.frame;
                let t = self.tracks.entry(model).or_default();
                if frame < t.untracked_until {
                    false
                } else {
                    if t.tracked {
                        t.dirty_streak = t.dirty_streak.saturating_add(1);
                    }
                    if t.dirty_streak >= 4 {
                        *t = RigTrack { tracked: false, dirty_streak: 0, untracked_until: frame + 64 };
                        false
                    } else {
                        t.tracked = true;
                        true
                    }
                }
            };
            scratch.joints.clear();
            if let Some(list) = self.rig_joints.get(&model) {
                scratch.joints.extend_from_slice(list);
            } else {
                let list: Vec<Ref> = cd
                    .dom
                    .descendants_of(model)
                    .filter(|i| joint_class(i.class.as_str()))
                    .map(|i| i.referent())
                    .collect();
                for &j in &list {
                    self.joint_rig.insert(j, model);
                }
                scratch.joints.extend_from_slice(&list);
                self.rig_joints.insert(model, list);
            }
            scratch.edges.clear();
            joint_count += scratch.joints.len() as u64;
            let mut reads: Vec<Ref> = Vec::with_capacity(if track { scratch.joints.len() * 3 + 1 } else { 0 });
            if track {
                reads.push(root);
            }
            for &j in &scratch.joints {
                if track {
                    reads.push(j);
                }
                let Some(joint) = cd.dom.get_by_ref(j) else { continue };
                if track {
                    for k in [ukey!("Part0"), ukey!("Part1"), ukey!("Attachment0"), ukey!("Attachment1")] {
                        if let Some(Variant::Ref(r)) = joint.properties.get(&k) {
                            if r.is_some() {
                                reads.push(*r);
                            }
                        }
                    }
                }
                let Some((p0, p1, c0, c1)) = self.frames(cd, joint) else {
                    continue;
                };
                if track {
                    reads.extend([p0, p1]);
                }
                let t = cframe_for(joint, ukey!("Transform")).unwrap_or(ident);
                scratch.edges.push(RigEdge {
                    parts: [p0, p1],
                    frames: [compose(&c0, &t), c1],
                    next: [usize::MAX; 2],
                });
            }
            scratch.heads.clear();
            for (edge_index, edge) in scratch.edges.iter_mut().enumerate().rev() {
                // Encoding the side in the link also handles joints authored
                // in reverse order. Keep one link for a self-joint.
                for side in 0..=usize::from(edge.parts[0] != edge.parts[1]) {
                    edge.next[side] = scratch.heads
                        .insert(edge.parts[side], edge_index * 2 + side)
                        .unwrap_or(usize::MAX);
                }
            }
            if track {
                for &r in &reads {
                    self.read_by.entry(r).or_default().push(model);
                }
                self.reads_of.insert(model, reads);
                self.posed.insert(model, root);
            }
            scratch.placed.clear();
            scratch.placed.insert(root);
            scratch.queue.push_back((root, at));
            while let Some((p, at)) = scratch.queue.pop_front() {
                let mut link = *scratch.heads.get(&p).unwrap_or(&usize::MAX);
                while link != usize::MAX {
                    let edge = &scratch.edges[link / 2];
                    let side = link % 2;
                    link = edge.next[side];
                    let other_side = 1 - side;
                    let other = edge.parts[other_side];
                    // Anchored parts stay anchored for the whole read-only
                    // traversal, so an already tested endpoint needs no revisit.
                    if !scratch.placed.insert(other) || anchored(cd, other) {
                        continue;
                    }
                    let xf = compose(&compose(&at, &edge.frames[side]), &edge.frames[other_side].inverse());
                    scratch.queue.push_back((other, xf));
                    scratch.writes.push((other, xf));
                    if check {
                        scratch.kept.push(kept);
                    }
                }
            }
        }
        rustudio_core::bench::add("cj_rigs_kept", 0.0, kept_count);
        rustudio_core::bench::add("cj_rig_joints", 0.0, joint_count);
        rustudio_core::bench::add("cj_rig_writes", 0.0, scratch.writes.len() as u64);
        let mut kept = std::mem::take(&mut scratch.kept).into_iter();
        for (p, xf) in scratch.writes.drain(..) {
            let cframe = ukey!("CFrame");
            let part = cd.dom.get_by_ref_mut(p);
            let same = part.as_deref().and_then(|i| cframe_for(i, cframe)).is_some_and(|o| {
                (o.pos - xf.pos).length() < 1e-4 && o.rot.abs_diff_eq(xf.rot, 1e-5)
            });
            if check && kept.next() == Some(true) {
                if !same {
                    rustudio_core::bench::add("cj_check_moved", 0.0, 1);
                    let name = cd.dom.get_by_ref(p).map(|i| i.name.clone()).unwrap_or_default();
                    eprintln!("CJ_CHECK kept rig part would move: {name} {p:?} frame {}", self.frame);
                }
                continue;
            }
            if same {
                continue;
            }
            if let Some(inst) = part {
                inst.properties.insert(cframe, Variant::CFrame(xf.to_cframe()));
            }
            cd.mark_prop(p, cframe);
        }
        self.pose_scratch = scratch;
        self.rigs = rigs;
    }
}

/// Root parts of the animated rigs under Workspace.
fn find_rigs(cd: &Document) -> Vec<Ref> {
    rig_roots(cd, &rig_controllers(cd))
}

fn is_rig_controller(class: &str) -> bool {
    class == "Humanoid" || class == "AnimationController"
}

/// The Humanoids and AnimationControllers under Workspace, in document order.
fn rig_controllers(cd: &Document) -> Vec<Ref> {
    let Some(ws) = cd.find_child(cd.dom.root_ref(), "Workspace") else {
        return Vec::new();
    };
    cd.dom.descendants_of(ws).filter(|i| is_rig_controller(i.class.as_str())).map(|i| i.referent()).collect()
}

/// Each controller's rig root (its model's HumanoidRootPart child, else the
/// model's PrimaryPart), once each, in the controllers' order.
fn rig_roots(cd: &Document, controllers: &[Ref]) -> Vec<Ref> {
    let mut out = Vec::new();
    for &c in controllers {
        let Some(i) = cd.dom.get_by_ref(c) else { continue };
        let model = i.parent();
        let root = cd.find_named(model, "HumanoidRootPart").filter(|r| {
            cd.dom.get_by_ref(*r).is_some_and(|p| p.parent() == model)
        });
        let root = root.or_else(|| part_ref(cd, model, ukey!("PrimaryPart")));
        if let Some(r) = root {
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use rbx_dom_weak::types::{CFrame, Matrix3, Vector3};
    use rbx_dom_weak::{ustr, InstanceBuilder};

    fn at(x: f32, y: f32, z: f32) -> CFrame {
        CFrame::new(Vector3::new(x, y, z), Matrix3::identity())
    }

    /// A Laser Tag view model: anchored Root → Motor6D (animated) → Body,
    /// with a Trim held to Body by a WeldConstraint and a Sight by a
    /// RigidConstraint. Both follow Body's animated pose.
    #[test]
    fn rigid_constraints_follow_an_animated_part() {
        let mut d = Document::new_place();
        let ws = d.find_child(d.dom.root_ref(), "Workspace").unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model"));
        let root = d.dom.insert(
            model,
            InstanceBuilder::new("Part").with_property("Anchored", true).with_property("CFrame", at(0.0, 10.0, 0.0)),
        );
        let body = d.dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 10.0, -1.0)));
        let trim = d.dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 11.0, -1.0)));
        let sight = d.dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 0.0, 0.0)));
        // Transform: 90° about X, so Body's +Y (where Trim sits) turns to +Z.
        let quarter = CFrame::new(
            Vector3::new(0.0, 0.0, 0.0),
            Matrix3 { x: Vector3::new(1.0, 0.0, 0.0), y: Vector3::new(0.0, 0.0, -1.0), z: Vector3::new(0.0, 1.0, 0.0) },
        );
        d.dom.insert(
            body,
            InstanceBuilder::new("Motor6D")
                .with_property("Part0", root)
                .with_property("Part1", body)
                .with_property("C0", at(0.0, 0.0, -1.0))
                .with_property("C1", at(0.0, 0.0, 0.0))
                .with_property("Transform", quarter),
        );
        d.dom.insert(
            trim,
            InstanceBuilder::new("WeldConstraint").with_property("Part0", body).with_property("Part1", trim),
        );
        let a0 = d.dom.insert(body, InstanceBuilder::new("Attachment").with_property("CFrame", at(0.0, 2.0, 0.0)));
        let a1 = d.dom.insert(sight, InstanceBuilder::new("Attachment").with_property("CFrame", at(0.0, 0.0, 0.0)));
        d.dom.insert(
            sight,
            InstanceBuilder::new("RigidConstraint").with_property("Attachment0", a0).with_property("Attachment1", a1),
        );
        d.mark_all();
        let mut cj = ClientJoints::default();
        cj.solve(&mut d, &Default::default());
        let pos = |p| cframe(&d, p, ukey!("CFrame")).unwrap().pos;
        assert!((pos(body) - Vec3::new(0.0, 10.0, -1.0)).length() < 1e-4, "{:?}", pos(body));
        // Body's local +Y is world +Z after the quarter turn.
        assert!((pos(trim) - Vec3::new(0.0, 10.0, 0.0)).length() < 1e-4, "{:?}", pos(trim));
        assert!((pos(sight) - Vec3::new(0.0, 10.0, 1.0)).length() < 1e-4, "{:?}", pos(sight));
    }

    #[test]
    fn replicated_rig_follows_joint_transform_changes() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model"));
        d.dom.insert(model, InstanceBuilder::new("Humanoid"));
        let root = d.dom.insert(
            model,
            InstanceBuilder::new("Part")
                .with_name("HumanoidRootPart")
                .with_property("CFrame", at(0.0, 5.0, 0.0)),
        );
        let limb = d.dom.insert(model, InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 0.0, 0.0)));
        let joint = d.dom.insert(
            limb,
            InstanceBuilder::new("Motor6D")
                .with_property("Part0", limb)
                .with_property("Part1", root)
                .with_property("Transform", at(-1.0, 0.0, 0.0)),
        );
        let replicated = [root, limb, joint].into_iter().map(|r| (r, r)).collect();
        d.mark_all();
        let mut joints = ClientJoints::default();
        joints.solve(&mut d, &replicated);
        assert_eq!(cframe(&d, limb, ukey!("CFrame")).unwrap().pos, Vec3::new(1.0, 5.0, 0.0));

        d.dom.get_by_ref_mut(joint).unwrap().properties.insert(ustr("Transform"), Variant::CFrame(at(-2.0, 0.0, 0.0)));
        d.mark_prop(joint, ustr("Transform"));
        joints.solve(&mut d, &replicated);
        assert_eq!(cframe(&d, limb, ukey!("CFrame")).unwrap().pos, Vec3::new(2.0, 5.0, 0.0));
    }

    /// A client script's aura part, welded to the character's (server-owned,
    /// unanchored) limb from a folder outside the character: it rides with
    /// the limb as the server moves it, like a massless part welded into a
    /// server assembly in Studio. The limb itself is never moved.
    #[test]
    fn client_parts_welded_to_a_server_part_ride_with_it() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let character = d.dom.insert(ws, InstanceBuilder::new("Model"));
        let limb = d.dom.insert(character, InstanceBuilder::new("Part").with_property("CFrame", at(5.0, 3.0, 0.0)));
        let vfx = d.dom.insert(ws, InstanceBuilder::new("Folder"));
        let aura = d.dom.insert(vfx, InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 0.0, 0.0)));
        d.dom.insert(
            aura,
            InstanceBuilder::new("Weld")
                .with_property("Part0", limb)
                .with_property("Part1", aura)
                .with_property("C0", at(0.0, 2.0, 0.0))
                .with_property("C1", at(0.0, 0.0, 0.0)),
        );
        let replicated: rustudio_core::RefMap = [(character, character), (limb, limb)].into_iter().collect();
        d.mark_all();
        let mut cj = ClientJoints::default();
        cj.solve(&mut d, &replicated);
        let pos = |d: &Document, p| cframe(d, p, ukey!("CFrame")).unwrap().pos;
        assert!((pos(&d, aura) - Vec3::new(5.0, 5.0, 0.0)).length() < 1e-4, "{:?}", pos(&d, aura));
        assert_eq!(pos(&d, limb), Vec3::new(5.0, 3.0, 0.0));
        // The server moved the limb (replication wrote its CFrame).
        d.dom.get_by_ref_mut(limb).unwrap().properties.insert(ustr("CFrame"), Variant::CFrame(at(25.0, 3.0, -4.0)));
        d.mark_prop(limb, ustr("CFrame"));
        cj.solve(&mut d, &replicated);
        assert!((pos(&d, aura) - Vec3::new(25.0, 5.0, -4.0)).length() < 1e-4, "{:?}", pos(&d, aura));
    }

    /// An anchored root moved by script carries a client-made limb with it.
    #[test]
    fn limbs_follow_an_anchored_root() {
        let mut d = Document::new_place();
        let root = d.dom.root_ref();
        let ws = d.find_child(root, "Workspace").unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model"));
        let hrp = d.dom.insert(
            model,
            InstanceBuilder::new("Part")
                .with_property("Anchored", true)
                .with_property("CFrame", at(10.0, 3.0, 9000.0)),
        );
        let torso = d.dom.insert(
            model,
            InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 0.0, 0.0)),
        );
        let arm = d.dom.insert(
            model,
            InstanceBuilder::new("Part").with_property("CFrame", at(0.0, 0.0, 0.0)),
        );
        d.dom.insert(
            torso,
            InstanceBuilder::new("Motor6D")
                .with_property("Part0", hrp)
                .with_property("Part1", torso)
                .with_property("C0", at(0.0, 1.0, 0.0))
                .with_property("C1", at(0.0, 0.0, 0.0)),
        );
        // Joined the other way round: the limb is Part0.
        d.dom.insert(
            arm,
            InstanceBuilder::new("Weld")
                .with_property("Part0", arm)
                .with_property("Part1", torso)
                .with_property("C0", at(0.0, 0.5, 0.0))
                .with_property("C1", at(1.5, 0.0, 0.0)),
        );
        d.mark_all();
        let mut cj = ClientJoints::default();
        cj.solve(&mut d, &Default::default());
        let pos = |p| cframe(&d, p, ukey!("CFrame")).unwrap().pos;
        assert!((pos(torso) - Vec3::new(10.0, 4.0, 9000.0)).length() < 1e-4);
        assert!((pos(arm) - Vec3::new(11.5, 3.5, 9000.0)).length() < 1e-4);
        assert!((pos(hrp) - Vec3::new(10.0, 3.0, 9000.0)).length() < 1e-4);
    }
}
