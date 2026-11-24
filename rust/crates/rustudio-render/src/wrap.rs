//! Layered clothing: Accessories whose Handle has a WrapLayer, fitted onto
//! the character's WrapTarget cages (Studio 0.747, checks/mesh-deform/AUDIT.md (d)).
//!
//! - Each body MeshPart's WrapTarget names its outer cage mesh, placed by
//!   CageOrigin in the part and stretched like the part's mesh (Size over the
//!   mesh's own extent).
//! - A WrapLayer names the garment's inner cage (ReferenceMeshContent, at
//!   ReferenceOrigin) and outer cage (CageMeshContent, at CageOrigin).
//!   Cages are cut from Roblox's one cage template, so a garment's inner cage
//!   vertex and a body cage vertex are the same point of the template when
//!   their UVs are equal: that pairs them.
//! - Layers go on in Order (lowest first): the first fits the body's cages,
//!   each next one the previous layer's fitted outer cage.
//! - The fit is Studio's cage field (cage_rbf.rs, measured through
//!   WrapDeformer): radial basis interpolation whose centres are the inner
//!   cage points found on the target (to their target point) and their outer
//!   cage partners (to that point plus their height over the inner cage;
//!   Puffiness doesn't change it, measured). Against Studio's jacket silhouette: IoU 0.95 front, 0.88 side.
//! - The fit is made once, in the pose the character has then; per frame the
//!   garment is skinned (skin_gpu.rs), not fitted again: Studio moves a
//!   garment point by the garment mesh's own weights, each mesh bone being
//!   the body part of the same name, times that part's motion since the fit
//!   (marker garments in Studio: 0.004 studs; with the sleeves weighted to
//!   the upper arm nothing moved when the elbow bent). AutoSkin
//!   EnabledOverride, or a garment without weights, takes the weights from
//!   the body cage points nearest to it instead.
//! - A garment is drawn with the body part its accessory is held to.
//!
//! Fitting runs on a background thread once per set of garments.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use glam::{Vec2, Vec3};
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::doc::Document;
use rustudio_core::math::Xform;
use crate::scene::{content_string, f32_of, vec3_of, xform_of};

/// A garment piece drawn with one body part, in that part's space.
#[derive(Clone)]
pub struct Piece {
    pub part: Ref,
    /// The garment's Handle (its texture and colour).
    pub handle: Ref,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Vec<Vec2>,
}

/// A fitted garment as a skinned mesh whose bones are body parts.
pub struct Garment {
    /// The garment's Handle (its texture and colour).
    pub handle: Ref,
    /// The body part it is drawn with (the most weight) and its space.
    pub owner: Ref,
    /// The garment in `owner`'s fitting-time space (skin.rs Base: unit box
    /// positions, bones = `parts`).
    pub base: Arc<crate::skin::Base>,
    /// Per bone: the body part and its CFrame when fitted.
    pub parts: Vec<(Ref, Xform)>,
    /// A key for the GPU skinning source (unique per fitting and layer).
    pub key: String,
}

/// Everything fitted on one character.
pub struct Fitted {
    pub pieces: Vec<Piece>,
    pub garments: Vec<Garment>,
    /// Garment vertices that found their inner cage point on the target by
    /// UV, out of all inner-cage vertices (per layer, in Order).
    pub matched: Vec<(usize, usize)>,
}

struct Target {
    part: Ref,
    name: String,
    xf: Xform,
    size: Vec3,
    mesh: String,
    cage: String,
    origin: Xform,
}

struct Layer {
    handle: Ref,
    xf: Xform,
    size: Vec3,
    mesh: String,
    inner: String,
    inner_origin: Xform,
    outer: String,
    outer_origin: Xform,
    order: i32,
    puffiness: f32,
    auto_skin: AutoSkin,
    /// The body part the accessory is held to (the garment is drawn with it).
    attached: Option<Ref>,
}

/// WrapLayer.AutoSkin: where a garment's per-frame skinning weights come
/// from. Disabled / EnabledPreserve keep the garment mesh's own weights
/// (its bones are named like the body parts); EnabledOverride, or a garment
/// with no weights, takes them from the body it fits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutoSkin {
    Disabled,
    EnabledPreserve,
    EnabledOverride,
}

struct Job {
    targets: Vec<Target>,
    layers: Vec<Layer>,
}

#[derive(Clone)]
enum State {
    Pending,
    Ready(Arc<Fitted>),
    Failed(String),
}

fn states() -> &'static Mutex<HashMap<String, State>> {
    static S: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The body part a layered accessory's Handle is held to (its
/// AccessoryWeld / AccessoryRigidConstraint / weld): the garment is drawn
/// with that part.
fn attached_part(doc: &Document, handle: Ref) -> Option<Ref> {
    let h = doc.dom.get_by_ref(handle)?;
    for c in h.children() {
        let Some(class) = doc.class_of(*c) else { continue };
        let other = |a: &str, b: &str| -> Option<Ref> {
            let pa = match doc.prop(*c, a) {
                Some(Variant::Ref(r)) => r,
                _ => Ref::none(),
            };
            let pb = match doc.prop(*c, b) {
                Some(Variant::Ref(r)) => r,
                _ => Ref::none(),
            };
            let (pa, pb) = if class == "RigidConstraint" {
                let up = |r: Ref| doc.dom.get_by_ref(r).map(|i| i.parent()).unwrap_or_else(Ref::none);
                (up(pa), up(pb))
            } else {
                (pa, pb)
            };
            if pa == handle && !pb.is_none() {
                Some(pb)
            } else if pb == handle && !pa.is_none() {
                Some(pa)
            } else {
                None
            }
        };
        let got = match class.as_str() {
            "RigidConstraint" => other("Attachment0", "Attachment1"),
            "Weld" | "ManualWeld" | "WeldConstraint" | "Motor6D" => other("Part0", "Part1"),
            _ => None,
        };
        if got.is_some() {
            return got;
        }
    }
    None
}

/// Whether `part` draws one of `model`'s layered garments (the part its
/// accessory is held to): it then redraws when the other body parts move,
/// the garment being skinned to them.
pub fn draws_garment(doc: &Document, model: Ref, part: Ref) -> bool {
    let Some(m) = doc.dom.get_by_ref(model) else { return false };
    // Characters are Models: a MeshPart straight under Workspace (or a
    // Folder) holds no garment, and scanning its thousands of siblings for
    // every such part made reading a place quadratic.
    if m.class != "Model" {
        return false;
    }
    m.children().iter().any(|c| {
        doc.class_of(*c).is_some_and(|k| rustudio_core::doc::is_a(&k, "Accoutrement"))
            && rustudio_core::default_scripts::child_named(doc, *c, "Handle")
                .is_some_and(|h| is_layered_handle(doc, h) && attached_part(doc, h) == Some(part))
    })
}

/// Changes whenever a fitting finished.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// Another thread's character work finished (hsr.rs): redraw.
pub fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub enum Lookup {
    /// No layered clothing on this character.
    None,
    Pending,
    Ready(Arc<Fitted>),
    Failed(String),
}

fn child_of_class(doc: &Document, parent: Ref, class: &str) -> Option<Ref> {
    doc.dom
        .get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| doc.class_of(*c).is_some_and(|k| k == class))
}

fn cf(doc: &Document, r: Ref, key: &str) -> Xform {
    match doc.prop(r, key) {
        Some(Variant::CFrame(c)) => Xform::from_cframe(&c),
        _ => Xform {
            pos: Vec3::ZERO,
            rot: glam::Mat3::IDENTITY,
        },
    }
}

fn content(doc: &Document, r: Ref, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| content_string(doc.prop(r, k)))
        .filter(|s| !s.is_empty())
}

/// Whether a part is a layered garment's Handle (drawn through its
/// character's fitting, not on its own).
pub fn is_layered_handle(doc: &Document, part: Ref) -> bool {
    child_of_class(doc, part, "WrapLayer")
        .is_some_and(|w| !matches!(doc.prop(w, "Enabled"), Some(Variant::Bool(false))))
        && doc
            .dom
            .get_by_ref(part)
            .and_then(|p| doc.dom.get_by_ref(p.parent()))
            .and_then(|acc| doc.dom.get_by_ref(acc.parent()))
            .is_some_and(|m| {
                m.children()
                    .iter()
                    .any(|c| doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
            })
}

/// The fitted layered clothing of the character `model` (queued on first ask).
pub fn fitted(doc: &Document, model: Ref) -> Lookup {
    let Some(m) = doc.dom.get_by_ref(model) else {
        return Lookup::None;
    };
    let mut targets = Vec::new();
    let mut layers = Vec::new();
    for c in m.children() {
        let Some(class) = doc.class_of(*c) else {
            continue;
        };
        if class == "MeshPart" {
            if let Some(w) = child_of_class(doc, *c, "WrapTarget") {
                let (Some(mesh), Some(cage), Some(xf)) = (
                    content(doc, *c, &["MeshContent", "MeshId"]),
                    content(doc, w, &["CageMeshContent", "CageMeshId"]),
                    xform_of(doc, *c),
                ) else {
                    continue;
                };
                targets.push(Target {
                    part: *c,
                    name: doc.dom.get_by_ref(*c).map(|i| i.name.clone()).unwrap_or_default(),
                    xf,
                    size: vec3_of(doc.prop(*c, "Size")).unwrap_or(Vec3::ONE),
                    mesh,
                    cage,
                    origin: cf(doc, w, "CageOrigin"),
                });
            }
        } else if class == "Accessory" {
            let Some(h) = rustudio_core::default_scripts::child_named(doc, *c, "Handle") else {
                continue;
            };
            if !is_layered_handle(doc, h) {
                continue;
            }
            let Some(w) = child_of_class(doc, h, "WrapLayer") else {
                continue;
            };
            let (Some(mesh), Some(inner), Some(outer), Some(xf)) = (
                content(doc, h, &["MeshContent", "MeshId"]),
                content(doc, w, &["ReferenceMeshContent", "ReferenceMeshId"]),
                content(doc, w, &["CageMeshContent", "CageMeshId"]),
                xform_of(doc, h),
            ) else {
                continue;
            };
            layers.push(Layer {
                handle: h,
                xf,
                size: vec3_of(doc.prop(h, "Size")).unwrap_or(Vec3::ONE),
                mesh,
                inner,
                inner_origin: cf(doc, w, "ReferenceOrigin"),
                outer,
                outer_origin: cf(doc, w, "CageOrigin"),
                order: match doc.prop(w, "Order") {
                    Some(Variant::Int32(o)) => o,
                    Some(Variant::Int64(o)) => o as i32,
                    _ => 1,
                },
                attached: attached_part(doc, h),
                auto_skin: match doc.prop(w, "AutoSkin") {
                    Some(Variant::Enum(e)) if e.to_u32() == 1 => AutoSkin::EnabledPreserve,
                    Some(Variant::Enum(e)) if e.to_u32() == 2 => AutoSkin::EnabledOverride,
                    _ => AutoSkin::Disabled,
                },
                // Studio clamps Puffiness to [-1, 1] in its setter (Studio:
                // maxss .rdata Studio = -1, minss Studio = 1); default 1
                // (FInt WrapPuffinessDefaultPercent = 100). The fit doesn't use it:
                // Studio 0.747's jacket draws pixel-identical at Puffiness 0.1,
                // 0.5 and 1 (set through the description, or on the WrapLayer
                // and refitted; checks/mesh-deform/AUDIT.md (d)).
                puffiness: f32_of(doc.prop(w, "Puffiness"))
                    .unwrap_or(rustudio_core::mech::WRAP_PUFFINESS_DEFAULT)
                    .clamp(
                        rustudio_core::mech::WRAP_PUFFINESS_MIN,
                        rustudio_core::mech::WRAP_PUFFINESS_MAX,
                    ),
            });
        }
    }
    if layers.is_empty() || targets.is_empty() {
        return Lookup::None;
    }
    layers.sort_by_key(|l| l.order);
    let key = format!(
        "{model:?}|{}|{}",
        targets
            .iter()
            .map(|t| format!("{:?}:{}:{}", t.part, t.mesh, t.cage))
            .collect::<Vec<_>>()
            .join(","),
        layers
            .iter()
            .map(|l| format!(
                "{:?}:{}:{}:{}:{}:{}:{:?}",
                l.handle, l.mesh, l.inner, l.outer, l.order, l.puffiness, l.auto_skin
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut s = states().lock().unwrap_or_else(|e| e.into_inner());
    match s.get(&key) {
        Some(State::Ready(f)) => return Lookup::Ready(f.clone()),
        Some(State::Pending) => return Lookup::Pending,
        Some(State::Failed(e)) => return Lookup::Failed(e.clone()),
        None => {}
    }
    s.insert(key.clone(), State::Pending);
    drop(s);
    let job = Job { targets, layers };
    std::thread::Builder::new()
        .name("layered clothing".into())
        .spawn(move || {
            let st = match fit(&job) {
                Ok(f) => State::Ready(Arc::new(f)),
                Err(e) => State::Failed(e),
            };
            states()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, st);
            GENERATION.fetch_add(1, Ordering::Relaxed);
        })
        .ok();
    Lookup::Pending
}

fn mesh(id: &str) -> Result<rustudio_core::mesh::MeshData, String> {
    let b = rustudio_core::assets::content_blocking(id)?;
    rustudio_core::mesh::parse(&b).map_err(|e| format!("{id}: {e}"))
}

/// A cage's distinct vertices (position, uv), welded from the triangle list.
fn cage_points(m: &rustudio_core::mesh::MeshData) -> Vec<(Vec3, Vec2)> {
    let mut seen = HashMap::new();
    let mut out = Vec::new();
    for (p, uv) in m.positions.iter().zip(&m.uvs) {
        let k = (
            (p.x * 1e4) as i64,
            (p.y * 1e4) as i64,
            (p.z * 1e4) as i64,
            (uv.x * 1e5) as i64,
            (uv.y * 1e5) as i64,
        );
        if seen.insert(k, ()).is_none() {
            out.push((*p, *uv));
        }
    }
    out
}

/// Mesh-space point → world, the way a MeshPart stretches its mesh: the
/// part's Size over the render mesh's own extent (`fit`), then `origin` in
/// the part, then the part.
fn place(xf: &Xform, origin: &Xform, fit: Vec3, p: Vec3) -> Vec3 {
    xf.apply(origin.apply(p * fit))
}

fn fit_scale(size: Vec3, m: &rustudio_core::mesh::MeshData) -> Vec3 {
    size / (m.max - m.min).max(Vec3::splat(1e-4))
}

/// UV key for pairing template vertices.
fn uv_key(uv: Vec2) -> (i64, i64) {
    ((uv.x * 1e4).round() as i64, (uv.y * 1e4).round() as i64)
}

/// A cage the next layer fits onto: world positions, uvs, owning part.
struct Surface {
    points: Vec<Vec3>,
    uvs: Vec<Vec2>,
    parts: Vec<usize>,
}

fn nearest_k(points: &[Vec3], p: Vec3, k: usize) -> Vec<(usize, f32)> {
    let mut best: Vec<(usize, f32)> = Vec::with_capacity(k + 1);
    for (i, q) in points.iter().enumerate() {
        let d = q.distance_squared(p);
        if best.len() < k || d < best[best.len() - 1].1 {
            let at = best.partition_point(|b| b.1 <= d);
            best.insert(at, (i, d));
            best.truncate(k);
        }
    }
    best
}

fn fit(job: &Job) -> Result<Fitted, String> {
    // Body cages in world space.
    let mut surface = Surface {
        points: Vec::new(),
        uvs: Vec::new(),
        parts: Vec::new(),
    };
    let parts: Vec<(Ref, Xform)> = job.targets.iter().map(|t| (t.part, t.xf)).collect();
    for (ti, t) in job.targets.iter().enumerate() {
        let render = mesh(&t.mesh)?;
        let cage = mesh(&t.cage)?;
        let s = fit_scale(t.size, &render);
        for (p, uv) in cage_points(&cage) {
            surface.points.push(place(&t.xf, &t.origin, s, p));
            surface.uvs.push(uv);
            surface.parts.push(ti);
        }
    }
    let mut pieces = Vec::new();
    let mut garments = Vec::new();
    let mut matched = Vec::new();
    for l in &job.layers {
        let render = mesh(&l.mesh)?;
        let inner = mesh(&l.inner)?;
        let outer = mesh(&l.outer)?;
        let s = fit_scale(l.size, &render);
        let by_uv: HashMap<(i64, i64), usize> = surface
            .uvs
            .iter()
            .enumerate()
            .map(|(i, uv)| (uv_key(*uv), i))
            .collect();
        // Inner cage: where it is, and where its template point is on the target.
        let inner_pts = cage_points(&inner);
        let mut rest = Vec::with_capacity(inner_pts.len());
        let mut goal = Vec::with_capacity(inner_pts.len());
        let mut owner = Vec::with_capacity(inner_pts.len());
        let mut hits = 0;
        let mut on_target = Vec::with_capacity(inner_pts.len());
        for (p, uv) in &inner_pts {
            let at = place(&l.xf, &l.inner_origin, s, *p);
            on_target.push(by_uv.contains_key(&uv_key(*uv)));
            let j = match by_uv.get(&uv_key(*uv)) {
                Some(j) => {
                    hits += 1;
                    *j
                }
                // Not on the template: the nearest target point by UV.
                None => surface
                    .uvs
                    .iter()
                    .enumerate()
                    .min_by(|a, b| {
                        a.1.distance_squared(*uv)
                            .total_cmp(&b.1.distance_squared(*uv))
                    })
                    .map(|(j, _)| j)
                    .unwrap_or(0),
            };
            rest.push(at);
            goal.push(surface.points.get(j).copied().unwrap_or(at));
            owner.push(surface.parts.get(j).copied().unwrap_or(0));
        }
        matched.push((hits, inner_pts.len()));
        if rest.is_empty() {
            continue;
        }
        // Where the garment goes (Studio's cage field, cage_rbf.rs): radial
        // basis interpolation whose centres are the inner cage points found
        // on the target by UV (to their target point) and the outer cage
        // points paired with them (to that target point plus their height
        // over the inner cage; not × Puffiness). Inner points not on the target
        // aren't centres: snapping them to the nearest UV threw the sleeves
        // out (silhouette against Studio's jacket, AUDIT.md (d)).
        let outer_pts = cage_points(&outer);
        let corner_world: Vec<Vec3> = render
            .positions
            .iter()
            .map(|p| l.xf.apply((*p - (render.min + render.max) * 0.5) * s))
            .collect();
        let rbf_fit = {
            let mut c_rest = Vec::new();
            let mut c_goal = Vec::new();
            let mut by_uv_inner: HashMap<(i64, i64), (Vec3, Vec3)> = HashMap::new();
            for (k, (_, uv)) in inner_pts.iter().enumerate() {
                if on_target[k] {
                    c_rest.push(rest[k]);
                    c_goal.push(goal[k]);
                    by_uv_inner.insert(uv_key(*uv), (rest[k], goal[k]));
                }
            }
            for (p, uv) in &outer_pts {
                let at = place(&l.xf, &l.outer_origin, s, *p);
                if let Some((r, g)) = by_uv_inner.get(&uv_key(*uv)) {
                    c_rest.push(at);
                    c_goal.push(*g + (at - *r));
                }
            }
            // Each distinct render vertex once, then the outer cage.
            let mut uniq: Vec<Vec3> = Vec::new();
            let mut at: HashMap<[u32; 3], usize> = HashMap::new();
            let corner_ix: Vec<usize> = corner_world
                .iter()
                .map(|p| *at.entry(p.to_array().map(f32::to_bits)).or_insert_with(|| {
                    uniq.push(*p);
                    uniq.len() - 1
                }))
                .collect();
            let nu = uniq.len();
            uniq.extend(outer_pts.iter().map(|(p, _)| place(&l.xf, &l.outer_origin, s, *p)));
            rustudio_core::cage_rbf::fit_points(&c_rest, &c_goal, &uniq).map(|q| {
                let corners: Vec<Vec3> = corner_ix.iter().map(|i| q[*i]).collect();
                (corners, q[nu..].to_vec())
            })
        };
        // Fit a point bound to the inner cage: where it goes, its main
        // body part and its weight per body part (target index, weight).
        let fit_weighted = |p: Vec3| -> (Vec3, usize, Vec<(usize, f32)>) {
            let near = nearest_k(&rest, p, 4);
            let ws: Vec<f32> = near.iter().map(|(_, d)| 1.0 / (d + 1e-6)).collect();
            let total: f32 = ws.iter().sum();
            let (mut base, mut to) = (Vec3::ZERO, Vec3::ZERO);
            let mut per: Vec<(usize, f32)> = Vec::new();
            for ((i, _), w) in near.iter().zip(&ws) {
                base += rest[*i] * (w / total);
                to += goal[*i] * (w / total);
                let o = owner[*i];
                match per.iter_mut().find(|e| e.0 == o) {
                    Some(e) => e.1 += w / total,
                    None => per.push((o, w / total)),
                }
            }
            (to + (p - base), owner[near[0].0], per)
        };
        // Fit a point bound to the inner cage.
        let fit_point = |p: Vec3| -> (Vec3, usize) {
            let (q, o, _) = fit_weighted(p);
            (q, o)
        };
        // The garment itself, cut by body part.
        let mut by_part: HashMap<usize, Piece> = HashMap::new();
        for tri in 0..render.positions.len() / 3 {
            let mut fitted = [Vec3::ZERO; 3];
            let mut owners = [0usize; 3];
            for k in 0..3 {
                let i = tri * 3 + k;
                // The render mesh is drawn centred on its bounds.
                (fitted[k], owners[k]) = fit_point(corner_world[i]);
                if let Some((q, _)) = &rbf_fit {
                    fitted[k] = q[i];
                }
            }
            let o = if owners[1] == owners[2] {
                owners[1]
            } else {
                owners[0]
            };
            let (part, pxf) = parts[o];
            let inv = pxf.inverse();
            let piece = by_part.entry(o).or_insert_with(|| Piece {
                part,
                handle: l.handle,
                positions: Vec::new(),
                normals: Vec::new(),
                uvs: Vec::new(),
            });
            for k in 0..3 {
                let i = tri * 3 + k;
                piece.positions.push(inv.apply(fitted[k]));
                piece
                    .normals
                    .push(inv.rot * (l.xf.rot * render.normals.get(i).copied().unwrap_or(Vec3::Y)));
                piece
                    .uvs
                    .push(render.uvs.get(i).copied().unwrap_or_default());
            }
        }
        pieces.extend(by_part.into_values());
        // The same garment as one mesh skinned to the body parts.
        {
            let mut pos_w = Vec::with_capacity(render.positions.len());
            let mut infl: Vec<Vec<(usize, f32)>> = Vec::with_capacity(render.positions.len());
            let mut weight_by_part = vec![0f32; parts.len()];
            // The garment's own weights (Studio, AutoSkin Disabled: sleeve
            // markers weighted to the upper arm stay with it when the elbow
            // bends): each mesh bone is the body part of the same name, a
            // bone with no such part its nearest named ancestor's.
            let own = render.skin.as_ref().filter(|_| l.auto_skin != AutoSkin::EnabledOverride);
            let bone_part: Vec<Option<usize>> = own
                .map(|sk| {
                    (0..sk.bones.len())
                        .map(|b| {
                            let mut at = Some(b);
                            while let Some(i) = at {
                                if let Some(t) = job.targets.iter().position(|t| t.name == sk.bones[i].name) {
                                    return Some(t);
                                }
                                at = sk.bones[i].parent;
                            }
                            None
                        })
                        .collect()
                })
                .unwrap_or_default();
            let fallback = job
                .targets
                .iter()
                .position(|t| t.name == "LowerTorso")
                .or_else(|| job.targets.iter().position(|t| t.name == "UpperTorso"))
                .unwrap_or(0);
            for i in 0..render.positions.len() {
                let (q, _, mut per) = fit_weighted(corner_world[i]);
                let q = rbf_fit.as_ref().map_or(q, |(f, _)| f[i]);
                if let Some(sk) = own {
                    // Raw u8/255 weights, as a skinned MeshPart uses them.
                    per.clear();
                    for k in 0..4 {
                        let (b, w) = (sk.corner_bones[i][k] as usize, sk.corner_weights[i][k]);
                        if w <= 0.0 || b >= sk.bones.len() {
                            continue;
                        }
                        let t = bone_part[b].unwrap_or(fallback);
                        match per.iter_mut().find(|e| e.0 == t) {
                            Some(e) => e.1 += w,
                            None => per.push((t, w)),
                        }
                    }
                    for e in &per {
                        weight_by_part[e.0] += e.1;
                    }
                } else {
                    per.sort_by(|a, b| b.1.total_cmp(&a.1));
                    per.truncate(4);
                    let t: f32 = per.iter().map(|e| e.1).sum();
                    for e in &mut per {
                        e.1 /= t.max(1e-6);
                        weight_by_part[e.0] += e.1;
                    }
                }
                pos_w.push(q);
                infl.push(per);
            }
            // Drawn with the part the accessory is held to (scene.rs redraws
            // that part when the others move), else the one with most weight.
            let main = l
                .attached
                .and_then(|a| parts.iter().position(|p| p.0 == a))
                .unwrap_or_else(|| (0..parts.len()).max_by(|a, b| weight_by_part[*a].total_cmp(&weight_by_part[*b])).unwrap_or(0));
            let (owner_ref, owner_xf) = parts[main];
            let inv = owner_xf.inverse();
            let local: Vec<Vec3> = pos_w.iter().map(|p| inv.apply(*p)).collect();
            let (lo, hi) = local.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)));
            let center = (lo + hi) * 0.5;
            let extent = (hi - lo).max(Vec3::splat(1e-4));
            // Bones: the body parts used, at their place in the owner's space.
            let used: Vec<usize> = (0..parts.len()).filter(|i| weight_by_part[*i] > 0.0 || *i == main).collect();
            let bones: Vec<rustudio_core::mesh::SkinBone> = used
                .iter()
                .map(|&i| {
                    // (skin.rs binds at pos − center: the garment's own centre frame)
                    let b = parts[i].1;
                    let x = Xform { pos: inv.apply(b.pos), rot: inv.rot * b.rot };
                    rustudio_core::mesh::SkinBone { name: format!("{:?}", parts[i].0), parent: None, rot: x.rot, pos: x.pos }
                })
                .collect();
            let corner_bones = infl
                .iter()
                .map(|per| {
                    let mut ids = [0u16; 4];
                    for (k, e) in per.iter().enumerate() {
                        ids[k] = used.iter().position(|u| *u == e.0).unwrap_or(0) as u16;
                    }
                    ids
                })
                .collect();
            let corner_weights = infl
                .iter()
                .map(|per| {
                    let mut ws = [0f32; 4];
                    for (k, e) in per.iter().enumerate() {
                        ws[k] = e.1;
                    }
                    ws
                })
                .collect();
            let n = local.len();
            let base = crate::skin::Base {
                positions: local.iter().map(|p| (*p - center) / extent).collect(),
                normals: (0..n)
                    .map(|i| (inv.rot * (l.xf.rot * render.normals.get(i).copied().unwrap_or(Vec3::Y))) * extent)
                    .collect(),
                uvs: (0..n).map(|i| render.uvs.get(i).copied().unwrap_or_default()).collect(),
                colors: Vec::new(),
                extent,
                center,
                skin: rustudio_core::mesh::Skin {
                    bones,
                    corner_bones,
                    corner_weights,
                    corner_vertex: (0..n as u32).collect(),
                    vertex_bones: Vec::new(),
                    vertex_weights: Vec::new(),
                },
                firsts: (0..n as u32).collect(),
                corner_unique: (0..n as u32).collect(),
                facs: None,
                whole: true,
            };
            garments.push(Garment {
                handle: l.handle,
                owner: owner_ref,
                base: Arc::new(base),
                parts: used.iter().map(|&i| parts[i]).collect(),
                key: format!("wrap:{}:{:?}", GENERATION.load(Ordering::Relaxed), l.handle),
            });
        }
        // This layer's outer cage, fitted, is what the next layer wears.
        let mut next = Surface {
            points: Vec::new(),
            uvs: Vec::new(),
            parts: Vec::new(),
        };
        for (k, (p, uv)) in outer_pts.into_iter().enumerate() {
            let (q, o) = fit_point(place(&l.xf, &l.outer_origin, s, p));
            let q = rbf_fit.as_ref().map_or(q, |(_, f)| f[k]);
            next.points.push(q);
            next.uvs.push(uv);
            next.parts.push(o);
        }
        if !next.points.is_empty() {
            surface = next;
        }
    }
    Ok(Fitted { pieces, garments, matched })
}

/// This frame's palette for a garment (bone i: its body part's motion since
/// the fitting, in the garment's centre frame), or None while every part
/// keeps its fitting-time place relative to the owner.
pub fn garment_palette(doc: &Document, g: &Garment) -> Option<Vec<Xform>> {
    let owner_now = xform_of(doc, g.owner)?;
    let owner_fit = g.parts.iter().find(|p| p.0 == g.owner).map(|p| p.1)?;
    let (oi, fi) = (owner_now.inverse(), owner_fit.inverse());
    let c = g.base.center;
    let mut moved = false;
    let mut out = Vec::with_capacity(g.parts.len());
    for (part, fit) in &g.parts {
        let now = xform_of(doc, *part).unwrap_or(*fit);
        let a = Xform { pos: oi.apply(now.pos), rot: oi.rot * now.rot };
        let b = Xform { pos: fi.apply(fit.pos), rot: fi.rot * fit.rot };
        let bi = b.inverse();
        let m = Xform { pos: a.apply(bi.pos), rot: a.rot * bi.rot };
        moved |= m.pos.length_squared() > 1e-10 || !m.rot.abs_diff_eq(glam::Mat3::IDENTITY, 1e-6);
        out.push(Xform { pos: m.rot * c + m.pos - c, rot: m.rot });
    }
    let _ = moved;
    Some(out)
}
