//! Dragging parts in the 3D view with the Select (or Move) tool: Studio's
//! freeform drag, ported from its DraggerFramework (SelectDragger.rbxm,
//! Luau bytecode; MECHANICS.md "Freeform drag parity") and checked against
//! 26 drags measured in an offline Studio (the `studio_*` tests below).
//!
//! - pressing on a part and dragging moves the selection; an unselected part
//!   is selected first;
//! - the selection's box (`DragBox`, SelectionInfo.computeInfo) is taken in
//!   the frame of one object's pivot: the only selected object, or for
//!   several the grabbed one (then re-centred on the box);
//! - with grid snap on, the grab point is rounded in the clicked face's
//!   surface frame (dispatchWorldClick.buildFreeformDragInfo);
//! - every move casts the mouse ray past the carried parts, builds the hit
//!   face's surface frame (DragHelper.getSurfaceMatrixForPrimitiveMesh,
//!   from Studio's getGeometry vertex / edge / face tables), rounds the hit
//!   on that frame's X and Z (computeDragSnapping; Shift turns snapping off
//!   while it's held), puts the grab point there and lifts the box so it
//!   rests on the surface (getDragTargetNew); the orientation is kept;
//! - over empty space the last surface's plane is extended
//!   (getSurfaceMatrixForSurfaceExtension); with no surface yet nothing moves;
//! - R turns 90° about the box axis nearest the surface normal under the
//!   mouse, T about the box axis nearest the camera's right vector
//!   (updateTiltRotateNew);
//! - Join Surfaces: on release each carried part is welded to every part it
//!   now touches face to face (JointPairs: a `Weld` parented to the moved
//!   part, C0 / C1 at the moved face's centre);
//! - one undo step per drag (joints included).

use std::collections::HashSet;

use egui::{Pos2, Rect};
use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::ustr;

use crate::app::Editor;
use crate::math::{Ray, Xform};
use crate::render::MeshKind;

#[derive(Clone)]
pub struct PartDrag {
    undo_parts: Option<Vec<(Ref, Xform)>>,
    undo_pivots: Option<Vec<PivotSnapshot>>,
    pivots: Vec<PivotSnapshot>,
    /// The part under the cursor when the drag began.
    grabbed: Ref,
    free: FreeDrag,
    /// Every part carried, with where it started.
    parts: Vec<(Ref, Xform)>,
    /// Their CFrame values as they were (undo puts these exact values back).
    originals: Vec<(Ref, Variant)>,
    carried: HashSet<Ref>,
    /// The last pointer position (R / T re-run the drag from it).
    last: Option<(Rect, Pos2)>,
    /// Shift held: grid snapping is off while dragging (shouldGridSnap).
    pub shift: bool,
    /// Alt held: Align Dragged Objects is off while dragging
    /// (FreeformDragger._shouldAlignDraggedObjects).
    pub alt: bool,
    pub moved: bool,
    /// Physical mode (DraggerMovementMode.Physical): the camera-plane drag.
    physical: Option<PhysDrag>,
}

/// FreeformDragger._updatePhysical: the grab point and the translation last
/// applied (PartMover.moveToWithIk with the collision mode).
#[derive(Clone, Debug)]
struct PhysDrag {
    grab: Vec3,
    /// Every part that moves: the selection and what is rigidly joined to it
    /// (the mechanism IKMoveTo carries), with where it started.
    parts: Vec<(Ref, Xform, Vec3, MeshKind, Variant)>,
    /// Anchored: Studio's IK dragger can't move the assembly at all.
    stuck: bool,
    last: Vec3,
}

/// Half the extent of a box (rotation `xf.rot`, `size`) along unit `n`.
fn half_extent(xf: &Xform, size: Vec3, n: Vec3) -> f32 {
    (0..3)
        .map(|i| n.dot(xf.rot.col(i)).abs() * size[i] * 0.5)
        .sum()
}

/// Studio's grid rounding (`math.floor(x / grid + 0.5) * grid`).
fn round_grid(x: f32, grid: f32) -> f32 {
    (x / grid + 0.5).floor() * grid
}

// ------------------------------------------------------------ geometry

/// A part as the dragger sees it: its CFrame, Size and shape.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Solid {
    pub xf: Xform,
    pub size: Vec3,
    pub kind: MeshKind,
}

/// One face of [`Geometry`]: a point on it, its outward normal, its edge
/// direction (getGeometry's `direction`) and its vertices, counter-clockwise
/// about the normal. `id` is the Enum.NormalId of the surface (Right 0, Top 1,
/// Back 2, Left 3, Bottom 4, Front 5).
#[derive(Clone, Debug)]
pub(crate) struct GFace {
    pub point: Vec3,
    pub normal: Vec3,
    pub dir: Vec3,
    pub verts: Vec<Vec3>,
    pub id: u8,
}

/// DraggerFramework.Utility.getGeometry: world vertices, edges and faces.
/// Blocks, balls and cylinders use the box tables; wedges their own.
/// (Corner wedges and meshes are treated as their box here.)
#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    pub verts: Vec<Vec3>,
    pub edges: Vec<(usize, usize)>,
    pub faces: Vec<GFace>,
}

impl Solid {
    pub fn geometry(&self) -> Geometry {
        let h = self.size * 0.5;
        let (r, u, b) = (self.xf.rot.col(0), self.xf.rot.col(1), self.xf.rot.col(2));
        let p = self.xf.pos;
        let v = |x: f32, y: f32, z: f32| p + r * (x * h.x) + u * (y * h.y) + b * (z * h.z);
        // Studio's tables are 1-based; these are the same lists, 0-based.
        if self.kind == MeshKind::Wedge {
            let verts = vec![
                v(1., 1., 1.), v(-1., 1., 1.), v(1., -1., 1.),
                v(1., -1., -1.), v(-1., -1., 1.), v(-1., -1., -1.),
            ];
            let edges = vec![(0, 1), (0, 3), (1, 5), (2, 3), (4, 5), (2, 4), (3, 5), (0, 2), (1, 4)];
            let slope = (b * -h.y + u * h.z).normalize_or(u);
            let f = |pt: usize, n: Vec3, d: Vec3, idx: &[usize], id: u8| GFace {
                point: verts[pt], normal: n, dir: d,
                verts: idx.iter().map(|i| verts[*i]).collect(), id,
            };
            let faces = vec![
                f(0, r, b, &[3, 0, 2], 0),
                f(1, -r, b, &[1, 5, 4], 3),
                f(2, -u, r, &[4, 5, 3, 2], 4),
                f(0, b, r, &[0, 1, 4, 2], 2),
                f(1, slope, slope.cross(r), &[1, 0, 3, 5], 5),
            ];
            return Geometry { verts, edges, faces };
        }
        let verts = vec![
            v(1., 1., 1.), v(1., 1., -1.), v(-1., 1., 1.), v(-1., 1., -1.),
            v(1., -1., 1.), v(1., -1., -1.), v(-1., -1., 1.), v(-1., -1., -1.),
        ];
        let edges = vec![
            (0, 1), (2, 3), (0, 2), (1, 3), (4, 5), (6, 7), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7),
        ];
        let f = |pt: usize, n: Vec3, d: Vec3, idx: &[usize], id: u8| GFace {
            point: verts[pt], normal: n, dir: d,
            verts: idx.iter().map(|i| verts[*i]).collect(), id,
        };
        let faces = vec![
            f(0, r, b, &[4, 5, 1, 0], 0),
            f(2, -r, b, &[2, 3, 7, 6], 3),
            f(0, u, r, &[0, 1, 3, 2], 1),
            f(4, -u, r, &[6, 7, 5, 4], 4),
            f(0, b, r, &[0, 2, 6, 4], 2),
            f(1, -b, r, &[5, 7, 3, 1], 5),
        ];
        Geometry { verts, edges, faces }
    }

    /// Where `ray` enters this solid: (distance, surface normal). A ray that
    /// starts inside doesn't hit (WorldRoot:Raycast).
    pub fn raycast(&self, ray: &Ray) -> Option<(f32, Vec3)> {
        if self.kind != MeshKind::Wedge {
            let t = ray.enter_obb(&self.xf, self.size)?;
            let p = ray.at(t);
            let local = self.xf.rot.transpose() * (p - self.xf.pos);
            let half = self.size * 0.5;
            let i = (0..3)
                .max_by(|a, b| (local[*a].abs() / half[*a].max(1e-6)).total_cmp(&(local[*b].abs() / half[*b].max(1e-6))))
                .unwrap_or(1);
            let n = self.xf.rot.col(i) * local[i].signum();
            return Some((t, n));
        }
        // Convex: clip the ray against every face plane.
        let g = self.geometry();
        let (mut tmin, mut tmax, mut best) = (f32::NEG_INFINITY, f32::INFINITY, None);
        for f in &g.faces {
            let dn = ray.dir.dot(f.normal);
            let dist = (f.point - ray.origin).dot(f.normal);
            if dn.abs() < 1e-9 {
                if dist < 0.0 {
                    return None;
                }
                continue;
            }
            let t = dist / dn;
            if dn < 0.0 {
                if t > tmin {
                    tmin = t;
                    best = Some(f.normal);
                }
            } else {
                tmax = tmax.min(t);
            }
        }
        (tmin <= tmax && tmin > 0.0).then_some(())?;
        Some((tmin, best?))
    }
}

/// DragHelper.findClosestBasis: a frame around `n` from the world axis least
/// aligned with it (used when no edge gives one).
fn closest_basis(n: Vec3) -> (Vec3, Vec3) {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z, -Vec3::X, -Vec3::Y, -Vec3::Z];
    let mut best = (Vec3::X, f32::INFINITY);
    for a in axes {
        let d = a.dot(n).abs();
        if d < best.1 {
            best = (a, d);
        }
    }
    let mut z = best.0.cross(n);
    if z.length() < 1e-5 {
        z = if n.x.abs() > 0.9 { Vec3::Y } else { Vec3::X };
    }
    let z = z.normalize();
    (-best.0.cross(z), z)
}

/// DragHelper.getSurfaceMatrixForPrimitiveMesh: the surface frame of the
/// face hit at `hit` (raycast normal `n`). Its origin is the end of the edge
/// whose line passes nearest the hit (any edge not above the face's plane),
/// Y is the normal, Z the edge direction and X = n × edge
/// (buildSurfaceMatrix: CFrame.fromMatrix(origin, -(edge × n), n)).
pub(crate) fn surface_matrix(g: &Geometry, hit: Vec3, n: Vec3) -> Xform {
    let face = g
        .faces
        .iter()
        .fold(None::<(&GFace, f32)>, |best, f| {
            let d = (hit - f.point).dot(f.normal).abs();
            match best {
                Some((_, bd)) if !(d < bd) => best,
                _ => Some((f, d)),
            }
        })
        .map(|(f, _)| f);
    let mut best: Option<(usize, usize, f32)> = None;
    if let Some(face) = face {
        for &(a, b) in &g.edges {
            let (pa, pb) = (g.verts[a], g.verts[b]);
            if (pa - face.point).dot(n) < 0.001 && (pb - face.point).dot(n) < 0.001 {
                let dir = (pb - pa).normalize_or_zero();
                let c = pa + dir * (hit - pa).dot(dir);
                let d = (hit - c).length();
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((a, b, d));
                }
            }
        }
    }
    if let Some((a, b, _)) = best {
        let (pa, pb) = (g.verts[a], g.verts[b]);
        let origin = if (pa - hit).length() < (pb - hit).length() { pa } else { pb };
        let edge = (pb - pa).normalize_or_zero();
        let x = n.cross(edge);
        if x.length() > 1e-5 {
            let x = x.normalize();
            return Xform { pos: origin, rot: Mat3::from_cols(x, n, x.cross(n)) };
        }
    }
    let (x, z) = closest_basis(n);
    Xform { pos: hit, rot: Mat3::from_cols(x, n, z) }
}

/// The selection's box (SelectionInfo.getLocalBoundingBox): a basis frame,
/// the box centre's offset in it and the box size in it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DragBox {
    pub basis: Xform,
    pub offset: Vec3,
    pub size: Vec3,
}

impl DragBox {
    /// SelectionInfo.computeInfo: the parts' boxes in `basis`; with more than
    /// one selected object (`objects != 1`) the basis moves to the box centre.
    pub fn new(basis: Xform, parts: &[(Xform, Vec3)], objects: usize) -> DragBox {
        let inv = basis.rot.transpose();
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for (xf, size) in parts {
            let c = inv * (xf.pos - basis.pos);
            let r = inv * xf.rot;
            let half = (r.col(0).abs() * size.x + r.col(1).abs() * size.y + r.col(2).abs() * size.z) * 0.5;
            lo = lo.min(c - half);
            hi = hi.max(c + half);
        }
        if !lo.is_finite() {
            return DragBox { basis, offset: Vec3::ZERO, size: Vec3::ZERO };
        }
        let (offset, size) = ((lo + hi) * 0.5, hi - lo);
        if objects != 1 {
            return DragBox { basis: Xform { pos: basis.apply(offset), rot: basis.rot }, offset: Vec3::ZERO, size };
        }
        DragBox { basis, offset, size }
    }
}

/// Rounds a matrix of quarter turns to exact 0 / ±1 (Utility.roundRotation).
fn round_rotation(m: Mat3) -> Mat3 {
    let r = |v: Vec3| Vec3::new(v.x.round(), v.y.round(), v.z.round());
    Mat3::from_cols(r(m.x_axis), r(m.y_axis), r(m.z_axis))
}

/// DragHelper.getSurfaceSnapForPartTreatedAsBox (Use Bounding Boxes): the
/// surface frame of `s` as its box (ExtentsCFrame / ExtentsSize). The face
/// is the box axis most aligned with the hit normal; the hit is projected
/// onto it; the origin is the face corner in the hit's quadrant and Z runs
/// along the nearer of the two edges there. Returns (projected hit, frame).
pub(crate) fn surface_matrix_box(s: &Solid, hit: Vec3, n: Vec3) -> (Vec3, Xform) {
    let axes = [s.xf.rot.col(0), s.xf.rot.col(1), s.xf.rot.col(2)];
    let half = s.size * 0.5;
    let (mut i, mut sign, mut best) = (0usize, 1.0f32, f32::NEG_INFINITY);
    for (k, a) in axes.iter().enumerate() {
        let d = a.dot(n);
        if d.abs() > best {
            best = d.abs();
            i = k;
            sign = if d >= 0.0 { 1.0 } else { -1.0 };
        }
    }
    let normal = axes[i] * sign;
    let centre = s.xf.pos + normal * half[i];
    let (j, k) = ((i + 1) % 3, (i + 2) % 3);
    let (u, v, hu, hv) = (axes[j], axes[k], half[j], half[k]);
    let d = hit - centre;
    let (a, b) = (d.dot(u), d.dot(v));
    let projected = centre + u * a + v * b;
    let (ca, cb) = (a.clamp(-hu, hu), b.clamp(-hv, hv));
    let corner = centre + u * if ca >= 0.0 { hu } else { -hu } + v * if cb >= 0.0 { hv } else { -hv };
    let edge = if hu - ca.abs() <= hv - cb.abs() { v } else { u };
    let x = normal.cross(edge).normalize_or_zero();
    (projected, Xform { pos: corner, rot: Mat3::from_cols(x, normal, x.cross(normal)) })
}

/// DragHelper.getSizeInSurface: the target part's extent along the surface
/// frame's X and Z (the box's projected size; Y is 0).
pub(crate) fn size_in_surface(s: &Solid, m: &Xform) -> glam::Vec2 {
    let r = m.rot.transpose() * s.xf.rot;
    let row = |i: usize| (0..3).map(|c| (s.size[c] * r.col(c)[i]).abs()).sum::<f32>();
    glam::Vec2::new(row(0), row(2))
}

/// Utility.snapRotationToPrimaryDirection (Align Dragged Objects): turns a
/// rotation to the nearest one whose axes lie on ±X / ±Y / ±Z. The axis
/// with the largest component snaps first, then the next; the third
/// follows from their cross product.
pub(crate) fn snap_rotation_to_primary(m: Mat3) -> Mat3 {
    fn largest(v: Vec3) -> f32 {
        v.x.abs().max(v.y.abs()).max(v.z.abs())
    }
    fn snap(v: Vec3) -> Vec3 {
        let mut best = (v, f32::NEG_INFINITY);
        for a in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            let d = v.dot(a);
            if best.1 < d {
                best = (a, d);
            }
        }
        best.0
    }
    fn safe_cross(a: Vec3, b: Vec3) -> Vec3 {
        let mut c = a.cross(b);
        if c.length() < 1e-6 {
            c = a.cross(if a.x.abs() < 0.99 { Vec3::X } else { Vec3::Y });
        }
        c.normalize()
    }
    let (mut r, mut u, b) = (m.col(0), m.col(1), m.col(2));
    let (lr, lu, lb) = (largest(r), largest(u), largest(b));
    if lu < lr && lb < lr {
        r = snap(r);
        if lb < lu {
            u = snap(u);
        } else {
            u = safe_cross(snap(b), r);
        }
    } else if lb < lu {
        u = snap(u);
        if lb < lr {
            r = snap(r);
        } else {
            r = safe_cross(u, snap(b));
        }
    } else {
        let bb = snap(b);
        if lu < lr {
            r = snap(r);
            u = safe_cross(bb, r);
        } else {
            u = snap(u);
            r = safe_cross(u, bb);
        }
    }
    Mat3::from_cols(r, u, r.cross(u))
}

/// The surface a drag rests on: the hit (projected onto the box face with
/// Use Bounding Boxes), its frame, and the target part's size in that frame
/// (None over empty space: getSurfaceMatrixForSurfaceExtension).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Surf {
    pub hit: Vec3,
    pub m: Xform,
    pub tsize: Option<glam::Vec2>,
}

/// The settings a free drag's target depends on (DraggerContext:
/// shouldGridSnap / shouldAlignDraggedObjects / shouldPartSnap).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DragOpts {
    pub grid: Option<f32>,
    /// Align Dragged Objects (and Alt not held).
    pub align: bool,
    /// Snap to Parts: the camera (position, forward, vertical FOV in degrees)
    /// that sets the soft-snap distance.
    pub soft: Option<(Vec3, Vec3, f32)>,
}

/// DragHelper.softSnapIntervals: of the selection's two edges and centre
/// (`c` ± `hs`) and the target's (`t` ± `ht`), the closest pair: (offset, line).
fn soft_snap_interval(c: f32, hs: f32, t: f32, ht: f32) -> (f32, f32) {
    let mut best = (f32::INFINITY, 0.0);
    for i in [-1.0f32, 0.0, 1.0] {
        let a = c + i * hs;
        for j in [-1.0f32, 0.0, 1.0] {
            let b = t + j * ht;
            let d = a - b;
            if d.abs() < best.0.abs() {
                best = (d, b);
            }
        }
    }
    best
}

/// DragHelper.chooseBetterSnap: the grid or the soft snap, whichever moves
/// less (the grid on a tie within 0.01).
fn better_snap(grid: Option<f32>, soft: Option<f32>) -> f32 {
    match (grid, soft) {
        (Some(g), Some(s)) => {
            if (g.abs() - s.abs()).abs() < 0.01 || g.abs() < s.abs() {
                g
            } else {
                s
            }
        }
        (Some(g), None) => g,
        (None, Some(s)) => s,
        (None, None) => 0.0,
    }
}

/// The state of one freeform drag (FreeformDragger + DragHelper), free of
/// the editor so tests can run it on Studio's numbers.
#[derive(Clone, Debug)]
pub(crate) struct FreeDrag {
    pub bbox: DragBox,
    /// The grab point in the box's basis (`draggedPoint`).
    pub dragged: Vec3,
    /// R / T quarter turns, in the basis (`tiltRotate`).
    pub tilt: Mat3,
    /// The last surface frame (`lastTargetMat`).
    pub last_target: Option<Xform>,
    /// Use Bounding Boxes: target parts are treated as their boxes.
    pub use_bbox: bool,
}

impl FreeDrag {
    /// `click` is where the press hit the clicked part, `clicked` that part's
    /// surface frame there (rounded on it when grid snap is on). With Use
    /// Bounding Boxes the grab point is the click put on the box face
    /// (dispatchWorldClick.buildFreeformDragInfo).
    pub fn begin(bbox: DragBox, click: Vec3, clicked: Option<Xform>, grid: Option<f32>, use_bbox: bool) -> FreeDrag {
        let mut grab = click;
        if let Some(m) = clicked {
            let l = m.rot.transpose() * (click - m.pos);
            let (mut x, mut z) = (l.x, l.z);
            if let Some(g) = grid {
                x = round_grid(x, g);
                z = round_grid(z, g);
            }
            if use_bbox {
                grab = m.apply(Vec3::new(x, 0.0, z));
            } else if grid.is_some() {
                grab = m.apply(Vec3::new(x, l.y, z));
            }
        }
        let dragged = bbox.basis.rot.transpose() * (grab - bbox.basis.pos);
        FreeDrag { bbox, dragged, tilt: Mat3::IDENTITY, last_target: None, use_bbox }
    }

    /// The surface under `ray`: the nearest hit (point, normal, solid), else
    /// the last surface's plane extended. None: nothing to drag onto.
    pub fn surface(&self, ray: &Ray, hit: Option<(Vec3, Vec3, Solid)>) -> Option<Surf> {
        if let Some((p, n, s)) = hit {
            let (hit, m) = if self.use_bbox { surface_matrix_box(&s, p, n) } else { (p, surface_matrix(&s.geometry(), p, n)) };
            return Some(Surf { hit, m, tsize: Some(size_in_surface(&s, &m)) });
        }
        let m = self.last_target?;
        let up = m.rot.col(1);
        let dn = ray.dir.dot(up);
        if dn.abs() < 1e-9 {
            return None;
        }
        let t = (m.pos - ray.origin).dot(up) / dn;
        (t.is_finite() && t > 0.0).then(|| Surf { hit: ray.at(t), m, tsize: None })
    }

    /// The selection's rotation relative to surface `m` (getDragTargetNew's
    /// R7): Align Dragged Objects snaps it to the surface's axes.
    fn relative_rotation(&self, m: &Xform, align: bool) -> Mat3 {
        let r = m.rot.transpose() * self.bbox.basis.rot;
        if align { snap_rotation_to_primary(r) } else { r }
    }

    /// DragHelper.getDragTargetNew: the transform that carries the selection
    /// from where it started to rest on the surface under the mouse
    /// (`mainCFrame * basis⁻¹`).
    pub fn target(&mut self, surface: Surf, opts: DragOpts) -> Xform {
        let Surf { hit, m, tsize } = surface;
        self.last_target = Some(m);
        let b = self.bbox.basis;
        let r7 = self.relative_rotation(&m, opts.align);
        let rt = r7 * self.tilt;
        let size = rt.col(0).abs() * self.bbox.size.x
            + rt.col(1).abs() * self.bbox.size.y
            + rt.col(2).abs() * self.bbox.size.z;
        let l = m.rot.transpose() * (hit - m.pos);
        // computeDragSnapping: grid and soft snap offsets on X / Z.
        let grid_d = |v: f32| opts.grid.map(|g| round_grid(v, g) - v);
        let (mut sx, mut sz) = (None, None);
        if let (Some((cpos, cfwd, fov)), Some(ts)) = (opts.soft, tsize) {
            let centre = l + rt * (self.bbox.offset - self.dragged);
            let thr = 0.02 * fov.to_radians().sin() * (hit - cpos).dot(cfwd) * crate::studio_settings::studio("DraggerSoftSnapMarginFactor", crate::mech::SOFT_SNAP_MARGIN as f64) as f32;
            let sgn = |v: f32| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 };
            let (dx, _) = soft_snap_interval(centre.x, size.x * 0.5, sgn(l.x) * 0.5 * ts.x, ts.x * 0.5);
            if dx.abs() < thr {
                sx = Some(-dx);
            }
            let (dz, _) = soft_snap_interval(centre.z, size.z * 0.5, sgn(l.z) * 0.5 * ts.y, ts.y * 0.5);
            if dz.abs() < thr {
                sz = Some(-dz);
            }
        }
        let x = l.x + better_snap(grid_d(l.x), sx);
        let z = l.z + better_snap(grid_d(l.z), sz);
        let base = m.pos + m.rot.col(0) * x + m.rot.col(2) * z;
        let lift = 0.5 * size.y - (rt * (self.bbox.offset - self.dragged)).y;
        let rot = round_rotation_if_exact(m.rot * rt);
        let pos = base + m.rot.col(1) * lift - rot * self.dragged;
        let trot = rot * b.rot.transpose();
        Xform { pos: pos - trot * b.pos, rot: trot }
    }

    /// DragHelper.updateTiltRotateNew: a quarter turn about the axis of the
    /// selection's frame on surface `m` (aligned with Align Dragged Objects)
    /// nearest `axis` (R: the surface normal; T: the camera's right).
    pub fn turn(&mut self, axis: Vec3, m: Option<Xform>, align: bool) {
        let rot = match m {
            Some(m) => m.rot * self.relative_rotation(&m, align),
            None => self.bbox.basis.rot,
        };
        let mut best = (Vec3::Y, f32::NEG_INFINITY);
        for a in [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z] {
            let d = (rot * a).dot(axis);
            if d > best.1 {
                best = (a, d);
            }
        }
        self.tilt = round_rotation(Mat3::from_axis_angle(best.0, std::f32::consts::FRAC_PI_2) * self.tilt);
    }
}

/// `b.rot * tilt` keeps f32 noise; an exactly axis-aligned result is
/// rounded like Studio's (its CFrames come out exact).
fn round_rotation_if_exact(m: Mat3) -> Mat3 {
    let close = |v: Vec3| (v - Vec3::new(v.x.round(), v.y.round(), v.z.round())).abs().max_element() < 1e-6;
    if close(m.x_axis) && close(m.y_axis) && close(m.z_axis) {
        round_rotation(m)
    } else {
        m
    }
}

// ------------------------------------------------------------- joints

/// JointPairs.isVertexInFace: strictly inside (0.01) every edge of `face`.
fn vertex_in_face(v: Vec3, face: &[Vec3], n: Vec3) -> bool {
    (0..face.len()).all(|i| {
        let a = face[(i + face.len() - 1) % face.len()];
        let b = face[i];
        (b - a).cross(v - a).dot(n) >= 0.01
    })
}

/// Math.intersectRayRay: the parameter along `d0` of the closest approach.
fn ray_ray(p0: Vec3, d0: Vec3, p1: Vec3, d1: Vec3) -> Option<f32> {
    let num = (p1 - p0).dot(d0) * d1.dot(d1) + (p0 - p1).dot(d1) * d0.dot(d1);
    let den = d0.dot(d0) * d1.dot(d1) - d0.dot(d1) * d0.dot(d1);
    (den != 0.0).then(|| num / den)
}

/// JointPairs.edgeIntersectsEdge.
fn edge_intersects_edge(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> bool {
    let (da, db) = (a1 - a0, b1 - b0);
    let (la, lb) = (da.length(), db.length());
    let cos = da.dot(db) / la / lb;
    let (ea, eb) = (0.01 / la, 0.01 / lb);
    if (cos.abs() - 1.0).abs() < 0.0001 {
        if cos > 0.0 {
            return false;
        }
        let t0 = (a0 - b0).dot(db) / lb / lb;
        if (a0 - (b0 + db * t0)).length() > 0.001 {
            return false;
        }
        let t1 = (a1 - b0).dot(db) / lb / lb;
        return t0.clamp(0.0, 1.0) - t1.clamp(0.0, 1.0) > 0.001;
    }
    let Some(t) = ray_ray(a0, da, b0, db) else { return false };
    if t < ea || t > 1.0 - ea {
        return false;
    }
    let Some(s) = ray_ray(b0, db, a0, da) else { return false };
    ea.max(eb) > 0.0 && eb <= s && s <= 1.0 - eb
}

fn edges_intersect(a: &[Vec3], b: &[Vec3]) -> bool {
    (0..a.len()).any(|i| {
        let (a0, a1) = (a[(i + a.len() - 1) % a.len()], a[i]);
        (0..b.len()).any(|j| edge_intersects_edge(a0, a1, b[(j + b.len() - 1) % b.len()], b[j]))
    })
}

/// JointPairs.tryToCreateJointPair: the first pair of faces that touch
/// (opposite normals within 0.999, planes within 0.05, overlapping); the
/// joint frame is the moved face's centre, X along its edge direction and Z
/// along its normal. Returns (C0, C1) for a Weld from `moved` to `other`;
/// None when no face pair touches or the shapes can't be joined (cylinders
/// only on their flat ends).
pub(crate) fn joint_between(moved: &Solid, other: &Solid) -> Option<(Xform, Xform)> {
    let (ga, gb) = (moved.geometry(), other.geometry());
    for fa in &ga.faces {
        for fb in &gb.faces {
            if fa.normal.dot(fb.normal) >= -0.999 || (fa.point - fb.point).dot(fb.normal).abs() >= 0.05 {
                continue;
            }
            let touching = fa.verts.iter().any(|v| vertex_in_face(*v, &fb.verts, fb.normal))
                || fb.verts.iter().any(|v| vertex_in_face(*v, &fa.verts, -fb.normal))
                || edges_intersect(&fa.verts, &fb.verts);
            if !touching {
                continue;
            }
            let ends = |s: &Solid, f: &GFace| s.kind != MeshKind::Cylinder || f.id == 0 || f.id == 3;
            if !ends(moved, fa) || !ends(other, fb) {
                return None;
            }
            let c = fa.verts.iter().copied().sum::<Vec3>() / fa.verts.len() as f32;
            let frame = Xform { pos: c, rot: Mat3::from_cols(fa.dir, fa.normal.cross(fa.dir), fa.normal) };
            let c0 = moved.xf.inverse().mul(&frame);
            let c1 = other.xf.inverse().mul(&frame);
            return Some((c0, c1));
        }
    }
    None
}

trait XformMul {
    fn mul(&self, o: &Xform) -> Xform;
}
impl XformMul for Xform {
    fn mul(&self, o: &Xform) -> Xform {
        Xform { pos: self.apply(o.pos), rot: self.rot * o.rot }
    }
}

// ------------------------------------------------------------- editor

impl Editor {
    pub(crate) fn solid_of(&self, sp: &crate::scene::ScenePart) -> Solid {
        Solid { xf: sp.xf, size: sp.size, kind: sp.draw.mesh }
    }

    /// The nearest part under `ray` that the drag doesn't carry (the
    /// dragger's raycast: transparent parts count, CanQuery off doesn't).
    fn drag_hit(&self, ray: &Ray, carried: &HashSet<Ref>) -> Option<(Vec3, Vec3, Solid)> {
        let mut best: Option<(f32, Vec3, Solid)> = None;
        for (sp, b) in self.scene.parts.iter().zip(&self.scene.bounds) {
            let oc = b.truncate() - ray.origin;
            let along = oc.dot(ray.dir);
            if oc.length_squared() - along * along > b.w * b.w || along < -b.w {
                continue;
            }
            if carried.contains(&sp.r) {
                continue;
            }
            let solid = self.solid_of(sp);
            let Some((t, n)) = solid.raycast(ray) else { continue };
            if best.as_ref().is_some_and(|(bt, _, _)| t >= *bt) {
                continue;
            }
            if matches!(self.doc.prop(sp.r, "CanQuery"), Some(Variant::Bool(false))) {
                continue;
            }
            best = Some((t, n, solid));
        }
        best.map(|(t, n, s)| (ray.at(t), n, s))
    }

    fn drag_grid(&self) -> Option<f32> {
        (self.snap_move && self.move_inc > 0.0).then_some(self.move_inc)
    }

    /// The free drag's settings this frame: Shift turns grid and part
    /// snapping off, Alt Align Dragged Objects.
    fn drag_opts(&self, shift: bool, alt: bool) -> DragOpts {
        DragOpts {
            grid: if shift { None } else { self.drag_grid() },
            align: self.align_dragged && !alt,
            soft: (self.part_snap && !shift).then(|| (self.cam.pos, self.cam.forward(), self.cam.fov_deg)),
        }
    }

    /// The pivot a selectable object turns about (Model:GetPivot / the
    /// part's CFrame * PivotOffset).
    pub(crate) fn object_pivot(&self, r: Ref) -> Option<Xform> {
        if let Some(p) = editor_model_pivot(&self.doc, r) {
            return Some(p);
        }
        let sp = self.scene.get(r)?;
        Some(match self.doc.prop(r, "PivotOffset") {
            Some(Variant::CFrame(cf)) => sp.xf.mul(&Xform::from_cframe(&cf)),
            _ => sp.xf,
        })
    }

    /// Starts dragging the part under `p`; false when there is none (the
    /// caller starts a selection box instead).
    pub fn start_part_drag(&mut self, rect: Rect, p: Pos2, add: bool) -> bool {
        let Some(raw) = self.pick(rect, p, true) else {
            return false;
        };
        let target = self.pick(rect, p, false).unwrap_or(raw);
        if !self.doc.selection.contains(&target) {
            if add {
                let mut sel = self.doc.selection.clone();
                sel.push(target);
                self.select(sel);
            } else {
                self.select(vec![target]);
            }
        }
        let Some(gp) = self.scene.get(raw) else {
            return false;
        };
        let clicked = self.solid_of(gp);
        let ray = self.cam.ray(rect, p);
        let (t, n) = clicked
            .raycast(&ray)
            .or_else(|| ray.hit_obb(&clicked.xf, clicked.size).map(|t| (t, Vec3::Y)))
            .unwrap_or((0.0, Vec3::Y));
        let click = ray.at(t);
        let parts: Vec<(Ref, Xform)> = self
            .selected_parts()
            .into_iter()
            .filter(|r| !matches!(self.doc.prop(*r, "Locked"), Some(Variant::Bool(true))))
            .filter_map(|r| self.scene.get(r).map(|sp| (r, sp.xf)))
            .collect();
        if parts.is_empty() {
            return false;
        }
        // The box's basis: the only selected object's pivot, or with several
        // the grabbed one's (SelectionInfo.computeInfo, measured in Studio).
        let objects: Vec<Ref> = self
            .doc
            .top_level(&self.doc.selection.clone())
            .into_iter()
            .filter(|r| self.scene.get(*r).is_some() || self.doc.class_of(*r).is_some_and(|c| c == "Model"))
            .collect();
        let basis_object = if objects.len() == 1 { objects[0] } else { target };
        let basis = self.object_pivot(basis_object).unwrap_or(clicked.xf);
        let boxes: Vec<(Xform, Vec3)> = parts
            .iter()
            .filter_map(|(r, xf)| self.scene.get(*r).map(|sp| (*xf, sp.size)))
            .collect();
        let bbox = DragBox::new(basis, &boxes, objects.len());
        let face = if self.use_bounding_boxes {
            surface_matrix_box(&clicked, click, n).1
        } else {
            surface_matrix(&clicked.geometry(), click, n)
        };
        let free = FreeDrag::begin(bbox, click, Some(face), self.drag_grid(), self.use_bounding_boxes);
        let physical = self.physical_drag.then(|| self.phys_drag_begin(&parts, free.bbox.basis.apply(free.dragged)));
        let originals = parts.iter().filter_map(|(r, _)| Some((*r, self.doc.prop(*r, "CFrame")?))).collect();
        let carried = parts.iter().map(|(r, _)| *r).collect();
        self.part_drag = Some(PartDrag {
            undo_parts: None,
            undo_pivots: None,
            pivots: selected_model_pivots(&self.doc),
            grabbed: raw,
            free,
            parts,
            originals,
            carried,
            last: None,
            shift: false,
            alt: false,
            moved: false,
            physical,
        });
        true
    }

    /// Moves the carried parts so the selection rests on the surface under `p`.
    pub fn update_part_drag(&mut self, rect: Rect, p: Pos2) {
        let Some(d) = self.part_drag.as_ref() else {
            return;
        };
        let ray = self.cam.ray(rect, p);
        if d.physical.is_some() {
            self.update_phys_drag(&ray);
            if let Some(d) = self.part_drag.as_mut() {
                d.last = Some((rect, p));
            }
            return;
        }
        let hit = self.drag_hit(&ray, &d.carried);
        let opts = self.drag_opts(d.shift, d.alt);
        let mut free = d.free.clone();
        let Some(surface) = free.surface(&ray, hit) else {
            if let Some(d) = self.part_drag.as_mut() {
                d.last = Some((rect, p));
            }
            return;
        };
        let t = free.target(surface, opts);
        let moves: Vec<(Ref, Xform)> = d
            .parts
            .iter()
            .map(|(r, xf)| (*r, t.mul(xf)))
            .collect();
        let moved = moves
            .iter()
            .zip(&d.parts)
            .any(|((_, a), (_, b))| (a.pos - b.pos).length() > 1e-4 || !a.rot.abs_diff_eq(b.rot, 1e-6));
        let pivots = d.pivots.clone();
        apply_model_pivots(&mut self.doc, &pivots, t);
        for (r, xf) in moves {
            self.doc
                .set_prop_live(r, ustr("CFrame"), Variant::CFrame(xf.to_cframe()));
        }
        if let Some(d) = self.part_drag.as_mut() {
            d.free = free;
            d.last = Some((rect, p));
            d.moved |= moved;
        }
    }

    /// R (turn) / T (tilt) while dragging: a quarter turn, then the drag is
    /// re-run from the last pointer position.
    pub fn rotate_part_drag(&mut self, tilt: bool) {
        let Some(d) = self.part_drag.as_ref() else { return };
        // Physical drags don't turn (FreeformDragger.rotate returns when
        // constraints are solved).
        if d.physical.is_some() {
            return;
        }
        let Some((rect, p)) = d.last else { return };
        let ray = self.cam.ray(rect, p);
        let hit = self.drag_hit(&ray, &d.carried);
        let m = d.free.surface(&ray, hit).map(|s| s.m);
        let axis = if tilt { Some(self.cam.right()) } else { m.map(|m| m.rot.col(1)) };
        let Some(axis) = axis else { return };
        let align = self.drag_opts(d.shift, d.alt).align;
        if let Some(d) = self.part_drag.as_mut() {
            d.free.turn(axis, m, align);
            d.moved = true;
        }
        self.update_part_drag(rect, p);
    }

    /// Ctrl+R (RotateSelectionAction) / Ctrl+T (TiltSelectionAction):
    /// a quarter turn of every selected part about the centre of their
    /// bounding box, measured in the last part's frame. One undo step,
    /// "SelectionRotate" / "SelectionTilt" (MECHANICS.md "Ctrl+R / Ctrl+T").
    pub fn rotate_selection(&mut self, tilt: bool) {
        let parts = self.selected_parts();
        let m = if tilt { tilt_matrix(self.cam.forward()) } else { ROTATE_MATRIX };
        quarter_turn_selection(&mut self.doc, &parts, m, tilt);
    }

    pub(crate) fn duplicate_part_drag(&mut self, map: &std::collections::HashMap<Ref, Ref>) {
        let Some(d) = self.part_drag.as_mut() else { return };
        d.parts.retain_mut(|(r, _)| {
            if let Some(copy) = map.get(r) { *r = *copy; true } else { false }
        });
        d.pivots.retain_mut(|p| { if let Some(copy) = map.get(&p.r) { p.r = *copy; true } else { false } });
        d.undo_pivots = Some(pivot_undo_baseline(&self.doc, &d.pivots));
        d.grabbed = map.get(&d.grabbed).copied().unwrap_or(d.grabbed);
        d.carried = d.parts.iter().map(|(r, _)| *r).collect();
        d.undo_parts = Some(d.parts.iter().filter_map(|(r, _)| {
            let Variant::CFrame(cf) = self.doc.dom.get_by_ref(*r)?.properties.get(&ustr("CFrame"))? else { return None };
            Some((*r, Xform::from_cframe(cf)))
        }).collect());
        d.moved = false;
    }

    /// Ends the drag: one undo step for everything it moved, and with Join
    /// Surfaces on the welds to whatever the parts now touch.
    pub fn finish_part_drag(&mut self) {
        let Some(d) = self.part_drag.take() else {
            return;
        };
        if !d.moved {
            return;
        }
        let mut changes: Vec<_> = d
            .undo_parts.as_ref().unwrap_or(&d.parts)
            .iter()
            .filter_map(|(r, xf)| {
                let new = self
                    .doc
                    .dom
                    .get_by_ref(*r)?
                    .properties
                    .get(&ustr("CFrame"))
                    .cloned()?;
                let old = match &d.undo_parts {
                    None => d.originals.iter().find(|(o, _)| o == r).map(|(_, v)| v.clone()),
                    Some(_) => None,
                }
                .unwrap_or_else(|| Variant::CFrame(xf.to_cframe()));
                Some((*r, ustr("CFrame"), Some(old), new))
            })
            .collect();
        changes.extend(pivot_changes(&self.doc, d.undo_pivots.as_ref().unwrap_or(&d.pivots)));
        // A physical drag also moved what's joined to the selection; and it
        // makes no surface joints (only _updateGeometric computes JointPairs).
        if let Some(ph) = &d.physical {
            for (r, _, _, _, old) in &ph.parts {
                if d.parts.iter().any(|(p, _)| p == r) {
                    continue;
                }
                if let Some(new) = self.doc.dom.get_by_ref(*r).and_then(|i| i.properties.get(&ustr("CFrame")).cloned()) {
                    changes.push((*r, ustr("CFrame"), Some(old.clone()), new));
                }
            }
        }
        let carried: Vec<Ref> = d.parts.iter().map(|(r, _)| *r).collect();
        let joints = if self.join_surfaces && d.physical.is_none() { self.surface_joints(&carried) } else { Vec::new() };
        self.doc.record_drag("Drag", changes, joints);
    }

    /// JointPairs.new + createJoints: a `Weld` from each moved part to every
    /// part it touches face to face (not carried, not already joined to it,
    /// centres within the two half-diagonals + 0.05). The welds are
    /// inserted (parent = the moved part) and returned for the undo step.
    pub(crate) fn surface_joints(&mut self, moved: &[Ref]) -> Vec<(Ref, Ref)> {
        let carried: HashSet<Ref> = moved.iter().copied().collect();
        self.scene.sync(&mut self.doc);
        let mut made = Vec::new();
        for &a in moved {
            let Some(sa) = self.scene.get(a).map(|sp| self.solid_of(sp)) else { continue };
            let ra = sa.size.length() / 2.0;
            let others: Vec<(Ref, Solid)> = self
                .scene
                .parts
                .iter()
                .filter(|sp| !carried.contains(&sp.r))
                .filter(|sp| (sp.xf.pos - sa.xf.pos).length() <= ra + sp.size.length() / 2.0 + 0.05)
                .map(|sp| (sp.r, self.solid_of(sp)))
                .collect();
            for (b, sb) in others {
                if joined(&self.doc, a, b) || in_humanoid_model(&self.doc, a) || in_humanoid_model(&self.doc, b) {
                    continue;
                }
                let Some((c0, c1)) = joint_between(&sa, &sb) else { continue };
                let w = self.doc.dom.insert(
                    a,
                    rbx_dom_weak::InstanceBuilder::new("Weld")
                        .with_name("Weld")
                        .with_property("Part0", Variant::Ref(a))
                        .with_property("Part1", Variant::Ref(b))
                        .with_property("C0", Variant::CFrame(c0.to_cframe()))
                        .with_property("C1", Variant::CFrame(c1.to_cframe())),
                );
                made.push((w, a));
            }
        }
        made
    }
}

// ------------------------------------------------------------ physical

/// Parts joined rigidly (JointInstances: Weld, Motor6D, Snap…, and enabled
/// WeldConstraints) to any of `start`: the assembly Studio's IK dragger
/// carries along.
pub(crate) fn rigid_assembly(doc: &crate::doc::Document, start: &[Ref]) -> Vec<Ref> {
    let mut links: std::collections::HashMap<Ref, Vec<Ref>> = std::collections::HashMap::new();
    for i in doc.dom.descendants() {
        if !(crate::doc::is_a(&i.class, "JointInstance") || i.class == "WeldConstraint") {
            continue;
        }
        if matches!(i.properties.get(&ustr("Enabled")), Some(Variant::Bool(false))) {
            continue;
        }
        let get = |k: &str| match i.properties.get(&ustr(k)) {
            Some(Variant::Ref(r)) if r.is_some() => Some(*r),
            _ => None,
        };
        if let (Some(a), Some(b)) = (get("Part0"), get("Part1")) {
            links.entry(a).or_default().push(b);
            links.entry(b).or_default().push(a);
        }
    }
    let mut seen: Vec<Ref> = start.to_vec();
    let mut i = 0;
    while i < seen.len() {
        for n in links.get(&seen[i]).into_iter().flatten() {
            if !seen.contains(n) {
                seen.push(*n);
            }
        }
        i += 1;
    }
    seen
}

impl Editor {
    /// Physical mode's drag start: the grab point and the assembly that
    /// moves (an anchored assembly doesn't move: IKMoveTo can't solve it).
    fn phys_drag_begin(&self, parts: &[(Ref, Xform)], grab: Vec3) -> PhysDrag {
        let start: Vec<Ref> = parts.iter().map(|(r, _)| *r).collect();
        let all = rigid_assembly(&self.doc, &start);
        let parts: Vec<(Ref, Xform, Vec3, MeshKind, Variant)> = all
            .iter()
            .filter_map(|r| {
                let sp = self.scene.get(*r)?;
                Some((*r, sp.xf, sp.size, sp.draw.mesh, self.doc.prop(*r, "CFrame")?))
            })
            .collect();
        let stuck = parts.iter().any(|(r, ..)| matches!(self.doc.prop(*r, "Anchored"), Some(Variant::Bool(true))));
        PhysDrag { grab, parts, stuck, last: Vec3::ZERO }
    }

    /// Physical mode: whether the assembly moved by `delta` would overlap a
    /// CanCollide part outside it (IKCollisionsMode.IncludeContactedMechanisms;
    /// touching is allowed).
    fn phys_overlaps(&self, ph: &PhysDrag, delta: Vec3) -> bool {
        let skip: HashSet<Ref> = ph.parts.iter().map(|p| p.0).collect();
        ph.parts.iter().any(|(_, xf, size, kind, _)| {
            let a = Solid { xf: Xform { pos: xf.pos + delta, rot: xf.rot }, size: *size, kind: *kind };
            let ra = size.length() * 0.5;
            self.scene.parts.iter().zip(&self.scene.bounds).any(|(sp, bd)| {
                !skip.contains(&sp.r)
                    && (bd.truncate() - a.xf.pos).length() <= ra + bd.w
                    && !matches!(self.doc.prop(sp.r, "CanCollide"), Some(Variant::Bool(false)))
                    && solids_overlap(&a, &self.solid_of(sp), crate::mech::COLLISION_OVERLAP)
            })
        })
    }

    /// FreeformDragger._updatePhysical: DragHelper.getCameraPlaneDragTarget
    /// (the mouse ray on the plane through the grab point facing the
    /// camera) gives a translation; with Collisions the assembly stops
    /// against other parts and slides along them, as the IK solve does.
    fn update_phys_drag(&mut self, ray: &Ray) {
        let Some(ph) = self.part_drag.as_ref().and_then(|d| d.physical.clone()) else { return };
        if ph.stuck {
            return;
        }
        let n = -self.cam.forward();
        let dn = ray.dir.dot(n);
        if dn.abs() < 1e-9 {
            return;
        }
        let t = (ph.grab - ray.origin).dot(n) / dn;
        if !(t >= 0.0) {
            return;
        }
        let want = ray.at(t) - ph.grab;
        let delta = if self.collisions {
            // Each axis in turn (largest move first), swept in steps no
            // longer than a quarter of the smallest part so nothing tunnels:
            // a blocked axis stops at contact (bisected to
            // COLLISION_EPS), free ones slide on.
            let step_len = ph.parts.iter().map(|p| p.2.min_element()).fold(f32::INFINITY, f32::min).max(0.05) * 0.25;
            let mut g = ph.last;
            let step = want - ph.last;
            let mut order = [0usize, 1, 2];
            order.sort_by(|a, b| step[*b].abs().total_cmp(&step[*a].abs()));
            for i in order {
                let (from, to) = (g[i], want[i]);
                let n = ((to - from).abs() / step_len).ceil().max(1.0) as usize;
                let at = |v: f32| {
                    let mut d = g;
                    d[i] = v;
                    d
                };
                let mut ok = from;
                for k in 1..=n {
                    let v = from + (to - from) * (k as f32 / n as f32);
                    if self.phys_overlaps(&ph, at(v)) {
                        ok = safe_amount(ok, v, crate::mech::COLLISION_EPS, |x| self.phys_overlaps(&ph, at(x)));
                        break;
                    }
                    ok = v;
                }
                g[i] = ok;
            }
            g
        } else {
            want
        };
        for (r, xf, ..) in &ph.parts {
            let moved = Xform { pos: xf.pos + delta, rot: xf.rot };
            self.doc.set_prop_live(*r, ustr("CFrame"), Variant::CFrame(moved.to_cframe()));
        }
        let pivots = self.part_drag.as_ref().map(|d| d.pivots.clone()).unwrap_or_default();
        apply_model_pivots(&mut self.doc, &pivots, Xform { pos: delta, rot: Mat3::IDENTITY });
        if let Some(d) = self.part_drag.as_mut() {
            if let Some(p) = d.physical.as_mut() {
                p.last = delta;
            }
            d.moved |= delta.length() > 1e-4;
        }
    }
}

// ---------------------------------------------------------- collisions

/// Whether two solids overlap by more than `tol` (BasePart.ArePartsTouchingOthers
/// with `overlapIgnored`): separating-axis test over both shapes' face
/// normals and edge-direction cross products.
pub(crate) fn solids_overlap(a: &Solid, b: &Solid, tol: f32) -> bool {
    let (ga, gb) = (a.geometry(), b.geometry());
    let dirs = |g: &Geometry| -> Vec<Vec3> {
        let mut out: Vec<Vec3> = Vec::new();
        for &(i, j) in &g.edges {
            let d = (g.verts[j] - g.verts[i]).normalize_or_zero();
            if d != Vec3::ZERO && !out.iter().any(|o| o.cross(d).length() < 1e-4) {
                out.push(d);
            }
        }
        out
    };
    let mut axes: Vec<Vec3> = ga.faces.iter().chain(&gb.faces).map(|f| f.normal).collect();
    for ea in dirs(&ga) {
        for eb in dirs(&gb) {
            let c = ea.cross(eb);
            if c.length() > 1e-4 {
                axes.push(c.normalize());
            }
        }
    }
    let span = |g: &Geometry, ax: Vec3| {
        g.verts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
            let d = v.dot(ax);
            (lo.min(d), hi.max(d))
        })
    };
    axes.iter().all(|ax| {
        let (a0, a1) = span(&ga, *ax);
        let (b0, b1) = span(&gb, *ax);
        a1.min(b1) - a0.max(b0) > tol
    })
}

/// TransformHandlesImplementation._safelyTransformParts on one handle
/// amount: the wanted amount if it doesn't overlap, else bisect from the
/// last good amount toward it (32 steps, stopping within `eps`), snapping
/// back to the last good one when the result is within 0.001 of it.
pub(crate) fn safe_amount(last_good: f32, wanted: f32, eps: f32, overlaps: impl Fn(f32) -> bool) -> f32 {
    if !overlaps(wanted) {
        return wanted;
    }
    let (mut a, mut b) = (last_good, wanted);
    for _ in 0..crate::mech::COLLISION_STEPS {
        if (a - b).abs() < eps {
            break;
        }
        let m = (a + b) * 0.5;
        if overlaps(m) {
            b = m;
        } else {
            a = m;
        }
    }
    if (a - last_good).abs() < 0.001 {
        last_good
    } else {
        a
    }
}

impl Editor {
    /// Collisions: whether parts at these (CFrame, Size) would overlap any
    /// part that isn't among them by more than COLLISION_OVERLAP.
    pub(crate) fn overlaps_others(&self, moved: &[(Ref, Xform, Vec3)]) -> bool {
        let skip: HashSet<Ref> = moved.iter().map(|(r, _, _)| *r).collect();
        moved.iter().any(|(r, xf, size)| {
            let kind = self.scene.get(*r).map(|sp| sp.draw.mesh).unwrap_or(MeshKind::Block);
            let a = Solid { xf: *xf, size: *size, kind };
            let ra = size.length() * 0.5;
            self.scene.parts.iter().zip(&self.scene.bounds).any(|(sp, bd)| {
                !skip.contains(&sp.r)
                    && (bd.truncate() - xf.pos).length() <= ra + bd.w
                    && solids_overlap(&a, &self.solid_of(sp), crate::mech::COLLISION_OVERLAP)
            })
        })
    }
}

/// H while dragging hides / shows the hotkey help (Studio keeps it as the
/// plugin setting HotkeyUsageHidden).
pub(crate) static HOTKEY_HELP_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Editor {
    /// DraggingParts._getHotkeyList / HotkeyUsageInfo: the key list Studio
    /// shows in the 3D view's bottom-right corner during a free drag
    /// (measured: a 202x161 panel 9 px from the corner, 27 px rows, rgb
    /// 32,34,39 behind, key chips rgb 106,111,129).
    pub(crate) fn draw_drag_hotkeys(&self, painter: &egui::Painter, rect: Rect) {
        if HOTKEY_HELP_HIDDEN.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let free: &[(&str, &str)] = &[
            ("T", "Tilt towards camera"),
            ("R", "Rotate around normal"),
            ("\u{2B07}Alt", "Use initial orientation"),
            ("\u{2B07}Shift", "Disable snapping"),
            ("Shift+X/C", "Toggle Snapping"),
            ("H", "Toggle hotkey help"),
        ];
        // DraggingHandle (measured on a Move arrow drag).
        let handle: &[(&str, &str)] = &[
            ("\u{2B07}Shift", "Disable snapping"),
            ("Shift+X/C", "Toggle Snapping"),
            ("\u{2B07}Tab", "Summon Handles"),
            ("H", "Toggle hotkey help"),
        ];
        let rows = if self.part_drag.is_some() {
            free
        } else if self.drag.as_ref().is_some_and(|d| d.handle.tool == crate::app::Tool::Move) {
            handle
        } else {
            return;
        };
        let height = 26.8 * rows.len() as f32 - 0.0;
        let panel = Rect::from_min_size(rect.right_bottom() - egui::vec2(211.0, height + 9.0), egui::vec2(202.0, height));
        painter.rect_filled(panel, 4.0, egui::Color32::from_rgb(32, 34, 39));
        let font = egui::FontId::proportional(11.5);
        for (i, (key, what)) in rows.iter().enumerate() {
            let cy = panel.top() + 13.0 + 26.8 * i as f32;
            let galley = painter.layout_no_wrap(key.to_string(), font.clone(), egui::Color32::from_rgb(235, 236, 240));
            let chip = Rect::from_min_size(Pos2::new(panel.left() + 4.0, cy - 9.0), egui::vec2(galley.size().x + 8.0, 18.0));
            painter.rect_filled(chip, 3.0, egui::Color32::from_rgb(106, 111, 129));
            painter.galley(Pos2::new(chip.left() + 4.0, cy - galley.size().y / 2.0), galley, egui::Color32::WHITE);
            painter.text(
                Pos2::new(chip.right() + 6.0, cy),
                egui::Align2::LEFT_CENTER,
                *what,
                font.clone(),
                egui::Color32::from_rgb(214, 216, 222),
            );
        }
    }
}

/// Whether a joint or weld already connects `a` and `b`.
fn joined(doc: &crate::doc::Document, a: Ref, b: Ref) -> bool {
    [a, b].iter().any(|&p| {
        doc.dom.descendants_of(p).any(|i| {
            (crate::doc::is_a(&i.class, "JointInstance") || i.class == "WeldConstraint")
                && {
                    let get = |k: &str| match i.properties.get(&ustr(k)) {
                        Some(Variant::Ref(r)) => Some(*r),
                        _ => None,
                    };
                    let (p0, p1) = (get("Part0"), get("Part1"));
                    (p0 == Some(a) && p1 == Some(b)) || (p0 == Some(b) && p1 == Some(a))
                }
        })
    })
}

/// JointPairs' isAHumanoidModel: no surface joints inside a character.
fn in_humanoid_model(doc: &crate::doc::Document, r: Ref) -> bool {
    let Some(parent) = doc.dom.get_by_ref(r).map(|i| i.parent()) else { return false };
    doc.class_of(parent).is_some_and(|c| c == "Model")
        && doc.dom.get_by_ref(parent).is_some_and(|m| {
            m.children().iter().any(|c| doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
        })
}
/// Scale modifiers from ExtrudeHandlesImplementation: Ctrl centers, Alt scales all axes.
pub(crate) fn scaled_size(
    size: Vec3,
    axis: usize,
    delta: f32,
    symmetric: bool,
    uniform: bool,
) -> (Vec3, f32) {
    let extent =
        (size[axis] + delta * if symmetric { 2.0 } else { 1.0 }).max(crate::mech::MIN_PART_SIZE);
    let mut result = size;
    if uniform {
        let factor = (extent / size[axis]).max(crate::mech::MIN_PART_SIZE / size.min_element());
        result *= factor;
    } else {
        result[axis] = extent;
    }
    (result, result[axis] - size[axis])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn studio_grid_rounds_negative_ties_up() {
        assert_eq!(round_grid(-1.5, 1.0), -1.0);
        assert_eq!(round_grid(1.5, 1.0), 2.0);
    }
    #[test]
    fn surface_snap_preserves_normal_coordinate() {
        let face = Xform { pos: Vec3::new(0.5, 3.0, 0.5), rot: Mat3::IDENTITY };
        let bbox = DragBox { basis: Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY }, offset: Vec3::ZERO, size: Vec3::ONE };
        let d = FreeDrag::begin(bbox, Vec3::new(1.2, 3.0, 2.3), Some(face), Some(1.0), false);
        assert_eq!(d.dragged, Vec3::new(1.5, 3.0, 2.5));
    }
    #[test]
    fn rotated_box_support_extent() {
        let xf = Xform {
            pos: Vec3::ZERO,
            rot: glam::Mat3::from_rotation_z(std::f32::consts::FRAC_PI_4),
        };
        assert!((half_extent(&xf, Vec3::splat(4.0), Vec3::Y) - 8.0_f32.sqrt()).abs() < 1e-5);
    }
    #[test]
    fn studio_scale_modifiers() {
        let size = Vec3::new(4.0, 2.0, 6.0);
        assert_eq!(
            scaled_size(size, 0, 1.0, false, false).0,
            Vec3::new(5.0, 2.0, 6.0)
        );
        assert_eq!(
            scaled_size(size, 0, 1.0, true, false).0,
            Vec3::new(6.0, 2.0, 6.0)
        );
        assert_eq!(
            scaled_size(size, 0, 2.0, false, true).0,
            Vec3::new(6.0, 3.0, 9.0)
        );
    }
    #[test]
    fn quarter_turn_preserves_group_spacing() {
        let r = glam::Mat3::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let a = Vec3::new(2.0, 0.0, 0.0);
        let b = Vec3::new(6.0, 0.0, 0.0);
        assert!((r * a - Vec3::new(0.0, 0.0, -2.0)).length() < 1e-5);
        assert!(((r * b - r * a).length() - 4.0).abs() < 1e-5);
        let tilt = glam::Mat3::from_rotation_x(std::f32::consts::FRAC_PI_2);
        assert!((tilt * Vec3::Y - Vec3::Z).length() < 1e-5);
    }
    #[test]
    fn all_tools_toggle_off_and_switch() {
        use crate::app::Tool;
        for tool in [
            Tool::Select,
            Tool::Move,
            Tool::Scale,
            Tool::Rotate,
            Tool::Transform,
        ] {
            assert_eq!(tool.toggle(tool), Tool::None);
            assert_eq!(Tool::None.toggle(tool), tool);
        }
        assert_eq!(Tool::Move.toggle(Tool::Scale), Tool::Scale);
    }
}

/// Editor pivot lookup mirrors Model:GetPivot: PrimaryPart (including its
/// PivotOffset), authored WorldPivotData, then the world bounds of descendants.
pub(crate) fn editor_model_pivot(doc: &crate::doc::Document, r: Ref) -> Option<Xform> {
    if doc.class_of(r)? != "Model" { return None; }
    if let Some(Variant::Ref(pp)) = doc.prop(r, "PrimaryPart") {
        if let Some(Variant::CFrame(cf)) = doc.prop(pp, "CFrame") {
            let base = Xform::from_cframe(&cf);
            let offset = match doc.prop(pp, "PivotOffset") {
                Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
                _ => Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY },
            };
            return Some(Xform { pos: base.apply(offset.pos), rot: base.rot * offset.rot });
        }
    }
    if let Some(Variant::OptionalCFrame(Some(cf))) = doc.prop(r, "WorldPivotData") {
        return Some(Xform::from_cframe(&cf));
    }
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for part in doc.dom.descendants_of(r) {
        if !crate::doc::is_a(&part.class, "BasePart") { continue; }
        let Some(Variant::CFrame(cf)) = doc.prop(part.referent(), "CFrame") else { continue };
        let xf = Xform::from_cframe(&cf);
        let size = match doc.prop(part.referent(), "Size") {
            Some(Variant::Vector3(v)) => crate::math::v3(v), _ => Vec3::ONE,
        };
        let half = Vec3::new(half_extent(&xf,size,Vec3::X),half_extent(&xf,size,Vec3::Y),half_extent(&xf,size,Vec3::Z));
        lo = lo.min(xf.pos-half); hi = hi.max(xf.pos+half);
    }
    Some(Xform { pos: if lo.is_finite() { (lo+hi)*0.5 } else { Vec3::ZERO }, rot: glam::Mat3::IDENTITY })
}

#[derive(Clone, Debug)]
pub(crate) struct PivotSnapshot { pub r: Ref, old: Option<Variant>, xf: Xform }

pub(crate) fn selected_model_pivots(doc: &crate::doc::Document) -> Vec<PivotSnapshot> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for root in &doc.selection {
        for r in std::iter::once(*root).chain(doc.dom.descendants_of(*root).map(|i| i.referent())) {
            if !seen.insert(r) || !doc.class_of(r).is_some_and(|c| c == "Model") { continue; }
            if matches!(doc.prop(r,"PrimaryPart"),Some(Variant::Ref(pp)) if doc.dom.get_by_ref(pp).is_some()) { continue; }
            if let Some(xf) = editor_model_pivot(doc,r) {
                result.push(PivotSnapshot { r, old: doc.prop(r,"WorldPivotData"), xf });
            }
        }
    }
    result
}
pub(crate) fn apply_model_pivots(doc: &mut crate::doc::Document, pivots: &[PivotSnapshot], delta: Xform) {
    for p in pivots {
        let xf = Xform { pos: delta.apply(p.xf.pos), rot: delta.rot * p.xf.rot };
        doc.set_prop_live(p.r,ustr("WorldPivotData"),Variant::OptionalCFrame(Some(xf.to_cframe())));
    }
}
/// RotateSelectionAction's matrix (Studio): 90° about world +Y.
pub(crate) const ROTATE_MATRIX: Mat3 = Mat3::from_cols(Vec3::NEG_Z, Vec3::Y, Vec3::X);

/// TiltSelectionAction's matrix (Studio): the camera's
/// heading `atan2(back.x, back.z)` in quarters, `floor((θ + 2.25π)·4/2π) & 3`
/// (quarter 0 is θ ∈ [−45°, 45°)), picks 90° about +X,
/// −Z, −X or +Z: the world axis nearest the camera's right, turned forwards.
pub(crate) fn tilt_matrix(cam_forward: Vec3) -> Mat3 {
    let theta = (-cam_forward.x).atan2(-cam_forward.z);
    let q = ((theta + 2.25 * std::f32::consts::PI) * 4.0 / std::f32::consts::TAU).floor() as i32 & 3;
    match q {
        0 => Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::NEG_Y),
        1 => Mat3::from_cols(Vec3::NEG_Y, Vec3::X, Vec3::Z),
        2 => Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y),
        _ => Mat3::from_cols(Vec3::Y, Vec3::NEG_X, Vec3::Z),
    }
}

/// The centre a quarter turn goes about (Studio): the box around every
/// part's corners in `basis`'s frame, back in world space.
pub(crate) fn quarter_turn_centre(parts: &[(Xform, Vec3)], basis: &Xform) -> Vec3 {
    let inv = basis.inverse();
    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
    for (xf, size) in parts {
        for i in 0..8 {
            let s = Vec3::new(
                if i & 1 == 0 { -0.5 } else { 0.5 },
                if i & 2 == 0 { -0.5 } else { 0.5 },
                if i & 4 == 0 { -0.5 } else { 0.5 },
            );
            let p = inv.apply(xf.apply(*size * s));
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    basis.apply((lo + hi) * 0.5)
}

/// Studio's clean-up of a turned CFrame (Studio then Studio):
/// every component to the nearest 1e-6 (|v| < 1e-5 → 0), then the rotation
/// columns re-orthonormalised (Gram-Schmidt, X first).
pub(crate) fn round_turned(xf: Xform) -> Xform {
    fn r(v: f32) -> f32 {
        if v.abs() < 1e-5 {
            0.0
        } else if v > 0.0 {
            (v * 1e6 - 0.5).ceil() / 1e6
        } else {
            (v * 1e6 + 0.5).floor() / 1e6
        }
    }
    let pos = Vec3::new(r(xf.pos.x), r(xf.pos.y), r(xf.pos.z));
    let c = |v: Vec3| Vec3::new(r(v.x), r(v.y), r(v.z));
    let x = c(xf.rot.x_axis).normalize();
    let y = c(xf.rot.y_axis);
    let y = (y - x * x.dot(y)).normalize();
    let z = c(xf.rot.z_axis);
    let z = (z - y * y.dot(z) - x * x.dot(z)).normalize();
    Xform { pos, rot: Mat3::from_cols(x, y, z) }
}

/// Turns `parts` (in selection order) by `m` about [`quarter_turn_centre`]
/// and moves the selected Models' WorldPivots with them, as one undo step.
/// Studio takes the box's frame from the last part of its list, which is
/// sorted by heap address (a std::set in Studio), so effectively any
/// part; rustudio uses the last selected part. Locked parts turn too, the
/// grid is ignored, nothing is pushed out of overlaps.
pub(crate) fn quarter_turn_selection(doc: &mut crate::doc::Document, parts: &[Ref], m: Mat3, tilt: bool) {
    let boxes: Vec<(Ref, Xform, Vec3)> = parts
        .iter()
        .filter_map(|&r| {
            let Some(Variant::CFrame(cf)) = doc.prop(r, "CFrame") else { return None };
            let size = match doc.prop(r, "Size") {
                Some(Variant::Vector3(v)) => crate::math::v3(v),
                _ => Vec3::ONE,
            };
            Some((r, Xform::from_cframe(&cf), size))
        })
        .collect();
    let Some(&(_, basis, _)) = boxes.last() else { return };
    let shapes: Vec<(Xform, Vec3)> = boxes.iter().map(|&(_, xf, size)| (xf, size)).collect();
    let centre = quarter_turn_centre(&shapes, &basis);
    let delta = Xform { pos: centre - m * centre, rot: m };
    let pivots = selected_model_pivots(doc);
    let mut changes = Vec::new();
    for (r, xf, _) in boxes {
        let old = doc.prop(r, "CFrame");
        let new = Variant::CFrame(round_turned(Xform { pos: delta.apply(xf.pos), rot: m * xf.rot }).to_cframe());
        doc.set_prop_live(r, ustr("CFrame"), new.clone());
        changes.push((r, ustr("CFrame"), old, new));
    }
    apply_model_pivots(doc, &pivots, delta);
    changes.extend(pivot_changes(doc, &pivots));
    doc.record_props(if tilt { "SelectionTilt" } else { "SelectionRotate" }, changes);
}

pub(crate) fn pivot_changes(doc: &crate::doc::Document, pivots: &[PivotSnapshot]) -> Vec<(Ref,rbx_dom_weak::Ustr,Option<Variant>,Variant)> {
    pivots.iter().filter_map(|p| Some((p.r,ustr("WorldPivotData"),p.old.clone(),doc.prop(p.r,"WorldPivotData")?))).collect()
}

#[cfg(test)]
mod quarter_turn_tests {
    use super::*;
    use super::quarter_turn_cases::*;
    use rbx_dom_weak::InstanceBuilder;

    fn xf(c: &[f32; 12]) -> Xform {
        Xform {
            pos: Vec3::new(c[0], c[1], c[2]),
            rot: Mat3::from_cols(Vec3::new(c[3], c[6], c[9]), Vec3::new(c[4], c[7], c[10]), Vec3::new(c[5], c[8], c[11])),
        }
    }
    fn close(name: &str, what: &str, got: Xform, want: &[f32; 12]) {
        let w = xf(want);
        assert!(
            got.pos.abs_diff_eq(w.pos, 2e-4) && got.rot.abs_diff_eq(w.rot, 2e-4),
            "{name} {what}: got {:?} {:?}, Studio {want:?}",
            got.pos,
            got.rot
        );
    }

    /// Every measured case, run through the same code as the editor action:
    /// the parts in Studio's list order (its frame part last), the matrix
    /// from the camera, then the CFrames, pivots and the undo step.
    #[test]
    fn studio_quarter_turns() {
        for &(name, sel, model, cam_pos, tilt, want) in CASES {
            let mut doc = crate::doc::Document::empty();
            let root = doc.dom.root_ref();
            let parent = if model {
                doc.dom.insert(root, InstanceBuilder::new("Model").with_name("M").with_property("WorldPivotData", Variant::OptionalCFrame(Some(pivot().to_cframe()))))
            } else {
                root
            };
            let mut refs = Vec::new();
            for &p in sel {
                let (c, size) = part(p);
                let parent = if model && p != "C" { parent } else { root };
                let mut b = InstanceBuilder::new("Part").with_name(p).with_property("CFrame", c.to_cframe()).with_property("Size", rbx_dom_weak::types::Vector3::new(size[0], size[1], size[2]));
                if name == "locked_ab" && p == "B" {
                    b = b.with_property("Locked", true);
                }
                refs.push((p, doc.dom.insert(parent, b)));
            }
            doc.selection = if model { vec![parent] } else { refs.iter().map(|r| r.1).collect() };
            if model && sel.contains(&"C") {
                doc.selection.push(refs.iter().find(|r| r.0 == "C").unwrap().1);
            }
            let fwd = (Vec3::new(0.0, 2.0, 0.0) - Vec3::from(cam_pos)).normalize();
            let m = if tilt { tilt_matrix(fwd) } else { ROTATE_MATRIX };
            let parts: Vec<Ref> = refs.iter().map(|r| r.1).collect();
            quarter_turn_selection(&mut doc, &parts, m, tilt);
            for (n, c) in want.iter() {
                let got = if *n == "M" {
                    super::editor_model_pivot(&doc, parent).unwrap()
                } else {
                    let r = refs.iter().find(|r| r.0 == *n).unwrap().1;
                    let Some(Variant::CFrame(cf)) = doc.prop(r, "CFrame") else { panic!() };
                    Xform::from_cframe(&cf)
                };
                close(name, n, got, c);
            }
            doc.undo();
            for (p, r) in &refs {
                let Some(Variant::CFrame(cf)) = doc.prop(*r, "CFrame") else { panic!() };
                let got = Xform::from_cframe(&cf);
                let want = part(p).0;
                assert!(got.pos.abs_diff_eq(want.pos, 1e-5) && got.rot.abs_diff_eq(want.rot, 1e-5), "{name} {p} after undo");
            }
        }
    }

    #[test]
    fn tilt_quarters_follow_the_camera_heading() {
        // Back vector straight along an axis, and both sides of the 45° seams.
        let at = |deg: f32| {
            let t = deg.to_radians();
            tilt_matrix(-Vec3::new(t.sin(), -0.6, t.cos()).normalize())
        };
        let x = Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::NEG_Y);
        assert_eq!(at(0.0), x);
        assert_eq!(at(44.0), x);
        assert_eq!(at(-45.0), x);
        assert_eq!(at(46.0), Mat3::from_cols(Vec3::NEG_Y, Vec3::X, Vec3::Z));
        assert_eq!(at(180.0), Mat3::from_cols(Vec3::X, Vec3::NEG_Z, Vec3::Y));
        assert_eq!(at(-90.0), Mat3::from_cols(Vec3::Y, Vec3::NEG_X, Vec3::Z));
        // The matrices are exact quarter turns about the camera-right axis.
        for deg in [0.0f32, 90.0, 180.0, 270.0] {
            let t = deg.to_radians();
            let back = Vec3::new(t.sin(), 0.0, t.cos());
            let right = Vec3::Y.cross(back);
            let want = Mat3::from_axis_angle(right, std::f32::consts::FRAC_PI_2);
            assert!(at(deg).abs_diff_eq(want, 1e-6), "{deg}");
        }
        assert!(ROTATE_MATRIX.abs_diff_eq(Mat3::from_rotation_y(std::f32::consts::FRAC_PI_2), 1e-7));
    }

    #[test]
    fn turned_cframes_are_rounded_like_studio() {
        let t = round_turned(Xform { pos: Vec3::new(1.000_000_4, -2.000_000_6, 0.000_004), rot: Mat3::from_rotation_y(0.3) });
        assert_eq!(t.pos, Vec3::new(1.0, -2.000001, 0.0));
        assert!((t.rot.x_axis.length() - 1.0).abs() < 1e-6);
    }
}

/// Ctrl+R / Ctrl+T measured in an offline Studio (MECHANICS.md "Ctrl+R /
/// Ctrl+T"): three anchored parts, the parts in Studio's internal list order
/// (the last one gives the box frame, see `quarter_turn_selection`), an
/// optional Model (WorldPivot `pivot()`, no PrimaryPart) holding A and B, the camera at `cam_pos`
/// looking at (0,2,0), Rotate or Tilt, and Studio's CFrame:GetComponents()
/// afterwards ("M" = the Model's GetPivot()).
#[cfg(test)]
pub(crate) mod quarter_turn_cases {
    use crate::math::Xform;
    use glam::{Mat3, Vec3};

    pub type Cf = [f32; 12];
    /// `CFrame.new(p) * CFrame.Angles(x, y, z)`.
    pub fn angles(p: [f32; 3], x: f32, y: f32, z: f32) -> Xform {
        Xform { pos: Vec3::from(p), rot: Mat3::from_rotation_x(x) * Mat3::from_rotation_y(y) * Mat3::from_rotation_z(z) }
    }
    /// The parts' starting CFrames and sizes.
    pub fn part(name: &str) -> (Xform, [f32; 3]) {
        match name {
            "A" => (angles([1.3, 2.7, -0.4], 0.3, 0.5, 0.1), [2.0, 4.0, 6.0]),
            "B" => (angles([6.0, 1.0, 3.0], 0.0, 0.4, 0.0), [1.0, 2.0, 3.0]),
            _ => (angles([-3.0, 5.0, 2.0], 0.2, 0.0, 0.7), [3.0, 1.0, 1.0]),
        }
    }
    /// The Model's WorldPivot.
    pub fn pivot() -> Xform {
        angles([5.0, 7.0, 1.0], 0.0, 0.5, 0.2)
    }
    const FRONT: [f32; 3] = [0.0, 20.0, 20.0];
    #[allow(clippy::type_complexity)]
    pub const CASES: &[(&str, &[&str], bool, [f32; 3], bool, &[(&str, Cf)])] = &[
        ("single_r", &["A"], false, FRONT, false, &[("A", [1.30000, 2.70000, -0.40000, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943])]),
        ("multi_r_frame_c", &["A", "B", "C"], false, FRONT, false, &[
            ("A", [0.65142, 2.70000, 1.18967, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943]),
            ("B", [4.05142, 1.00000, -3.51033, -0.38942, 0.00000, 0.92106, 0.00000, 1.00000, 0.00000, -0.92106, 0.00000, -0.38942]),
            ("C", [3.05142, 5.00000, 5.48967, 0.12799, 0.15195, 0.98007, 0.63138, 0.74960, -0.19867, -0.76484, 0.64422, 0.00000])]),
        ("multi_r_frame_a", &["C", "B", "A"], false, FRONT, false, &[
            ("A", [-1.78487, 2.70000, 2.10988, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943]),
            ("B", [1.61513, 1.00000, -2.59012, -0.38942, 0.00000, 0.92106, 0.00000, 1.00000, 0.00000, -0.92106, 0.00000, -0.38942]),
            ("C", [0.61513, 5.00000, 6.40988, 0.12799, 0.15195, 0.98007, 0.63138, 0.74960, -0.19867, -0.76484, 0.64422, 0.00000])]),
        ("model_pivot_r", &["A", "B"], true, FRONT, false, &[
            ("A", [1.66464, 2.70000, 2.01807, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943]),
            ("B", [5.06465, 1.00000, -2.68193, -0.38942, 0.00000, 0.92106, 0.00000, 1.00000, 0.00000, -0.92106, 0.00000, -0.38942]),
            ("M", [3.06465, 7.00000, -1.68193, -0.46987, 0.09525, 0.87758, 0.19867, 0.98007, 0.00000, -0.86009, 0.17435, -0.47943])]),
        ("model_and_part_r", &["A", "B", "C"], true, FRONT, false, &[
            ("A", [0.65142, 2.70000, 1.18967, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943]),
            ("C", [3.05142, 5.00000, 5.48967, 0.12799, 0.15195, 0.98007, 0.63138, 0.74960, -0.19867, -0.76484, 0.64422, 0.00000]),
            ("M", [2.05142, 7.00000, -2.51033, -0.46987, 0.09525, 0.87758, 0.19867, 0.98007, 0.00000, -0.86009, 0.17435, -0.47943])]),
        ("locked_ab", &["B", "A"], false, FRONT, false, &[
            ("A", [1.92850, 2.70000, 2.85312, -0.42622, 0.33977, 0.83839, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943]),
            ("B", [5.32850, 1.00000, -1.84688, -0.38942, 0.00000, 0.92106, 0.00000, 1.00000, 0.00000, -0.92106, 0.00000, -0.38942])]),
        ("tilt_front", &["A"], false, FRONT, true, &[("A", [1.30000, 2.70000, -0.40000, 0.87320, -0.08761, 0.47943, 0.42622, -0.33977, -0.83839, 0.23635, 0.93642, -0.25934])]),
        ("tilt_from_plus_x", &["A"], false, [20.0, 10.0, 5.0], true, &[("A", [1.30000, 2.70000, -0.40000, 0.23635, 0.93642, -0.25934, -0.87320, 0.08761, -0.47943, -0.42622, 0.33977, 0.83839])]),
        ("tilt_from_back", &["A"], false, [-6.0, 10.0, -20.0], true, &[("A", [1.30000, 2.70000, -0.40000, 0.87320, -0.08761, 0.47943, -0.42622, 0.33977, 0.83839, -0.23635, -0.93642, 0.25934])]),
        ("tilt_multi_from_minus_x", &["A", "B", "C"], false, [-20.0, 5.0, 3.0], true, &[
            ("A", [2.16897, 2.62788, -0.40000, -0.23635, -0.93642, 0.25934, 0.87320, -0.08761, 0.47943, -0.42622, 0.33977, 0.83839]),
            ("B", [3.86897, 7.32788, 3.00000, 0.00000, -1.00000, 0.00000, 0.92106, 0.00000, 0.38942, -0.38942, 0.00000, 0.92106]),
            ("C", [-0.13103, -1.67212, 2.00000, -0.63138, -0.74960, 0.19867, 0.76484, -0.64422, 0.00000, 0.12799, 0.15195, 0.98007])]),
    ];
}

#[cfg(test)]
mod model_pivot_tests {
    use super::*;
    use rbx_dom_weak::InstanceBuilder;
    #[test]
    fn studio_world_pivot_translation_and_undo() {
        let mut doc = crate::doc::Document::empty();
        let model = doc.dom.insert(doc.dom.root_ref(), InstanceBuilder::new("Model")
            .with_property("WorldPivotData", Variant::OptionalCFrame(Some(Xform { pos: Vec3::new(23.,5.,1.), rot: glam::Mat3::IDENTITY }.to_cframe()))));
        let part = doc.dom.insert(model, InstanceBuilder::new("Part")
            .with_property("CFrame", Xform { pos: Vec3::new(20.,2.,0.), rot: glam::Mat3::IDENTITY }.to_cframe()));
        doc.selection = vec![model];
        let pivots = selected_model_pivots(&doc);
        apply_model_pivots(&mut doc,&pivots,Xform { pos: Vec3::new(2.,0.,0.),rot: glam::Mat3::IDENTITY });
        assert_eq!(editor_model_pivot(&doc,model).unwrap().pos,Vec3::new(25.,5.,1.));
        doc.record_props("Drag",pivot_changes(&doc,&pivots));
        doc.undo();
        assert_eq!(editor_model_pivot(&doc,model).unwrap().pos,Vec3::new(23.,5.,1.));
        doc.set_prop_live(model,ustr("PrimaryPart"),Variant::Ref(part));
        doc.set_prop_live(part,ustr("CFrame"),Variant::CFrame(Xform {pos:Vec3::new(22.,2.,0.),rot:glam::Mat3::IDENTITY}.to_cframe()));
        doc.set_prop_live(part,ustr("PivotOffset"),Variant::CFrame(Xform {pos:Vec3::new(1.,2.,3.),rot:glam::Mat3::IDENTITY}.to_cframe()));
        assert_eq!(editor_model_pivot(&doc,model).unwrap().pos,Vec3::new(23.,4.,3.));
        assert!(selected_model_pivots(&doc).is_empty());
    }
}

/// Snapshot the copies at the duplication point without changing drag geometry.
pub(crate) fn pivot_undo_baseline(doc: &crate::doc::Document, pivots: &[PivotSnapshot]) -> Vec<PivotSnapshot> {
    pivots.iter().cloned().map(|mut p| { p.old = doc.prop(p.r, "WorldPivotData"); p }).collect()
}


/// The drags measured in an offline Studio (MECHANICS.md "Freeform drag
/// parity"), shared by the unit tests and the editor self-test
/// (selftest/drag_parity.rs): the scenes and Studio's resulting CFrames.
pub(crate) mod studio_cases {
    use super::*;

    pub fn ang(x: f32, y: f32, z: f32) -> Mat3 {
        Mat3::from_rotation_x(x.to_radians()) * Mat3::from_rotation_y(y.to_radians()) * Mat3::from_rotation_z(z.to_radians())
    }
    pub fn solid(pos: [f32; 3], size: [f32; 3], rot: Mat3, kind: MeshKind) -> Solid {
        Solid { xf: Xform { pos: Vec3::from(pos), rot }, size: Vec3::from(size), kind }
    }
    pub fn block(pos: [f32; 3], size: [f32; 3]) -> Solid {
        solid(pos, size, Mat3::IDENTITY, MeshKind::Block)
    }

    /// The scene of a case: named solids, the dragged names, the box basis
    /// (pivot) and how many objects were selected.
    pub fn scene(name: &str) -> (Vec<(&'static str, Solid)>, Vec<&'static str>, Xform, usize) {
        let mut s = vec![("Baseplate", block([0.0, -2.0, 0.0], [512.0, 4.0, 512.0]))];
        let id = Mat3::IDENTITY;
        if name.starts_with("model") {
            let mut b = block([3.0, 1.0, 0.0], [2.0, 2.0, 2.0]);
            let pivot = if name.starts_with("model_tiltpp") {
                b.xf.rot = ang(25.0, 0.0, 0.0);
                b.xf
            } else if name.starts_with("model_tilt") {
                Xform { pos: Vec3::new(5.0, 7.0, 1.0), rot: ang(30.0, 0.0, 15.0) }
            } else if name == "model_primary" {
                b.xf.rot = ang(0.0, 30.0, 0.0);
                b.xf
            } else if name.starts_with("model_pivot") {
                Xform { pos: Vec3::new(5.0, 7.0, 1.0), rot: ang(0.0, 30.0, 0.0) }
            } else {
                Xform { pos: Vec3::new(1.0, 2.0, 0.0), rot: id }
            };
            s.push(("A", block([0.0, 2.0, 0.0], [4.0; 3])));
            s.push(("B", b));
            return (s, vec!["A", "B"], pivot, 1);
        }
        if name.starts_with("group") {
            let p2 = if name.starts_with("group_tilt") {
                solid([-4.0, 1.5, 3.0], [2.0; 3], ang(30.0, 0.0, 20.0), MeshKind::Block)
            } else {
                solid([-4.0, 1.0, 3.0], [2.0; 3], ang(0.0, 30.0, 0.0), MeshKind::Block)
            };
            s.push(("Probe", block([0.0, 2.0, 0.0], [4.0; 3])));
            s.push(("Probe2", p2));
            let grabbed = if name.ends_with("grab_p2") { p2.xf } else { s[1].1.xf };
            return (s, vec!["Probe", "Probe2"], grabbed, 2);
        }
        let rot = if name.starts_with("rotated") { ang(0.0, 30.0, 0.0) } else { id };
        let probe = solid([0.0, 2.0, 0.0], [2.0, 4.0, 6.0], rot, MeshKind::Block);
        s.push(("Probe", probe));
        if name.starts_with("wedge") {
            s.push(("Wedge", solid([8.0, 2.0, -4.0], [6.0, 4.0, 6.9282], ang(0.0, 180.0, 0.0), MeshKind::Wedge)));
        }
        if name.starts_with("part_top") {
            s.push(("Target", block([9.0, 2.0, -8.0], [12.0, 4.0, 12.0])));
        }
        if name.starts_with("part_side") {
            s.push(("Wall", block([8.0, 4.0, -8.0], [8.0, 8.0, 4.0])));
        }
        (s, vec!["Probe"], probe.xf, 1)
    }

    pub const CASES: &[(&str, (f32, f32), (f32, f32), Option<f32>, &str, &[(&str, [f32; 12])])] = &[
        ("flat_snap1", (441.0, 319.0), (538.0, 298.0), Some(1.0), "", &[("Probe", [6.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("flat_snap05", (441.0, 319.0), (538.0, 298.0), Some(0.5), "", &[("Probe", [7.00000, 2.00000, -7.50000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("flat_snapoff", (441.0, 319.0), (538.0, 298.0), None, "", &[("Probe", [6.78342, 2.00000, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("flat_rot_r", (441.0, 319.0), (538.0, 298.0), Some(1.0), "r", &[("Probe", [6.00000, 2.00000, -5.00000, 0.00000, 0.00000, 1.00000, 0.00000, 1.00000, 0.00000, -1.00000, 0.00000, 0.00000])]),
        ("flat_tilt_t", (441.0, 319.0), (538.0, 298.0), Some(1.0), "t", &[("Probe", [6.00000, 3.00000, -8.00000, 1.00000, 0.00000, 0.00000, 0.00000, 0.00000, -1.00000, 0.00000, 1.00000, 0.00000])]),
        ("flat_r_t", (441.0, 319.0), (538.0, 298.0), None, "rt", &[("Probe", [6.51291, 1.00000, -8.25159, 0.00000, 0.00000, 1.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000])]),
        ("wedge30", (441.0, 319.0), (552.0, 300.0), Some(1.0), "", &[("Probe", [6.00000, 4.73205, -2.26795, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("wedge30_off", (441.0, 319.0), (552.0, 300.0), None, "", &[("Probe", [6.95338, 4.45120, -1.78150, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("part_top", (441.0, 319.0), (583.0, 221.0), Some(1.0), "", &[("Probe", [9.00000, 6.00000, -11.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("part_top_off", (441.0, 319.0), (583.0, 221.0), None, "", &[("Probe", [9.79344, 6.00000, -10.38101, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("part_side", (441.0, 319.0), (584.0, 232.0), Some(1.0), "", &[("Probe", [8.00000, 3.00000, -3.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("part_side_off", (441.0, 319.0), (584.0, 232.0), None, "", &[("Probe", [8.75779, 3.20530, -3.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("group", (441.0, 319.0), (538.0, 298.0), Some(1.0), "", &[("Probe", [6.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("Probe2", [2.00000, 1.00000, -4.00000, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603])]),
        ("group_off", (441.0, 319.0), (538.0, 298.0), None, "", &[("Probe", [6.78342, 2.00000, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("Probe2", [2.78342, 1.00000, -4.04042, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603])]),
        ("model_nopp", (441.0, 319.0), (538.0, 298.0), Some(1.0), "", &[("M", [7.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("A", [6.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.00000, 1.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("model_pivot", (441.0, 319.0), (538.0, 298.0), Some(1.0), "", &[("M", [11.00000, 7.00000, -6.00000, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603]), ("A", [6.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.00000, 1.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("model_primary", (441.0, 319.0), (538.0, 298.0), Some(1.0), "", &[("M", [9.00000, 1.00000, -7.00000, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603]), ("A", [6.00000, 2.00000, -7.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.00000, 1.00000, -7.00000, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603])]),
        ("model_pivot_off", (441.0, 319.0), (538.0, 298.0), None, "", &[("M", [11.78342, 7.00000, -6.04042, 0.86603, 0.00000, 0.50000, 0.00000, 1.00000, 0.00000, -0.50000, 0.00000, 0.86603]), ("A", [6.78342, 2.00000, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.78342, 1.00000, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("group_tilt_off", (441.0, 319.0), (538.0, 298.0), None, "", &[("Probe", [6.78342, 2.11000, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("Probe2", [2.78342, 1.61000, -4.04042, 0.93969, -0.34202, 0.00000, 0.29620, 0.81380, -0.50000, 0.17101, 0.46985, 0.86603])]),
        ("model_tilt_off", (441.0, 319.0), (538.0, 298.0), None, "", &[("M", [11.78342, 9.62708, -6.04042, 0.96593, -0.25882, 0.00000, 0.22414, 0.83652, -0.50000, 0.12941, 0.48296, 0.86603]), ("A", [6.78342, 4.62708, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.78342, 3.62708, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000])]),
        ("model_tiltpp_off", (441.0, 319.0), (538.0, 298.0), None, "", &[("M", [9.78342, 2.53209, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 0.90631, -0.42262, 0.00000, 0.42262, 0.90631]), ("A", [6.78342, 3.53209, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("B", [9.78342, 2.53209, -7.04042, 1.00000, 0.00000, 0.00000, 0.00000, 0.90631, -0.42262, 0.00000, 0.42262, 0.90631])]),
        ("rotated_r", (437.0, 313.0), (538.0, 298.0), None, "r", &[("Probe", [7.00563, 2.00000, -5.93604, -0.50000, 0.00000, 0.86603, 0.00000, 1.00000, 0.00000, -0.86603, 0.00000, -0.50000])]),
        ("rotated_t", (437.0, 313.0), (538.0, 298.0), None, "t", &[("Probe", [6.19330, 3.00000, -7.92103, 0.86603, 0.50000, 0.00000, 0.00000, 0.00000, -1.00000, -0.50000, 0.86603, 0.00000])]),
        ("wedge_r", (441.0, 319.0), (552.0, 300.0), None, "r", &[("Probe", [6.68287, 3.01916, -1.30114, 0.00000, 0.00000, 1.00000, 0.00000, 1.00000, 0.00000, -1.00000, 0.00000, 0.00000])]),
        ("group_tilt_r", (441.0, 319.0), (538.0, 298.0), None, "rt", &[("Probe", [6.51291, 5.28171, -8.25159, 0.00000, 0.00000, 1.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000]), ("Probe2", [9.51291, 1.28171, -8.75159, 0.17101, 0.46985, 0.86603, 0.93969, -0.34202, 0.00000, 0.29620, 0.81380, -0.50000])]),
        ("model_tilt_r", (441.0, 319.0), (538.0, 298.0), None, "t", &[("M", [11.81823, 5.49907, -3.70759, 0.96593, 0.00000, 0.25882, 0.22414, -0.50000, -0.83652, 0.12941, 0.86603, -0.48296]), ("A", [6.36854, 4.45521, -8.20327, 0.93301, 0.08710, 0.34914, 0.34592, 0.05024, -0.93692, -0.09914, 0.99493, 0.01675]), ("B", [9.08048, 5.44272, -9.49564, 0.93301, 0.08710, 0.34914, 0.34592, 0.05024, -0.93692, -0.09914, 0.99493, 0.01675])]),
        ("group_tilt_grab_p2", (354.0, 383.0), (538.0, 298.0), None, "", &[("Probe", [11.11370, 5.87141, -10.02208, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000, 0.00000, 0.00000, 0.00000, 1.00000]), ("Probe2", [7.11370, 5.37141, -7.02208, 0.93969, -0.34202, 0.00000, 0.29620, 0.81380, -0.50000, 0.17101, 0.46985, 0.86603])]),
    ];
}

/// The drags measured in an offline Studio (MECHANICS.md "Freeform drag
/// parity"): 862x675 viewport, Camera.CFrame = lookAt((0,20,20), (0,2,0)),
/// FieldOfView 70, a 512x4x512 baseplate at (0,-2,0); press / release pixels
/// and DraggerService.LinearSnapIncrement as used there; R / T pressed
/// mid-drag. Expected values are Studio's CFrame:GetComponents() after the
/// drag (models: their parts; rows are x y z then the rotation row-major).
#[cfg(test)]
mod studio_parity_tests {
    use super::*;
    use super::studio_cases::*;
    use crate::camera::Camera;
    use egui::Vec2;

    const W: f32 = 862.0;
    const H: f32 = 675.0;

    fn cam() -> Camera {
        let mut c = Camera::default();
        c.pos = Vec3::new(0.0, 20.0, 20.0);
        let f = (Vec3::new(0.0, 2.0, 0.0) - c.pos).normalize();
        let r = f.cross(Vec3::Y).normalize();
        let u = r.cross(f);
        c.set_rotation(Mat3::from_cols(r, u, -f));
        c.fov_deg = 70.0;
        c
    }
    fn ray(px: f32, py: f32) -> Ray {
        cam().ray(Rect::from_min_size(Pos2::ZERO, Vec2::new(W, H)), Pos2::new(px, py))
    }
    fn nearest(s: &[(&str, Solid)], ray: &Ray, skip: &[&str]) -> Option<(Vec3, Vec3, Solid)> {
        s.iter()
            .filter(|(n, _)| !skip.contains(n))
            .filter_map(|(_, so)| so.raycast(ray).map(|(t, n)| (t, n, *so)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(t, n, so)| (ray.at(t), n, so))
    }

    /// The editor's drag (start_part_drag / rotate / update_part_drag) on a
    /// plain list of solids.
    fn simulate(name: &str, press: (f32, f32), release: (f32, f32), grid: Option<f32>, keys: &str) -> Vec<(&'static str, Xform)> {
        let (s, sel, basis, objects) = scene(name);
        let pr = ray(press.0, press.1);
        let (click, n, clicked) = nearest(&s, &pr, &[]).expect("press hits a part");
        let carried: Vec<(&'static str, Solid)> = s.iter().filter(|(n, _)| sel.contains(n)).copied().collect();
        let boxes: Vec<(Xform, Vec3)> = carried.iter().map(|(_, so)| (so.xf, so.size)).collect();
        let bbox = DragBox::new(basis, &boxes, objects);
        let face = surface_matrix(&clicked.geometry(), click, n);
        let mut d = FreeDrag::begin(bbox, click, Some(face), grid, false);
        let rr = ray(release.0, release.1);
        for k in keys.chars() {
            let axis = if k == 't' {
                cam().right()
            } else {
                d.surface(&rr, nearest(&s, &rr, &sel)).unwrap().m.rot.col(1)
            };
            let m = d.surface(&rr, nearest(&s, &rr, &sel)).map(|s| s.m);
            d.turn(axis, m, false);
        }
        let surface = d.surface(&rr, nearest(&s, &rr, &sel)).expect("release over a surface");
        // (Measured with Align Dragged Objects and Snap to Parts off.)
        let t = d.target(surface, DragOpts { grid, align: false, soft: None });
        let mut out: Vec<(&'static str, Xform)> = carried.iter().map(|(n, so)| (*n, t.mul(&so.xf))).collect();
        if name.starts_with("model") {
            // The Model's pivot moves with it (WorldPivot / PrimaryPart).
            out.push(("M", t.mul(&basis)));
        }
        out
    }


    #[test]
    fn studio_freeform_drags() {
        let mut bad = Vec::new();
        for (name, press, release, grid, keys, expect) in CASES {
            let got = simulate(name, *press, *release, *grid, keys);
            for (part, want) in expect.iter() {
                let Some((_, xf)) = got.iter().find(|(n, _)| n == part) else {
                    bad.push(format!("{name}/{part}: not dragged"));
                    continue;
                };
                let pos_err = (xf.pos - Vec3::new(want[0], want[1], want[2])).abs().max_element();
                let mut rot_err = 0.0f32;
                for i in 0..3 {
                    for j in 0..3 {
                        rot_err = rot_err.max((xf.rot.col(j)[i] - want[3 + i * 3 + j]).abs());
                    }
                }
                // Studio prints 5 decimals: position within 0.001, rotation
                // to the printed precision.
                if pos_err > 0.001 || rot_err > 2e-5 {
                    bad.push(format!("{name}/{part}: pos {:?} rot err {rot_err} pos err {pos_err}", xf.pos));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// Join Surfaces after the Studio drags (studio_joins): one Weld per
    /// touching pair, parented to the moved part, C0 / C1 as Studio made them.
    #[test]
    fn studio_join_surfaces() {
        let base = block([0.0, -2.0, 0.0], [512.0, 4.0, 512.0]);
        let probe = |p: [f32; 3]| block(p, [2.0, 4.0, 6.0]);
        let check = |moved: Solid, other: Solid, c0: [f32; 12], c1: [f32; 12]| {
            let (a, b) = joint_between(&moved, &other).expect("touching faces weld");
            for (xf, w) in [(a, c0), (b, c1)] {
                assert!((xf.pos - Vec3::new(w[0], w[1], w[2])).abs().max_element() < 1e-3, "{xf:?} vs {w:?}");
                for i in 0..3 {
                    for j in 0..3 {
                        assert!((xf.rot.col(j)[i] - w[3 + i * 3 + j]).abs() < 1e-4, "{xf:?} vs {w:?}");
                    }
                }
            }
        };
        let down = [1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0];
        let cat = |p: [f32; 3], r: [f32; 9]| {
            let mut o = [0.0; 12];
            o[..3].copy_from_slice(&p);
            o[3..].copy_from_slice(&r);
            o
        };
        // Probe dropped on the baseplate (snap 1) and snapped off.
        check(probe([6.0, 2.0, -7.0]), base, cat([0.0, -2.0, 0.0], down), cat([6.0, 2.0, -7.0], down));
        check(probe([6.78342, 2.0, -7.04042]), base, cat([0.0, -2.0, 0.0], down), cat([6.7834, 2.0, -7.0404], down));
        // On top of Target (12x4x12 at (9,2,-8)).
        check(probe([9.0, 6.0, -11.0]), block([9.0, 2.0, -8.0], [12.0, 4.0, 12.0]), cat([0.0, -2.0, 0.0], down), cat([0.0, 2.0, -3.0], down));
        // Against Wall's +Z face (8x8x4 at (8,4,-8)).
        let front = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0];
        check(probe([8.0, 3.0, -3.0]), block([8.0, 4.0, -8.0], [8.0, 8.0, 4.0]), cat([0.0, 0.0, -3.0], front), cat([0.0, -1.0, 2.0], front));
        // The group's rotated cube.
        let p2 = solid([2.0, 1.0, -4.0], [2.0; 3], ang(0.0, 30.0, 0.0), MeshKind::Block);
        check(p2, base, cat([0.0, -1.0, 0.0], down), cat([2.0, 2.0, -4.0], [0.866, 0.5, 0.0, 0.0, 0.0, -1.0, -0.5, 0.866, 0.0]));
        // Not touching: lifted 0.1 off the baseplate, or a wall face beside it.
        assert!(joint_between(&probe([6.0, 2.1, -7.0]), &base).is_none());
        assert!(joint_between(&probe([6.0, 2.0, -7.0]), &block([20.0, 2.0, -7.0], [4.0; 3])).is_none());
    }

    /// Collisions on a Move handle (Studio: a 4-stud cube at the origin
    /// dragged along +X into a 2x8x8 wall at (8,4,0) stopped at x = 5.00074,
    /// overlapping by less than 0.001; Collisions off it went to 7.98286).
    #[test]
    fn studio_move_handle_collision() {
        let wall = block([8.0, 4.0, 0.0], [2.0, 8.0, 8.0]);
        let at = |x: f32| block([x, 2.0, 0.0], [4.0; 3]);
        let hits = |x: f32| solids_overlap(&at(x), &wall, crate::mech::COLLISION_OVERLAP);
        // The mouse passes through intermediate points as Studio's did.
        let mut good = 0.0;
        for i in 1..=16 {
            good = safe_amount(good, 7.98286 * i as f32 / 16.0, crate::mech::COLLISION_EPS, hits);
        }
        assert!((good - 5.00074).abs() < 0.001, "{good}");
        assert!(good <= 5.001);
        // Flush faces only touch; a jump past the wall is allowed.
        assert!(!hits(5.0) && hits(5.01));
        assert_eq!(safe_amount(5.0, 12.0022, crate::mech::COLLISION_EPS, hits), 12.0022);
        // The baseplate under the cube is touching, not overlapping.
        assert!(!solids_overlap(&at(0.0), &block([0.0, -2.0, 0.0], [512.0, 4.0, 512.0]), crate::mech::COLLISION_OVERLAP));
    }

    /// The camera ray is Studio's ViewportPointToRay: the centre pixel looks
    /// straight ahead (measured: (431, 337.5) gave the LookVector).
    #[test]
    fn viewport_ray_matches_studio() {
        let r = ray(431.0, 337.5);
        let f = (Vec3::new(0.0, 2.0, 0.0) - Vec3::new(0.0, 20.0, 20.0)).normalize();
        // (f32 unprojection through the inverse view-projection)
        assert!((r.dir - f).length() < 1e-4, "{:?}", r.dir - f);
    }
}

/// Dragger settings measured in a Studio (checks/dragger-modes/
/// findings.md): 1506x842 viewport, Camera.CFrame = lookAt((0,20,20),
/// (0,0,−5)) (Studio re-aimed the set camera at its Focus), FieldOfView 70, the 512x4x512 baseplate at (0,−2,0), the
/// 2x4x6 Probe at (0,2,0); drags with a scripted mouse `drag` (press, release pixels
/// as Camera:ViewportPointToRay takes them). Expected: Probe's
/// CFrame:GetComponents().
pub(crate) mod mode_cases {
    use super::*;
    use super::studio_cases::{ang, block, solid};

    pub const W: f32 = 1506.0;
    pub const H: f32 = 842.0;

    /// The scene: named solids (Probe first after the baseplate).
    pub fn scene(name: &str) -> Vec<(&'static str, Solid)> {
        let mut s = vec![("Baseplate", block([0.0, -2.0, 0.0], [512.0, 4.0, 512.0]))];
        let yaw = if name.starts_with("rot30") { ang(0.0, 30.0, 0.0) } else { Mat3::IDENTITY };
        s.push(("Probe", solid([0.0, 2.0, 0.0], [2.0, 4.0, 6.0], yaw, MeshKind::Block)));
        if name.starts_with("wedge") {
            s.push(("Wedge", solid([8.0, 2.0, -4.0], [6.0, 4.0, 6.9282], ang(0.0, 180.0, 0.0), MeshKind::Wedge)));
        }
        if name.starts_with("target") {
            s.push(("Target", block([9.0, 2.0, -8.0], [12.0, 4.0, 12.0])));
        }
        if name.starts_with("wall") {
            s.push(("Wall", block([5.0, 4.0, 0.0], [2.0, 8.0, 8.0])));
        }
        s
    }

    /// (name, press, release, grid, align, use bounding boxes, snap to parts, expected Probe CFrame)
    #[allow(clippy::type_complexity)]
    pub const CASES: &[(&str, (f32, f32), (f32, f32), Option<f32>, bool, bool, bool, [f32; 12])] = &[
        ("wedge_align", (753.0, 500.0), (914.0, 420.0), Some(1.0), true, false, false,
            [8.0, 4.73205, -4.73205, 1.0, 0.0, 0.0, 0.0, 0.86603, -0.5, 0.0, 0.5, 0.86603]),
        ("wedge_noalign", (753.0, 500.0), (914.0, 420.0), Some(1.0), false, false, false,
            [8.0, 5.59808, -3.76795, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        ("wedge_bbox", (753.0, 500.0), (914.0, 420.0), Some(1.0), true, true, false,
            [8.0, 6.0, -6.5359, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        ("target_partsnap_off", (753.0, 500.0), (1043.0, 357.0), None, true, false, false,
            [14.24963, 6.0, -8.03405, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        ("target_partsnap_on", (753.0, 500.0), (1043.0, 357.0), None, true, false, true,
            [14.0, 6.0, -8.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        // Collisions ticked: Studio's geometric free drag still overlaps the
        // Target (FreeformDragger._updateGeometric never checks).
        ("target_overlap", (753.0, 500.0), (810.0, 520.0), None, true, false, false,
            [2.51709, 2.0, -1.00173, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
        ("rot30_align", (753.0, 500.0), (951.0, 500.0), Some(1.0), true, false, false,
            [10.0, 2.0, -1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
    ];

    /// Camera:ViewportPointToRay(px).Direction in that Studio for the pixels
    /// used (the measured drags are reproduced from these rays; rustudio's
    /// own Camera::ray at 1506x842 differs, so the self-test turns them
    /// back into its view pixels with Camera::project).
    pub fn studio_ray(px: f32, py: f32) -> Option<Vec3> {
        let t: &[((f32, f32), [f32; 3])] = &[
            ((753.0, 500.0), [0.000000, -0.721098, -0.692833]),
            ((914.0, 420.0), [0.258662, -0.602180, -0.755297]),
            ((1043.0, 357.0), [0.432452, -0.485572, -0.759740]),
            ((810.0, 520.0), [0.093136, -0.740032, -0.666092]),
            ((951.0, 500.0), [0.310382, -0.685484, -0.658616]),
            ((700.0, 380.0), [-0.087607, -0.567931, -0.818401]),
        ];
        t.iter().find(|(p, _)| *p == (px, py)).map(|(_, d)| Vec3::from(*d))
    }

    pub fn camera() -> crate::camera::Camera {
        let mut c = crate::camera::Camera::default();
        c.pos = Vec3::new(0.0, 20.0, 20.0);
        let f = (Vec3::new(0.0, 0.0, -5.0) - c.pos).normalize();
        let r = f.cross(Vec3::Y).normalize();
        c.set_rotation(Mat3::from_cols(r, r.cross(f), -f));
        c.fov_deg = 70.0;
        c
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;
    use super::mode_cases::*;

    /// Studio's own mouse ray for the pixel (the rays are what the drags
    /// depend on; see `studio_ray`).
    fn ray(px: f32, py: f32) -> Ray {
        let d = studio_ray(px, py).expect("a measured pixel");
        Ray { origin: camera().pos, dir: d.normalize() }
    }
    fn nearest(s: &[(&str, Solid)], ray: &Ray, skip: &str) -> Option<(Vec3, Vec3, Solid)> {
        s.iter()
            .filter(|(n, _)| *n != skip)
            .filter_map(|(_, so)| so.raycast(ray).map(|(t, n)| (t, n, *so)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(t, n, so)| (ray.at(t), n, so))
    }

    #[test]
    fn studio_dragger_settings() {
        let cam = camera();
        let mut bad = Vec::new();
        for (name, press, release, grid, align, bbox, snap, want) in CASES {
            let s = scene(name);
            let probe = s[1].1;
            let pr = ray(press.0, press.1);
            let (click, n, clicked) = nearest(&s, &pr, "").unwrap();
            let face = if *bbox { surface_matrix_box(&clicked, click, n).1 } else { surface_matrix(&clicked.geometry(), click, n) };
            let mut d = FreeDrag::begin(DragBox::new(probe.xf, &[(probe.xf, probe.size)], 1), click, Some(face), *grid, *bbox);
            let rr = ray(release.0, release.1);
            let surf = d.surface(&rr, nearest(&s, &rr, "Probe")).unwrap();
            let opts = DragOpts { grid: *grid, align: *align, soft: snap.then(|| (cam.pos, cam.forward(), cam.fov_deg)) };
            let xf = d.target(surf, opts).mul(&probe.xf);
            let pos_err = (xf.pos - Vec3::new(want[0], want[1], want[2])).abs().max_element();
            let mut rot_err = 0.0f32;
            for i in 0..3 {
                for j in 0..3 {
                    rot_err = rot_err.max((xf.rot.col(j)[i] - want[3 + i * 3 + j]).abs());
                }
            }
            if pos_err > 0.002 || rot_err > 2e-5 {
                bad.push(format!("{name}: got {:?} rot {:?} (pos err {pos_err}, rot err {rot_err})", xf.pos, xf.rot));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    #[test]
    fn snap_rotation_picks_nearest_axes() {
        let m = snap_rotation_to_primary(studio_cases::ang(10.0, 30.0, -20.0));
        assert!(m.abs_diff_eq(Mat3::IDENTITY, 1e-6), "{m:?}");
        let m = snap_rotation_to_primary(studio_cases::ang(0.0, 80.0, 0.0));
        assert!(m.abs_diff_eq(Mat3::from_rotation_y(std::f32::consts::FRAC_PI_2), 1e-6), "{m:?}");
    }
}
