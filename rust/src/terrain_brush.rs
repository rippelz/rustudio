//! The Terrain Editor's brushes, as Studio's TerrainEditor plugin does
//! them (BuiltInPlugins/TerrainEditor.rbxm, read as bytecode):
//! Src/Util/Operations/{Draw,Sculpt,Smooth,Flatten,Paint}Operation and the
//! helpers CalculateBrushOccupancy, CalculateMagnitudePercent,
//! ConvertForPivot, ClampVoxelBoundaries, getDraggedPositions,
//! CalculateAutoMaterial. Sizes are in voxels (1..64, the brush is
//! size × 4 studs across); one `step` is one OnStep of the plugin's
//! operation, which Studio runs once per frame while the button is held.
//!
//! Draw fills with the engine's FillBall / FillBlock / FillCylinder
//! (`Voxels::fill`, the same share rule: 0.5 + (r − d) / 4, which matches
//! a Draw click read back from Studio). Smooth's kernel is the engine's
//! Terrain:SmoothRegion, which plugins can't call or read: here it is a
//! 3×3×3 average of a signed distance field built from the occupancies,
//! which keeps flat ground flat, melts one-voxel bumps, fills one-voxel
//! pits and turns a one-voxel step into a ramp a few voxels wide, as
//! Studio's does (measured in the offline Studio).

use glam::Vec3;

use rustudio_core::terrain::{Voxels, VOXEL, WATER};

pub const VR: f32 = VOXEL;
/// Constants.MinimumOccupancy / MaximumOccupancy / MinimumSmooth.
pub const MIN_OCC: f32 = 1.0 / 256.0;
pub const MIN_SMOOTH: f32 = 0.0625;

/// BrushSize {Size = 6, Height = 6, Locked = true}; slider 1..64 step 1.
pub const DEFAULT_SIZE: f32 = 6.0;
pub const SIZE_MIN: f32 = 1.0;
pub const SIZE_MAX: f32 = 64.0;
/// Strength: default 1, slider 0..1 step 0.01.
pub const DEFAULT_STRENGTH: f32 = 1.0;
pub const STRENGTH_MIN: f32 = 0.0;

/// One operation step per frame (BaseOperation yields task.wait(0)
/// between steps; Sculpt / Smooth / Paint have no budget). Studio runs at
/// 60 frames a second in edit mode, so rustudio steps at that rate however
/// fast it draws.
pub const INTERVAL_S: f32 = 1.0 / 60.0;
/// Flatten's 0.5 ms budget lets it step a few times in one frame; three
/// is what a 6-voxel Flatten fits on a typical machine (an estimate).
pub const FLATTEN_STEPS_PER_FRAME: usize = 3;

/// The material picker's order (Util.getTerrainMaterialItems /
/// getMaterials: Enum.Material order of the terrain materials), as
/// SmoothGrid indices; Air left out.
pub const PICKER_ORDER: [u8; 23] = [16, 13, 5, 17, 4, 15, 9, 2, 14, 18, 19, 21, 12, 22, 8, 20, 6, 11, 3, 10, 1, 7, 0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrushOp {
    Draw,
    Sculpt,
    Smooth,
    Flatten,
    Paint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Sphere,
    Cube,
    Cylinder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pivot {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLock {
    Off,
    Auto,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlattenMode {
    Erode,
    Grow,
    Both,
}

/// What one brush step needs (the plugin's payload).
#[derive(Debug, Clone)]
pub struct Params {
    pub op: BrushOp,
    pub shape: Shape,
    /// Voxels across (BrushSize.Size).
    pub size: f32,
    /// Voxels tall for Cube / Cylinder (BrushSize.Height).
    pub height: f32,
    pub strength: f32,
    pub pivot: Pivot,
    /// Draw / Sculpt: Subtract (Ctrl flips it while held).
    pub subtract: bool,
    /// Draw / Sculpt with Shift held: a Smooth step instead.
    pub smooth_instead: bool,
    pub ignore_water: bool,
    /// Source material (SmoothGrid index).
    pub material: u8,
    /// Paint's Replace mode: the target.
    pub target: Option<u8>,
    pub auto_material: bool,
    pub flatten: FlattenMode,
    /// Flatten's plane height (studs): the brush centre at the click, or
    /// the Fixed plane.
    pub plane_y: f32,
}

impl Params {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(op: BrushOp) -> Params {
        Params {
            op,
            shape: Shape::Sphere,
            size: DEFAULT_SIZE,
            height: DEFAULT_SIZE,
            strength: DEFAULT_STRENGTH,
            pivot: Pivot::Center,
            subtract: false,
            smooth_instead: false,
            ignore_water: false,
            material: 2,
            target: None,
            auto_material: false,
            flatten: FlattenMode::Both,
            plane_y: 0.0,
        }
    }

    /// The brush's height in studs (a sphere is as tall as it is wide).
    pub fn height_studs(&self) -> f32 {
        (if self.shape == Shape::Sphere { self.size } else { self.height }) * VR
    }
}

/// A stroke's running state (the operation's `State`).
#[derive(Debug, Clone, Default)]
pub struct StrokeState {
    /// Where the last step's interpolation ended.
    pub position: Option<Vec3>,
    /// Draw / Paint: the last position applied (a still mouse adds nothing).
    pub last_position: Option<Vec3>,
    /// Sculpt: where the last step's closest edit was.
    pub next_start: Option<Vec3>,
}

// ------------------------------------------------------------ helpers

/// Util.CalculateBrushOccupancy: a one-voxel linear edge.
pub fn brush_occupancy(dist: f32, radius: f32) -> f32 {
    ((radius - dist) / VR).clamp(0.0, 1.0)
}

/// Util.CalculateMagnitudePercent: the cosine strength curve.
pub fn magnitude_percent(dist: f32, radius: f32) -> f32 {
    ((dist / radius).min(1.0) * std::f32::consts::FRAC_PI_2).cos()
}

/// Util.ConvertForPivot: Top hangs the brush below the point, Bottom sits
/// it on the point.
pub fn convert_for_pivot(pivot: Pivot, pos: Vec3, height: f32) -> Vec3 {
    match pivot {
        Pivot::Top => pos - Vec3::Y * height * 0.5,
        Pivot::Bottom => pos + Vec3::Y * height * 0.5,
        Pivot::Center => pos,
    }
}

/// Util.getSnappedToVoxel: to the nearest voxel corner.
pub fn snap_to_voxel(p: Vec3) -> Vec3 {
    (p / VR).round() * VR
}

/// Util.ClampVoxelBoundaries: the voxel box (first voxel, count) around a
/// brush of `radius` (studs) and `height` (studs).
pub fn clamp_bounds(center: Vec3, radius: f32, height: f32) -> ([i32; 3], [i32; 3]) {
    let half = Vec3::new(radius, height * 0.5, radius);
    let lo = ((center - half) / VR).floor();
    let hi = ((center + half) / VR).ceil();
    let lo = [lo.x as i32, lo.y as i32, lo.z as i32];
    let hi = [hi.x as i32, hi.y as i32, hi.z as i32];
    (lo, [0, 1, 2].map(|k| (hi[k] - lo[k]).max(0)))
}

/// Util.getDraggedPositions: the brush centres between the last step's
/// and this one, half a radius apart, at most radius + 20 studs of travel.
pub fn dragged_positions(from: Vec3, to: Vec3, radius: f32) -> Vec<Vec3> {
    let delta = to - from;
    let dist = delta.length();
    let step = radius * 0.5;
    if !(step < dist) || step <= 0.0 {
        return vec![to];
    }
    let dir = delta / dist;
    let dist = dist.min(step * 2.0 + 20.0);
    let n = (dist / step - 0.1).ceil().max(1.0) as usize;
    (1..=n).map(|i| from + dir * dist * (i as f32 / n as f32)).collect()
}

/// The brush centre the gizmo shows for a point under the mouse.
pub fn brush_center(hit: Vec3, p: &Params, snap: bool) -> Vec3 {
    let hit = if snap { snap_to_voxel(hit) } else { hit };
    convert_for_pivot(p.pivot, hit, p.height_studs())
}

/// Util.getPlaneIntersection.
pub fn ray_plane(origin: Vec3, dir: Vec3, point: Vec3, normal: Vec3) -> Option<Vec3> {
    let d = dir.dot(normal);
    if d.abs() < 1e-6 {
        return None;
    }
    let t = (point - origin).dot(normal) / d;
    (t >= 0.0).then(|| origin + dir * t)
}

/// Util.getCameraLookPlane: the camera's look vector rounded to a
/// (possibly diagonal) axis: Plane Lock Auto's plane normal.
pub fn camera_look_plane(look: Vec3) -> Vec3 {
    look.round().normalize_or(Vec3::Y)
}

/// The brush box read into arrays (1-based in the plugin, 0-based here).
struct Block {
    lo: [i32; 3],
    n: [i32; 3],
    mat: Vec<u8>,
    occ: Vec<f32>,
}

impl Block {
    fn read(v: &Voxels, lo: [i32; 3], n: [i32; 3]) -> Block {
        let len = (n[0].max(0) * n[1].max(0) * n[2].max(0)) as usize;
        let mut b = Block { lo, n, mat: vec![0; len], occ: vec![0.0; len] };
        for z in 0..n[2] {
            for y in 0..n[1] {
                for x in 0..n[0] {
                    let (m, o) = v.get(lo[0] + x, lo[1] + y, lo[2] + z);
                    let i = b.idx(x, y, z);
                    b.mat[i] = m;
                    b.occ[i] = if m == 0 { 0.0 } else { o };
                }
            }
        }
        b
    }

    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        (x + self.n[0] * (y + self.n[1] * z)) as usize
    }

    fn inside(&self, x: i32, y: i32, z: i32) -> bool {
        x >= 0 && y >= 0 && z >= 0 && x < self.n[0] && y < self.n[1] && z < self.n[2]
    }

    /// Offset of a voxel's centre from `pos` (studs).
    fn offset(&self, x: i32, y: i32, z: i32, pos: Vec3) -> Vec3 {
        Vec3::new(
            (self.lo[0] + x) as f32 + 0.5,
            (self.lo[1] + y) as f32 + 0.5,
            (self.lo[2] + z) as f32 + 0.5,
        ) * VR
            - pos
    }

    /// The quantised radius the falloff uses: half the box's width.
    fn radius(&self) -> f32 {
        self.n[0] as f32 * VR * 0.5
    }

    /// Writes the cells that differ from what `v` holds.
    fn write(&self, v: &mut Voxels) {
        let mut out = Vec::new();
        for z in 0..self.n[2] {
            for y in 0..self.n[1] {
                for x in 0..self.n[0] {
                    let i = self.idx(x, y, z);
                    let w = [self.lo[0] + x, self.lo[1] + y, self.lo[2] + z];
                    let (m0, o0) = v.get(w[0], w[1], w[2]);
                    let (m, o) = (self.mat[i], self.occ[i].clamp(0.0, 1.0));
                    let (m, o) = if m == 0 || o < MIN_OCC { (0, 0.0) } else { (m, o) };
                    if m != m0 || (o - if m0 == 0 { 0.0 } else { o0 }).abs() > 0.5 / 255.0 {
                        out.push((w, m, o));
                    }
                }
            }
        }
        v.edit(out);
    }
}

/// sphere3D / circular flags and the falloff distance (Draw, Sculpt,
/// Smooth, Flatten, Paint all compute them the same way).
fn falloff_dist(shape: Shape, size: f32, d: Vec3) -> Option<f32> {
    let sphere = shape == Shape::Sphere && size > 2.0;
    let circular = (shape == Shape::Cylinder && size > 2.0) || sphere;
    if sphere {
        Some(d.length())
    } else if circular {
        Some((d.x * d.x + d.z * d.z).sqrt())
    } else {
        None
    }
}

/// Util.CalculateAutoMaterial: the LAST non-Air material among the 27
/// cells around (and including) a voxel, in the plugin's loop order
/// (dx, then dy, then dz) — it counts them but keeps the last one seen.
fn auto_material(b: &Block, x: i32, y: i32, z: i32) -> Option<u8> {
    let mut out = None;
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let (i, j, k) = (x + dx, y + dy, z + dz);
                if b.inside(i, j, k) {
                    let m = b.mat[b.idx(i, j, k)];
                    if m != 0 {
                        out = Some(m);
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------ one step

/// One step of a brush at `hit` (the point under the mouse, snapped if
/// asked): the voxel box it changed (first, last), or None when it did
/// nothing (Draw / Paint with the mouse still).
pub fn step(v: &mut Voxels, p: &Params, hit: Vec3, st: &mut StrokeState) -> Option<([i32; 3], [i32; 3])> {
    let mut touched: Option<([i32; 3], [i32; 3])> = None;
    let mut note = |lo: [i32; 3], n: [i32; 3]| {
        let hi = [0, 1, 2].map(|k| lo[k] + n[k] - 1);
        touched = Some(match touched {
            None => (lo, hi),
            Some((a, b)) => ([0, 1, 2].map(|k| a[k].min(lo[k])), [0, 1, 2].map(|k| b[k].max(hi[k]))),
        });
    };
    let op = if p.smooth_instead && matches!(p.op, BrushOp::Draw | BrushOp::Sculpt) { BrushOp::Smooth } else { p.op };
    let mut size = p.size.clamp(SIZE_MIN, SIZE_MAX);
    let mut bh = p.height_studs();
    let mut bs = size * VR;
    let mut r = bs * 0.5;
    if op == BrushOp::Draw && !p.subtract {
        // Draw's Add brush is 2 studs smaller (radius 1 less).
        size -= 1.0;
        bh -= 2.0;
        bs -= 2.0;
        r -= 1.0;
    }
    let center = convert_for_pivot(p.pivot, hit, bh);
    let from = st.position.unwrap_or(center);
    let positions = dragged_positions(from, center, r);
    st.position = positions.last().copied();
    match op {
        BrushOp::Draw => {
            let mat = if p.subtract { 0 } else { p.material };
            for pos in positions {
                if st.last_position == Some(pos) && !p.auto_material {
                    return touched;
                }
                st.last_position = Some(pos);
                let (lo, n) = clamp_bounds(pos, r, bh);
                let geometric = (!p.subtract || !p.ignore_water) && !p.auto_material;
                if geometric {
                    match p.shape {
                        Shape::Sphere => fill_ball(v, pos, r, mat),
                        Shape::Cube => {
                            let h = Vec3::new(bs, bh, bs) * 0.5;
                            v.fill((pos - h).to_array(), (pos + h).to_array(), mat, |c| {
                                let d = (Vec3::from(c) - pos).abs() - h;
                                d.max(Vec3::ZERO).length() + d.max_element().min(0.0)
                            });
                        }
                        Shape::Cylinder => {
                            if n[0] as f32 * VR <= 2.0 * VR {
                                fill_ball(v, pos, r, mat);
                            } else {
                                let hh = bh * 0.5;
                                v.fill((pos - Vec3::new(r, hh, r)).to_array(), (pos + Vec3::new(r, hh, r)).to_array(), mat, |c| {
                                    let d = Vec3::from(c) - pos;
                                    let q = glam::Vec2::new(glam::Vec2::new(d.x, d.z).length() - r, d.y.abs() - hh);
                                    q.max(glam::Vec2::ZERO).length() + q.max_element().min(0.0)
                                });
                            }
                        }
                    }
                    let pad = [lo[0] - 1, lo[1] - 1, lo[2] - 1];
                    note(pad, n.map(|c| c + 2));
                    continue;
                }
                // Voxel path: Auto Material, or Subtract with Ignore Water.
                let mut b = Block::read(v, lo, n);
                let orig = Block { lo, n, mat: b.mat.clone(), occ: b.occ.clone() };
                let rr = b.radius();
                for z in 0..n[2] {
                    for y in 0..n[1] {
                        for x in 0..n[0] {
                            let i = b.idx(x, y, z);
                            let d = b.offset(x, y, z, pos);
                            let target = falloff_dist(p.shape, size, d).map_or(1.0, |dist| brush_occupancy(dist, rr));
                            let (occ, m) = (orig.occ[i], orig.mat[i]);
                            if !p.subtract {
                                if occ < target {
                                    b.occ[i] = target;
                                    b.mat[i] = if p.auto_material { auto_material(&orig, x, y, z).unwrap_or(m) } else { p.material };
                                }
                            } else if (!p.ignore_water || m != WATER) && target >= MIN_OCC {
                                b.occ[i] = 0.0;
                                b.mat[i] = 0;
                            }
                        }
                    }
                }
                b.write(v);
                note(lo, n);
            }
        }
        BrushOp::Sculpt => {
            for pos in positions {
                let (lo, n) = clamp_bounds(pos, r, bh);
                sculpt(v, p, pos, lo, n, size, st);
                note(lo, n);
            }
        }
        BrushOp::Smooth => {
            for pos in positions {
                let (lo, n) = clamp_bounds(pos, r, bh);
                smooth(v, p, pos, lo, n, size);
                note(lo, n);
            }
        }
        BrushOp::Flatten => {
            // The box comes from the current centre once (the plugin's
            // quirk); the falloff is centred on each dragged position.
            let (lo, n) = clamp_bounds(center, r, bh);
            for pos in positions {
                flatten(v, p, pos, lo, n, size);
            }
            note(lo, n);
        }
        BrushOp::Paint => {
            for pos in positions {
                if st.last_position == Some(pos) {
                    return touched;
                }
                st.last_position = Some(pos);
                let (lo, n) = clamp_bounds(pos, r, bh);
                paint(v, p, pos, lo, n, size);
                note(lo, n);
            }
        }
    }
    touched
}

/// Terrain:FillBall (the engine's share rule, `Voxels::fill`).
pub fn fill_ball(v: &mut Voxels, c: Vec3, r: f32, mat: u8) {
    v.fill((c - Vec3::splat(r)).to_array(), (c + Vec3::splat(r)).to_array(), mat, |q| (Vec3::from(q) - c).length() - r);
}

const N6: [[i32; 3]; 6] = [[-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];

/// SculptOperation.OnStep for one position: a flood fill along the
/// surface inside the brush box from the voxel under the brush; Grow
/// adds the 6 neighbours' mean occupancy, Erode takes away the share of
/// empty space around the voxel, both × strength curve × edge × cosine.
fn sculpt(v: &mut Voxels, p: &Params, pos: Vec3, lo: [i32; 3], n: [i32; 3], size: f32, st: &mut StrokeState) {
    let b = Block::read(v, lo, n);
    let mut w = Block { lo, n, mat: b.mat.clone(), occ: b.occ.clone() };
    let rr = b.radius();
    let sphere = p.shape == Shape::Sphere && size > 2.0;
    let circular = (p.shape == Shape::Cylinder && size > 2.0) || sphere;
    let s = p.strength + 0.08;
    let erode_strength = s * (size.ln() / 14.0 + 0.025);
    let grow_strength = s * (size.ln() / 28.0 + 0.025);
    let add = !p.subtract;
    let on_surface = p.pivot == Pivot::Center;
    let water = |m: u8| p.ignore_water && m == WATER;
    let occ_at = |x: i32, y: i32, z: i32| -> f32 {
        let i = b.idx(x, y, z);
        if water(b.mat[i]) {
            0.0
        } else {
            b.occ[i]
        }
    };
    let mut found = on_surface;
    let seed = match st.next_start {
        Some(ns) if !on_surface && magnitude_percent((ns - pos).length(), rr) > 0.5 => ns,
        _ => pos,
    };
    let sv = [0, 1, 2].map(|k| ((seed[k] - lo[k] as f32 * VR) / VR).round() as i32 - 1);
    let mut next_mag = size * VR;
    let mut next_start = st.next_start;
    // The plugin's visited index uses S.X twice (kept: it decides which
    // cells the fill skips).
    let key = |x: i32, y: i32, z: i32| -> i64 {
        let (x, y, z) = (x as i64 + 1, y as i64 + 1, z as i64 + 1);
        x + n[0] as i64 * (y + n[0] as i64 * z)
    };
    let mut visited: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut queue: Vec<[i32; 3]> = Vec::new();
    let (mut has_air, mut has_full) = (false, false);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                let c = [sv[0] + dx, sv[1] + dy, sv[2] + dz];
                if b.inside(c[0], c[1], c[2]) {
                    visited.insert(key(c[0], c[1], c[2]));
                    let o = b.occ[b.idx(c[0], c[1], c[2])];
                    if o == 0.0 {
                        has_air = true;
                    } else if o == 1.0 {
                        has_full = true;
                    } else {
                        found = true;
                    }
                    queue.push(c);
                }
            }
        }
    }
    if has_air && has_full {
        found = true;
    }
    let check_neighbors = |c: [i32; 3]| -> (bool, bool) {
        let (mut all_empty, mut all_full) = (true, true);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let q = [c[0] + dx, c[1] + dy, c[2] + dz];
                    if b.inside(q[0], q[1], q[2]) {
                        let o = occ_at(q[0], q[1], q[2]);
                        if o > 0.0 {
                            all_empty = false;
                        }
                        if o < 1.0 {
                            all_full = false;
                        }
                        if !all_empty && !all_full {
                            return (false, false);
                        }
                    }
                }
            }
        }
        (all_empty, all_full)
    };
    let mut guard = 0usize;
    while let Some(c) = queue.pop() {
        guard += 1;
        if guard > 4_000_000 {
            break;
        }
        let i = b.idx(c[0], c[1], c[2]);
        let (mut occ, mut mat) = (b.occ[i], b.mat[i]);
        let air_like = water(mat) || mat == 0;
        if !found {
            if water(mat) {
                occ = 0.0;
            }
            if (has_air && occ != 0.0) || (has_full && occ != 1.0) {
                found = true;
            }
            if found {
                visited.clear();
            }
        }
        if found {
            let off = b.offset(c[0], c[1], c[2], pos);
            let dist = off.length();
            let mut track = |off: Vec3| {
                if !on_surface && off.length() < next_mag {
                    next_mag = off.length();
                    next_start = Some(off + pos);
                }
            };
            if !add && occ > 0.0 && !air_like {
                // erode
                let bo = if circular { brush_occupancy(dist, rr) } else { 1.0 };
                if bo > 0.5 {
                    let mp = if circular { magnitude_percent(dist, rr) } else { 1.0 };
                    let (mut empty_n, mut air_amount) = (false, 6.0f32);
                    for d in N6 {
                        let q = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                        if b.inside(q[0], q[1], q[2]) {
                            let o = occ_at(q[0], q[1], q[2]);
                            if o <= 0.0 {
                                empty_n = true;
                            }
                            air_amount -= o;
                        }
                    }
                    let mut new = if occ < 1.0 || empty_n { occ - (air_amount / 6.0) * erode_strength * bo * mp } else { occ };
                    new = new.max(0.0);
                    if new <= MIN_OCC {
                        w.occ[i] = 0.0;
                        w.mat[i] = 0;
                        track(off);
                    } else if new != occ {
                        w.occ[i] = new;
                        track(off);
                    }
                }
            } else if add && (occ < 1.0 || air_like) {
                if water(mat) {
                    mat = 0;
                    occ = 0.0;
                }
                let bo = if circular { brush_occupancy(dist, rr) } else { 1.0 };
                if occ != 1.0 && bo >= 0.5 {
                    let mp = if circular { magnitude_percent(dist, rr) } else { 1.0 };
                    let (mut full_n, mut sum, mut count) = (false, 0.0f32, 0);
                    for d in N6 {
                        let q = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                        // The plugin's exclusive upper bound here (quirk):
                        // 0 < c < S, 1-based.
                        let inb = (0..3).all(|k| q[k] >= 0 && q[k] < n[k] - 1);
                        if inb {
                            let o = occ_at(q[0], q[1], q[2]);
                            if o >= 1.0 {
                                full_n = true;
                            }
                            count += 1;
                            sum += o;
                        }
                    }
                    let avg = if count == 0 { 0.0 } else { sum / count as f32 };
                    let mut new = if occ > 0.0 || full_n { occ + avg * grow_strength * bo * mp } else { occ };
                    new = new.min(1.0);
                    if mat == 0 && new > 0.0 {
                        let m = if p.auto_material { auto_material(&b, c[0], c[1], c[2]).unwrap_or(0) } else { p.material };
                        w.mat[i] = if p.ignore_water && m == WATER { 0 } else { m };
                    }
                    if new != occ {
                        w.occ[i] = new;
                        track(off);
                    }
                }
            }
        }
        // addNeighbors
        let (all_empty, all_full) = check_neighbors(c);
        let skip = found
            && ((add && (all_full || ((occ == 0.0 || air_like) && all_empty)))
                || (!add && (all_empty || (occ == 1.0 && !air_like && all_full))));
        if skip {
            continue;
        }
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let q = [c[0] + dx, c[1] + dy, c[2] + dz];
                    if !b.inside(q[0], q[1], q[2]) {
                        continue;
                    }
                    let cand = if !found {
                        true
                    } else {
                        let o = occ_at(q[0], q[1], q[2]);
                        (add && o < 1.0 && !((occ == 0.0 || air_like) && o == 0.0))
                            || (!add && o > 0.0 && !(occ == 1.0 && !air_like && o == 1.0))
                    };
                    if cand && visited.insert(key(q[0], q[1], q[2])) {
                        queue.push(q);
                    }
                }
            }
        }
    }
    st.next_start = next_start;
    w.write(v);
}

/// Stand-in for the engine's Terrain:SmoothRegion(region, 4, strength):
/// the occupancies as a signed distance (studs, + inside) reaching two
/// voxels past the surface, averaged over each voxel's 3×3×3 block, back
/// to occupancy, blended by `strength`. Materials: a voxel that fills
/// takes its most common solid neighbour's.
pub fn smooth_region(b_mat: &[u8], b_occ: &[f32], n: [i32; 3], strength: f32) -> (Vec<u8>, Vec<f32>) {
    let idx = |x: i32, y: i32, z: i32| (x + n[0] * (y + n[1] * z)) as usize;
    let inside = |x: i32, y: i32, z: i32| x >= 0 && y >= 0 && z >= 0 && x < n[0] && y < n[1] && z < n[2];
    let len = b_occ.len();
    // Distance: partial voxels know theirs; full / empty ones take the
    // nearest surface (chamfer passes), capped at 3 voxels.
    let cap = 3.0 * VR;
    let mut sdf = vec![0.0f32; len];
    let mut known = vec![false; len];
    for i in 0..len {
        let o = if b_mat[i] == 0 { 0.0 } else { b_occ[i] };
        if o > 0.0 && o < 1.0 {
            sdf[i] = (o - 0.5) * VR;
            known[i] = true;
        } else {
            sdf[i] = if o >= 1.0 { cap } else { -cap };
        }
    }
    // Faces between a full and an empty voxel put the surface half way.
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let i = idx(x, y, z);
                if known[i] {
                    continue;
                }
                let solid = sdf[i] > 0.0;
                for d in N6 {
                    let (a, bb, c) = (x + d[0], y + d[1], z + d[2]);
                    if inside(a, bb, c) {
                        let j = idx(a, bb, c);
                        if !known[j] && (sdf[j] > 0.0) != solid {
                            sdf[i] = if solid { VR * 0.5 } else { -VR * 0.5 };
                            known[i] = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    // Chamfer: distances grow by a voxel per step away from the surface.
    for _ in 0..3 {
        let prev = sdf.clone();
        for z in 0..n[2] {
            for y in 0..n[1] {
                for x in 0..n[0] {
                    let i = idx(x, y, z);
                    if known[i] {
                        continue;
                    }
                    let s = prev[i].signum();
                    let mut best = prev[i].abs();
                    for d in N6 {
                        let (a, bb, c) = (x + d[0], y + d[1], z + d[2]);
                        if inside(a, bb, c) {
                            let j = idx(a, bb, c);
                            if prev[j].signum() == s || prev[j] == 0.0 {
                                best = best.min(prev[j].abs() + VR);
                            } else {
                                best = best.min(VR * 0.5);
                            }
                        }
                    }
                    sdf[i] = s * best.min(cap);
                }
            }
        }
    }
    let mut mat = b_mat.to_vec();
    let mut occ = b_occ.to_vec();
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let i = idx(x, y, z);
                let (mut sum, mut cnt) = (0.0, 0.0);
                for dz in -1..=1 {
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (a, bb, c) = (x + dx, y + dy, z + dz);
                            // Outside the box the field carries on as at the edge.
                            let (a, bb, c) = (a.clamp(0, n[0] - 1), bb.clamp(0, n[1] - 1), c.clamp(0, n[2] - 1));
                            sum += sdf[idx(a, bb, c)];
                            cnt += 1.0;
                        }
                    }
                }
                let d = sum / cnt;
                let target = (0.5 + d / VR).clamp(0.0, 1.0);
                let o0 = if b_mat[i] == 0 { 0.0 } else { b_occ[i] };
                let o = o0 + (target - o0) * strength;
                occ[i] = o;
                if b_mat[i] == 0 && o > 0.0 {
                    // the most common solid material around
                    let mut count = [0u32; 32];
                    for dz in -1..=1 {
                        for dy in -1..=1 {
                            for dx in -1..=1 {
                                let (a, bb, c) = (x + dx, y + dy, z + dz);
                                if inside(a, bb, c) {
                                    let m = b_mat[idx(a, bb, c)] as usize;
                                    if m != 0 && m < 32 {
                                        count[m] += 1;
                                    }
                                }
                            }
                        }
                    }
                    let best = (1..32).max_by_key(|m| count[*m]).filter(|m| count[*m] > 0);
                    mat[i] = best.map_or(0, |m| m as u8);
                } else if o <= 0.0 {
                    mat[i] = 0;
                }
            }
        }
    }
    (mat, occ)
}

/// SmoothOperation.OnStep for one position.
fn smooth(v: &mut Voxels, p: &Params, pos: Vec3, lo: [i32; 3], n: [i32; 3], size: f32) {
    // A voxel of margin so the kernel sees past the box's edge.
    let lo2 = [lo[0] - 1, lo[1] - 1, lo[2] - 1];
    let n2 = n.map(|c| c + 2);
    let big = Block::read(v, lo2, n2);
    let (s_mat, s_occ) = smooth_region(&big.mat, &big.occ, n2, p.strength);
    let mut w = Block::read(v, lo, n);
    let rr = w.radius();
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let i = w.idx(x, y, z);
                let j = big.idx(x + 1, y + 1, z + 1);
                let (o, m) = (w.occ[i], w.mat[i]);
                if p.ignore_water && m == WATER {
                    continue;
                }
                let d = w.offset(x, y, z, pos);
                let (bo, mp) = match falloff_dist(p.shape, size, d) {
                    Some(dist) => (brush_occupancy(dist, rr), magnitude_percent(dist, rr)),
                    None => (1.0, 1.0),
                };
                let (sm, so) = (s_mat[j], s_occ[j]);
                let was_empty = sm == 0 || so <= 0.0;
                let new = (o + (so - o) * p.strength * bo * mp).clamp(0.0, 1.0);
                let mut out_m = sm;
                if new != so {
                    if was_empty && new >= MIN_SMOOTH {
                        out_m = if m != 0 { m } else { s_mat[j].max(neighbour_solid(&big, x + 1, y + 1, z + 1)) };
                    } else if new <= 0.0 {
                        out_m = 0;
                    }
                }
                w.occ[i] = new;
                w.mat[i] = if new <= 0.0 { 0 } else if out_m == 0 { m.max(neighbour_solid(&big, x + 1, y + 1, z + 1)) } else { out_m };
            }
        }
    }
    w.write(v);
}

fn neighbour_solid(b: &Block, x: i32, y: i32, z: i32) -> u8 {
    for d in N6 {
        let (a, bb, c) = (x + d[0], y + d[1], z + d[2]);
        if b.inside(a, bb, c) {
            let m = b.mat[b.idx(a, bb, c)];
            if m != 0 && m != WATER {
                return m;
            }
        }
    }
    0
}

/// FlattenOperation.OnStep for one position: column by column, Grow fills
/// up to the plane from below, Erode cuts down to it from above, each with
/// a budget of max(cos falloff × strength × 0.25 × ln(size) / 8, 1/256).
fn flatten(v: &mut Voxels, p: &Params, pos: Vec3, lo: [i32; 3], n: [i32; 3], size: f32) {
    let b = Block::read(v, lo, n);
    let mut w = Block { lo, n, mat: b.mat.clone(), occ: b.occ.clone() };
    let rr = b.radius();
    let sphere = p.shape == Shape::Sphere && size > 2.0;
    let circular = (p.shape == Shape::Cylinder && size > 2.0) || sphere;
    let grow = matches!(p.flatten, FlattenMode::Grow | FlattenMode::Both);
    let erode = matches!(p.flatten, FlattenMode::Erode | FlattenMode::Both);
    let pl = p.plane_y;
    let min_y = lo[1] as f32 * VR;
    // 0-based plane voxel; its cap is floor(P/4) − P/4 (≤ 0) as the plugin
    // computes it, so the surface lands at the plane voxel's bottom.
    let plane_idx = (((pl - min_y) / VR).floor() as i32).clamp(0, n[1] - 1);
    let plane_frac = (((pl / VR).floor() - pl / VR) * 256.0).floor() / 256.0;
    let water = |m: u8| p.ignore_water && m == WATER;
    for x in 0..n[0] {
        for z in 0..n[2] {
            let d0 = b.offset(x, 0, z, pos);
            let hd = (d0.x * d0.x + d0.z * d0.z).sqrt();
            let hf = if circular { magnitude_percent(hd, rr) } else { 1.0 };
            let budget0 = (hf * p.strength * 0.25 * (size.ln() / 8.0)).max(MIN_OCC);
            let g_at = |y: i32| -> f32 {
                if !circular {
                    return 1.0;
                }
                let d = b.offset(x, y, z, pos);
                brush_occupancy(if sphere { d.length() } else { hd }, rr)
            };
            if grow {
                let mut budget = budget0;
                let mut started = false;
                for y in 0..=plane_idx {
                    let i = b.idx(x, y, z);
                    let (occ, mut mat) = (b.occ[i], b.mat[i]);
                    if g_at(y) <= 0.0 {
                        continue;
                    }
                    if occ == 0.0 && !started {
                        continue;
                    }
                    started = true;
                    if occ == 1.0 {
                        continue;
                    }
                    let mut new = occ + budget;
                    if new > 1.0 {
                        budget = new - 1.0;
                        new = 1.0;
                    } else {
                        budget = 0.0;
                    }
                    if y == plane_idx && new > plane_frac {
                        new = plane_frac;
                    }
                    if occ == 0.0 && y > 0 {
                        mat = b.mat[b.idx(x, y - 1, z)];
                    }
                    if water(mat) || mat == 0 || occ == new {
                        break;
                    }
                    w.mat[i] = mat;
                    w.occ[i] = new.clamp(0.0, 1.0);
                    if budget == 0.0 {
                        break;
                    }
                }
            }
            if erode {
                let mut budget = budget0;
                let mut started = false;
                for y in (plane_idx..n[1]).rev() {
                    let i = b.idx(x, y, z);
                    let (occ, mut mat) = (b.occ[i], b.mat[i]);
                    if g_at(y) <= 0.0 {
                        continue;
                    }
                    if occ == 1.0 && !started {
                        continue;
                    }
                    started = true;
                    if occ == 0.0 {
                        continue;
                    }
                    let mut new = occ - budget;
                    if new < 0.0 {
                        budget = -new;
                        new = 0.0;
                    } else {
                        budget = 0.0;
                    }
                    if y == plane_idx && new < plane_frac {
                        new = plane_frac;
                    }
                    if new <= 0.0 {
                        mat = 0;
                    }
                    if water(mat) {
                        break;
                    }
                    w.mat[i] = mat;
                    w.occ[i] = new.clamp(0.0, 1.0);
                    if budget == 0.0 {
                        break;
                    }
                }
            }
        }
    }
    w.write(v);
}

/// PaintOperation.OnStep for one position (legacy path): Paint sets the
/// material inside the brush; Replace swaps source for target (Air as
/// the source fills empty voxels with the target).
fn paint(v: &mut Voxels, p: &Params, pos: Vec3, lo: [i32; 3], n: [i32; 3], size: f32) {
    let mut b = Block::read(v, lo, n);
    let rr = b.radius();
    let src = p.material;
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let i = b.idx(x, y, z);
                let (occ, mat) = (b.occ[i], b.mat[i]);
                let g = match (p.target, p.shape) {
                    (Some(_), Shape::Cube) => 1.0,
                    _ => falloff_dist(p.shape, size, b.offset(x, y, z, pos)).map_or(1.0, |d| brush_occupancy(d, rr)),
                };
                if g <= 0.0 || (p.ignore_water && mat == WATER) {
                    continue;
                }
                match p.target {
                    None => {
                        if occ > 0.0 {
                            b.mat[i] = src;
                        }
                    }
                    Some(tgt) => {
                        if src == 0 {
                            if occ <= 0.0 && tgt != 0 {
                                b.mat[i] = tgt;
                                b.occ[i] = 1.0;
                            }
                        } else if mat == src && occ > 0.0 {
                            b.mat[i] = tgt;
                            if tgt == 0 {
                                b.occ[i] = 0.0;
                            }
                        }
                    }
                }
            }
        }
    }
    b.write(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ground(top_voxel: i32) -> Voxels {
        let mut v = Voxels::empty();
        let mut w = Vec::new();
        for z in -16..16 {
            for x in -16..16 {
                for y in -4..top_voxel {
                    w.push(([x, y, z], 2u8, 1.0));
                }
            }
        }
        v.edit(w);
        v
    }

    #[test]
    fn helpers_match_the_plugin() {
        assert_eq!(brush_occupancy(10.0, 12.0), 0.5);
        assert_eq!(brush_occupancy(13.0, 12.0), 0.0);
        assert!((magnitude_percent(6.0, 12.0) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(magnitude_percent(20.0, 12.0).abs() < 1e-6, true);
        assert_eq!(convert_for_pivot(Pivot::Bottom, Vec3::ZERO, 24.0), Vec3::new(0.0, 12.0, 0.0));
        assert_eq!(convert_for_pivot(Pivot::Top, Vec3::ZERO, 24.0), Vec3::new(0.0, -12.0, 0.0));
        // ClampVoxelBoundaries: floor / ceil to the grid.
        assert_eq!(clamp_bounds(Vec3::new(1.0, 2.0, 3.0), 12.0, 24.0), ([-3, -3, -3], [7, 7, 7]));
        assert_eq!(snap_to_voxel(Vec3::new(5.9, -2.1, 2.0)), Vec3::new(4.0, -4.0, 4.0));
    }

    #[test]
    fn dragged_positions_step_half_a_radius() {
        let p = dragged_positions(Vec3::ZERO, Vec3::new(30.0, 0.0, 0.0), 12.0);
        // step 6, travel capped at 12·2·… = min(30, 6·2+20) = 30 → ceil(5 − 0.1) = 5
        assert_eq!(p.len(), 5);
        assert!((p[4].x - 30.0).abs() < 1e-4);
        let far = dragged_positions(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0), 12.0);
        assert!((far.last().unwrap().x - 32.0).abs() < 1e-4, "lags to radius+20: {:?}", far.last());
        assert_eq!(dragged_positions(Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), 12.0).len(), 1);
    }

    /// Draw's Add is Terrain:FillBall(centre, radius − 1): Studio's own
    /// FillBall((0, 2, 0), 11, Grass) read back in the offline Studio (voxel z centre
    /// 2, x centres −18..18, rows y centres 2, 6, 10, 14).
    #[test]
    fn draw_is_studios_fillball() {
        let mut v = Voxels::empty();
        let mut st = StrokeState::default();
        let p = Params::new(BrushOp::Draw);
        step(&mut v, &p, Vec3::new(0.0, 2.0, 0.0), &mut st);
        let studio: [[f32; 10]; 4] = [
            [0.00, 0.00, 0.70, 1.00, 1.00, 1.00, 1.00, 0.70, 0.00, 0.00],
            [0.00, 0.00, 0.51, 1.00, 1.00, 1.00, 1.00, 0.51, 0.00, 0.00],
            [0.00, 0.00, 0.01, 0.70, 1.00, 1.00, 0.70, 0.01, 0.00, 0.00],
            [0.00, 0.00, 0.00, 0.00, 0.17, 0.17, 0.00, 0.00, 0.00, 0.00],
        ];
        for (row, want) in studio.iter().enumerate() {
            for (col, w) in want.iter().enumerate() {
                let (m, o) = v.get(col as i32 - 5, row as i32, 0);
                let o = if m == 0 { 0.0 } else { o };
                assert!((o - w).abs() < 0.011, "y{row} x{col}: {o} vs Studio {w}");
            }
        }
        // A still mouse adds nothing more.
        assert!(step(&mut v, &p, Vec3::new(0.0, 2.0, 0.0), &mut st).is_none());
        // Subtract carves it back out.
        let sub = Params { subtract: true, ..p };
        let mut st = StrokeState::default();
        step(&mut v, &sub, Vec3::new(0.0, 6.0, 0.0), &mut st);
        assert_eq!(v.get(0, 1, 0).0, 0);
    }

    /// A Draw click in Studio's editor on flat ground (top y = 0), read
    /// back: within a few hundredths of one FillBall at the hit (Studio's
    /// hit landed a little off centre, hence the looser bound).
    #[test]
    fn draw_click_close_to_studio() {
        let mut v = ground(0);
        let mut st = StrokeState::default();
        step(&mut v, &Params::new(BrushOp::Draw), Vec3::new(0.0, 2.0, 0.0), &mut st);
        let studio: [[f32; 10]; 4] = [
            [0.00, 0.00, 0.77, 1.00, 1.00, 1.00, 1.00, 0.74, 0.00, 0.00],
            [0.00, 0.00, 0.57, 1.00, 1.00, 1.00, 1.00, 0.55, 0.00, 0.00],
            [0.00, 0.00, 0.06, 0.76, 1.00, 1.00, 0.74, 0.04, 0.00, 0.00],
            [0.00, 0.00, 0.00, 0.00, 0.21, 0.21, 0.00, 0.00, 0.00, 0.00],
        ];
        for (row, want) in studio.iter().enumerate() {
            for (col, w) in want.iter().enumerate() {
                let (m, o) = v.get(col as i32 - 5, row as i32, 0);
                let o = if m == 0 { 0.0 } else { o };
                assert!((o - w).abs() < 0.08, "y{row} x{col}: {o} vs Studio {w}");
            }
        }
    }

    /// Sculpt (Add, size 6, strength 1) held on flat ground: Studio's
    /// click read back in the offline Studio lasted about 40 frames and left the brush's
    /// core full one voxel up and ≈0.2 in the next (y1 row 0.20 0.20 0.09).
    #[test]
    fn sculpt_grows_like_studio_and_erodes() {
        let mut v = ground(0);
        let p = Params::new(BrushOp::Sculpt);
        let mut st = StrokeState::default();
        for _ in 0..40 {
            step(&mut v, &p, Vec3::new(0.0, 0.0, 0.0), &mut st);
        }
        for x in -2..=1 {
            assert_eq!(v.get(x, 0, 0), (2, 1.0), "core full at x {x}");
        }
        for (x, want) in [(-1, 0.20), (0, 0.20), (1, 0.09)] {
            let (_, o) = v.get(x, 1, 0);
            assert!((o - want).abs() < 0.05, "y1 x{x}: {o} vs Studio {want}");
        }
        assert_eq!(v.get(-5, 0, 0).0, 0, "outside the brush: untouched");
        // Erode takes it back down.
        let sub = Params { subtract: true, ..p };
        let mut st = StrokeState::default();
        for _ in 0..200 {
            step(&mut v, &sub, Vec3::new(0.0, 4.0, 0.0), &mut st);
        }
        assert_eq!(v.get(0, 1, 0).0, 0, "eroded the grown top");
    }

    #[test]
    fn smooth_keeps_flat_ground_and_melts_a_bump() {
        let mut v = ground(0);
        v.set([0, 0, 0], 8, 1.0);
        let mut p = Params::new(BrushOp::Smooth);
        p.size = 12.0;
        let mut st = StrokeState::default();
        step(&mut v, &p, Vec3::new(2.0, 0.0, 2.0), &mut st);
        let (m, o) = v.get(0, 0, 0);
        assert!(m == 0 || o < 0.35, "bump melted: {o}");
        let (_, g) = v.get(3, -1, 3);
        assert!(g > 0.95, "flat ground stays: {g}");
        assert_eq!(v.get(3, 0, 3).0, 0, "nothing grows on flat ground");
    }

    #[test]
    fn flatten_cuts_to_the_plane() {
        // Ground to y = 0 and a 3-voxel-tall block on it.
        let mut v = ground(0);
        for x in -1..=1 {
            for z in -1..=1 {
                for y in 0..3 {
                    v.set([x, y, z], 8, 1.0);
                }
            }
        }
        let mut p = Params::new(BrushOp::Flatten);
        p.plane_y = 4.0;
        p.flatten = FlattenMode::Erode;
        let mut st = StrokeState::default();
        for _ in 0..200 {
            step(&mut v, &p, Vec3::new(2.0, 4.0, 2.0), &mut st);
        }
        assert_eq!(v.get(0, 2, 0).0, 0, "above the plane is cut");
        assert_eq!(v.get(0, 0, 0).1, 1.0, "below the plane stays");
        // Grow fills a dip up to the plane voxel's bottom.
        let mut v = ground(0);
        let mut p = Params::new(BrushOp::Flatten);
        p.plane_y = 9.0;
        p.flatten = FlattenMode::Grow;
        let mut st = StrokeState::default();
        for _ in 0..400 {
            step(&mut v, &p, Vec3::new(2.0, 9.0, 2.0), &mut st);
        }
        assert!(v.get(0, 0, 0).1 > 0.99 && v.get(0, 1, 0).1 > 0.99, "filled to y=8");
        assert_eq!(v.get(0, 2, 0).0, 0, "not past the plane voxel");
    }

    #[test]
    fn paint_and_replace() {
        let mut v = ground(0);
        let mut p = Params::new(BrushOp::Paint);
        p.material = 8;
        let mut st = StrokeState::default();
        step(&mut v, &p, Vec3::new(2.0, -2.0, 2.0), &mut st);
        assert_eq!(v.get(0, -1, 0).0, 8);
        assert_eq!(v.get(0, 0, 0).0, 0, "air stays air");
        assert_eq!(v.get(12, -1, 0).0, 2, "outside the brush");
        let mut r = Params::new(BrushOp::Paint);
        r.material = 8;
        r.target = Some(6);
        let mut st = StrokeState::default();
        step(&mut v, &r, Vec3::new(2.0, -2.0, 2.0), &mut st);
        assert_eq!(v.get(0, -1, 0).0, 6);
    }

    #[test]
    fn auto_material_takes_the_last_neighbour() {
        let b = Block { lo: [0; 3], n: [3, 3, 3], mat: vec![0; 27], occ: vec![0.0; 27] };
        assert_eq!(auto_material(&b, 1, 1, 1), None);
        let mut b = b;
        b.mat[0] = 8; // (0,0,0)
        let i = b.idx(2, 0, 0);
        b.mat[i] = 6; // dx = +1 comes last in the loop
        assert_eq!(auto_material(&b, 1, 1, 1), Some(6));
    }
}

