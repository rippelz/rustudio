//! Studio's stored collision geometry for MeshParts and unions:
//! `PhysicalConfigData`, a shared string holding a `CSGPHS` blob. Studio's
//! spatial queries (Raycast, Spherecast, Blockcast, Shapecast) and physics
//! use these convex pieces, not the render mesh: a Hull MeshPart's ray hits
//! the hull's bevels, a Default one meets its decomposition, a Box one its
//! box. The blob also says which CollisionFidelity the part has (the place
//! file has no CollisionFidelity property for these classes).
//!
//! Layout, version 8 (Studio: encoder Studio, decoder
//! Studio, Edgebreaker compressor Studio):
//!
//! - `"CSGPHS"`, u32 version (8), u8 geometry type at byte 10 (1 Hull,
//!   2 Default decomposition, 3 PreciseConvexDecomposition; ≤ 7 accepted),
//!   one more byte, then a zstd frame. `"CSGPHS" 0 0 0 0 "BLOCK"` is a Box
//!   part: no pieces.
//! - Decompressed: u32 hull count, vertex count, triangle count, most vertices
//!   in one hull, most triangles in one hull, extra-block bytes, CLERS bit
//!   count, CLERS bytes, vertex bytes; f32×6 bounds; the extra block; the
//!   CLERS words; the vertices (f32×3, hull after hull, in the part's
//!   InitialSize space centred on its CFrame: Size / InitialSize scales them).
//! - CLERS: Rossignac's Edgebreaker codes per hull, C `0`, S `100`, R `101`,
//!   L `110`, E `111`, written most significant bit first into u32 words (the
//!   last, partial word right-aligned); the first triangle has no symbol. A
//!   hull's symbols end when its E's close every S (depth from 1 to 0); its
//!   vertex count is its C count + 3 (the code only splits the vertex list
//!   into hulls here: each piece is convex, so its faces are its hull's).
//! - Extra block (pieces Edgebreaker can't take, e.g. flat double-sided
//!   quads): u32 n, n u32 index offsets, the indices; u32 m, m u32 float
//!   offsets, the floats. Each range pairs an index list with
//!   a vertex list: an explicit triangle mesh.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant};

use crate::doc::Document;

/// Enum.CollisionFidelity values.
pub const FIDELITY_DEFAULT: u32 = 0;
pub const FIDELITY_HULL: u32 = 1;
pub const FIDELITY_BOX: u32 = 2;
pub const FIDELITY_PRECISE: u32 = 3;

/// One convex piece: its corners and its faces as outward planes (n·p ≤ d),
/// in the part's InitialSize space.
#[derive(Debug, Clone)]
pub struct Hull {
    pub points: Vec<Vec3>,
    pub planes: Vec<(Vec3, f32)>,
    pub lo: Vec3,
    pub hi: Vec3,
    /// No thickness (two opposite faces in one plane): Doorframe_a has
    /// such pieces. Rays crossing one hit it; a ray starting on one isn't
    /// inside it.
    pub flat: bool,
}

/// A part's decoded collision geometry.
#[derive(Debug)]
pub struct Pieces {
    /// Enum.CollisionFidelity the blob was made for.
    pub fidelity: u32,
    pub hulls: Vec<Hull>,
    /// Explicit triangles (the extra block), both windings stored.
    pub tris: Vec<[Vec3; 3]>,
}

/// What a PhysicalConfigData blob says.
#[derive(Debug, Clone)]
pub enum Geometry {
    /// Box fidelity: the part's box.
    Block,
    Pieces(Arc<Pieces>),
}

impl Geometry {
    pub fn fidelity(&self) -> u32 {
        match self {
            Geometry::Block => FIDELITY_BOX,
            Geometry::Pieces(p) => p.fidelity,
        }
    }
}

fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "truncated".to_string())
}

fn f32_at(b: &[u8], o: usize) -> Result<f32, String> {
    u32_at(b, o).map(f32::from_bits)
}

/// Per-hull vertex counts from the CLERS stream.
fn hull_sizes(words: &[u8], nbits: usize, hulls: usize) -> Result<Vec<usize>, String> {
    let nwords = nbits.div_ceil(32);
    let bit = |i: usize| -> Result<u8, String> {
        let w = i / 32;
        let word = u32_at(words, w * 4)?;
        let k = if w + 1 < nwords || nbits % 32 == 0 { 32 } else { nbits % 32 };
        Ok(((word >> (k - 1 - i % 32)) & 1) as u8)
    };
    let mut i = 0usize;
    let mut out = Vec::with_capacity(hulls);
    while out.len() < hulls {
        let mut depth = 1i32;
        let mut c = 0usize;
        while depth > 0 {
            if i >= nbits {
                return Err("CLERS stream ends inside a hull".into());
            }
            if bit(i)? == 0 {
                c += 1;
                i += 1;
                continue;
            }
            if i + 3 > nbits {
                return Err("CLERS stream ends inside a symbol".into());
            }
            match (bit(i + 1)?, bit(i + 2)?) {
                (0, 0) => depth += 1, // S
                (1, 1) => depth -= 1, // E
                _ => {}               // R, L
            }
            i += 3;
        }
        out.push(c + 3);
    }
    if i != nbits {
        return Err(format!("CLERS stream has {} bits left over", nbits - i));
    }
    Ok(out)
}

/// The outward face planes of a convex point set (parry's hull).
fn hull_of(points: Vec<Vec3>) -> Option<Hull> {
    if points.len() < 4 {
        return None;
    }
    let pts: Vec<_> = points.iter().map(|p| rapier3d::prelude::Vector::new(p.x, p.y, p.z)).collect();
    let (verts, faces) = rapier3d::parry::transformation::convex_hull(&pts);
    if faces.is_empty() {
        return None;
    }
    let v: Vec<Vec3> = verts.iter().map(|p| Vec3::new(p.x, p.y, p.z)).collect();
    let centre = v.iter().copied().sum::<Vec3>() / v.len() as f32;
    let mut planes: Vec<(Vec3, f32)> = Vec::with_capacity(faces.len());
    for f in &faces {
        let [a, b, c] = f.map(|i| v[i as usize]);
        let mut n = (b - a).cross(c - a);
        if n.length_squared() < 1e-14 {
            continue;
        }
        n = n.normalize();
        if n.dot(centre - a) > 0.0 {
            n = -n;
        }
        let d = n.dot(a);
        // Coplanar triangles give the same plane once.
        if !planes.iter().any(|(m, e)| m.dot(n) > 1.0 - 1e-5 && (e - d).abs() < 1e-5) {
            planes.push((n, d));
        }
    }
    let lo = points.iter().copied().fold(Vec3::splat(f32::MAX), Vec3::min);
    let hi = points.iter().copied().fold(Vec3::splat(f32::MIN), Vec3::max);
    let flat = planes
        .iter()
        .any(|(n, d)| planes.iter().any(|(m, e)| n.dot(*m) < -1.0 + 1e-5 && (d + e).abs() < 1e-4));
    Some(Hull { points, planes, lo, hi, flat })
}

/// Decodes a PhysicalConfigData blob (None: not a CSGPHS v8 blob this reads).
pub fn decode(blob: &[u8]) -> Result<Geometry, String> {
    if blob.len() < 6 || &blob[..6] != b"CSGPHS" {
        return Err("not a CSGPHS blob".into());
    }
    if blob.get(10..15) == Some(b"BLOCK") {
        return Ok(Geometry::Block);
    }
    let version = u32_at(blob, 6)?;
    if version != 8 || blob.len() < 12 {
        return Err(format!("CSGPHS version {version} not read"));
    }
    let fidelity = match blob[10] {
        1 => FIDELITY_HULL,
        2 => FIDELITY_DEFAULT,
        3 => FIDELITY_PRECISE,
        t => return Err(format!("geometry type {t}")),
    };
    let raw = zstd::stream::decode_all(&blob[12..]).map_err(|e| e.to_string())?;
    let h: Vec<usize> = (0..9).map(|i| u32_at(&raw, i * 4).map(|v| v as usize)).collect::<Result<_, _>>()?;
    let (nhull, nvert, ntri, extra, nbits, clers_bytes, vert_bytes) = (h[0], h[1], h[2], h[5], h[6], h[7], h[8]);
    let extra_at = 36 + 24;
    let clers_at = extra_at + extra;
    let vert_at = clers_at + clers_bytes;
    if vert_at + vert_bytes != raw.len() || vert_bytes != nvert * 12 {
        return Err("CSGPHS sections don't add up".into());
    }
    let sizes = hull_sizes(&raw[clers_at..vert_at], nbits, nhull)?;
    if sizes.iter().sum::<usize>() != nvert || sizes.iter().map(|v| 2 * v - 4).sum::<usize>() != ntri {
        return Err("CSGPHS hull sizes don't match the header".into());
    }
    let vert = |i: usize| -> Result<Vec3, String> {
        let o = vert_at + i * 12;
        Ok(Vec3::new(f32_at(&raw, o)?, f32_at(&raw, o + 4)?, f32_at(&raw, o + 8)?))
    };
    let mut hulls = Vec::with_capacity(nhull);
    let mut at = 0usize;
    for n in sizes {
        let pts: Vec<Vec3> = (at..at + n).map(vert).collect::<Result<_, _>>()?;
        at += n;
        if let Some(hull) = hull_of(pts) {
            hulls.push(hull);
        }
    }
    let tris = if extra > 0 { extra_tris(&raw[extra_at..clers_at])? } else { Vec::new() };
    Ok(Geometry::Pieces(Arc::new(Pieces { fidelity, hulls, tris })))
}

/// The extra block's explicit triangle meshes.
fn extra_tris(b: &[u8]) -> Result<Vec<[Vec3; 3]>, String> {
    let mut o = 0usize;
    let ranges = |o: &mut usize| -> Result<Vec<usize>, String> {
        let n = u32_at(b, *o)? as usize;
        *o += 4;
        let offs: Vec<usize> = (0..n).map(|i| u32_at(b, *o + i * 4).map(|v| v as usize)).collect::<Result<_, _>>()?;
        *o += n * 4;
        Ok(offs)
    };
    let idx_offs = ranges(&mut o)?;
    let idx_at = o;
    o += idx_offs.last().copied().unwrap_or(0) * 4;
    let flt_offs = ranges(&mut o)?;
    let flt_at = o;
    let mut out = Vec::new();
    for k in 0..idx_offs.len().saturating_sub(1) {
        let (i0, i1) = (idx_offs[k], idx_offs[k + 1]);
        let f0 = *flt_offs.get(k).ok_or("extra block ranges differ")?;
        let v = |i: usize| -> Result<Vec3, String> {
            let p = flt_at + (f0 + i * 3) * 4;
            Ok(Vec3::new(f32_at(b, p)?, f32_at(b, p + 4)?, f32_at(b, p + 8)?))
        };
        for t in (i0..i1).step_by(3) {
            let ix = [t, t + 1, t + 2].map(|j| u32_at(b, idx_at + j * 4).map(|v| v as usize));
            let [a, bb, c] = ix;
            out.push([v(a?)?, v(bb?)?, v(c?)?]);
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Registry {
    /// Blob hash → its geometry and registry id (0 for Box / unreadable).
    by_blob: HashMap<Vec<u8>, (Option<Geometry>, u32)>,
    /// Id − 1 → pieces.
    pieces: Vec<Arc<Pieces>>,
    /// (id, Size / InitialSize bits, Size bits) → the sweep shapes.
    shapes: HashMap<(u32, [u32; 3], [u32; 3]), Arc<Vec<rapier3d::prelude::SharedShape>>>,
}

fn registry() -> &'static Mutex<Registry> {
    static C: OnceLock<Mutex<Registry>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// A part's collision geometry from its PhysicalConfigData (decoded once per
/// distinct blob) and its registry id for `pieces` / `shapes` (0: none), or
/// None when it has no blob this reads.
pub fn of_part_id(d: &Document, r: Ref) -> Option<(Geometry, u32)> {
    let Some(Variant::SharedString(s)) = d.prop(r, "PhysicalConfigData") else {
        return None;
    };
    let key = s.hash().as_bytes().to_vec();
    let mut reg = registry().lock().ok()?;
    if let Some((g, id)) = reg.by_blob.get(&key) {
        return g.clone().map(|g| (g, *id));
    }
    let g = decode(s.data()).ok();
    let id = match &g {
        Some(Geometry::Pieces(p)) => {
            reg.pieces.push(p.clone());
            reg.pieces.len() as u32
        }
        _ => 0,
    };
    reg.by_blob.insert(key, (g.clone(), id));
    g.map(|g| (g, id))
}

pub fn of_part(d: &Document, r: Ref) -> Option<Geometry> {
    of_part_id(d, r).map(|(g, _)| g)
}

/// The pieces registered under `id` (from `of_part_id`).
pub fn pieces(id: u32) -> Option<Arc<Pieces>> {
    let reg = registry().lock().ok()?;
    reg.pieces.get((id as usize).checked_sub(1)?).cloned()
}

/// The pieces of `id` stretched by `scale` (to a part of `size`) and pulled
/// in by SWEEP_SHRINK, as parry shapes (convex hulls, then the explicit
/// triangles) in the part's space, for sweeps; cached.
pub fn shapes(id: u32, scale: Vec3, size: Vec3) -> Option<Arc<Vec<rapier3d::prelude::SharedShape>>> {
    use rapier3d::prelude::{SharedShape, Vector};
    let key = (id, scale.to_array().map(f32::to_bits), size.to_array().map(f32::to_bits));
    if let Some(s) = registry().lock().ok()?.shapes.get(&key) {
        return Some(s.clone());
    }
    let p = pieces(id)?;
    let half = size * 0.5;
    let pull = Vec3::select(half.cmpgt(Vec3::splat(SWEEP_SHRINK)), (half - SWEEP_SHRINK) / half, Vec3::ZERO);
    let k = scale * pull;
    let v = |q: Vec3| Vector::new(q.x * k.x, q.y * k.y, q.z * k.z);
    let mut out: Vec<SharedShape> = p
        .hulls
        .iter()
        .filter_map(|h| SharedShape::convex_hull(&h.points.iter().map(|q| v(*q)).collect::<Vec<_>>()))
        .collect();
    out.extend(p.tris.iter().map(|t| SharedShape::triangle(v(t[0]), v(t[1]), v(t[2]))));
    let out = Arc::new(out);
    let mut reg = registry().lock().ok()?;
    if reg.shapes.len() > 4096 {
        reg.shapes.clear();
    }
    reg.shapes.insert(key, out.clone());
    Some(out)
}

/// Studio's Spherecast / Blockcast / Shapecast meet a part's pieces pulled
/// in towards the part's centre: each axis scaled by (half − 0.05) / half,
/// half being the part's half size on that axis (a Hull TrimWall 2 studs
/// wide: a sphere stops at x = 0.95 where the ray meets x = 1, at its 20-stud
/// end at 9.95; its 45° bevel moves only 0.0045, and the contact on the
/// bevel's corner is the scaled corner (0.869, 1.0) of (0.915, 1.05)).
/// Rays meet the pieces as stored.
pub const SWEEP_SHRINK: f32 = 0.05;

/// A ray (world origin, unit direction, length) against a part's pieces
/// (part pose `xf`, stretched by `scale`): the nearest piece it enters, as
/// (distance, world normal). A ray that starts inside any piece misses the
/// whole part, as it misses a box it starts in (Studio: from inside one of
/// Doorframe_a's pieces a ray passes the pieces beside it).
pub fn ray(p: &Pieces, xf: &crate::math::Xform, scale: Vec3, o: Vec3, dir: Vec3, len: f32) -> Option<(f32, Vec3)> {
    let inv = xf.rot.transpose();
    let s = scale.max(Vec3::splat(1e-6));
    let lo = (inv * (o - xf.pos)) / s;
    let ld = (inv * dir) / s;
    // On a piece's surface counts as inside (Studio: from a point on a
    // PlanterCenter piece's face the ray passes the whole union).
    let inside = |h: &Hull| {
        !h.flat
            && lo.cmpge(h.lo - 1e-4).all()
            && lo.cmple(h.hi + 1e-4).all()
            && h.planes.iter().all(|(n, d)| n.dot(lo) <= *d + 1e-5)
    };
    if p.hulls.iter().any(inside) {
        return None;
    }
    let mut best: Option<(f32, Vec3)> = None;
    for h in &p.hulls {
        // Bounds first.
        let (mut a, mut b) = (0.0f32, best.map_or(len, |b| b.0));
        let mut ok = true;
        for k in 0..3 {
            if ld[k].abs() < 1e-12 {
                if lo[k] < h.lo[k] - 1e-4 || lo[k] > h.hi[k] + 1e-4 {
                    ok = false;
                    break;
                }
            } else {
                let (t1, t2) = ((h.lo[k] - 1e-4 - lo[k]) / ld[k], (h.hi[k] + 1e-4 - lo[k]) / ld[k]);
                a = a.max(t1.min(t2));
                b = b.min(t1.max(t2));
            }
        }
        if !ok || a > b {
            continue;
        }
        let mut enter = f32::NEG_INFINITY;
        let mut exit = f32::INFINITY;
        for (n, d) in &h.planes {
            let denom = n.dot(ld);
            let dist = d - n.dot(lo);
            if denom.abs() < 1e-12 {
                if dist < 0.0 {
                    enter = f32::INFINITY;
                    break;
                }
                continue;
            }
            let t = dist / denom;
            if denom < 0.0 {
                enter = enter.max(t);
            } else if t < exit {
                exit = t;
            }
        }
        // Only touching an edge or corner (no length inside) is a miss;
        // crossing a flat piece is a hit.
        let chord = (exit - enter) * (ld * s).length();
        if enter.is_finite() && enter >= 0.0 && (h.flat || chord > 1e-4) && enter <= len && best.is_none_or(|b| enter < b.0) {
            // The faces entered there: through an edge or corner, their
            // normals summed (as Studio does for a box's).
            let at = lo + ld * enter;
            let n: Vec3 = h
                .planes
                .iter()
                .filter(|(n, d)| n.dot(ld) < -1e-12 && (n.dot(at) - d).abs() <= 1e-4)
                .map(|(n, _)| *n / s)
                .map(|n| n.normalize_or_zero())
                .sum();
            best = Some((enter, (xf.rot * n).normalize_or_zero()));
        }
    }
    for t in &p.tris {
        let (e1, e2) = (t[1] - t[0], t[2] - t[0]);
        let q = ld.cross(e2);
        let det = e1.dot(q);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv_det = 1.0 / det;
        let sv = lo - t[0];
        let u = sv.dot(q) * inv_det;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let r = sv.cross(e1);
        let w = ld.dot(r) * inv_det;
        if w < 0.0 || u + w > 1.0 {
            continue;
        }
        let dist = e2.dot(r) * inv_det;
        if dist >= 0.0 && dist <= len && best.is_none_or(|b| dist < b.0) {
            let mut n = e1.cross(e2);
            if n.dot(ld) > 0.0 {
                n = -n;
            }
            best = Some((dist, (xf.rot * (n / s)).normalize_or_zero()));
        }
    }
    best
}

/// Physics' collider from the pieces registered under `id`, stretched by
/// `scale` (Size / InitialSize): Studio's own convex pieces (its physics
/// collides with these, not with a decomposition of the render mesh), as a
/// compound; flat pieces and the explicit triangles are left out (they hold
/// no volume). With `touch_only`, one hull around every piece (a moving
/// CanCollide=false part only reports touches). Cached.
pub fn collider_shape(id: u32, scale: Vec3, touch_only: bool) -> Option<rapier3d::prelude::SharedShape> {
    use rapier3d::prelude::{Pose, SharedShape, Vector};
    type Key = (u32, [u32; 3], bool);
    static SHAPES: OnceLock<Mutex<HashMap<Key, Option<SharedShape>>>> = OnceLock::new();
    let key = (id, scale.to_array().map(f32::to_bits), touch_only);
    let cache = SHAPES.get_or_init(Default::default);
    if let Some(s) = cache.lock().ok()?.get(&key) {
        return s.clone();
    }
    let p = pieces(id)?;
    let v = |q: &Vec3| Vector::new(q.x * scale.x, q.y * scale.y, q.z * scale.z);
    let solid: Vec<&Hull> = p.hulls.iter().filter(|h| !h.flat && h.points.len() >= 4).collect();
    let shape = if touch_only {
        let all: Vec<Vector> = solid.iter().flat_map(|h| h.points.iter().map(v)).collect();
        SharedShape::convex_hull(&all)
    } else {
        let mut parts: Vec<(Pose, SharedShape)> = solid
            .iter()
            .filter_map(|h| SharedShape::convex_hull(&h.points.iter().map(v).collect::<Vec<_>>()))
            .map(|s| (Pose::IDENTITY, s))
            .collect();
        match parts.len() {
            0 => None,
            1 => parts.pop().map(|(_, s)| s),
            _ => Some(SharedShape::compound(parts)),
        }
    };
    if let Ok(mut c) = cache.lock() {
        if c.len() > 4096 {
            c.clear();
        }
        c.insert(key, shape.clone());
    }
    shape
}

/// Enum.CollisionFidelity of a MeshPart / union: the property when set,
/// else what its PhysicalConfigData was made for (Studio derives it from
/// the blob: the file doesn't store the property).
pub fn fidelity(d: &Document, r: Ref) -> Option<u32> {
    if let Some(Variant::Enum(e)) = d.dom.get_by_ref(r).and_then(|i| i.properties.get(&rbx_dom_weak::ustr("CollisionFidelity"))) {
        return Some(e.to_u32());
    }
    of_part(d, r).map(|g| g.fidelity())
}

/// InitialSize → Size scale for the pieces (a flat axis keeps 1).
pub fn scale(initial: Vec3, size: Vec3) -> Vec3 {
    let s = |i: f32, z: f32| if i.abs() > 1e-5 { z / i } else { 1.0 };
    Vec3::new(s(initial.x, size.x), s(initial.y, size.y), s(initial.z, size.z))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-hull blob: a 2×2×2 cube encoded as Studio does (8 vertices,
    /// 12 triangles, 23 CLERS bits from Laser Tag's Trim_Kit_A blob).
    pub(crate) fn cube_blob() -> Vec<u8> {
        let verts: [[f32; 3]; 8] = [
            [-1.0, 1.0, -1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, -1.0],
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let mut raw = Vec::new();
        for v in [1u32, 8, 12, 8, 12, 0, 23, 4, 96] {
            raw.extend(v.to_le_bytes());
        }
        for v in [-1.0f32, -1.0, -1.0, 1.0, 1.0, 1.0] {
            raw.extend(v.to_bits().to_le_bytes());
        }
        // The CLERS word of Laser Tag's blob 26 (one partial word).
        raw.extend(0x0014_5bb7u32.to_le_bytes());
        for v in verts.iter().flatten() {
            raw.extend(v.to_bits().to_le_bytes());
        }
        let mut blob = b"CSGPHS".to_vec();
        blob.extend(8u32.to_le_bytes());
        blob.extend([2u8, 0]);
        blob.extend(zstd::stream::encode_all(&raw[..], 3).unwrap());
        blob
    }

    #[test]
    fn decodes_a_one_hull_blob() {
        let Geometry::Pieces(p) = decode(&cube_blob()).unwrap() else { panic!("block") };
        assert_eq!(p.fidelity, FIDELITY_DEFAULT);
        assert_eq!(p.hulls.len(), 1);
        assert_eq!(p.hulls[0].points.len(), 8);
        assert_eq!(p.hulls[0].planes.len(), 6);
        assert_eq!(p.hulls[0].lo, Vec3::splat(-1.0));
    }

    #[test]
    fn block_blob_is_box_fidelity() {
        let g = decode(b"CSGPHS\0\0\0\0BLOCK").unwrap();
        assert_eq!(g.fidelity(), FIDELITY_BOX);
    }

    #[test]
    fn clers_splits_hulls() {
        // Two cubes back to back: "C C E C C C E E R R S" is 23 bits each…
        // here two copies of the 23-bit cube stream in one 46-bit stream.
        let one: u32 = 0x0014_5bb7;
        let bits: Vec<u8> = (0..23).rev().map(|i| ((one >> i) & 1) as u8).collect();
        let all: Vec<u8> = bits.iter().chain(bits.iter()).copied().collect();
        let mut words = vec![0u32; 2];
        for (i, b) in all.iter().enumerate() {
            let w = i / 32;
            let k = if w == 0 { 32 } else { 46 - 32 };
            words[w] |= (*b as u32) << (k - 1 - i % 32);
        }
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(hull_sizes(&bytes, 46, 2).unwrap(), vec![8, 8]);
    }
}
