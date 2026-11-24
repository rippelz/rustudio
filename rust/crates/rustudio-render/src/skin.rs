//! Skinned MeshParts: the mesh's bones and weights (mesh.rs `Skin`) driven
//! by Bone instances, as Studio 0.747 skins them (measured with marker
//! captures, checks/mesh-deform/AUDIT.md):
//! - Each mesh bone is matched by name with a Bone under the MeshPart, else
//!   under another part of its assembly (jointed: Weld, WeldConstraint,
//!   Motor6D, …; an importer's "Bones under the root part" rig). Its current
//!   frame is the Bone's TransformedWorldCFrame (CFrame × Transform up the
//!   chain); a mesh bone with no Bone follows its parent bone rigidly.
//! - Its bind frame is the mesh's own bone frame, centred like the vertices,
//!   position scaled by Size / InitialSize, rotation as is.
//! - Linear blend: each vertex moves by Σ wᵢ (currentᵢ × bindᵢ⁻¹), with the
//!   file's u8 weights / 255 NOT renormalised, blended in world space, so
//!   weight missing from 255 pulls the vertex toward the world origin.
//! Deformed on the GPU (skin_gpu.rs) when a part's bones move; a skinned
//! mesh whose bones are all at rest (and whose weights are whole) draws the
//! shared mesh untouched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use glam::{Mat3, Vec2, Vec3, Vec4};
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::doc::Document;
use rustudio_core::math::Xform;

/// A skinned mesh in the renderer's normalized space (unit box), with what
/// is needed to deform it.
pub struct Base {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
    pub colors: Vec<Vec4>,
    pub extent: Vec3,
    pub center: Vec3,
    pub skin: rustudio_core::mesh::Skin,
    /// Unique vertices: the first corner of each, and each corner's vertex.
    pub firsts: Vec<u32>,
    pub corner_unique: Vec<u32>,
    /// The mesh's FACS rig (dynamic heads), posed by a FaceControls child.
    pub facs: Option<rustudio_core::mesh::Facs>,
    /// Every corner's weights sum to 255/255 (else even the rest pose
    /// moves: Studio pulls the missing weight toward the world origin).
    pub whole: bool,
}

/// Parsed skinned meshes by content id (None: not skinned / unreadable).
static BASES: std::sync::LazyLock<Mutex<HashMap<String, Option<Arc<Base>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The skinned base of a mesh, parsed once from its bytes.
pub fn base(id: &str, bytes: &[u8]) -> Option<Arc<Base>> {
    if !rustudio_core::assets::place_content_ok(id) {
        return None;
    }
    if let Some(b) = BASES.lock().ok()?.get(id) {
        return b.clone();
    }
    let b = rustudio_core::mesh::parse(bytes).ok().and_then(|m| {
        let skin = m.skin.clone()?;
        if skin.corner_bones.len() != m.positions.len() || skin.bones.is_empty() {
            return None;
        }
        let extent = (m.max - m.min).max(Vec3::splat(1e-4));
        let center = (m.max + m.min) * 0.5;
        let colors = m.gpu_colors();
        let (firsts, corner_unique) = uniques(&skin.corner_vertex, m.positions.len());
        let whole = skin.corner_weights.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-3);
        Some(Arc::new(Base {
            whole,
            firsts,
            corner_unique,
            positions: m.positions.iter().map(|p| (*p - center) / extent).collect(),
            normals: m.normals.iter().map(|n| *n * extent).collect(),
            uvs: m.uvs,
            colors,
            extent,
            center,
            skin,
            facs: rustudio_core::mesh::parse_mesh_facs(bytes),
        }))
    });
    if let Ok(mut g) = BASES.lock() {
        g.insert(id.to_string(), b.clone());
    }
    rustudio_core::assets::bump_mesh_generation();
    b
}

/// The skinned base of a mesh already parsed by `base`, without its bytes.
/// Drops the skin base of `id` (its bytes changed: a live EditableMesh).
pub fn forget_base(id: &str) {
    if let Ok(mut b) = BASES.lock() {
        b.remove(id);
    }
    rustudio_core::assets::bump_mesh_generation();
    rig_changed();
}

/// A live skinned EditableMesh's colour / UV edits written into its parsed
/// base in place (corner of the mesh file, value; colours RGBA 0..1): the
/// base as it now is, or None when there is nothing to patch in place (not
/// parsed yet, held elsewhere right now, no vertex colours in it, or the
/// corners don't line up) and the caller rebuilds from fresh bytes.
pub fn patch_base(id: &str, colors: &[(u32, [f32; 4])], uvs: &[(u32, [f32; 2])]) -> Option<Arc<Base>> {
    let mut g = BASES.lock().ok()?;
    let arc = g.get_mut(id)?.as_mut()?;
    let b = Arc::get_mut(arc)?;
    let n = b.positions.len();
    // (corner i of the file is corner i here: one file vertex per corner)
    let lined = |i: u32| (i as usize) < n && b.skin.corner_vertex.get(i as usize) == Some(&i);
    if (!colors.is_empty() && b.colors.len() != n) || (!uvs.is_empty() && b.uvs.len() != n) {
        return None;
    }
    if !colors.iter().all(|c| lined(c.0)) || !uvs.iter().all(|u| lined(u.0)) {
        return None;
    }
    for (i, c) in colors {
        // (`MeshData::gpu_colors`: a vertex colour tint, alpha −1 − a)
        b.colors[*i as usize] = Vec4::new(c[0], c[1], c[2], -1.0 - c[3]);
    }
    for (i, u) in uvs {
        b.uvs[*i as usize] = Vec2::from_array(*u);
    }
    Some(arc.clone())
}

pub fn cached_base(id: &str) -> Option<Arc<Base>> {
    BASES.lock().ok()?.get(id).cloned().flatten()
}

/// A skinned part's triangles as drawn this frame (unit box, like
/// assets::mesh_triangles), or None at rest / not skinned.
pub fn posed_triangles(doc: &Document, part: Ref, id: &str) -> Option<Vec<[Vec3; 3]>> {
    let base = cached_base(id)?;
    if !lookup(doc, part, &base).any {
        return None;
    }
    let (mats, scale, origin) = pose(doc, part, &base)?;
    let (p, _) = deform(&base, &mats, scale, origin);
    Some(p.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect())
}

/// Groups corners by mesh vertex: (first corner of each vertex, corner →
/// vertex slot). Without vertex ids every corner is its own vertex.
fn uniques(corner_vertex: &[u32], corners: usize) -> (Vec<u32>, Vec<u32>) {
    if corner_vertex.len() != corners {
        return ((0..corners as u32).collect(), (0..corners as u32).collect());
    }
    let mut slot: HashMap<u32, u32> = HashMap::new();
    let mut firsts = Vec::new();
    let mut map = Vec::with_capacity(corners);
    for (c, v) in corner_vertex.iter().enumerate() {
        let s = *slot.entry(*v).or_insert_with(|| {
            firsts.push(c as u32);
            firsts.len() as u32 - 1
        });
        map.push(s);
    }
    (firsts, map)
}

/// The parts whose Bones skin `part`: itself, then the parts jointed to it
/// (its assembly: Studio finds an importer rig's Bones under the root part).
pub fn bone_hosts(doc: &Document, part: Ref) -> Vec<Ref> {
    let mut hosts = vec![part];
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()).filter(|m| {
        doc.class_of(*m).is_some_and(|c| rustudio_core::doc::is_a(&c, "Model"))
    }) else {
        return hosts;
    };
    let mut edges = Vec::new();
    for i in doc.dom.descendants_of(model) {
        if rustudio_core::body::is_rigid_joint(&i.class) && rustudio_core::body::joint_enabled(doc, i.referent()) {
            if let Some(e) = rustudio_core::body::joint_parts(doc, i.referent()) {
                edges.push(e);
            }
        }
    }
    let mut at = 0;
    while at < hosts.len() {
        let h = hosts[at];
        for (a, b) in &edges {
            for (x, y) in [(*a, *b), (*b, *a)] {
                if x == h && !hosts.contains(&y) {
                    hosts.push(y);
                }
            }
        }
        at += 1;
    }
    hosts
}

/// Whether a part is skinned by Bones (its own or its assembly's) or has a
/// FaceControls child (only those parts are skinned).
/// `has_bones` from the cached bone lookup once the mesh is parsed (the
/// assembly walk then runs only when the rig changes).
pub fn part_has_bones(doc: &Document, part: Ref, mesh: &str) -> bool {
    match cached_base(mesh) {
        Some(base) => lookup(doc, part, &base).any,
        None => has_bones(doc, part),
    }
}

pub fn has_bones(doc: &Document, part: Ref) -> bool {
    let gen = RIG_GEN.load(std::sync::atomic::Ordering::Relaxed);
    let mut all = RIG_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    if all.len() > 8 && !all.contains_key(&doc.id) {
        all.clear();
    }
    let index = all.entry(doc.id).or_default();
    if index.gen != gen {
        *index = RigIndex::build(doc, gen);
    }
    // No Bone or FaceControls anywhere (most places): nothing to walk. The
    // assembly walk below ran per mesh part on every redo of the draws,
    // seconds a frame in a place with thousands of jointed mesh parts.
    if index.bone_above.is_empty() && index.face_above.is_empty() {
        return false;
    }
    if index.face_above.contains(&part) || index.bone_above.contains(&part) {
        return true;
    }
    if index.bone_above.is_empty() {
        return false;
    }
    // A Bone under another part of its assembly (`bone_hosts`): the joints
    // under its model, grouped once per model.
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()).filter(|m| {
        doc.class_of(*m).is_some_and(|c| rustudio_core::doc::is_a(&c, "Model"))
    }) else {
        return false;
    };
    if !index.models.contains_key(&model) {
        let rig = ModelRig::build(doc, model, &index.bone_above);
        index.models.insert(model, rig);
    }
    let rig = &index.models[&model];
    rig.group.get(&part).is_some_and(|g| rig.with_bones[*g as usize])
}

/// Whether `part` is jointed to at least one other part inside a Model
/// (candidate for S15 / BasePart-driven skinning when its mesh has skin data).
pub fn part_in_jointed_model(doc: &Document, part: Ref) -> bool {
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()).filter(|m| {
        doc.class_of(*m).is_some_and(|c| rustudio_core::doc::is_a(&c, "Model"))
    }) else {
        return false;
    };
    let gen = RIG_GEN.load(std::sync::atomic::Ordering::Relaxed);
    let mut all = RIG_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    if all.len() > 8 && !all.contains_key(&doc.id) {
        all.clear();
    }
    let index = all.entry(doc.id).or_default();
    if index.gen != gen {
        *index = RigIndex::build(doc, gen);
    }
    if !index.models.contains_key(&model) {
        let rig = ModelRig::build(doc, model, &index.bone_above);
        index.models.insert(model, rig);
    }
    index.models[&model].group.contains_key(&part)
}

/// Rest-pose CFrame of every BasePart in `part`'s jointed assembly, relative
/// to `part` (`rest[part] = Identity`, `rest[B] = P_rest⁻¹ × B_rest`),
/// computed from the assembly's enabled rigid joints (Motor6D / Weld /
/// RigidConstraint / WeldConstraint / AnimationConstraint) with joint
/// `Transform = Identity` — matching Studio's `Skeleton::computeTransforms`
/// rest pose and `FastCluster` BasePart skinning.
fn assembly_rest_poses(doc: &Document, part: Ref) -> HashMap<Ref, Xform> {
    let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
    let mut rest = HashMap::new();
    rest.insert(part, ident);
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()).filter(|m| {
        doc.class_of(*m).is_some_and(|c| rustudio_core::doc::is_a(&c, "Model"))
    }) else {
        return rest;
    };
    let att_local = |a: Ref, p: Ref| -> Xform {
        let mut chain = Vec::new();
        let mut cur = a;
        while cur != p {
            let Some(i) = doc.dom.get_by_ref(cur) else { break };
            if rustudio_core::doc::is_a(&i.class, "Attachment") {
                chain.push(cf(doc, cur, "CFrame"));
            }
            cur = i.parent();
        }
        let mut out = ident;
        for step in chain.into_iter().rev() {
            out = mul(&out, &step);
        }
        out
    };
    let mut adj: HashMap<Ref, Vec<(Ref, Xform)>> = HashMap::new();
    for i in doc.dom.descendants_of(model) {
        let j = i.referent();
        if !rustudio_core::body::is_rigid_joint(&i.class)
            || !rustudio_core::body::joint_enabled(doc, j)
        {
            continue;
        }
        let edge = if i.class == "WeldConstraint" {
            rustudio_core::body::joint_parts(doc, j).map(|(p0, p1)| {
                let rel = mul(&inv(&cf(doc, p0, "CFrame")), &cf(doc, p1, "CFrame"));
                (p0, p1, rel)
            })
        } else if i.class == "RigidConstraint" {
            let pr = |k: &str| match doc.prop(j, k) {
                Some(Variant::Ref(x)) if x.is_some() => Some(x),
                _ => None,
            };
            pr("Attachment0").zip(pr("Attachment1")).and_then(|(a0, a1)| {
                let p0 = rustudio_core::body::attachment_part(doc, a0)?;
                let p1 = rustudio_core::body::attachment_part(doc, a1)?;
                let rel = mul(&att_local(a0, p0), &inv(&att_local(a1, p1)));
                Some((p0, p1, rel))
            })
        } else {
            rustudio_core::body::joint_frames(doc, j)
                .map(|(p0, p1, c0, c1)| (p0, p1, mul(&c0, &inv(&c1))))
        };
        if let Some((p0, p1, rel)) = edge {
            if p0 != p1 {
                adj.entry(p0).or_default().push((p1, rel));
                adj.entry(p1).or_default().push((p0, inv(&rel)));
            }
        }
    }
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(part);
    while let Some(cur) = queue.pop_front() {
        let cur_xf = rest[&cur];
        if let Some(neighbors) = adj.get(&cur) {
            for (next, step) in neighbors {
                if !rest.contains_key(next) {
                    rest.insert(*next, mul(&cur_xf, step));
                    queue.push_back(*next);
                }
            }
        }
    }
    rest
}

/// Where a document's Bones and FaceControls are (for `has_bones`), valid
/// until the rig changes, as the bone lookups are.
#[derive(Default)]
struct RigIndex {
    gen: u64,
    /// Every instance with a Bone / a FaceControls below it.
    bone_above: std::collections::HashSet<Ref>,
    face_above: std::collections::HashSet<Ref>,
    models: HashMap<Ref, ModelRig>,
}

/// The rigid joints under one model as groups of jointed parts (what
/// `bone_hosts` walks, for every part of the model at once).
struct ModelRig {
    group: HashMap<Ref, u32>,
    /// Per group: some part of it has a Bone below it.
    with_bones: Vec<bool>,
}

impl ModelRig {
    fn build(doc: &Document, model: Ref, bone_above: &std::collections::HashSet<Ref>) -> Self {
        let mut slot: HashMap<Ref, u32> = HashMap::new();
        let mut parent: Vec<u32> = Vec::new();
        fn find(parent: &mut [u32], mut i: u32) -> u32 {
            while parent[i as usize] != i {
                parent[i as usize] = parent[parent[i as usize] as usize];
                i = parent[i as usize];
            }
            i
        }
        for i in doc.dom.descendants_of(model) {
            if rustudio_core::body::is_rigid_joint(&i.class) && rustudio_core::body::joint_enabled(doc, i.referent()) {
                if let Some((a, b)) = rustudio_core::body::joint_parts(doc, i.referent()) {
                    let mut at = |r: Ref| {
                        *slot.entry(r).or_insert_with(|| {
                            parent.push(parent.len() as u32);
                            parent.len() as u32 - 1
                        })
                    };
                    let (a, b) = (at(a), at(b));
                    let (a, b) = (find(&mut parent, a), find(&mut parent, b));
                    parent[a as usize] = b;
                }
            }
        }
        let mut with_bones = vec![false; parent.len()];
        let mut group = HashMap::with_capacity(slot.len());
        for (r, i) in slot {
            let g = find(&mut parent, i);
            group.insert(r, g);
            with_bones[g as usize] |= bone_above.contains(&r);
        }
        ModelRig { group, with_bones }
    }
}

impl RigIndex {
    fn build(doc: &Document, gen: u64) -> Self {
        let mut index = RigIndex { gen, ..Default::default() };
        for i in doc.dom.descendants() {
            let set = match i.class.as_str() {
                "Bone" => &mut index.bone_above,
                "FaceControls" => &mut index.face_above,
                _ => continue,
            };
            let mut cur = i.parent();
            while let Some(p) = doc.dom.get_by_ref(cur) {
                if !set.insert(cur) {
                    break;
                }
                cur = p.parent();
            }
        }
        index
    }
}

static RIG_INDEX: std::sync::LazyLock<Mutex<HashMap<u64, RigIndex>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// What a skinned part redraws on: its FaceControls and the Bones that
/// skin it. With its mesh's skeleton known (`mesh`'s base parsed): only the
/// Bones its mesh bones resolve to and the Bones / Attachments above them
/// (a Bone the mesh doesn't use moving doesn't redraw it); else every Bone
/// of its assembly.
pub fn deps_of(doc: &Document, part: Ref, mesh: Option<&str>) -> Vec<Ref> {
    if let Some(base) = mesh.and_then(cached_base) {
        return lookup(doc, part, &base).deps.clone();
    }
    // No FaceControls below the part and no Bone in its jointed assembly
    // (has_bones, from the rig index built once per model): nothing to
    // find. Walking the whole model for joints for every mesh part made
    // opening a large test place (50k parts) spend 10 s here.
    if !has_bones(doc, part) {
        return Vec::new();
    }
    let mut out = face_controls(doc, part);
    out.extend(bones_of(doc, part));
    out
}

fn face_controls(doc: &Document, part: Ref) -> Vec<Ref> {
    doc.dom.descendants_of(part).filter(|i| i.class == "FaceControls").map(|i| i.referent()).collect()
}

/// Bumped when Bones, rigid joints or FaceControls are added, removed,
/// renamed or re-pointed (scene.rs sees the marks): cached bone lookups
/// are rebuilt after it.
static RIG_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn rig_changed() {
    RIG_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Whether instances of this class change which Bones skin a part.
pub fn rig_class(class: &str) -> bool {
    matches!(class, "Bone" | "FaceControls") || rustudio_core::body::is_rigid_joint(class)
}

/// What drives one mesh bone: a Bone instance, or a BasePart in the same
/// jointed assembly (S15 / R15 skinned body parts: Studio's
/// `ModelTransformSourceBasePart` / `FastCluster`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Bone(Ref),
    Part(Ref),
}

/// A part's mesh bones resolved to Bone or jointed BasePart instances (by
/// mesh bone index), and what it redraws on.
pub struct Lookup {
    gen: u64,
    doc: u64,
    pub bones: Vec<Option<Target>>,
    /// For each `Target::Part(B)`: `B_rest⁻¹ × P_rest` (precomputed from the
    /// assembly's joint rest pose, cached until `RIG_GEN` changes).
    pub part_rest: Vec<Option<Xform>>,
    pub deps: Vec<Ref>,
    /// The part is skinned: Bones (its own or its assembly's), FaceControls,
    /// or jointed BaseParts (S15 / R15).
    pub any: bool,
}

static LOOKUPS: std::sync::LazyLock<Mutex<HashMap<Ref, (usize, Arc<Lookup>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The part's bone lookup for this mesh (cached until the rig changes):
/// mesh bone name → the part's own Bone of that name, else a Bone of its
/// assembly, else a jointed BasePart of its assembly (S15 / R15).
pub fn lookup(doc: &Document, part: Ref, base: &Base) -> Arc<Lookup> {
    let gen = RIG_GEN.load(std::sync::atomic::Ordering::Relaxed);
    let key = base as *const Base as usize;
    if let Some((k, l)) = LOOKUPS.lock().ok().and_then(|m| m.get(&part).cloned()) {
        if k == key && l.gen == gen && l.doc == doc.id && l.bones.len() == base.skin.bones.len() {
            return l;
        }
    }
    let _s = rustudio_core::bench::scope("skin_lookup");
    let mut by_name: HashMap<&str, Ref> = HashMap::new();
    fn own<'d>(doc: &'d Document, h: Ref, by_name: &mut HashMap<&'d str, Ref>) {
        for i in doc.dom.descendants_of(h) {
            if i.class == "Bone" {
                by_name.entry(i.name.as_str()).or_insert(i.referent());
            }
        }
    }
    own(doc, part, &mut by_name);
    // Walk the assembly's joints only when some mesh bone has no Bone on
    // `part` itself (the BFS over the model's joints is the costly part).
    let mut rest_map: HashMap<Ref, Xform> = HashMap::new();
    let mut part_by_name: HashMap<&str, Ref> = HashMap::new();
    if base.skin.bones.iter().any(|b| !by_name.contains_key(b.name.as_str())) {
        rest_map = assembly_rest_poses(doc, part);
        if rest_map.len() > 1 {
            if let Some(i) = doc.dom.get_by_ref(part) {
                part_by_name.insert(i.name.as_str(), part);
            }
            for &h in rest_map.keys() {
                if h == part {
                    continue;
                }
                own(doc, h, &mut by_name);
                if let Some(i) = doc.dom.get_by_ref(h) {
                    part_by_name.entry(i.name.as_str()).or_insert(h);
                }
            }
        }
    }
    let mut bones: Vec<Option<Target>> = Vec::with_capacity(base.skin.bones.len());
    let mut part_rest: Vec<Option<Xform>> = Vec::with_capacity(base.skin.bones.len());
    let mut has_other_part = false;
    for b in &base.skin.bones {
        if let Some(&r) = by_name.get(b.name.as_str()) {
            bones.push(Some(Target::Bone(r)));
            part_rest.push(None);
        } else if let Some(&p) = part_by_name.get(b.name.as_str()) {
            if p != part {
                has_other_part = true;
            }
            bones.push(Some(Target::Part(p)));
            part_rest.push(rest_map.get(&p).map(inv));
        } else {
            bones.push(None);
            part_rest.push(None);
        }
    }
    let mut deps = face_controls(doc, part);
    let any = !deps.is_empty() || !by_name.is_empty() || has_other_part || has_bones(doc, part);
    let mut seen: std::collections::HashSet<Ref> = deps.iter().copied().collect();
    for t in bones.iter().flatten() {
        match *t {
            Target::Bone(b) => {
                // The Bone and the Bones / Attachments above it (its frame's chain).
                let mut cur = b;
                while let Some(i) = doc.dom.get_by_ref(cur) {
                    if rustudio_core::doc::is_a(&i.class, "BasePart") || !seen.insert(cur) {
                        break;
                    }
                    if i.class == "Bone" || rustudio_core::doc::is_a(&i.class, "Attachment") {
                        deps.push(cur);
                    }
                    cur = i.parent();
                }
            }
            Target::Part(p) if has_other_part => {
                if seen.insert(p) {
                    deps.push(p);
                }
            }
            Target::Part(_) => {}
        }
    }
    // When another jointed BasePart drives this mesh, moving `part` itself
    // also changes `part_inv * other_part`, so `part` must re-skin on its own
    // CFrame change (otherwise scene.rs treats it as a rigid move).
    if has_other_part && seen.insert(part) {
        deps.push(part);
    }
    let l = Arc::new(Lookup { gen, doc: doc.id, bones, part_rest, deps, any });
    if let Ok(mut m) = LOOKUPS.lock() {
        m.insert(part, (key, l.clone()));
    }
    l
}

/// A destroyed / redrawn-without-skin part's cached lookups and last
/// palettes go.
pub fn forget_part(part: Ref) {
    if let Ok(mut m) = LOOKUPS.lock() {
        m.remove(&part);
    }
    if let Ok(mut m) = LAST.lock() {
        m.remove(&(part, 0));
        m.remove(&(part, R15_CLOTHING_TAG));
    }
}

/// FACS offsets (bone name → local frame) from the part's FaceControls.
fn facs_offsets(doc: &Document, part: Ref, base: &Base, scale: Vec3) -> HashMap<String, Xform> {
    let mut out = HashMap::new();
    let Some(f) = &base.facs else { return out };
    let Some(fc) = doc
        .dom
        .get_by_ref(part)
        .and_then(|i| i.children().iter().copied().find(|c| doc.class_of(*c).is_some_and(|k| k == "FaceControls")))
    else {
        return out;
    };
    let value = |c: &str| match doc.prop(fc, c) {
        Some(Variant::Float32(v)) => v,
        Some(Variant::Float64(v)) => v as f32,
        _ => 0.0,
    };
    for (name, p, r) in rustudio_core::mesh::pose(f, &value) {
        if p == [0.0; 3] && r == [0.0; 3] {
            continue;
        }
        let rot = Mat3::from_euler(glam::EulerRot::XYZ, r[0].to_radians(), r[1].to_radians(), r[2].to_radians());
        out.insert(name, Xform { pos: Vec3::from(p) * scale, rot });
    }
    out
}

/// The Bones that may skin a part: its own descendants first, then the
/// other parts of its assembly's (by name, the first one wins).
pub fn bones_of(doc: &Document, part: Ref) -> Vec<Ref> {
    bone_hosts(doc, part)
        .into_iter()
        .flat_map(|h| doc.dom.descendants_of(h).filter(|i| i.class == "Bone").map(|i| i.referent()).collect::<Vec<_>>())
        .collect()
}

fn cf(doc: &Document, r: Ref, k: &str) -> Xform {
    match doc.prop(r, k) {
        Some(Variant::CFrame(c)) => Xform::from_cframe(&c),
        _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
    }
}

fn mul(a: &Xform, b: &Xform) -> Xform {
    Xform { pos: a.pos + a.rot * b.pos, rot: a.rot * b.rot }
}

fn inv(a: &Xform) -> Xform {
    let rt = a.rot.transpose();
    Xform { pos: -(rt * a.pos), rot: rt }
}

/// TransformedWorldCFrames of Bones, memoised up the chain (one pass per
/// part per frame rather than a walk to the part for every Bone): the
/// parent's (Bone / Attachment: its transformed frame; BasePart: CFrame)
/// × CFrame × Transform, as attachment.rs composes them.
struct Frames<'a> {
    doc: &'a Document,
    memo: &'a mut rustc_hash::FxHashMap<Ref, Xform>,
}

/// Bone frames computed this frame, shared by every part that reads them
/// (a character's MeshParts over one skeleton evaluate it once), valid
/// while the document's revision holds.
struct WorldMemo {
    doc: u64,
    revision: u64,
    memo: rustc_hash::FxHashMap<Ref, Xform>,
}

static WORLD: std::sync::LazyLock<Mutex<WorldMemo>> =
    std::sync::LazyLock::new(|| Mutex::new(WorldMemo { doc: 0, revision: 0, memo: Default::default() }));

impl Frames<'_> {
    fn world(&mut self, r: Ref) -> Xform {
        if let Some(x) = self.memo.get(&r) {
            return *x;
        }
        let doc = self.doc;
        let id = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        let Some(i) = doc.dom.get_by_ref(r) else { return id };
        let mut local = cf(doc, r, "CFrame");
        if i.class == "Bone" {
            local = mul(&local, &cf(doc, r, "Transform"));
        }
        // The nearest Attachment / BasePart above (Models and folders skipped).
        let mut up = i.parent();
        let parent = loop {
            let Some(p) = doc.dom.get_by_ref(up) else { break id };
            if rustudio_core::doc::is_a(&p.class, "Attachment") {
                break self.world(up);
            }
            if rustudio_core::doc::is_a(&p.class, "BasePart") {
                break if p.class == "Terrain" { id } else { cf(doc, up, "CFrame") };
            }
            up = p.parent();
        };
        let w = mul(&parent, &local);
        self.memo.insert(r, w);
        w
    }
}

/// The skinning matrices (current × bind⁻¹, in part space), or None when
/// every bone is at its bind pose.
pub fn matrices(doc: &Document, part: Ref, base: &Base, scale: Vec3) -> Option<Vec<Xform>> {
    let look = lookup(doc, part, base);
    let mut world = WORLD.lock().unwrap_or_else(|e| e.into_inner());
    if world.doc != doc.id || world.revision != doc.revision {
        world.doc = doc.id;
        world.revision = doc.revision;
        world.memo.clear();
    }
    let mut frames = Frames { doc, memo: &mut world.memo };
    let n = base.skin.bones.len();
    let bind: Vec<Xform> = base
        .skin
        .bones
        .iter()
        .map(|b| Xform { pos: (b.pos - base.center) * scale, rot: b.rot })
        .collect();
    let bind_inv: Vec<Xform> = bind.iter().map(inv).collect();
    let facs = if base.facs.is_some() { facs_offsets(doc, part, base, scale) } else { HashMap::new() };
    let part_inv = inv(&cf(doc, part, "CFrame"));
    let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
    let mut mats: Vec<Option<Xform>> = vec![None; n];
    // Parents come before children in Roblox meshes; resolve in passes.
    for _ in 0..n.max(1) {
        let mut progress = false;
        for i in 0..n {
            if mats[i].is_some() {
                continue;
            }
            let sb = &base.skin.bones[i];
            match look.bones[i] {
                Some(Target::Bone(b)) => {
                    let now = mul(&part_inv, &frames.world(b));
                    mats[i] = Some(mul(&now, &bind_inv[i]));
                    progress = true;
                }
                Some(Target::Part(p)) => {
                    let m = if p == part {
                        ident
                    } else if let Some(pr) = look.part_rest[i] {
                        mul(&part_inv, &mul(&cf(doc, p, "CFrame"), &pr))
                    } else {
                        ident
                    };
                    mats[i] = Some(m);
                    progress = true;
                }
                None => {
                    // No Bone or BasePart: follows its parent rigidly, times
                    // any FaceControls FACS offset for this bone.
                    let local = facs.get(&sb.name).copied();
                    let apply_facs = |base_m: Xform| match local {
                        Some(l) => mul(&base_m, &mul(&bind[i], &mul(&l, &bind_inv[i]))),
                        None => base_m,
                    };
                    match sb.parent {
                        None => {
                            mats[i] = Some(apply_facs(ident));
                            progress = true;
                        }
                        Some(p) => {
                            if let Some(pm) = mats[p] {
                                mats[i] = Some(apply_facs(pm));
                                progress = true;
                            }
                        }
                    }
                }
            }
        }
        if !progress {
            break;
        }
    }
    let mats: Vec<Xform> = mats.into_iter().map(|m| m.unwrap_or(ident)).collect();
    let rest = mats
        .iter()
        .all(|m| m.pos.length_squared() < 1e-10 && m.rot.abs_diff_eq(Mat3::IDENTITY, 1e-5));
    (!rest || !base.whole).then_some(mats)
}

/// Deformed positions and normals (normalized space) for the matrices:
/// each mesh vertex once, copied to its corners. `origin` is the world
/// origin in part space (studs): the weight a vertex lacks from 1 pulls it
/// there, as Studio blends in world space.
pub fn deform(base: &Base, mats: &[Xform], scale: Vec3, origin: Vec3) -> (Vec<Vec3>, Vec<Vec3>) {
    let to_part = base.extent * scale;
    let from_part = Vec3::ONE / to_part.max(Vec3::splat(1e-6));
    let one = |i: usize| -> (Vec3, Vec3) {
        let pp = base.positions[i] * to_part;
        let n = base.normals[i] / base.extent;
        let ids = base.skin.corner_bones[i];
        let ws = base.skin.corner_weights[i];
        let mut q = Vec3::ZERO;
        let mut m = Vec3::ZERO;
        let mut total = 0.0;
        for k in 0..4 {
            let w = ws[k];
            if w <= 0.0 {
                continue;
            }
            if let Some(x) = mats.get(ids[k] as usize) {
                q += (x.pos + x.rot * pp) * w;
                m += (x.rot * n) * w;
                total += w;
            }
        }
        // (u8 weights: a real shortfall is ≥ 1/255; below 1e-4 it is rounding.)
        let short = 1.0 - total;
        if short.abs() > 1e-4 {
            q += origin * short;
        }
        (q * from_part, m.normalize_or_zero() * base.extent)
    };
    let verts: Vec<(Vec3, Vec3)> = base.firsts.iter().map(|&c| one(c as usize)).collect();
    let mut pos = Vec::with_capacity(base.positions.len());
    let mut nrm = Vec::with_capacity(base.positions.len());
    for (c, u) in base.corner_unique.iter().enumerate() {
        match verts.get(*u as usize) {
            Some((p, n)) => {
                pos.push(*p);
                nrm.push(*n);
            }
            None => {
                let (p, n) = one(c);
                pos.push(p);
                nrm.push(n);
            }
        }
    }
    (pos, nrm)
}

/// The world origin in a part's space (studs).
pub fn origin_of(doc: &Document, part: Ref) -> Vec3 {
    inv(&cf(doc, part, "CFrame")).pos
}

/// The part's Size / InitialSize (mesh units → studs), else Size / extent.
pub fn scale_of(doc: &Document, part: Ref, base: &Base) -> Vec3 {
    let v = |k: &str| match doc.prop(part, k) {
        Some(Variant::Vector3(v)) => Some(Vec3::new(v.x, v.y, v.z)),
        _ => None,
    };
    let size = v("Size").unwrap_or(base.extent);
    match v("InitialSize").or_else(|| v("MeshSize")) {
        Some(i) if i.min_element() > 1e-4 => size / i,
        _ => size / base.extent,
    }
}

/// Per-part mesh slots holding deformed copies (index, corner count), by
/// part and tag (0: the part's mesh, DECAL_TAG + face: its decals).
static SLOTS: std::sync::LazyLock<Mutex<HashMap<(Ref, u32), (u32, usize)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The palette each part's slot was last skinned with (part, tag): a part
/// redone with the same pose skips the skinning pass.
static LAST: std::sync::LazyLock<Mutex<HashMap<(Ref, u32), (usize, Vec<Xform>, Vec3, Vec3)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether slot (`part`, `tag`) already holds this pose of `base`; if not,
/// remembers it as the one about to be drawn.
pub fn same_pose(part: Ref, tag: u32, base: &Base, palette: &[Xform], scale: Vec3, origin: Vec3) -> bool {
    let Ok(mut m) = LAST.lock() else { return false };
    let key = base as *const Base as usize;
    let eq = |a: &Xform, b: &Xform| a.pos == b.pos && a.rot == b.rot;
    if let Some((k, p, s, o)) = m.get(&(part, tag)) {
        if *k == key && *s == scale && *o == origin && p.len() == palette.len() && p.iter().zip(palette).all(|(a, b)| eq(a, b)) {
            return true;
        }
    }
    m.insert((part, tag), (key, palette.to_vec(), scale, origin));
    false
}

/// Slot tag of a skinned R15 body part's classic-clothing atlas copy.
pub const R15_CLOTHING_TAG: u32 = 1;

/// Slot tag of a skinned part's decal copy for a face (`slot_of_tag`).
pub const DECAL_TAG: u32 = 16;

/// A skinned R15 body part's base with UVs remapped into the composited
/// classic-clothing atlas (`assets::r15_atlas_uvs`), cached by `(key, part_name)`.
pub fn r15_clothing_base(base: &Base, key: &str, part_name: &str) -> Option<Arc<Base>> {
    static CLOTHING: std::sync::LazyLock<Mutex<HashMap<String, Option<Arc<Base>>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    let k = format!("{key}|r15cloth|{part_name}");
    if let Some(b) = CLOTHING.lock().ok()?.get(&k) {
        return b.clone();
    }
    let b = rustudio_core::assets::r15_atlas_uvs(part_name, &base.uvs).map(|uvs| {
        Arc::new(Base {
            positions: base.positions.clone(),
            normals: base.normals.clone(),
            uvs,
            colors: Vec::new(),
            extent: base.extent,
            center: base.center,
            skin: base.skin.clone(),
            firsts: base.firsts.clone(),
            corner_unique: base.corner_unique.clone(),
            facs: None,
            whole: base.whole,
        })
    });
    if let Ok(mut g) = CLOTHING.lock() {
        g.insert(k, b.clone());
    }
    b
}

/// RUSTUDIO_SKIN_CPU=1: deform on the CPU and re-upload (the old path,
/// kept for comparison and for machines without compute).
pub fn cpu_skinning() -> bool {
    static CPU: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CPU.get_or_init(|| std::env::var_os("RUSTUDIO_SKIN_CPU").is_some_and(|v| v != "0"))
}

/// This frame's pose of a skinned part: the palette (current × bind⁻¹ per
/// mesh bone, part space) and the part's scale, or None at rest.
pub fn pose(doc: &Document, part: Ref, base: &Base) -> Option<(Vec<Xform>, Vec3, Vec3)> {
    let scale = scale_of(doc, part, base);
    let _sc = rustudio_core::bench::scope("skin_matrices");
    let mats = matrices(doc, part, base, scale)?;
    Some((mats, scale, origin_of(doc, part)))
}

/// The part's deformed-copy slot from an earlier frame.
pub fn slot_of(part: Ref, corners: usize) -> Option<u32> {
    slot_of_tag(part, 0, corners)
}

pub fn set_slot(part: Ref, idx: u32, corners: usize) {
    set_slot_tag(part, 0, idx, corners)
}

pub fn slot_of_tag(part: Ref, tag: u32, corners: usize) -> Option<u32> {
    SLOTS.lock().ok()?.get(&(part, tag)).copied().filter(|(_, len)| *len == corners).map(|(i, _)| i)
}

pub fn set_slot_tag(part: Ref, tag: u32, idx: u32, corners: usize) {
    if let Ok(mut s) = SLOTS.lock() {
        s.insert((part, tag), (idx, corners));
    }
}

/// Decal / Texture on a skinned MeshPart, as Studio 0.747 draws it
/// (checks/mesh-deform/AUDIT.md (g), tools/decalprobe.py): the image is
/// projected in bind pose onto the mesh triangles whose geometric normal
/// (mesh space, unscaled; vertex normals don't count) points most along
/// the face's axis, planar over the part's box face, and those triangles
/// then deform with the skin. The projection is from the mesh file's own
/// coordinates (a mesh not centred on its bounds takes the image off
/// centre, clamped at its edges). The copy shares the mesh's bones, so the
/// part's skin palette drives it. None: no triangle faces that way.
pub fn decal_base(base: &Base, key: &str, face: u32) -> Option<Arc<Base>> {
    static DECALS: std::sync::LazyLock<Mutex<HashMap<String, Option<Arc<Base>>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    let k = format!("{key}|decal{face}");
    if let Some(b) = DECALS.lock().ok()?.get(&k) {
        return b.clone();
    }
    let mut out = Base {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        colors: Vec::new(),
        extent: base.extent,
        center: base.center,
        skin: rustudio_core::mesh::Skin {
            bones: base.skin.bones.clone(),
            corner_bones: Vec::new(),
            corner_weights: Vec::new(),
            corner_vertex: Vec::new(),
            vertex_bones: Vec::new(),
            vertex_weights: Vec::new(),
        },
        firsts: Vec::new(),
        corner_unique: Vec::new(),
        facs: None,
        whole: base.whole,
    };
    let with_ids = base.skin.corner_vertex.len() == base.positions.len();
    for t in 0..base.positions.len() / 3 {
        let p: [Vec3; 3] = std::array::from_fn(|k| base.positions[t * 3 + k]);
        // (normalized positions × extent = mesh space)
        let geo = (p[1] - p[0]).cross(p[2] - p[0]) * Vec3::new(
            base.extent.y * base.extent.z,
            base.extent.x * base.extent.z,
            base.extent.x * base.extent.y,
        );
        if crate::scene::decal_face_of(geo) != Some(face) {
            continue;
        }
        let n_norm = ((p[1] - p[0]).cross(p[2] - p[0])).normalize_or_zero();
        for (k, v) in p.iter().enumerate() {
            let c = t * 3 + k;
            out.positions.push(*v + n_norm * 0.0005);
            out.normals.push(geo.normalize_or_zero() * base.extent);
            // (from the mesh file's own coordinates, not re-centred on its
            // bounds: Studio's rows on the minifig, bounds centre y 2.95,
            // sit 2.95 studs off a centred projection; past the image's
            // edge it clamps)
            out.uvs.push(crate::scene::decal_face_uv(face, *v + base.center / base.extent));
            out.skin.corner_bones.push(base.skin.corner_bones[c]);
            out.skin.corner_weights.push(base.skin.corner_weights[c]);
            if with_ids {
                out.skin.corner_vertex.push(base.skin.corner_vertex[c]);
            }
        }
    }
    let b = (!out.positions.is_empty()).then(|| {
        let (firsts, corner_unique) = uniques(&out.skin.corner_vertex, out.positions.len());
        out.firsts = firsts;
        out.corner_unique = corner_unique;
        Arc::new(out)
    });
    if let Ok(mut g) = DECALS.lock() {
        g.insert(k, b.clone());
    }
    b
}

/// The renderer mesh to draw a skinned part with this frame on the CPU
/// path: a deformed copy in the part's own slot (uploaded with
/// `upload(slot, …)`, `slot` None for a new one), or None at rest.
pub fn skinned_mesh(
    doc: &Document,
    part: Ref,
    base: &Base,
    upload: &mut dyn FnMut(Option<u32>, &[Vec3], &[Vec3], &[Vec2], &[Vec4]) -> u32,
) -> Option<u32> {
    let (mats, scale, origin) = pose(doc, part, base)?;
    let sc = rustudio_core::bench::scope("skin_deform");
    let (p, n) = deform(base, &mats, scale, origin);
    drop(sc);
    let have = slot_of(part, p.len());
    let _sc = rustudio_core::bench::scope("skin_upload");
    let idx = upload(have, &p, &n, &base.uvs, &base.colors);
    // Bench: vertex bytes sent (a full Vertex per corner).
    rustudio_core::bench::add("skin_upload_bytes", 0.0, p.len() as u64 * 64);
    set_slot(part, idx, p.len());
    Some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustudio_core::mesh::{Skin, SkinBone};

    fn base() -> Base {
        // Two corners: one fully on bone 0, one fully on bone 1 (child).
        Base {
            positions: vec![Vec3::new(0.0, 0.5, 0.0), Vec3::new(0.0, -0.5, 0.0)],
            normals: vec![Vec3::Z, Vec3::Z],
            uvs: vec![Vec2::ZERO; 2],
            colors: Vec::new(),
            extent: Vec3::ONE,
            center: Vec3::ZERO,
            facs: None,
            whole: true,
            firsts: vec![0, 1],
            corner_unique: vec![0, 1],
            skin: Skin {
                bones: vec![
                    SkinBone { name: "Root".into(), parent: None, rot: Mat3::IDENTITY, pos: Vec3::ZERO },
                    SkinBone { name: "Jaw".into(), parent: Some(0), rot: Mat3::IDENTITY, pos: Vec3::new(0.0, -0.5, 0.0) },
                ],
                corner_bones: vec![[0, 0, 0, 0], [1, 0, 0, 0]],
                corner_weights: vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
                corner_vertex: vec![0, 1],
                vertex_bones: Vec::new(),
                vertex_weights: Vec::new(),
            },
        }
    }

    #[test]
    fn a_turned_child_bone_moves_only_its_corners() {
        let b = base();
        // Jaw rotated 90° about X around its own origin at (0,-0.5,0): the
        // corner sitting on the jaw origin stays, so move it off first.
        let mut b2 = base();
        b2.positions[1] = Vec3::new(0.0, -0.5, 0.25);
        let jaw_now = Xform { pos: Vec3::new(0.0, -0.5, 0.0), rot: Mat3::from_rotation_x(std::f32::consts::FRAC_PI_2) };
        let bind_jaw = Xform { pos: Vec3::new(0.0, -0.5, 0.0), rot: Mat3::IDENTITY };
        let mats = vec![Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY }, mul(&jaw_now, &inv(&bind_jaw))];
        let (p, _) = deform(&b2, &mats, Vec3::ONE, Vec3::ZERO);
        assert!(p[0].abs_diff_eq(Vec3::new(0.0, 0.5, 0.0), 1e-5));
        // (0,0,0.25) relative to the jaw turned 90° about X → (0,-0.25,0).
        assert!(p[1].abs_diff_eq(Vec3::new(0.0, -0.75, 0.0), 1e-5), "{:?}", p[1]);
        let _ = b;
    }

    // deps_of's early out (has_bones) gives what the full walk gives: a
    // Bone or FaceControls on the part, on a welded part of its model, on
    // an unjointed part of that model, or nowhere.
    #[test]
    fn deps_of_early_out_matches_the_walk() {
        use rbx_dom_weak::InstanceBuilder;
        let walk = |doc: &rustudio_core::doc::Document, part: Ref| {
            let mut out = face_controls(doc, part);
            out.extend(bones_of(doc, part));
            out
        };
        for case in 0..5 {
            let mut doc = rustudio_core::doc::Document::new_place();
            let ws = doc.workspace().unwrap();
            let model = doc.dom.insert(ws, InstanceBuilder::new("Model"));
            let parts: Vec<Ref> = (0..6)
                .map(|i| doc.dom.insert(model, InstanceBuilder::new("MeshPart").with_name(format!("P{i}"))))
                .collect();
            let loose = doc.dom.insert(ws, InstanceBuilder::new("MeshPart").with_name("Loose"));
            // P0-P1-P2 welded; P3-P4 welded; P5 alone.
            for (a, b) in [(0, 1), (1, 2), (3, 4)] {
                doc.dom.insert(model, InstanceBuilder::new("Weld")
                    .with_property("Part0", parts[a])
                    .with_property("Part1", parts[b]));
            }
            match case {
                1 => { doc.dom.insert(parts[2], InstanceBuilder::new("Bone").with_name("B")); }
                2 => { doc.dom.insert(parts[4], InstanceBuilder::new("FaceControls")); }
                3 => { doc.dom.insert(parts[5], InstanceBuilder::new("Bone").with_name("C")); }
                4 => { doc.dom.insert(loose, InstanceBuilder::new("Bone").with_name("D")); }
                _ => {}
            }
            rig_changed();
            for &p in parts.iter().chain([&loose]) {
                assert_eq!(deps_of(&doc, p, None), walk(&doc, p), "case {case}");
            }
            // (the welded group finds its Bone through the joints)
            assert_eq!(!deps_of(&doc, parts[0], None).is_empty(), case == 1, "case {case}");
        }
    }

    #[test]
    fn bone_instances_drive_the_mesh() {
        use rbx_dom_weak::InstanceBuilder;
        let mut doc = rustudio_core::doc::Document::new_place();
        let ws = doc.workspace().unwrap();
        let part = doc.dom.insert(ws, InstanceBuilder::new("MeshPart").with_name("Head")
            .with_property("Size", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0))
            .with_property("InitialSize", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0)));
        let bind = Xform { pos: Vec3::new(0.0, -0.5, 0.0), rot: Mat3::IDENTITY };
        let turn = Xform { pos: Vec3::ZERO, rot: Mat3::from_rotation_x(std::f32::consts::FRAC_PI_2) };
        doc.dom.insert(part, InstanceBuilder::new("Bone").with_name("Jaw")
            .with_property("CFrame", bind.to_cframe())
            .with_property("Transform", turn.to_cframe()));
        let mut b = base();
        b.positions[1] = Vec3::new(0.0, -0.5, 0.25);
        assert!(has_bones(&doc, part));
        let mats = matrices(&doc, part, &b, Vec3::ONE).expect("a moved bone");
        let (p, _) = deform(&b, &mats, Vec3::ONE, Vec3::ZERO);
        assert!(p[0].abs_diff_eq(Vec3::new(0.0, 0.5, 0.0), 1e-5));
        assert!(p[1].abs_diff_eq(Vec3::new(0.0, -0.75, 0.0), 1e-5), "{:?}", p[1]);
        // Back at rest: the shared mesh draws.
        let jaw = doc.dom.get_by_ref(part).unwrap().children()[0];
        doc.dom.get_by_ref_mut(jaw).unwrap().properties.insert(rbx_dom_weak::ustr("Transform"), Variant::CFrame(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY }.to_cframe()));
        // A property write is journalled (it bumps the revision the Bone frame memo keys on).
        doc.mark_prop(jaw, rbx_dom_weak::ustr("Transform"));
        assert!(matrices(&doc, part, &b, Vec3::ONE).is_none());
    }

    #[test]
    fn s15_baseparts_deform_seamlessly_across_motor6d() {
        use rbx_dom_weak::InstanceBuilder;
        let mut doc = rustudio_core::doc::Document::new_place();
        let ws = doc.workspace().unwrap();
        let model = doc.dom.insert(ws, InstanceBuilder::new("Model").with_name("Avatar"));
        let lower_rest = Xform { pos: Vec3::new(0.0, 2.0, 0.0), rot: Mat3::IDENTITY };
        let upper_rest = Xform { pos: Vec3::new(0.0, 3.0, 0.0), rot: Mat3::IDENTITY };
        // Joint at world y = 2.5: C0 = (0, +0.5, 0) in LowerTorso, C1 = (0, -0.5, 0) in UpperTorso.
        let c0 = Xform { pos: Vec3::new(0.0, 0.5, 0.0), rot: Mat3::IDENTITY };
        let c1 = Xform { pos: Vec3::new(0.0, -0.5, 0.0), rot: Mat3::IDENTITY };
        let lower = doc.dom.insert(
            model,
            InstanceBuilder::new("MeshPart")
                .with_name("LowerTorso")
                .with_property("CFrame", lower_rest.to_cframe())
                .with_property("Size", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0))
                .with_property("InitialSize", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0)),
        );
        let upper = doc.dom.insert(
            model,
            InstanceBuilder::new("MeshPart")
                .with_name("UpperTorso")
                .with_property("CFrame", upper_rest.to_cframe())
                .with_property("Size", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0))
                .with_property("InitialSize", rbx_dom_weak::types::Vector3::new(1.0, 1.0, 1.0)),
        );
        doc.dom.insert(
            upper,
            InstanceBuilder::new("Motor6D")
                .with_name("Waist")
                .with_property("Part0", Variant::Ref(lower))
                .with_property("Part1", Variant::Ref(upper))
                .with_property("C0", c0.to_cframe())
                .with_property("C1", c1.to_cframe()),
        );
        rig_changed();

        // Both meshes have arbitrary Blender bone rotations (non-identity) like real S15 assets.
        let blender_rot = Mat3::from_euler(glam::EulerRot::XYZ, 2.5, -0.7, 1.2);
        let make_s15_base = |seam_local: Vec3| Base {
            positions: vec![seam_local],
            normals: vec![Vec3::Z],
            uvs: vec![Vec2::ZERO],
            colors: Vec::new(),
            extent: Vec3::ONE,
            center: Vec3::ZERO,
            facs: None,
            whole: true,
            firsts: vec![0],
            corner_unique: vec![0],
            skin: Skin {
                bones: vec![
                    SkinBone { name: "Root".into(), parent: None, rot: Mat3::IDENTITY, pos: Vec3::ZERO },
                    SkinBone { name: "LowerTorso".into(), parent: Some(0), rot: blender_rot, pos: Vec3::new(1.0, -2.0, 0.3) },
                    SkinBone { name: "UpperTorso".into(), parent: Some(1), rot: blender_rot.transpose(), pos: Vec3::new(-0.4, 0.8, 0.2) },
                ],
                corner_bones: vec![[1, 2, 0, 0]],
                corner_weights: vec![[0.5, 0.5, 0.0, 0.0]],
                corner_vertex: vec![0],
                vertex_bones: Vec::new(),
                vertex_weights: Vec::new(),
            },
        };
        // Seam vertex at world (0.2, 2.5, 0.3): local (0.2, +0.5, 0.3) in LowerTorso, (0.2, -0.5, 0.3) in UpperTorso.
        let lower_base = make_s15_base(Vec3::new(0.2, 0.5, 0.3));
        let upper_base = make_s15_base(Vec3::new(0.2, -0.5, 0.3));

        // 1. At rest: matrices() returns None even with arbitrary Blender bone rotations.
        assert!(part_in_jointed_model(&doc, lower));
        assert!(lookup(&doc, lower, &lower_base).any);
        assert!(lookup(&doc, upper, &upper_base).any);
        assert!(matrices(&doc, lower, &lower_base, Vec3::ONE).is_none());
        assert!(matrices(&doc, upper, &upper_base, Vec3::ONE).is_none());
        // Both parts depend on both LowerTorso and UpperTorso so either moving re-skins both.
        let ldeps = &lookup(&doc, lower, &lower_base).deps;
        assert!(ldeps.contains(&lower) && ldeps.contains(&upper), "{ldeps:?}");

        // 2. Bend Waist 45° around X + 20° around Z: UpperTorso = LowerTorso * C0 * T * inv(C1).
        let t = Xform { pos: Vec3::ZERO, rot: Mat3::from_euler(glam::EulerRot::XYZ, 0.785, 0.0, 0.35) };
        let upper_now = mul(&mul(&mul(&lower_rest, &c0), &t), &inv(&c1));
        doc.dom
            .get_by_ref_mut(upper)
            .unwrap()
            .properties
            .insert(rbx_dom_weak::ustr("CFrame"), Variant::CFrame(upper_now.to_cframe()));
        doc.mark_prop(upper, rbx_dom_weak::ustr("CFrame"));

        let lm = matrices(&doc, lower, &lower_base, Vec3::ONE).expect("lower deforms");
        let um = matrices(&doc, upper, &upper_base, Vec3::ONE).expect("upper deforms");
        let (lp, _) = deform(&lower_base, &lm, Vec3::ONE, origin_of(&doc, lower));
        let (up, _) = deform(&upper_base, &um, Vec3::ONE, origin_of(&doc, upper));
        let l_world = lower_rest.apply(lp[0]);
        let u_world = upper_now.apply(up[0]);
        assert!(
            l_world.abs_diff_eq(u_world, 1e-5),
            "seam gap! lower={l_world:?} upper={u_world:?}"
        );
    }
}

#[cfg(test)]
mod cache_probe {
    /// `SKIN_PROBE=path cargo test skin::cache_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_mesh_file() {
        let Ok(p) = std::env::var("SKIN_PROBE") else { return };
        let b = std::fs::read(p).unwrap();
        let m = rustudio_core::mesh::parse(&b).unwrap();
        println!("corners {} skin {:?}", m.positions.len(), m.skin.as_ref().map(|s| (s.bones.len(), s.corner_bones.len())));
        if let Some(s) = &m.skin {
            for bn in s.bones.iter().take(8) {
                println!("bone {} parent {:?} pos {:?}", bn.name, bn.parent, bn.pos);
            }
        }
        let f = rustudio_core::mesh::parse_mesh_facs(&b);
        println!("facs {:?}", f.as_ref().map(|f| (f.bones.len(), f.controls.len(), f.cols, f.two.len(), f.three.len())));
        if let Some(f) = f {
            println!("controls {:?}", &f.controls);
            println!("bones {:?}", &f.bones[..f.bones.len().min(12)]);
        }
    }
}
