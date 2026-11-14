//! Exact overlap tests for WorldRoot:GetPartsInPart. Roblox tests the parts'
//! geometry there, not their bounding boxes (GetPartBoundsInBox is the
//! bounds query): a rotated brush's or a baked terrain chunk's box covers
//! far more than the part does. Place A's nav-grid builder relies on it for its
//! headroom probes.

use glam::{Mat3, Quat, Vec3};
use rapier3d::parry;
use rapier3d::prelude as rp;

use crate::doc::{self, Document};
use crate::math::Xform;

/// A part's shape for queries (as physics.rs builds its colliders):
/// Block, Ball (radius = smallest half size), Cylinder (along X, radius =
/// the smaller of the Y / Z half sizes), Wedge (slope faces -Z, rising to
/// the top-back edge), CornerWedge (apex at the top +X / -Z corner).
pub const BOX: u8 = 0;
pub const BALL: u8 = 1;
pub const CYLINDER: u8 = 2;
pub const WEDGE: u8 = 3;
pub const CORNER_WEDGE: u8 = 4;

/// The query shape of a BasePart (MeshParts and unions are BOX here: they
/// have their own mesh path).
pub fn kind_of(d: &Document, r: rbx_dom_weak::types::Ref, class: &str) -> u8 {
    match class {
        "WedgePart" => WEDGE,
        "CornerWedgePart" => CORNER_WEDGE,
        c if doc::is_a(c, "Part") => match crate::scene::enum_u32(d.prop(r, "Shape")) {
            Some(0) => BALL,
            Some(2) => CYLINDER,
            Some(3) => WEDGE,
            Some(4) => CORNER_WEDGE,
            _ => BOX,
        },
        _ => BOX,
    }
}

/// The faces of a convex shape in unit-cube space (corners at ±1): n·p <= d.
fn planes(kind: u8) -> &'static [([f32; 3], f32)] {
    const BOXP: [([f32; 3], f32); 6] = [
        ([1.0, 0.0, 0.0], 1.0),
        ([-1.0, 0.0, 0.0], 1.0),
        ([0.0, 1.0, 0.0], 1.0),
        ([0.0, -1.0, 0.0], 1.0),
        ([0.0, 0.0, 1.0], 1.0),
        ([0.0, 0.0, -1.0], 1.0),
    ];
    // Corners (±1, -1, ±1), (±1, 1, 1): the slope through the bottom-front
    // and top-back edges.
    const WEDGEP: [([f32; 3], f32); 5] = [
        ([1.0, 0.0, 0.0], 1.0),
        ([-1.0, 0.0, 0.0], 1.0),
        ([0.0, -1.0, 0.0], 1.0),
        ([0.0, 0.0, 1.0], 1.0),
        ([0.0, 1.0, -1.0], 0.0),
    ];
    // Corners (±1, -1, ±1), apex (1, 1, -1).
    const CORNERP: [([f32; 3], f32); 5] = [
        ([1.0, 0.0, 0.0], 1.0),
        ([0.0, -1.0, 0.0], 1.0),
        ([0.0, 0.0, -1.0], 1.0),
        ([-1.0, 1.0, 0.0], 0.0),
        ([0.0, 1.0, 1.0], 0.0),
    ];
    match kind {
        WEDGE => &WEDGEP,
        CORNER_WEDGE => &CORNERP,
        _ => &BOXP,
    }
}

/// A ray (world origin `o`, unit direction `dir`) against a part's true
/// shape, entered from outside (a ray starting inside the part misses it,
/// as for boxes): (distance, world normal).
pub fn ray_part(xf: &Xform, size: Vec3, kind: u8, o: Vec3, dir: Vec3) -> Option<(f32, Vec3)> {
    let inv = xf.rot.transpose();
    let lo = inv * (o - xf.pos);
    let ld = inv * dir;
    let h = (size * 0.5).max(Vec3::splat(1e-4));
    let (t, n) = match kind {
        BALL => {
            let r = h.min_element();
            let b = lo.dot(ld);
            let c = lo.length_squared() - r * r;
            if c <= 0.0 {
                return None;
            }
            let disc = b * b - c * ld.length_squared();
            if disc < 0.0 {
                return None;
            }
            let t = (-b - disc.sqrt()) / ld.length_squared();
            if t < 0.0 {
                return None;
            }
            (t, (lo + ld * t).normalize_or_zero())
        }
        CYLINDER => {
            let r = h.y.min(h.z);
            // Along the axis: the slab |x| <= h.x.
            let (mut sx0, mut sx1) = (f32::NEG_INFINITY, f32::INFINITY);
            if ld.x.abs() < 1e-9 {
                if lo.x.abs() > h.x {
                    return None;
                }
            } else {
                let a = (-h.x - lo.x) / ld.x;
                let b = (h.x - lo.x) / ld.x;
                (sx0, sx1) = (a.min(b), a.max(b));
            }
            // Across it: y² + z² <= r².
            let (mut sc0, mut sc1) = (f32::NEG_INFINITY, f32::INFINITY);
            let a = ld.y * ld.y + ld.z * ld.z;
            let c = lo.y * lo.y + lo.z * lo.z - r * r;
            if a < 1e-12 {
                if c > 0.0 {
                    return None;
                }
            } else {
                let b = lo.y * ld.y + lo.z * ld.z;
                let disc = b * b - a * c;
                if disc < 0.0 {
                    return None;
                }
                let s = disc.sqrt();
                (sc0, sc1) = ((-b - s) / a, (-b + s) / a);
            }
            let enter = sx0.max(sc0);
            let exit = sx1.min(sc1);
            if enter > exit || enter < 0.0 {
                return None;
            }
            let n = if sx0 >= sc0 {
                Vec3::new(-ld.x.signum(), 0.0, 0.0)
            } else {
                let p = lo + ld * enter;
                Vec3::new(0.0, p.y, p.z).normalize_or_zero()
            };
            (enter, n)
        }
        _ => {
            // Convex polyhedron in unit-cube space (the ray parameter is
            // unchanged by the scaling).
            let u = lo / h;
            let ud = ld / h;
            let (mut enter, mut exit) = (f32::NEG_INFINITY, f32::INFINITY);
            let mut en = Vec3::ZERO;
            for (n, d) in planes(kind) {
                let n = Vec3::from(*n);
                let den = n.dot(ud);
                let num = d - n.dot(u);
                if den.abs() < 1e-12 {
                    if num < 0.0 {
                        return None;
                    }
                    continue;
                }
                let t = num / den;
                if den < 0.0 {
                    if t > enter {
                        enter = t;
                        en = n;
                    }
                } else {
                    exit = exit.min(t);
                }
                if enter > exit {
                    return None;
                }
            }
            if enter < 0.0 {
                return None;
            }
            // Unit-space normal to part space: inverse-transpose of the scale.
            (enter, (en / h).normalize_or_zero())
        }
    };
    Some((t, xf.rot * n))
}

fn rv(v: Vec3) -> rp::Vector {
    rp::Vector::new(v.x, v.y, v.z)
}

fn gv(v: rp::Vector) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

/// The parry shape of a part and its turn inside the part (cylinders: parry's
/// run along Y, Roblox's along X).
fn parry_shape(kind: u8, size: Vec3) -> (rp::SharedShape, Mat3) {
    let h = (size * 0.5).max(Vec3::splat(1e-4));
    let hull = |kind: u8| -> Option<rp::SharedShape> {
        let c: &[[f32; 3]] = if kind == WEDGE {
            &[
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
                [-1.0, -1.0, 1.0],
                [-1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
            ]
        } else {
            &[
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
                [-1.0, -1.0, 1.0],
                [1.0, 1.0, -1.0],
            ]
        };
        let pts: Vec<rp::Vector> = c.iter().map(|p| rv(Vec3::from(*p) * h)).collect();
        rp::SharedShape::convex_hull(&pts)
    };
    match kind {
        BALL => (rp::SharedShape::ball(h.min_element()), Mat3::IDENTITY),
        CYLINDER => (
            rp::SharedShape::cylinder(h.x, h.y.min(h.z)),
            Mat3::from_rotation_z(-std::f32::consts::FRAC_PI_2),
        ),
        WEDGE | CORNER_WEDGE => (
            hull(kind).unwrap_or_else(|| rp::SharedShape::cuboid(h.x, h.y, h.z)),
            Mat3::IDENTITY,
        ),
        _ => (rp::SharedShape::cuboid(h.x, h.y, h.z), Mat3::IDENTITY),
    }
}

fn parry_pose(xf: &Xform, off: Mat3) -> rp::Pose {
    let q = Quat::from_mat3(&(xf.rot * off)).normalize();
    rp::Pose::from_parts(rv(xf.pos), rp::Rotation::from_xyzw(q.x, q.y, q.z, q.w))
}

/// Whether two parts' true shapes overlap (GetPartsInPart).
pub fn part_part(a: &Xform, a_size: Vec3, a_kind: u8, b: &Xform, b_size: Vec3, b_kind: u8) -> bool {
    if a_kind == BOX && b_kind == BOX {
        return obb_obb(a, a_size, b, b_size);
    }
    let (sa, oa) = parry_shape(a_kind, a_size);
    let (sb, ob) = parry_shape(b_kind, b_size);
    parry::query::intersection_test(&parry_pose(a, oa), &*sa, &parry_pose(b, ob), &*sb)
        .map(|i| i.intersecting)
        .unwrap_or_else(|_| obb_obb(a, a_size, b, b_size))
}

/// A shape (`probe_kind` / `probe_size`, turned by `probe.rot`, starting at
/// `probe.pos`) swept along unit `dir` for `len` studs against a part's true
/// shape: (distance, the touching point on the part, the part's normal there).
/// A probe that starts overlapping the part doesn't hit it, as in Roblox.
#[allow(clippy::too_many_arguments)]
pub fn sweep_part(
    probe: &Xform,
    probe_size: Vec3,
    probe_kind: u8,
    dir: Vec3,
    len: f32,
    part: &Xform,
    size: Vec3,
    kind: u8,
) -> Option<(f32, Vec3, Vec3)> {
    // Boxes exactly: parry's GJK cast drifts on big parts (a 0.4 sphere met
    // Laser Tag's 121×5×381 FLOOR up to 0.29 studs early at a grazing angle,
    // where Studio is exact).
    if kind == BOX && probe_kind == BALL {
        return sphere_box(probe.pos, probe_size.min_element() * 0.5, dir, len, part, size);
    }
    let toi = if kind == BOX && probe_kind == BOX {
        Some(box_box_toi(probe, probe_size, dir, len, part, size)?)
    } else {
        None
    };
    let (st, ot) = parry_shape(kind, size);
    let (sp, op) = parry_shape(probe_kind, probe_size);
    // With the exact time known, parry only finds the touching point, cast
    // from just short of it.
    let back = toi.map_or(0.0, |(t, _)| (t - 0.05).max(0.0));
    let start = Xform { pos: probe.pos + dir * back, rot: probe.rot };
    let opts = parry::query::ShapeCastOptions {
        max_time_of_impact: if toi.is_some() { (len - back).min(1.0) } else { len },
        target_distance: 0.0,
        stop_at_penetration: true,
        compute_impact_geometry_on_penetration: false,
    };
    let hit = parry::query::cast_shapes(
        &parry_pose(part, ot),
        rp::Vector::ZERO,
        &*st,
        &parry_pose(&start, op),
        rv(dir),
        &*sp,
        opts,
    )
    .ok()
    .flatten();
    let rot = part.rot * ot;
    match (toi, hit) {
        (Some((t, n)), Some(hit)) => Some((t, part.pos + rot * gv(hit.witness1), n)),
        (Some((t, n)), None) => Some((t, probe.pos + dir * t, n)),
        (None, Some(hit)) if hit.time_of_impact > 1e-5 => {
            let point = part.pos + rot * gv(hit.witness1);
            let n = (rot * gv(hit.normal1)).normalize_or_zero();
            Some((hit.time_of_impact, point, n))
        }
        _ => None,
    }
}

/// A sphere (centre `c`, radius `r`) swept along unit `dir` for `len`
/// against a box: (distance, the touching point on the box, its normal).
/// The distance from the centre to the box is convex along the path, so the
/// first contact is a root left of its minimum. None when the sphere starts
/// touching the box (Roblox skips parts a cast starts in).
fn sphere_box(c: Vec3, r: f32, dir: Vec3, len: f32, part: &Xform, size: Vec3) -> Option<(f32, Vec3, Vec3)> {
    use glam::DVec3;
    let inv = part.rot.transpose();
    let o = (inv * (c - part.pos)).as_dvec3();
    let d = (inv * dir).as_dvec3();
    let h = (size * 0.5).as_dvec3();
    let r = r as f64;
    let gap = |t: f64| {
        let p = o + d * t;
        (p - p.clamp(-h, h)).length() - r
    };
    if gap(0.0) <= 0.0 {
        return None;
    }
    // Where the path crosses the box grown by r (the rounded box lies inside).
    let (mut a, mut b) = (0.0f64, len as f64);
    for k in 0..3 {
        let g = h[k] + r;
        if d[k].abs() < 1e-12 {
            if o[k].abs() > g {
                return None;
            }
        } else {
            let (t1, t2) = ((-g - o[k]) / d[k], (g - o[k]) / d[k]);
            a = a.max(t1.min(t2));
            b = b.min(t1.max(t2));
        }
    }
    if a > b {
        return None;
    }
    // The nearest approach in [a, b] (golden section), then the first root.
    let (mut lo, mut hi) = (a, b);
    let k = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..90 {
        let (m1, m2) = (hi - k * (hi - lo), lo + k * (hi - lo));
        if gap(m1) < gap(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let tm = 0.5 * (lo + hi);
    if gap(tm) > 1e-9 {
        return None;
    }
    let (mut lo, mut hi) = (a, tm);
    for _ in 0..80 {
        let m = 0.5 * (lo + hi);
        if gap(m) > 0.0 {
            lo = m;
        } else {
            hi = m;
        }
    }
    let t = hi;
    let p = o + d * t;
    let q = p.clamp(-h, h);
    let mut n = p - q;
    if n.length() < 1e-9 {
        let a = (p / h).abs();
        n = if a.x >= a.y && a.x >= a.z {
            DVec3::X * p.x.signum()
        } else if a.y >= a.z {
            DVec3::Y * p.y.signum()
        } else {
            DVec3::Z * p.z.signum()
        };
    }
    let n = (part.rot * n.normalize().as_vec3()).normalize_or_zero();
    Some((t as f32, part.pos + part.rot * q.as_vec3(), n))
}

/// A box (`probe`, `probe_size`) moving along unit `dir` for `len` against
/// a box: the time of first contact and the part's face normal there, by the
/// moving separating-axis test (the 15 axes of two boxes). None when it
/// misses or starts overlapping.
fn box_box_toi(probe: &Xform, probe_size: Vec3, dir: Vec3, len: f32, part: &Xform, size: Vec3) -> Option<(f32, Vec3)> {
    use glam::DVec3;
    let ax: [DVec3; 3] = [probe.rot.x_axis, probe.rot.y_axis, probe.rot.z_axis].map(|v| v.as_dvec3());
    let bx: [DVec3; 3] = [part.rot.x_axis, part.rot.y_axis, part.rot.z_axis].map(|v| v.as_dvec3());
    let (ha, hb) = ((probe_size * 0.5).as_dvec3(), (size * 0.5).as_dvec3());
    let c0 = (part.pos - probe.pos).as_dvec3();
    let v = dir.as_dvec3();
    let mut axes: Vec<DVec3> = ax.iter().chain(bx.iter()).copied().collect();
    for a in &ax {
        for b in &bx {
            let c = a.cross(*b);
            if c.length() > 1e-6 {
                axes.push(c.normalize());
            }
        }
    }
    let (mut enter, mut exit) = (f64::NEG_INFINITY, f64::INFINITY);
    let mut normal = DVec3::ZERO;
    for l in axes {
        let ra: f64 = (0..3).map(|i| ha[i] * ax[i].dot(l).abs()).sum();
        let rb: f64 = (0..3).map(|i| hb[i] * bx[i].dot(l).abs()).sum();
        let rr = ra + rb;
        let (cl, vl) = (c0.dot(l), v.dot(l));
        if vl.abs() < 1e-12 {
            if cl.abs() > rr {
                return None;
            }
            continue;
        }
        let (t1, t2) = ((cl - rr) / vl, (cl + rr) / vl);
        let (t1, t2) = (t1.min(t2), t1.max(t2));
        if t1 > enter {
            enter = t1;
            // From the part towards the probe at contact.
            normal = if (c0 - v * t1).dot(l) > 0.0 { -l } else { l };
        }
        exit = exit.min(t2);
    }
    if enter > exit || enter <= 0.0 || enter > len as f64 {
        return None;
    }
    Some((enter as f32, normal.as_vec3().normalize_or_zero()))
}

/// A probe (as for `sweep_part`) swept against a part's collision pieces
/// (parry shapes in the part's space, posed by `part`): the first piece it
/// meets as (distance, touching point, the piece's normal there). A piece
/// the probe starts overlapping isn't hit; the others still are.
#[allow(clippy::too_many_arguments)]
pub fn sweep_pieces(
    probe: &Xform,
    probe_size: Vec3,
    probe_kind: u8,
    dir: Vec3,
    len: f32,
    part: &Xform,
    pieces: &[rp::SharedShape],
) -> Option<(f32, Vec3, Vec3)> {
    let (sp, op) = parry_shape(probe_kind, probe_size);
    let pose = parry_pose(probe, op);
    let ppose = parry_pose(part, Mat3::IDENTITY);
    let mut best: Option<(f32, Vec3, Vec3)> = None;
    for s in pieces {
        let opts = parry::query::ShapeCastOptions {
            max_time_of_impact: best.map_or(len, |b| b.0),
            target_distance: 0.0,
            stop_at_penetration: true,
            compute_impact_geometry_on_penetration: false,
        };
        let Ok(Some(hit)) = parry::query::cast_shapes(&ppose, rp::Vector::ZERO, &**s, &pose, rv(dir), &*sp, opts) else {
            continue;
        };
        if hit.time_of_impact <= 1e-5 || best.is_some_and(|b| hit.time_of_impact >= b.0) {
            continue;
        }
        let point = part.pos + part.rot * gv(hit.witness1);
        let n = (part.rot * gv(hit.normal1)).normalize_or_zero();
        best = Some((hit.time_of_impact, point, n));
    }
    best
}

/// A shape (as for `sweep_part`) swept against terrain triangles (world,
/// counter-clockwise seen from outside). Only front faces count: a shape
/// inside terrain doesn't hit it on the way out. Returns the first contact
/// as (index into `tris`, distance, touching point, contact normal), or
/// `Err(())` when the shape starts touching a front face (Studio then
/// reports no terrain hit at all).
pub fn sweep_triangles(
    probe: &Xform,
    probe_size: Vec3,
    probe_kind: u8,
    dir: Vec3,
    len: f32,
    tris: &[(usize, [Vec3; 3])],
) -> Result<Option<(usize, f32, Vec3, Vec3)>, ()> {
    let (sp, op) = parry_shape(probe_kind, probe_size);
    let pose = parry_pose(probe, op);
    let opts = parry::query::ShapeCastOptions {
        max_time_of_impact: len,
        target_distance: 0.0,
        stop_at_penetration: true,
        compute_impact_geometry_on_penetration: false,
    };
    let mut best: Option<(usize, f32, Vec3, Vec3)> = None;
    for (i, t) in tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).normalize_or_zero();
        if n == Vec3::ZERO || n.dot(dir) > 1e-6 {
            continue;
        }
        let tri = parry::shape::Triangle::new(rv(t[0]), rv(t[1]), rv(t[2]));
        let Ok(Some(hit)) = parry::query::cast_shapes(
            &rp::Pose::IDENTITY,
            rp::Vector::ZERO,
            &tri,
            &pose,
            rv(dir),
            &*sp,
            opts,
        ) else {
            continue;
        };
        if hit.time_of_impact <= 1e-5 {
            return Err(());
        }
        if best.is_none_or(|(_, bt, ..)| hit.time_of_impact < bt) {
            // The contact normal (a sphere on a corner: from the corner to
            // its centre), as Studio reports it.
            let cn = gv(hit.normal1).normalize_or_zero();
            best = Some((*i, hit.time_of_impact, gv(hit.witness1), if cn == Vec3::ZERO { n } else { cn }));
        }
    }
    Ok(best)
}

/// Separating-axis test between two oriented boxes (centre, rotation, size).
pub fn obb_obb(a: &Xform, a_size: Vec3, b: &Xform, b_size: Vec3) -> bool {
    let ha = a_size * 0.5;
    let hb = b_size * 0.5;
    let ra: [Vec3; 3] = [a.rot.x_axis, a.rot.y_axis, a.rot.z_axis];
    let rb: [Vec3; 3] = [b.rot.x_axis, b.rot.y_axis, b.rot.z_axis];
    let d = b.pos - a.pos;
    let mut axes: Vec<Vec3> = Vec::with_capacity(15);
    axes.extend_from_slice(&ra);
    axes.extend_from_slice(&rb);
    for x in &ra {
        for y in &rb {
            let c = x.cross(*y);
            if c.length_squared() > 1e-10 {
                axes.push(c.normalize());
            }
        }
    }
    for ax in axes {
        let pa = ha.x * ra[0].dot(ax).abs() + ha.y * ra[1].dot(ax).abs() + ha.z * ra[2].dot(ax).abs();
        let pb = hb.x * rb[0].dot(ax).abs() + hb.y * rb[1].dot(ax).abs() + hb.z * rb[2].dot(ax).abs();
        if d.dot(ax).abs() > pa + pb {
            return false;
        }
    }
    true
}

/// Separating-axis test between an axis-aligned box (half extents `h`, at
/// the origin) and a triangle.
fn aabb_triangle(h: Vec3, t: [Vec3; 3]) -> bool {
    let e = [t[1] - t[0], t[2] - t[1], t[0] - t[2]];
    let sep = |ax: Vec3| -> bool {
        if ax.length_squared() < 1e-12 {
            return false;
        }
        let p = [t[0].dot(ax), t[1].dot(ax), t[2].dot(ax)];
        let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
        let r = h.x * ax.x.abs() + h.y * ax.y.abs() + h.z * ax.z.abs();
        lo > r || hi < -r
    };
    for ax in [Vec3::X, Vec3::Y, Vec3::Z] {
        if sep(ax) {
            return false;
        }
    }
    if sep(e[0].cross(e[1])) {
        return false;
    }
    for a in [Vec3::X, Vec3::Y, Vec3::Z] {
        for ed in e {
            if sep(a.cross(ed)) {
                return false;
            }
        }
    }
    true
}

/// Whether point `p` (in the mesh's unit space) is inside the closed mesh:
/// an odd number of crossings along +X.
fn inside_mesh(p: Vec3, tris: &[[Vec3; 3]]) -> bool {
    let dir = Vec3::new(1.0, 1e-4, 2e-4).normalize();
    let mut n = 0;
    for t in tris {
        let e1 = t[1] - t[0];
        let e2 = t[2] - t[0];
        let q = dir.cross(e2);
        let det = e1.dot(q);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv = 1.0 / det;
        let s = p - t[0];
        let u = s.dot(q) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let r = s.cross(e1);
        let v = dir.dot(r) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        if e2.dot(r) * inv > 0.0 {
            n += 1;
        }
    }
    n % 2 == 1
}

/// An oriented box against a MeshPart's triangles (unit-mesh space, scaled
/// by `mesh_size` and placed at `mesh`): touching any triangle, or wholly
/// inside the mesh.
pub fn obb_mesh(b: &Xform, b_size: Vec3, mesh: &Xform, mesh_size: Vec3, tris: &[[Vec3; 3]]) -> bool {
    // Work in the box's frame: each triangle to world, then into the box's axes.
    let inv: Mat3 = b.rot.transpose();
    let h = b_size * 0.5;
    let to_box = |v: Vec3| inv * (mesh.pos + mesh.rot * (v * mesh_size) - b.pos);
    for t in tris {
        if aabb_triangle(h, [to_box(t[0]), to_box(t[1]), to_box(t[2])]) {
            return true;
        }
    }
    // No surface crossing: the box is entirely inside the mesh, or outside it.
    let centre = mesh.rot.transpose() * (b.pos - mesh.pos) / mesh_size.max(Vec3::splat(1e-4));
    inside_mesh(centre, tris)
}

/// `obb_mesh` through the mesh's BVH: only triangles near the box are tested.
pub fn obb_mesh_bvh(b: &Xform, b_size: Vec3, mesh: &Xform, mesh_size: Vec3, bvh: &crate::mesh_bvh::MeshBvh) -> bool {
    let inv: Mat3 = b.rot.transpose();
    let h = b_size * 0.5;
    let to_box = |v: Vec3| inv * (mesh.pos + mesh.rot * (v * mesh_size) - b.pos);
    // The box's bounds in unit-mesh space.
    let ms = mesh_size.max(Vec3::splat(1e-4));
    let to_mesh = mesh.rot.transpose();
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { -h.x } else { h.x },
            if i & 2 == 0 { -h.y } else { h.y },
            if i & 4 == 0 { -h.z } else { h.z },
        );
        let w = b.pos + b.rot * c;
        let m = to_mesh * (w - mesh.pos) / ms;
        lo = lo.min(m);
        hi = hi.max(m);
    }
    let eps = Vec3::splat(1e-5);
    if bvh.any_in_box(lo - eps, hi + eps, |t| aabb_triangle(h, [to_box(t[0]), to_box(t[1]), to_box(t[2])])) {
        return true;
    }
    let centre = to_mesh * (b.pos - mesh.pos) / ms;
    bvh.crossings(centre, Vec3::new(1.0, 1e-4, 2e-4).normalize()) % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: Vec3) -> Xform {
        Xform { pos: p, rot: Mat3::IDENTITY }
    }

    #[test]
    fn boxes_overlap_exactly() {
        assert!(obb_obb(&at(Vec3::ZERO), Vec3::ONE, &at(Vec3::new(0.9, 0.0, 0.0)), Vec3::ONE));
        assert!(!obb_obb(&at(Vec3::ZERO), Vec3::ONE, &at(Vec3::new(1.1, 0.0, 0.0)), Vec3::ONE));
        // A long bar turned 45°: its bounding box covers a point it misses.
        let bar = Xform {
            pos: Vec3::ZERO,
            rot: Mat3::from_rotation_y(std::f32::consts::FRAC_PI_4),
        };
        // (+45° about Y turns its length along (1, 0, -1).)
        let probe = at(Vec3::new(4.0, 0.0, 4.0));
        assert!(!obb_obb(&bar, Vec3::new(20.0, 1.0, 1.0), &probe, Vec3::splat(0.5)));
        assert!(obb_obb(&bar, Vec3::new(20.0, 1.0, 1.0), &at(Vec3::new(4.0, 0.0, -4.0)), Vec3::splat(0.5)));
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 2e-3
    }

    /// The Place A nav repros (Place A-UITEST-NOTES.md), Studio's numbers.
    #[test]
    fn rays_hit_true_shapes() {
        let down = Vec3::new(0.0, -1.0, 0.0);
        let (t, n) = ray_part(&at(Vec3::new(0.0, 110.0, 0.0)), Vec3::splat(4.0), BALL, Vec3::new(1.0, 120.0, 0.0), down).unwrap();
        assert!((t - 8.268).abs() < 2e-3 && close(n, Vec3::new(0.5, 0.866, 0.0)), "{t} {n}");
        let (t, n) = ray_part(&at(Vec3::new(10.0, 110.0, 0.0)), Vec3::splat(4.0), CYLINDER, Vec3::new(10.0, 120.0, 1.0), down).unwrap();
        assert!((t - 8.268).abs() < 2e-3 && close(n, Vec3::new(0.0, 0.866, 0.5)), "{t} {n}");
        let (t, n) = ray_part(&at(Vec3::new(20.0, 102.0, 0.0)), Vec3::splat(4.0), WEDGE, Vec3::new(20.0, 120.0, 0.0), down).unwrap();
        assert!((t - 18.0).abs() < 2e-3 && close(n, Vec3::new(0.0, 0.7071, -0.7071)), "{t} {n}");
        // Cylinder end cap, wedge back face, misses past the ball's edge.
        let (t, n) = ray_part(&at(Vec3::ZERO), Vec3::splat(4.0), CYLINDER, Vec3::new(10.0, 0.0, 0.0), -Vec3::X).unwrap();
        assert!((t - 8.0).abs() < 1e-3 && close(n, Vec3::X));
        let (t, n) = ray_part(&at(Vec3::ZERO), Vec3::splat(4.0), WEDGE, Vec3::new(0.0, 1.0, 10.0), -Vec3::Z).unwrap();
        assert!((t - 8.0).abs() < 1e-3 && close(n, Vec3::Z));
        assert!(ray_part(&at(Vec3::ZERO), Vec3::splat(4.0), BALL, Vec3::new(1.9, 10.0, 1.9), down).is_none());
        assert!(ray_part(&at(Vec3::ZERO), Vec3::splat(4.0), WEDGE, Vec3::new(0.0, 10.0, -1.5), down).map(|h| (h.0 - 11.5).abs() < 1e-3).unwrap());
        // Starting inside: no hit.
        assert!(ray_part(&at(Vec3::ZERO), Vec3::splat(4.0), BALL, Vec3::ZERO, down).is_none());
    }

    #[test]
    fn parts_overlap_by_shape() {
        let ball = at(Vec3::new(0.0, 110.0, 0.0));
        let corner = at(Vec3::new(1.8, 111.8, 0.0));
        assert!(!part_part(&ball, Vec3::splat(4.0), BALL, &corner, Vec3::splat(0.5), BOX));
        assert!(part_part(&ball, Vec3::splat(4.0), BALL, &at(Vec3::new(1.0, 111.0, 0.0)), Vec3::splat(0.5), BOX));
        // Above the wedge's low front, inside its box.
        let w = at(Vec3::ZERO);
        assert!(!part_part(&w, Vec3::splat(4.0), WEDGE, &at(Vec3::new(0.0, 1.5, -1.5)), Vec3::splat(0.5), BOX));
        assert!(part_part(&w, Vec3::splat(4.0), WEDGE, &at(Vec3::new(0.0, 1.5, 1.5)), Vec3::splat(0.5), BOX));
    }

    #[test]
    fn sweeps_hit_true_shapes() {
        // A 1-stud ball dropped onto a ball: touches its top at y = 112.
        let (t, p, n) = sweep_part(&at(Vec3::new(0.0, 120.0, 0.0)), Vec3::splat(1.0), BALL, -Vec3::Y, 20.0,
            &at(Vec3::new(0.0, 110.0, 0.0)), Vec3::splat(4.0), BALL).unwrap();
        assert!((t - 7.5).abs() < 1e-2 && close(p, Vec3::new(0.0, 112.0, 0.0)) && close(n, Vec3::Y), "{t} {p} {n}");
        // Past a box's corner diagonally: the grown box would hit, the sphere doesn't.
        assert!(sweep_part(&at(Vec3::new(2.4, 10.0, 2.4)), Vec3::splat(1.0), BALL, -Vec3::Y, 20.0,
            &at(Vec3::ZERO), Vec3::splat(4.0), BOX).is_none());
    }

    /// Laser Tag's FLOOR (121×5×381.5) and its sky-high containment slab,
    /// Studio's Spherecast / Blockcast numbers (checks/laser-tag-parity/
    /// raycast): exact at grazing angles, where parry's GJK came up short.
    #[test]
    fn sweeps_meet_big_boxes_exactly() {
        let floor = at(Vec3::new(-83.5, 2.5, 160.75));
        let fsize = Vec3::new(121.0, 5.0, 381.5);
        let o = at(Vec3::new(-83.0, 9.0, 160.0));
        for (d, r, want) in [
            (Vec3::new(10.0, -1.0, 0.0), 0.4, 36.1796),
            (Vec3::new(10.0, -1.0, 0.0), 0.1, 39.1945),
            (Vec3::new(10.0, -1.0, 0.0), 2.0, 20.0980),
            (Vec3::new(3.0, -1.0, 0.0), 0.4, 11.3842),
            (Vec3::new(0.0, -1.0, 0.0), 0.4, 3.5999),
        ] {
            let (t, p, n) = sweep_part(&o, Vec3::splat(r * 2.0), BALL, d.normalize(), 100.0, &floor, fsize, BOX).unwrap();
            assert!((t - want).abs() < 2e-3 && (p.y - 5.0).abs() < 1e-3 && close(n, Vec3::Y), "{d} {r}: {t} {p} {n}");
        }
        let (t, _, n) = sweep_part(&o, Vec3::ONE, BOX, Vec3::new(10.0, -1.0, 0.0).normalize(), 100.0, &floor, fsize, BOX).unwrap();
        assert!((t - 35.1746).abs() < 2e-3 && close(n, Vec3::Y), "{t} {n}");
        let slab = Xform {
            pos: Vec3::new(40.6045532, 42.3855896, 153.331039),
            rot: Mat3::from_cols(-Vec3::X, Vec3::Y, -Vec3::Z),
        };
        let ssize = Vec3::new(378.88, 31.752857, 389.20905);
        let o = at(Vec3::new(40.0, 12.0, 153.0));
        let up = Vec3::new(3.0, 1.0, 0.0).normalize();
        let (t, _, n) = sweep_part(&o, Vec3::splat(0.8), BALL, up, 100.0, &slab, ssize, BOX).unwrap();
        assert!((t - 44.6161).abs() < 2e-3 && close(n, -Vec3::Y), "{t} {n}");
        let (t, _, _) = sweep_part(&o, Vec3::ONE, BOX, up, 100.0, &slab, ssize, BOX).unwrap();
        assert!((t - 44.3009).abs() < 2e-3, "{t}");
        // Starting inside / touching: skipped.
        assert!(sweep_part(&at(Vec3::new(-83.0, 5.3, 160.0)), Vec3::splat(0.8), BALL, -Vec3::Y, 10.0, &floor, fsize, BOX).is_none());
    }

    /// A slab mesh (unit cube triangles) only overlaps where it really is.
    #[test]
    fn box_against_mesh() {
        let c = [
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
        ];
        let q = |a: usize, b: usize, cc: usize, d: usize| [[c[a], c[b], c[cc]], [c[a], c[cc], c[d]]];
        let mut tris = Vec::new();
        for f in [q(0, 1, 2, 3), q(4, 5, 6, 7), q(0, 1, 5, 4), q(3, 2, 6, 7), q(0, 3, 7, 4), q(1, 2, 6, 5)] {
            tris.extend(f);
        }
        let slab = at(Vec3::ZERO);
        let size = Vec3::new(10.0, 1.0, 10.0);
        // Above the slab: clear; crossing its top: hit; buried inside: hit.
        assert!(!obb_mesh(&at(Vec3::new(0.0, 2.0, 0.0)), Vec3::splat(1.0), &slab, size, &tris));
        assert!(obb_mesh(&at(Vec3::new(0.0, 0.8, 0.0)), Vec3::splat(1.0), &slab, size, &tris));
        assert!(obb_mesh(&at(Vec3::ZERO), Vec3::splat(0.2), &slab, size, &tris));
    }
}
