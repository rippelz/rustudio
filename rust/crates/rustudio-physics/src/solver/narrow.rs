//! Studio's own contact generation (GEOM.md; addresses 0.744's): the FeaturePoly
//! paths for block–block (BlockBlockContact slot 18 Studio, GEOM §2) and
//! block / wedge / corner-wedge pairs (PolyPolyContact Studio, §3), and the
//! ball paths (BallBall Studio, BallPolyMesh Studio, §5). All f32, in the
//! Studio' order of operations where GEOM / Studio gives it.
//!
//! rapier / parry keep the broad phase and pair finding; cylinders, meshes, unions
//! and terrain (Studio's Bullet path, GEOM §4, not specified far enough) still take
//! parry's manifolds (contact.rs).
//!
//! A frame is Studio's CFrame layout: R row-major (R[3r+c]; body axis k = column k)
//! then the position (`Frame::m[9..12]`).

use glam::{Quat, Vec3};
use rapier3d::prelude as rp;

/// Geometry classes (GEOM §1.1, Studio).
pub const CLASS_BALL: u8 = 1;
pub const CLASS_BLOCK: u8 = 2;
pub const CLASS_POLY: u8 = 3;

/// Poly mesh kinds (geometry vtbl, GEOM §3.1).
pub const MESH_BLOCK: u8 = 0;
pub const MESH_WEDGE: u8 = 1;
pub const MESH_CORNER: u8 = 2;

/// Connector feature kinds (conn): 0 face vertex (box), 1 edge–edge, 2 clip
/// point, 3 poly edge point, 4 poly vertex point; BALL = the ball paths' one
/// connector (contact).
pub const KIND_BALL: u8 = 5;

/// .rdata Studio: the size clamp of block half extents.
const MIN_SIZE: f32 = 0.05;

#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub m: [f32; 12],
}

impl Frame {
    pub fn from_pose(rot: Quat, pos: Vec3) -> Frame {
        let r = glam::Mat3::from_quat(rot);
        let (c0, c1, c2) = (r.x_axis, r.y_axis, r.z_axis);
        Frame { m: [c0.x, c1.x, c2.x, c0.y, c1.y, c2.y, c0.z, c1.z, c2.z, pos.x, pos.y, pos.z] }
    }
    #[inline]
    pub fn pos(&self) -> Vec3 {
        Vec3::new(self.m[9], self.m[10], self.m[11])
    }
    /// Body axis k in the world (column k; Studio reads// + 4k).
    #[inline]
    pub fn axis(&self, k: usize) -> Vec3 {
        Vec3::new(self.m[k], self.m[3 + k], self.m[6 + k])
    }
    /// toLocal Studio: Rᵀ·v, x = (v.y·R3 + v.x·R0) + v.z·R6, y = (v.x·R1 + v.y·R4) +
    /// v.z·R7, z = (v.x·R2 + v.y·R5) + v.z·R8.
    #[inline]
    pub fn to_local(&self, v: Vec3) -> Vec3 {
        let r = &self.m;
        Vec3::new((v.y * r[3] + v.x * r[0]) + v.z * r[6], (v.x * r[1] + v.y * r[4]) + v.z * r[7], (v.x * r[2] + v.y * r[5]) + v.z * r[8])
    }
    /// R·v + pos, in Studio's output order: x = ((v.z·R2 + v.y·R1) + v.x·R0) + px.
    #[inline]
    pub fn to_world(&self, v: Vec3) -> Vec3 {
        let r = &self.m;
        Vec3::new(((v.z * r[2] + v.y * r[1]) + v.x * r[0]) + r[9], ((v.z * r[5] + v.y * r[4]) + v.x * r[3]) + r[10], ((v.z * r[8] + v.y * r[7]) + v.x * r[6]) + r[11])
    }
    /// R·v (no translation), same order.
    #[inline]
    pub fn rot(&self, v: Vec3) -> Vec3 {
        let r = &self.m;
        Vec3::new((v.z * r[2] + v.y * r[1]) + v.x * r[0], (v.z * r[5] + v.y * r[4]) + v.x * r[3], (v.z * r[8] + v.y * r[7]) + v.x * r[6])
    }
    /// inverse Studio: Rᵀ, t' = 0 − ((t1·R3 + t0·R0) + t2·R6) …
    pub fn inverse(&self) -> Frame {
        let p = &self.m;
        let (t0, t1, t2) = (p[9], p[10], p[11]);
        Frame {
            m: [
                p[0], p[3], p[6], p[1], p[4], p[7], p[2], p[5], p[8],
                0.0 - ((t1 * p[3] + t0 * p[0]) + t2 * p[6]),
                0.0 - ((t1 * p[4] + t0 * p[1]) + t2 * p[7]),
                0.0 - ((t1 * p[5] + t0 * p[2]) + t2 * p[8]),
            ],
        }
    }
    /// self · b, Studio(self, out, b).
    pub fn mul(&self, b: &Frame) -> Frame {
        let a = &self.m;
        let b = &b.m;
        Frame {
            m: [
                (a[1] * b[3] + a[0] * b[0]) + a[2] * b[6],
                (a[1] * b[4] + a[0] * b[1]) + a[2] * b[7],
                (a[1] * b[5] + a[0] * b[2]) + a[2] * b[8],
                (a[4] * b[3] + a[3] * b[0]) + a[5] * b[6],
                (a[4] * b[4] + a[3] * b[1]) + a[5] * b[7],
                (a[4] * b[5] + a[3] * b[2]) + a[5] * b[8],
                (a[7] * b[3] + a[6] * b[0]) + a[8] * b[6],
                (a[7] * b[4] + a[6] * b[1]) + a[8] * b[7],
                (a[7] * b[5] + a[6] * b[2]) + a[8] * b[8],
                ((a[1] * b[10] + a[0] * b[9]) + a[2] * b[11]) + a[9],
                ((a[4] * b[10] + a[3] * b[9]) + a[5] * b[11]) + a[10],
                ((a[7] * b[10] + a[6] * b[9]) + a[8] * b[11]) + a[11],
            ],
        }
    }
}

/// One primitive as the narrow phase sees it.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub class: u8,
    pub mesh: u8,
    /// Full size (poly meshes use it unclamped, GEOM §3.1 / Open 5).
    pub size: Vec3,
    /// Clamped half extents max(size, 0.05)·0.5 (GEOM conventions, §1.3).
    pub h: Vec3,
    /// Ball radius (prim).
    pub radius: f32,
    pub f: Frame,
}

/// The part's CFrame as Studio's narrow phase reads it (prim). For a body with
/// Studio's own pose (STEP §4.2 [L2]) the rotation is the body's f32 matrix (the
/// write-back Studio copies R into the part CFrame) and the position
/// the one written to rapier; a collider offset in its body (welded parts) is composed
/// with Studio / the packed A·v (the weld chain's own order is not read). Else
/// rapier's quaternion pose.
pub fn part_frame(co: &rp::Collider, bodies: &super::body::BodySet) -> Frame {
    use super::body::{flag, m3};
    if let Some(s) = co.parent().map(|h| bodies.slot(h)).filter(|&s| s != super::body::NONE) {
        let s = s as usize;
        if bodies.flags[s] & flag::STUDIO_POSE != 0 {
            let r = m3::from_glam(&bodies.mat[s]);
            let p = bodies.pos[s];
            let (r, p) = match co.position_wrt_parent() {
                Some(l) if l.rotation != rp::Rotation::IDENTITY || l.translation != rp::Vector::ZERO => {
                    let lr = m3::from_glam(&glam::Mat3::from_quat(super::body::q(l.rotation)));
                    (m3::mul(&r, &lr), m3::mul_vec(&r, super::body::v(l.translation)) + p)
                }
                _ => (r, p),
            };
            return Frame { m: [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], p.x, p.y, p.z] };
        }
    }
    let pose = co.position();
    Frame::from_pose(super::body::q(pose.rotation), super::body::v(pose.translation))
}

/// The part shape behind a collider made by physics.rs `collider` (cuboid, ball, or
/// the wedge / corner-wedge hulls — the same vertex tables as GEOM §3.1); None for
/// everything else (cylinders, meshes, unions, terrain → parry).
pub fn shape_of(co: &rp::Collider, bodies: &super::body::BodySet) -> Option<Shape> {
    let f = part_frame(co, bodies);
    shape_parts(co.shape()).map(|(class, mesh, size, radius)| Shape { class, mesh, size, h: clamp_half(size), radius, f })
}

/// Whether narrow.rs makes the contacts of a pair of these two shapes (`shape_of` +
/// `handles`, without a frame): the rapier hook that skips parry's manifolds for them.
pub fn handles_shapes(a: &dyn rp::Shape, b: &dyn rp::Shape) -> bool {
    match (shape_parts(a), shape_parts(b)) {
        (Some(x), Some(y)) => x.0 <= CLASS_POLY && y.0 <= CLASS_POLY,
        _ => false,
    }
}

/// The class `handles_shapes` tests (0 = not narrow.rs's), for the per-collider cache.
pub fn handled_class(a: &dyn rp::Shape) -> u8 {
    shape_parts(a).map_or(0, |x| if x.0 <= CLASS_POLY { x.0 } else { 0 })
}

/// (class, mesh, size, ball radius) of a part shape; None for shapes narrow.rs does not know.
fn shape_parts(sh: &dyn rp::Shape) -> Option<(u8, u8, Vec3, f32)> {
    if let Some(c) = sh.as_cuboid() {
        let size = super::body::v(c.half_extents) * 2.0;
        return Some((CLASS_BLOCK, MESH_BLOCK, size, 0.0));
    }
    if let Some(b) = sh.as_ball() {
        let size = Vec3::splat(b.radius * 2.0);
        return Some((CLASS_BALL, MESH_BLOCK, size, b.radius));
    }
    let poly = sh.as_convex_polyhedron()?;
    let pts = poly.points();
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for p in pts {
        let p = super::body::v(*p);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let size = hi - lo;
    let mid = (hi + lo) * 0.5;
    if mid.abs().max_element() > 1e-4 * size.max_element().max(1.0) {
        return None;
    }
    let sign = |p: Vec3| -> [i8; 3] { [if p.x > 0.0 { 1 } else { -1 }, if p.y > 0.0 { 1 } else { -1 }, if p.z > 0.0 { 1 } else { -1 }] };
    let has = |table: &[[i8; 3]]| pts.len() == table.len() && table.iter().all(|t| pts.iter().any(|p| sign(super::body::v(*p)) == *t));
    let mesh = if has(&WEDGE_SIGNS) {
        MESH_WEDGE
    } else if has(&CORNER_SIGNS) {
        MESH_CORNER
    } else {
        return None;
    };
    Some((CLASS_POLY, mesh, size, 0.0))
}

/// Wedge vertices (.rdata Studio, × size·0.5 signs): v0 (+,+,+) v1 (+,−,+)
/// v2 (+,−,−) v3 (−,+,+) v4 (−,−,+) v5 (−,−,−) (GEOM §3.1).
pub const WEDGE_SIGNS: [[i8; 3]; 6] = [[1, 1, 1], [1, -1, 1], [1, -1, -1], [-1, 1, 1], [-1, -1, 1], [-1, -1, -1]];
/// Corner wedge: v0 (+,−,+) v1 (+,−,−) v2 (−,−,−) v3 (−,−,+) v4 (+,+,−).
pub const CORNER_SIGNS: [[i8; 3]; 5] = [[1, -1, 1], [1, -1, -1], [-1, -1, -1], [-1, -1, 1], [1, 1, -1]];

#[inline]
fn clamp_half(s: Vec3) -> Vec3 {
    // s = (0.05 < s) ? s : 0.05; h = s·0.5.
    let c = |x: f32| if MIN_SIZE < x { x } else { MIN_SIZE };
    Vec3::new(c(s.x) * 0.5, c(s.y) * 0.5, c(s.z) * 0.5)
}

// ------------------------------------------------------------------ temps

/// One stack temporary connector (Studio step 3): the manifold point and its
/// features. `a_first`: the connector's A is the contact's A. `row_at_p`: ctor arg 1
/// (row point = p, CONTACT §1.1), else the row point is p + n·sep/2.
#[derive(Debug, Clone, Copy)]
pub struct Temp {
    pub p: Vec3,
    pub n: Vec3,
    pub sep: f32,
    pub kind: u8,
    pub id_a: i32,
    pub id_b: i32,
    pub a_first: bool,
    pub row_at_p: bool,
}

impl Default for Temp {
    fn default() -> Self {
        Temp { p: Vec3::ZERO, n: Vec3::ZERO, sep: 0.0, kind: 0, id_a: -1, id_b: -1, a_first: true, row_at_p: false }
    }
}

/// The eight temporaries, n = 0 = free.
#[derive(Debug, Clone, Copy, Default)]
pub struct Temps {
    pub t: [Temp; 8],
}

impl Temps {
    pub fn clear(&mut self) {
        for t in &mut self.t {
            t.n = Vec3::ZERO;
        }
    }
    /// Studio: the first temp with n == 0, or temp 7; its A set to `a_first`.
    pub(crate) fn take(&mut self, a_first: bool, kind: u8) -> &mut Temp {
        let i = self.t.iter().position(|t| t.n == Vec3::ZERO).unwrap_or(7);
        let t = &mut self.t[i];
        t.kind = kind;
        t.a_first = a_first;
        t.row_at_p = false;
        t
    }
}

// ------------------------------------------------------------------ pair state

/// Persistent per-pair narrow-phase state: BlockBlock contact..
/// (startBody, startAxis, featA, featB; init 0xffff0000) and PolyPoly's separating
/// feature (contact).
#[derive(Debug, Clone, Copy)]
pub struct PairState {
    pub start_body: u8,
    pub start_axis: u8,
    pub feat_a: u8,
    pub feat_b: u8,
    pub fv: Option<FaceVertex>,
}

impl Default for PairState {
    fn default() -> Self {
        PairState { start_body: 0, start_axis: 0, feat_a: 0xff, feat_b: 0xff, fv: None }
    }
}

/// FaceVertex feature (GEOM §3.2): P's face against Q's vertex / face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceVertex {
    /// P is the contact's A.
    pub p_is_a: bool,
    pub face_p: i32,
    pub face_q: i32,
    pub alt_q: i32,
}

/// Contact A of a pair (GEOM §1.2): the lower class; for equal classes the smaller
/// u64 at prim (not identified, GEOM Open 6) — taken as the part's creation
/// order (fitted: stack3 tick 3 matches Studio to 1e-5 only with the plate, created
/// first, as A; RUSTUDIO_SOLVER_PAIR_A=high flips it).
pub fn first_is_a(s1: &Shape, s2: &Shape, c1: u64, c2: u64) -> bool {
    static HIGH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let high = *HIGH.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_PAIR_A").is_ok_and(|v| v == "high"));
    if s1.class != s2.class {
        return s1.class < s2.class;
    }
    (c1 < c2) != high
}

/// Whether narrow.rs makes this pair's contacts (else parry's manifolds are used).
pub fn handles(a: &Shape, b: &Shape) -> bool {
    a.class <= CLASS_POLY && b.class <= CLASS_POLY
}

/// The pair's world AABBs overlap (Studio step 2, Studio boxes).
fn aabb_overlap(a: &Shape, b: &Shape) -> bool {
    let ext = |s: &Shape| {
        let e = if s.class == CLASS_BALL { Vec3::splat(s.radius) } else { s.size * 0.5 };
        let r = &s.f.m;
        Vec3::new(
            r[0].abs() * e.x + r[1].abs() * e.y + r[2].abs() * e.z,
            r[3].abs() * e.x + r[4].abs() * e.y + r[5].abs() * e.z,
            r[6].abs() * e.x + r[7].abs() * e.y + r[8].abs() * e.z,
        )
    };
    let (ea, eb) = (ext(a), ext(b));
    let d = (a.f.pos() - b.f.pos()).abs();
    d.x <= ea.x + eb.x && d.y <= ea.y + eb.y && d.z <= ea.z + eb.z
}

/// Contact slot 6 for a pair narrow.rs handles: `a`, `b` = contact A, B. Fills
/// `out` (cleared first) and returns whether any point was made.
pub fn collide(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, out: &mut Temps) -> bool {
    out.clear();
    match (a.class, b.class) {
        (CLASS_BALL, CLASS_BALL) => ball_ball(a, b, margin, out),
        (CLASS_BALL, _) => ball_poly(a, b, margin, out),
        _ => {
            if !aabb_overlap(a, b) {
                return false;
            }
            if a.class == CLASS_BLOCK && b.class == CLASS_BLOCK {
                block_block(a, b, st, margin, out)
            } else {
                super::narrow_poly::poly_poly(a, b, st, margin, out)
            }
        }
    }
}

// ------------------------------------------------------------------ block–block

/// Face → vertices (.rdata Studio), CCW from outside.
pub const FV: [[u8; 4]; 6] = [[0, 2, 3, 1], [0, 1, 5, 4], [0, 4, 6, 2], [4, 5, 7, 6], [2, 6, 7, 3], [1, 3, 7, 5]];
/// Face 2-D bases (.rdata Studio): rows u, v, w (w = outward normal).
const BASIS: [[f32; 9]; 6] = [
    [0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0],
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0],
    [0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, -1.0],
];

/// Vertex code c (GEOM §1.3): bit 2 → −x, bit 1 → −y, bit 0 → −z.
#[inline]
pub fn vertex(code: u8, h: Vec3) -> Vec3 {
    Vec3::new(if code & 4 != 0 { -h.x } else { h.x }, if code & 2 != 0 { -h.y } else { h.y }, if code & 1 != 0 { -h.z } else { h.z })
}

/// Studio (GEOM §2).
fn block_block(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, out: &mut Temps) -> bool {
    let mut best = f32::INFINITY;
    if !face_axes(a, b, st, margin, &mut best) {
        st.feat_a = 0xff;
        st.feat_b = 0xff;
        return false;
    }
    // .rdata Studio = 1/1.01.
    best *= (1.0f64 / 1.01) as f32;
    let mut edge = false;
    if !edge_axes(a, b, st, margin, &mut best, &mut edge) {
        st.feat_a = 0xff;
        st.feat_b = 0xff;
        return false;
    }
    if !edge {
        face_contact(a, b, st, margin, out)
    } else {
        edge_contact(a, b, st, margin, out)
    }
}

/// Studio (GEOM §2.1): the six face axes from the stored (body, axis), with the
/// 1 % face hysteresis.
fn face_axes(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, best: &mut f32) -> bool {
    let prev = [st.feat_a as i8 as i32, st.feat_b as i8 as i32];
    let prev_face = st.feat_a < 6 || st.feat_b < 6;
    let mut prev_ov = f32::INFINITY;
    let sb = st.start_body as i32;
    let sa = st.start_axis as i32;
    for bi in sb..sb + 2 {
        let body = (bi % 2) as usize;
        let (r, o) = if body == 0 { (a, b) } else { (b, a) };
        let po = o.f.pos();
        let pr = r.f.pos();
        let dd = Vec3::new(po.x - pr.x, po.y - pr.y, po.z - pr.z);
        // d_k = dd · axis_r,k: (dy·R[1][k] + dx·R[0][k]) + dz·R[2][k].
        // d_k = (dy·R[1][k] + dx·R[0][k]) + dz·R[2][k] (Studio's own sum).
        let d = r.f.to_local_n(dd);
        let ho = o.size;
        for ai in sa..sa + 3 {
            let k = (ai % 3) as usize;
            let c = o.f.to_local(r.f.axis(k));
            let hr = [r.h.x, r.h.y, r.h.z][k];
            let ov = ((clamp_s(ho.x) * 0.5 * c.x).abs() + (clamp_s(ho.y) * 0.5 * c.y).abs() + (clamp_s(ho.z) * 0.5 * c.z).abs() + hr) - d[k].abs();
            if ov <= margin {
                st.start_body = body as u8;
                st.start_axis = k as u8;
                return false;
            }
            let f = if d[k] <= 0.0 { k as i32 + 3 } else { k as i32 };
            if prev_face && prev[body] == f {
                prev_ov = ov;
            }
            if ov < *best {
                *best = ov;
                if body == 0 {
                    st.feat_a = f as u8;
                    st.feat_b = 0xff;
                } else {
                    st.feat_b = f as u8;
                    st.feat_a = 0xff;
                }
                st.start_body = body as u8;
                st.start_axis = k as u8;
            }
        }
    }
    // .rdata Studio = 1.01.
    if prev_face && (st.feat_a as i8 as i32 != prev[0] || st.feat_b as i8 as i32 != prev[1]) && prev_ov <= *best * 1.01 {
        *best = prev_ov;
        st.feat_a = prev[0] as u8;
        st.feat_b = prev[1] as u8;
    }
    true
}

#[inline]
fn clamp_s(x: f32) -> f32 {
    if MIN_SIZE < x { x } else { MIN_SIZE }
}

/// Studio(v, eps): v /= |v| when |v| > eps; returns |v|, or 0 when not.
#[inline]
fn normalize(v: &mut Vec3, eps: f32) -> f32 {
    let l2 = v.x * v.x + v.y * v.y + v.z * v.z;
    let l = l2.sqrt();
    if l <= eps {
        return 0.0;
    }
    let k = 1.0 / l;
    *v = Vec3::new(v.x * k, v.y * k, v.z * k);
    l
}

/// Studio: edge id of `f`'s edge along axis `ax` in the quadrant of `dir`.
fn edge_of(f: &Frame, ax: usize, dir: Vec3) -> u8 {
    let l = f.to_local(dir);
    let (f1, f2) = match ax {
        0 => (l.y, l.z),
        1 => (l.z, l.x),
        _ => (l.x, l.y),
    };
    let base = (ax * 4) as u8;
    if f2 <= 0.0 {
        if 0.0 < f1 { base + 3 } else { base + 2 }
    } else if f1 <= 0.0 {
        base + 1
    } else {
        base
    }
}

/// Studio (GEOM §2.2): the nine edge axes, A's edges outer.
fn edge_axes(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, best: &mut f32, edge: &mut bool) -> bool {
    let (pa, pb) = (a.f.pos(), b.f.pos());
    let t = Vec3::new(pb.x - pa.x, pb.y - pa.y, pb.z - pa.z);
    for i in 0..3 {
        let ea = a.f.axis(i);
        for j in 0..3 {
            let eb = b.f.axis(j);
            let mut c = Vec3::new(ea.y * eb.z - ea.z * eb.y, ea.z * eb.x - ea.x * eb.z, ea.x * eb.y - ea.y * eb.x);
            let l = normalize(&mut c, 1e-6);
            // .rdata Studio = 0.001: (near) parallel stops every remaining test.
            if l <= 0.001 {
                return true;
            }
            let s = (c.x * t.x + c.y * t.y) + c.z * t.z;
            let la = a.f.to_local(c);
            let lb = b.f.to_local(c);
            let ov = ((la.x * a.h.x).abs() + (la.y * a.h.y).abs() + (la.z * a.h.z).abs() + (lb.x * b.h.x).abs() + (lb.y * b.h.y).abs() + (lb.z * b.h.z).abs()) - s.abs();
            if ov <= margin {
                return false;
            }
            if ov < *best {
                *best = ov;
                let sg = if 0.0 < s { 1.0 } else { -1.0 };
                st.feat_a = edge_of(&a.f, i, c * sg) + 6;
                st.feat_b = edge_of(&b.f, j, c * -sg) + 6;
                *edge = true;
            }
        }
    }
    true
}

/// A polygon point of the clip: (u, v, w), id (−1 = made by the clip), Reduce's flag.
#[derive(Debug, Clone, Copy, Default)]
struct PPt {
    x: [f32; 3],
    id: i8,
    flag: bool,
}

/// Studio (GEOM §2.3): clip the incident face against the reference face.
fn face_contact(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, out: &mut Temps) -> bool {
    let ref_is_a = st.feat_a != 0xff;
    let f = if ref_is_a { st.feat_a } else { st.feat_b } as usize;
    let (rf, inc) = if ref_is_a { (a, b) } else { (b, a) };
    let k = f % 3;
    let sgn = if f < 3 { 1.0f32 } else { -1.0 };
    let n = -(rf.f.axis(k) * sgn);
    // Studio: the incident face.
    let l = inc.f.to_local_n(n);
    let (ax, ay, az) = (l.x.abs(), l.y.abs(), l.z.abs());
    let fi = if ax <= ay {
        if az < ay { if 0.0 < l.y { 1 } else { 4 } } else if 0.0 < l.z { 2 } else { 5 }
    } else if az < ax {
        if 0.0 < l.x { 0 } else { 3 }
    } else if 0.0 < l.z {
        2
    } else {
        5
    };
    let m = rf.f.inverse().mul(&inc.f);
    let bs = &BASIS[f];
    let hr = vertex(FV[f][0], rf.h);
    let rect = match f {
        0 => [rf.h.y, rf.h.z],
        1 => [rf.h.z, rf.h.x],
        2 => [rf.h.x, rf.h.y],
        3 => [rf.h.z, rf.h.y],
        4 => [rf.h.x, rf.h.z],
        _ => [rf.h.y, rf.h.x],
    };
    let mut poly = [PPt { id: -1, ..Default::default() }; 8];
    let mm = &m.m;
    for (i, pt) in poly.iter_mut().take(4).enumerate() {
        let v = vertex(FV[fi][i], inc.h);
        let px = ((v.y * mm[1] + v.x * mm[0]) + v.z * mm[2]) + mm[9];
        let py = ((v.z * mm[5] + v.y * mm[4]) + v.x * mm[3]) + mm[10];
        let pz = ((v.z * mm[8] + v.y * mm[7]) + v.x * mm[6]) + mm[11];
        let w = ((bs[8] * pz + 0.0) + bs[7] * py) + bs[6] * px;
        let u = (bs[2] * pz + bs[1] * py) + bs[0] * px;
        let vv = (pz * bs[5] + py * bs[4]) + px * bs[3];
        *pt = PPt { x: [u, vv, w], id: i as i8, flag: false };
    }
    let mut np = 4usize;
    clip(&rect, &mut poly, &mut np);
    let off = ((hr.z * bs[8] + 0.0) + hr.y * bs[7]) + hr.x * bs[6];
    let thr = -margin;
    let mut sel = [0usize; 4];
    let ns = reduce(&mut poly[..np], off, thr, &mut sel);
    if poly_trace() {
        let pts: Vec<String> = poly[..np].iter().map(|q| format!("({:.9} {:.9} {:.9} id{} f{})", q.x[0], q.x[1], q.x[2] - off, q.id, q.flag as u8)).collect();
        eprintln!("poly ref_is_a {} f {} fi {} n {} sel {:?} : {}", ref_is_a, f, fi, np, &sel[..ns], pts.join(" "));
    }
    let mut any = false;
    for &j in &sel[..ns] {
        let pt = poly[j];
        let (u, vv, w) = (pt.x[0], pt.x[1], pt.x[2]);
        let sep = w - off;
        let q = Vec3::new((vv * bs[3] + u * bs[0]) + bs[6] * w, (vv * bs[4] + u * bs[1]) + bs[7] * w, (vv * bs[5] + u * bs[2]) + bs[8] * w);
        let p = rf.f.to_world(q);
        // A = the incident body, B = the reference body.
        let t = out.take(!ref_is_a, 0);
        t.p = p;
        t.n = n;
        t.sep = sep;
        t.id_a = pt.id as i32;
        t.id_b = f as i32;
        if pt.id == -1 {
            t.id_a = fi as i32;
            t.kind = 2;
        }
        if sep < thr {
            any = true;
        } else {
            t.n = Vec3::ZERO;
        }
    }
    any
}

impl Frame {
    /// toLocal as Studio computes it: x = (n.y·R3 + n.x·R0) + R6·n.z,
    /// y = (n.y·R4 + n.x·R1) + R7·n.z, z = (n.y·R5 + n.x·R2) + R8·n.z.
    #[inline]
    fn to_local_n(&self, v: Vec3) -> Vec3 {
        let r = &self.m;
        Vec3::new((v.y * r[3] + v.x * r[0]) + r[6] * v.z, (v.y * r[4] + v.x * r[1]) + r[7] * v.z, (v.y * r[5] + v.x * r[2]) + r[8] * v.z)
    }
}

/// Clip Studio: Sutherland–Hodgman against u ≤ hu, v ≤ hv, u ≥ −hu, v ≥ −hv
/// (.rdata Studio), ≤ 8 points, every output starting at the edge (0, 1).
fn clip(rect: &[f32; 2], poly: &mut [PPt; 8], n: &mut usize) {
    const PLANES: [(usize, f32); 4] = [(0, 1.0), (1, 1.0), (0, -1.0), (1, -1.0)];
    let mut buf = [PPt { id: -1, ..Default::default() }; 8];
    for &(ax, s) in &PLANES {
        let cnt = *n;
        let mut o = 0usize;
        let h = s * rect[ax];
        let mut push = |p: PPt, o: &mut usize| {
            if *o < 8 {
                buf[*o] = p;
            }
            *o += 1;
        };
        for i in 0..cnt {
            let cur = poly[i];
            let nxt = poly[(i + 1) % cnt];
            let nxt_out = (h - nxt.x[ax]) * s < 0.0;
            let dc = h - cur.x[ax];
            let lerp = |first_nxt: bool| {
                let t0 = dc / (nxt.x[ax] - cur.x[ax]);
                let t1 = if 0.0 <= t0 { t0 } else { 0.0 };
                let t = if t1 <= 1.0 { t1 } else { 1.0 };
                let r = 1.0 - t;
                let u = if first_nxt { t * nxt.x[0] + r * cur.x[0] } else { r * cur.x[0] + t * nxt.x[0] };
                let mut x = [u, r * cur.x[1] + t * nxt.x[1], t * nxt.x[2] + r * cur.x[2]];
                x[ax] = rect[ax] * s;
                PPt { x, id: -1, flag: false }
            };
            if s * dc < 0.0 {
                if !nxt_out {
                    push(lerp(false), &mut o);
                    push(nxt, &mut o);
                }
            } else if nxt_out {
                push(lerp(true), &mut o);
            } else {
                push(nxt, &mut o);
            }
        }
        let o = o.min(8);
        poly[..o].copy_from_slice(&buf[..o]);
        *n = o;
    }
}

/// |cross2(k − a, b − a)| in (u, v), as Studio.
#[inline]
fn area(p: &[PPt], a: usize, b: usize, k: usize) -> f32 {
    let (ax, ay) = (p[a].x[0], p[a].x[1]);
    ((p[k].x[0] - ax) * (p[b].x[1] - ay) - (p[k].x[1] - ay) * (p[b].x[0] - ax)).abs()
}

/// Studio: the flagged point strictly between a and b (cyclic) with the largest
/// area, strict >, must be > 0.
fn third(p: &[PPt], a: usize, b: usize) -> Option<usize> {
    let n = p.len();
    let mut best = 0.0f32;
    let mut r = None;
    let mut k = (a + 1) % n;
    while k != b {
        if p[k].flag {
            let s = area(p, a, b, k);
            if best < s {
                best = s;
                r = Some(k);
            }
        }
        k = (k + 1) % n;
    }
    r
}

/// Reduce Studio (GEOM §2.3): ≤ 4 indices in polygon order from the deepest.
fn poly_trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_POLY_TRACE").is_some())
}

fn reduce_tie() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_REDUCE_TIE").is_ok_and(|v| v == "1"))
}

fn reduce(p: &mut [PPt], off: f32, thr: f32, sel: &mut [usize; 4]) -> usize {
    let n = p.len();
    if n == 0 {
        return 0;
    }
    // 1: flag w − off ≤ thr; i0 = the smallest, ties → the last.
    let mut lo = thr;
    let mut i0 = None;
    for (i, q) in p.iter_mut().enumerate() {
        let s = q.x[2] - off;
        if s <= thr {
            q.flag = true;
            // RUSTUDIO_SOLVER_REDUCE_TIE=1 (diagnostic, LOOSE-3 item 2): depths compared as equal.
            if s <= lo || reduce_tie() {
                lo = s;
                i0 = Some(i);
            }
        }
    }
    let Some(i0) = i0 else { return 0 };
    sel[0] = i0;
    // 2: i1 = the flagged point farthest from i0 (squared (u, v, w) distance), strict >.
    let mut far = 0.0f32;
    let mut i1 = None;
    let mut k = (i0 + 1) % n;
    while k != i0 {
        if p[k].flag {
            let dy = p[i0].x[1] - p[k].x[1];
            let dx = p[i0].x[0] - p[k].x[0];
            let dz = p[i0].x[2] - p[k].x[2];
            let d = (dy * dy + dx * dx) + dz * dz;
            if far < d {
                far = d;
                i1 = Some(k);
            }
        }
        k = (k + 1) % n;
    }
    let Some(i1) = i1 else { return 1 };
    // 3: the third point.
    if let Some(t) = third(p, i0, i1) {
        sel[1] = t;
        sel[2] = i1;
    } else if let Some(t) = third(p, i1, i0) {
        sel[1] = i1;
        sel[2] = t;
    } else {
        sel[1] = i1;
        return 2;
    }
    // 4: Fourth Studio: the best point over the triangle's three arcs.
    let tri = [sel[0], sel[1], sel[2]];
    let mut best = 0.0f32;
    let mut r = None;
    let mut e = 0usize;
    for ed in 0..3 {
        let (s0, s1) = (tri[ed], tri[(ed + 1) % 3]);
        let mut bb = 0.0f32;
        let mut rr = None;
        let mut k = (s0 + 1) % n;
        while k != s1 {
            if p[k].flag {
                let s = area(p, s0, s1, k);
                if bb < s {
                    bb = s;
                    rr = Some(k);
                }
            }
            k = (k + 1) % n;
        }
        // edge 0 needs > 0; edges 1, 2 replace only if strictly larger.
        if rr.is_some() && best < bb {
            best = bb;
            r = rr;
            e = ed;
        }
    }
    let Some(r) = r else { return 3 };
    // 5: r after position e (std::rotate when e ≠ 2).
    sel[3] = r;
    if e != 2 {
        sel[e + 1..4].rotate_right(1);
    }
    4
}

/// Studio (GEOM §2.4): one edge–edge point.
fn edge_contact(a: &Shape, b: &Shape, st: &mut PairState, margin: f32, out: &mut Temps) -> bool {
    let ea = st.feat_a as i32 - 6;
    let eb = st.feat_b as i32 - 6;
    let (aa, qa) = ((ea >> 2) as usize, (ea & 3) as usize);
    let (ab, qb) = ((eb >> 2) as usize, (eb & 3) as usize);
    let ida = aa as i32 + 3 * (ea & 1);
    let idb = ab as i32 + 3 * (eb & 1);
    let pa0 = a.f.to_world(vertex(FV[aa][qa], a.h));
    let pb0 = b.f.to_world(vertex(FV[ab][qb], b.h));
    let da = a.f.axis((ida % 3) as usize) * (1 - 2 * (ida / 3)) as f32;
    let db = b.f.axis((idb % 3) as usize) * (1 - 2 * (idb / 3)) as f32;
    // Studio: closest points of the two lines.
    let c = da.dot(db);
    if 1.0 - c * c <= 1e-6 {
        return false;
    }
    let r = pb0 - pa0;
    let f = 1.0 / (-1.0 - (-c) * c);
    let (rda, rdb) = (r.dot(da), r.dot(db));
    let ta = c * f * rdb - f * rda;
    let tb = f * (-c) * rda + f * rdb;
    let pa = pa0 + da * ta;
    let pb = pb0 + db * tb;
    let mut n = pa - pb;
    let l = normalize(&mut n, 1e-6);
    let sep = -l;
    let t = out.take(true, 1);
    t.id_a = ida;
    t.id_b = idb;
    t.p = pa;
    t.n = n;
    t.sep = sep;
    if sep < -margin {
        true
    } else {
        t.n = Vec3::ZERO;
        false
    }
}

// ------------------------------------------------------------------ balls

/// BallBall Studio (GEOM §5), flag Studio taken as 0 (Open 3).
fn ball_ball(a: &Shape, b: &Shape, margin: f32, out: &mut Temps) -> bool {
    let (ca, cb) = (a.f.pos(), b.f.pos());
    let d = cb - ca;
    let l = a.radius + b.radius - margin;
    if !(l > 0.0 && d.length_squared() < l * l) {
        return false;
    }
    let mut n = d;
    let len = normalize(&mut n, 1e-6);
    let sep = d.length() - (a.radius + b.radius);
    let _ = len;
    let t = out.take(true, KIND_BALL);
    t.id_a = 0;
    t.id_b = 0;
    t.p = ca + n * a.radius;
    t.n = n;
    t.sep = sep;
    true
}

/// BallPolyMesh Studio (GEOM §5): A = the ball, B = the block / wedge.
fn ball_poly(a: &Shape, b: &Shape, margin: f32, out: &mut Temps) -> bool {
    let c = b.f.to_local(a.f.pos() - b.f.pos());
    // 2: closest feature: q (local), direction g, signed squared distance f.
    let (q, g, f) = if b.class == CLASS_BLOCK {
        closest_box(c, b.h)
    } else {
        super::narrow_poly::closest_poly(c, b)
    };
    let r = a.radius;
    let l = r - margin;
    let active = (f >= 0.0 && l >= 0.0 && f <= l * l) || (f < 0.0 && (l >= 0.0 || -f <= l * l));
    if !active {
        return false;
    }
    let dist = if f < 0.0 { -(-f).sqrt() } else { f.sqrt() };
    let sep = dist - r;
    let gh = if dist <= 0.0 { g / g.length() } else { g / dist };
    let n = -b.f.rot(gh);
    let p = b.f.rot(q) + b.f.pos() - n * (sep * 0.5);
    let t = out.take(true, KIND_BALL);
    t.id_a = 0;
    t.id_b = 0;
    t.p = p;
    t.n = n;
    t.sep = sep;
    t.row_at_p = true;
    true
}

/// Studio stand-in (GEOM Open 4: its semantics are from the call site): the
/// closest point q of a box to c, the direction g from the surface toward c (outward
/// face normal when c is inside), f = signed squared distance.
fn closest_box(c: Vec3, h: Vec3) -> (Vec3, Vec3, f32) {
    let q = c.clamp(-h, h);
    let g = c - q;
    let d2 = g.length_squared();
    if d2 > 0.0 {
        return (q, g, d2);
    }
    // Inside: the nearest face.
    let dist = h - c.abs();
    let k = if dist.x <= dist.y && dist.x <= dist.z { 0 } else if dist.y <= dist.z { 1 } else { 2 };
    let mut q = c;
    let s = if c[k] >= 0.0 { 1.0 } else { -1.0 };
    q[k] = h[k] * s;
    let mut g = Vec3::ZERO;
    g[k] = s;
    (q, g, -(dist[k] * dist[k]))
}
