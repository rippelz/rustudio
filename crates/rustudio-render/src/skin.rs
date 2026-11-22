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
    let b = parse_base(bytes);
    if let Ok(mut g) = BASES.lock() {
        g.insert(id.to_string(), b.clone());
    }
    rustudio_core::assets::bump_mesh_generation();
    b
}

/// Skinned bases being parsed on the asset workers (`base_async`), each
/// with its ticket: one forgotten meanwhile (`forget_base`) is dropped.
static PARSING: std::sync::LazyLock<Mutex<(u64, HashMap<String, u64>)>> =
    std::sync::LazyLock::new(|| Mutex::new((0, HashMap::new())));

/// Bumped when a skinned base parsed on the asset workers is in.
static BASE_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Changes when `base_async` has a new base ready (the parts waiting on it
/// are redone).
pub fn base_generation() -> u64 {
    BASE_GEN.load(std::sync::atomic::Ordering::Acquire)
}

/// `base`, parsed on the asset workers (as Roblox's MeshContentProvider
/// parses meshes on its worker, asset_work.rs): Some once parsed (None
/// inside: not skinned), None while it parses.
pub fn base_async(id: &str, bytes: Arc<Vec<u8>>) -> Option<Option<Arc<Base>>> {
    if !rustudio_core::assets::place_content_ok(id) {
        return Some(None);
    }
    if let Some(b) = BASES.lock().ok()?.get(id) {
        return Some(b.clone());
    }
    if rustudio_core::assets::sync_decode() {
        return Some(base(id, &bytes));
    }
    let mut p = PARSING.lock().ok()?;
    if p.1.contains_key(id) {
        return None;
    }
    p.0 += 1;
    let ticket = p.0;
    p.1.insert(id.to_string(), ticket);
    let id = id.to_string();
    rustudio_core::asset_work::spawn(move || {
        let b = parse_base(&bytes);
        let Ok(mut p) = PARSING.lock() else { return };
        if p.1.get(&id) != Some(&ticket) {
            return;
        }
        p.1.remove(&id);
        if let Ok(mut g) = BASES.lock() {
            g.insert(id, b);
        }
        drop(p);
        rustudio_core::assets::bump_mesh_generation();
        BASE_GEN.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    });
    None
}

fn parse_base(bytes: &[u8]) -> Option<Arc<Base>> {
    rustudio_core::mesh::parse(bytes).ok().and_then(|m| {
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
    })
}

/// The skinned base of a mesh already parsed by `base`, without its bytes.
/// Drops the skin base of `id` (its bytes changed: a live EditableMesh).
pub fn forget_base(id: &str) {
    if let Ok(mut b) = BASES.lock() {
        b.remove(id);
    }
    if let Ok(mut p) = PARSING.lock() {
        p.1.remove(id);
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
    if !has_bones(doc, part) {
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
        *index = RigIndex { gen, ..Default::default() };
    }
    if *index.parts.entry(part).or_insert_with(|| rigged_below(doc, part, true)) {
        return true;
    }
    // A Bone under another part of its assembly (`bone_hosts`): the joints
    // under its model, grouped once per model.
    let Some(model) = doc.dom.get_by_ref(part).map(|i| i.parent()).filter(|m| {
        doc.class_of(*m).is_some_and(|c| rustudio_core::doc::is_a(&c, "Model"))
    }) else {
        return false;
    };
    // (a model with no Bone below it has nothing to group: most places. A
    // Bone under a part outside the model, jointed to it from inside, is
    // not seen; avatar and importer rigs keep theirs inside: estimated)
    let rig = index.models.entry(model).or_insert_with(|| {
        let _s = rustudio_core::bench::scope("skin_model_rig");
        rigged_below(doc, model, false).then(|| ModelRig::build(doc, model))
    });
    rig.as_ref().is_some_and(|rig| rig.group.get(&part).is_some_and(|g| rig.with_bones[*g as usize]))
}

/// Which parts are skinned (for `has_bones`), valid until the rig changes,
/// as the bone lookups are: filled in per part and per model as they are
/// asked about. (It was a walk of the whole document on every rig change:
/// 10 ms each time a character with a dynamic head arrived.)
#[derive(Default)]
struct RigIndex {
    gen: u64,
    /// Part → a Bone or FaceControls is below it.
    parts: HashMap<Ref, bool>,
    /// Model → its jointed groups (None: no Bone below the model).
    models: HashMap<Ref, Option<ModelRig>>,
}

/// The rigid joints under one model as groups of jointed parts (what
/// `bone_hosts` walks, for every part of the model at once).
struct ModelRig {
    group: HashMap<Ref, u32>,
    /// Per group: some part of it has a Bone below it.
    with_bones: Vec<bool>,
}

/// Whether a Bone (or, with `face`, a FaceControls) is below `r`.
fn rigged_below(doc: &Document, r: Ref, face: bool) -> bool {
    doc.dom.descendants_of(r).any(|i| i.class == "Bone" || (face && i.class == "FaceControls"))
}

impl ModelRig {
    fn build(doc: &Document, model: Ref) -> Self {
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
            if !with_bones[g as usize] {
                with_bones[g as usize] = rigged_below(doc, r, false);
            }
        }
        ModelRig { group, with_bones }
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

/// A part's mesh bones resolved to Bone instances (by mesh bone index),
/// and what it redraws on.
pub struct Lookup {
    gen: u64,
    doc: u64,
    pub bones: Vec<Option<Ref>>,
    pub deps: Vec<Ref>,
    /// The part is skinned: Bones (its own or its assembly's) or FaceControls.
    pub any: bool,
}

static LOOKUPS: std::sync::LazyLock<Mutex<HashMap<Ref, (usize, Arc<Lookup>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The part's bone lookup for this mesh (cached until the rig changes):
/// mesh bone name → the part's own Bone of that name, else one of its
/// assembly's (the first by name wins).
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
    // The assembly's only when some mesh bone has no Bone here (the BFS
    // over the model's joints is the costly part).
    if base.skin.bones.iter().any(|b| !by_name.contains_key(b.name.as_str())) {
        for h in bone_hosts(doc, part).into_iter().skip(1) {
            own(doc, h, &mut by_name);
        }
    }
    let bones: Vec<Option<Ref>> = base.skin.bones.iter().map(|b| by_name.get(b.name.as_str()).copied()).collect();
    let mut deps = face_controls(doc, part);
    let any = !deps.is_empty() || !by_name.is_empty() || has_bones(doc, part);
    let mut seen: std::collections::HashSet<Ref> = deps.iter().copied().collect();
    for b in bones.iter().flatten() {
        // The Bone and the Bones / Attachments above it (its frame's chain).
        let mut cur = *b;
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
    let l = Arc::new(Lookup { gen, doc: doc.id, bones, deps, any });
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
    let mut now: Vec<Option<Xform>> = vec![None; n];
    // Parents come before children in Roblox meshes; resolve in passes.
    for _ in 0..n.max(1) {
        let mut progress = false;
        for i in 0..n {
            if now[i].is_some() {
                continue;
            }
            let sb = &base.skin.bones[i];
            if let Some(b) = look.bones[i] {
                now[i] = Some(mul(&part_inv, &frames.world(b)));
                progress = true;
            } else {
                // No Bone instance: the bind frame relative to its parent,
                // times the FaceControls' FACS offset for this bone.
                let local = facs.get(&sb.name).copied();
                match sb.parent {
                    None => {
                        now[i] = Some(match local {
                            Some(l) => mul(&bind[i], &l),
                            None => bind[i],
                        });
                        progress = true;
                    }
                    Some(p) => {
                        if let Some(pn) = now[p] {
                            let rest = mul(&pn, &mul(&bind_inv[p], &bind[i]));
                            now[i] = Some(match local {
                                Some(l) => mul(&rest, &l),
                                None => rest,
                            });
                            progress = true;
                        }
                    }
                }
            }
        }
        if !progress {
            break;
        }
    }
    let mats: Vec<Xform> = (0..n)
        .map(|i| mul(&now[i].unwrap_or(bind[i]), &bind_inv[i]))
        .collect();
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

/// Slot tag of a skinned part's decal copy for a face (`slot_of_tag`).
pub const DECAL_TAG: u32 = 16;

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
