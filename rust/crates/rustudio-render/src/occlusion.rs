//! Conservative CPU software occlusion culling (matching Studio 0.748: buffer
//! sizing, OBB projection, front-to-back sort & raster dispatch, visibility
//! test).
//!
//! Rasterizes conservative inscribed boxes of large opaque solid parts into a
//! low-resolution reverse-depth (`1 / clip.w`) buffer with an 8x8 tile Hi-Z
//! hierarchy, then conservatively tests candidate world-space AABBs.
//!
//! Conservativeness invariants:
//! - Only strictly opaque, closed solid shapes contribute occluders.
//! - Each occluder quad is inner-conservatively rasterized (a coarse pixel is
//!   written only if the entire `[px, px+1] x [py, py+1]` cell lies inside the
//!   projected quad) using the farthest (smallest `1/w`) depth over the cell.
//! - Any occluder containing or too close to the camera is skipped.
//! - Each occludee AABB is outer-conservatively tested (ceil/floor expanded
//!   pixel bounds, nearest corner `max(1/w)` biased slightly toward the camera)
//!   and is never culled if any corner crosses behind the near plane.

use glam::{Mat4, Vec3, Vec4};

pub const WIDTH: usize = 96;
pub const HEIGHT: usize = 56;
const TILE: usize = 8;
const TILES_X: usize = WIDTH / TILE;
const TILES_Y: usize = HEIGHT / TILE;

/// Near-plane clip threshold for clipping occluder quads in clip space.
const NEAR_CLIP_W: f32 = 0.2;
/// Maximum number of top front-to-back occluders rasterized per frame.
const MAX_OCCLUDERS: usize = 24;
/// Maximum number of largest occluders retained per 128-stud spatial cell.
const MAX_PER_CELL: usize = 16;
/// Depth bias applied to occludee nearest `1/w` so coplanar decals/surfaces on
/// an occluder face never self-occlude.
const DEPTH_BIAS: f32 = 1.005;
/// Minimum screen-coverage score (`area_proxy / near_dist^2`) to rasterize an occluder.
const MIN_SCORE: f32 = 0.02;

/// Oriented solid box used as a conservative occluder.
#[derive(Clone, Copy, Debug)]
pub struct OccluderBox {
    /// Static instance slot (`u32::MAX` for dynamic items).
    pub slot: u32,
    pub center: Vec3,
    /// World-space half-extent vectors along the box's three orthogonal axes.
    pub axes: [Vec3; 3],
    /// World-space bounding sphere radius.
    pub radius: f32,
    /// Approximate surface-area proxy for ranking screen coverage.
    pub area_proxy: f32,
}

/// Coarse spatial cell of static occluders sorted by `area_proxy` descending,
/// allowing frustum rejection and O(1) early termination by distance.
#[derive(Clone, Copy, Debug)]
pub struct OccluderCell {
    pub lo: Vec3,
    pub hi: Vec3,
    pub start: u32,
    pub count: u32,
}

impl OccluderBox {
    /// Constructs an occluder box from a part's world transform columns,
    /// part size, and local-space inscribed box `[local_lo, local_hi]` in unit
    /// part coordinates (`[-0.5, 0.5]^3` for a full block).
    pub fn from_part(
        slot: u32,
        r0: [f32; 4],
        r1: [f32; 4],
        r2: [f32; 4],
        size: [f32; 3],
        local_lo: Vec3,
        local_hi: Vec3,
    ) -> Option<Self> {
        let local_center = (local_lo + local_hi) * 0.5;
        let local_half = (local_hi - local_lo) * 0.5;
        let sx = size[0].abs();
        let sy = size[1].abs();
        let sz = size[2].abs();
        let hx = local_half.x * sx;
        let hy = local_half.y * sy;
        let hz = local_half.z * sz;
        if !(hx.is_finite() && hy.is_finite() && hz.is_finite()) {
            return None;
        }
        let max_h = hx.max(hy).max(hz);
        let min_h = hx.min(hy).min(hz);
        // Keep only substantial walls, floors, and solid volumes as occluders.
        if max_h < 2.5 || min_h < 0.2 {
            return None;
        }
        let area_proxy = 4.0 * (hx * hy + hy * hz + hz * hx);
        if area_proxy < 64.0 {
            return None;
        }
        // `r0`, `r1`, `r2` store the rotation basis axes (`x_axis`, `y_axis`, `z_axis`)
        // in `[0..3]` and world translation `pos` in `[3]` (`render::to_raw`).
        let col0 = Vec3::new(r0[0], r0[1], r0[2]);
        let col1 = Vec3::new(r1[0], r1[1], r1[2]);
        let col2 = Vec3::new(r2[0], r2[1], r2[2]);
        let pos = Vec3::new(r0[3], r1[3], r2[3]);
        let center = pos
            + col0 * (local_center.x * sx)
            + col1 * (local_center.y * sy)
            + col2 * (local_center.z * sz);
        let axes = [col0 * hx, col1 * hy, col2 * hz];
        let radius = (axes[0].length_squared() + axes[1].length_squared() + axes[2].length_squared()).sqrt();
        Some(Self { slot, center, axes, radius, area_proxy })
    }

    pub fn aabb(&self) -> (Vec3, Vec3) {
        let ext = self.axes[0].abs() + self.axes[1].abs() + self.axes[2].abs();
        (self.center - ext, self.center + ext)
    }
}

/// Groups static occluders into coarse 128-stud spatial cells, sorted within
/// each cell by `area_proxy` descending for fast per-frame culling.
pub fn build_occluder_cells(occluders: &mut Vec<OccluderBox>, cell_size: f32) -> Vec<OccluderCell> {
    if occluders.is_empty() {
        return Vec::new();
    }
    let cell_key = |p: Vec3| -> [i32; 3] {
        let c = (p / cell_size).floor();
        [c.x as i32, c.y as i32, c.z as i32]
    };
    occluders.sort_unstable_by(|a, b| {
        cell_key(a.center)
            .cmp(&cell_key(b.center))
            .then_with(|| b.area_proxy.total_cmp(&a.area_proxy))
    });
    let mut cells: Vec<OccluderCell> = Vec::new();
    let mut kept: Vec<OccluderBox> = Vec::with_capacity(occluders.len().min(4096));
    let mut cur_key = [i32::MIN; 3];
    for occ in occluders.drain(..) {
        let k = cell_key(occ.center);
        let (lo, hi) = occ.aabb();
        match cells.last_mut() {
            Some(c) if cur_key == k => {
                if (c.count as usize) < MAX_PER_CELL {
                    c.count += 1;
                    c.lo = c.lo.min(lo);
                    c.hi = c.hi.max(hi);
                    kept.push(occ);
                }
            }
            _ => {
                cur_key = k;
                let start = kept.len() as u32;
                kept.push(occ);
                cells.push(OccluderCell { lo, hi, start, count: 1 });
            }
        }
    }
    *occluders = kept;
    cells
}

/// Result of testing a world-space AABB against the software occlusion buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Strictly behind rasterized opaque occluders on every covered pixel.
    Occluded,
    /// Strictly in front of all occluders in its screen region (or no occluders there).
    Visible,
    /// Overlaps some nearer occluder(s) or straddles the camera plane: batch is
    /// visible, but individual instances inside it may still be occluded.
    Partial,
}

/// Reusable per-frame software occlusion buffer.
pub struct OcclusionBuffer {
    depth: Vec<f32>,
    tile_min: Vec<f32>,
    tile_max: Vec<f32>,
    view_proj: Mat4,
    active: bool,
    candidates: Vec<(f32, f32, usize)>,
}

impl Default for OcclusionBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OcclusionBuffer {
    pub fn new() -> Self {
        Self {
            depth: vec![0.0; WIDTH * HEIGHT],
            tile_min: vec![0.0; TILES_X * TILES_Y],
            tile_max: vec![0.0; TILES_X * TILES_Y],
            view_proj: Mat4::IDENTITY,
            active: false,
            candidates: Vec::new(),
        }
    }

    /// Returns whether any occluders were rasterized this frame.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Clears the buffer and rasterizes up to `MAX_OCCLUDERS` front-to-back
    /// solid occluders visible from `cam`.
    #[allow(clippy::too_many_arguments)]
    pub fn build<'a>(
        &mut self,
        view_proj: Mat4,
        cam: Vec3,
        planes: &[Vec4; 5],
        draw_distance: f32,
        static_occluders: &'a [OccluderBox],
        static_cells: &[OccluderCell],
        static_alive: impl Fn(u32) -> bool,
        dynamic_occluders: &'a [OccluderBox],
    ) {
        self.active = false;
        self.view_proj = view_proj;
        self.candidates.clear();

        let n_static = static_occluders.len();
        let draw_dist_finite = draw_distance.is_finite();
        let mut push_cand = |idx: usize, occ: &OccluderBox| {
            let delta = occ.center - cam;
            let dist_sq = delta.length_squared();
            if draw_dist_finite {
                let max_d = draw_distance + occ.radius;
                if dist_sq > max_d * max_d {
                    return;
                }
            }
            if dist_sq > 4.0 * occ.radius * occ.radius && occ.area_proxy < 0.25 * MIN_SCORE * dist_sq {
                return;
            }
            for p in planes {
                if p.x * occ.center.x + p.y * occ.center.y + p.z * occ.center.z + p.w < -occ.radius {
                    return;
                }
            }
            let dist = dist_sq.sqrt();
            let near_dist = (dist - occ.radius).max(0.5);
            let score = occ.area_proxy / (near_dist * near_dist);
            if score < MIN_SCORE {
                return;
            }
            self.candidates.push((score, near_dist, idx));
        };

        if static_cells.is_empty() {
            for (i, occ) in static_occluders.iter().enumerate() {
                if static_alive(occ.slot) {
                    push_cand(i, occ);
                }
            }
        } else {
            for cell in static_cells {
                let visible = planes.iter().all(|p| {
                    let v = Vec3::new(
                        if p.x > 0.0 { cell.hi.x } else { cell.lo.x },
                        if p.y > 0.0 { cell.hi.y } else { cell.lo.y },
                        if p.z > 0.0 { cell.hi.z } else { cell.lo.z },
                    );
                    p.truncate().dot(v) + p.w >= 0.0
                });
                if !visible {
                    continue;
                }
                let nearest = cam.clamp(cell.lo, cell.hi);
                let cell_dist_sq = (nearest - cam).length_squared();
                if draw_dist_finite && cell_dist_sq > draw_distance * draw_distance {
                    continue;
                }
                // `cell.lo..cell.hi` encloses every occluder's AABB in this cell, so
                // every occluder's `near_dist >= cell_dist` (up to `max(0.5)`).
                let min_near_sq = cell_dist_sq.max(0.25);
                let min_area = MIN_SCORE * min_near_sq;
                let start = cell.start as usize;
                let end = start + cell.count as usize;
                for (i, occ) in static_occluders[start..end].iter().enumerate() {
                    if occ.area_proxy < min_area {
                        break;
                    }
                    if static_alive(occ.slot) {
                        push_cand(start + i, occ);
                    }
                }
            }
        }
        for (i, occ) in dynamic_occluders.iter().enumerate() {
            push_cand(n_static + i, occ);
        }

        if self.candidates.is_empty() {
            return;
        }

        // Keep top MAX_OCCLUDERS by screen-coverage proxy, then sort front-to-back.
        if self.candidates.len() > MAX_OCCLUDERS {
            self.candidates.select_nth_unstable_by(MAX_OCCLUDERS, |a, b| b.0.total_cmp(&a.0));
            self.candidates.truncate(MAX_OCCLUDERS);
        }
        self.candidates.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));

        self.depth.fill(0.0);
        let mut rasterized = 0usize;
        for ci in 0..self.candidates.len() {
            let idx = self.candidates[ci].2;
            let occ = if idx < n_static {
                &static_occluders[idx]
            } else {
                &dynamic_occluders[idx - n_static]
            };
            if self.rasterize_box(occ, cam) {
                rasterized += 1;
            }
        }
        if rasterized == 0 {
            return;
        }

        // Build 8x8 tile Hi-Z min (farthest) and max (nearest) depth.
        for ty in 0..TILES_Y {
            let y0 = ty * TILE;
            for tx in 0..TILES_X {
                let x0 = tx * TILE;
                let mut tmin = f32::INFINITY;
                let mut tmax = 0.0f32;
                for dy in 0..TILE {
                    let row = &self.depth[(y0 + dy) * WIDTH + x0..(y0 + dy) * WIDTH + x0 + TILE];
                    for &d in row {
                        if d < tmin {
                            tmin = d;
                        }
                        if d > tmax {
                            tmax = d;
                        }
                    }
                }
                let tidx = ty * TILES_X + tx;
                self.tile_min[tidx] = tmin;
                self.tile_max[tidx] = tmax;
            }
        }
        self.active = true;
    }

    /// Tests a world-space AABB `[lo, hi]` against the Hi-Z buffer using a
    /// single matrix multiply and incremental corner offsets.
    #[inline]
    pub fn test_aabb(&self, lo: Vec3, hi: Vec3) -> Visibility {
        if !self.active {
            return Visibility::Visible;
        }
        let vp = self.view_proj;
        let c0 = vp * lo.extend(1.0);
        let dx = vp.x_axis * (hi.x - lo.x);
        let dy = vp.y_axis * (hi.y - lo.y);
        let dz = vp.z_axis * (hi.z - lo.z);

        let min_w = c0.w + dx.w.min(0.0) + dy.w.min(0.0) + dz.w.min(0.0);
        if min_w <= NEAR_CLIP_W {
            return Visibility::Partial;
        }
        let max_w = c0.w + dx.w.max(0.0) + dy.w.max(0.0) + dz.w.max(0.0);
        let max_inv_w = 1.0 / min_w;
        let min_inv_w = 1.0 / max_w;

        let c1 = c0 + dx;
        let c2 = c0 + dy;
        let c3 = c1 + dy;
        let c4 = c0 + dz;
        let c5 = c1 + dz;
        let c6 = c2 + dz;
        let c7 = c3 + dz;

        let corners = [c0, c1, c2, c3, c4, c5, c6, c7];
        let mut ndc_min_x = f32::INFINITY;
        let mut ndc_max_x = f32::NEG_INFINITY;
        let mut ndc_min_y = f32::INFINITY;
        let mut ndc_max_y = f32::NEG_INFINITY;
        for c in corners {
            let iw = 1.0 / c.w;
            let nx = c.x * iw;
            let ny = c.y * iw;
            ndc_min_x = ndc_min_x.min(nx);
            ndc_max_x = ndc_max_x.max(nx);
            ndc_min_y = ndc_min_y.min(ny);
            ndc_max_y = ndc_max_y.max(ny);
        }

        if ndc_max_x < -1.0 || ndc_min_x > 1.0 || ndc_max_y < -1.0 || ndc_min_y > 1.0 {
            return Visibility::Visible;
        }

        let min_x = (ndc_min_x * 0.5 + 0.5) * WIDTH as f32;
        let max_x = (ndc_max_x * 0.5 + 0.5) * WIDTH as f32;
        let min_y = (0.5 - ndc_max_y * 0.5) * HEIGHT as f32;
        let max_y = (0.5 - ndc_min_y * 0.5) * HEIGHT as f32;

        // Outer-conservative pixel bounds.
        let px0 = (min_x.floor() as i32).clamp(0, WIDTH as i32 - 1) as usize;
        let px1 = (max_x.ceil() as i32).clamp(0, WIDTH as i32 - 1) as usize;
        let py0 = (min_y.floor() as i32).clamp(0, HEIGHT as i32 - 1) as usize;
        let py1 = (max_y.ceil() as i32).clamp(0, HEIGHT as i32 - 1) as usize;

        let tx0 = px0 / TILE;
        let tx1 = px1 / TILE;
        let ty0 = py0 / TILE;
        let ty1 = py1 / TILE;

        let biased_near = max_inv_w * DEPTH_BIAS;
        let mut all_tiles_occlude = true;
        let mut any_tile_nearer = false;

        for ty in ty0..=ty1 {
            let row = ty * TILES_X;
            for tx in tx0..=tx1 {
                let tidx = row + tx;
                if self.tile_min[tidx] <= biased_near {
                    all_tiles_occlude = false;
                }
                if self.tile_max[tidx] > min_inv_w {
                    any_tile_nearer = true;
                }
            }
        }

        if all_tiles_occlude {
            return Visibility::Occluded;
        }
        if !any_tile_nearer {
            return Visibility::Visible;
        }

        // Exact pixel refinement for compact screen rects.
        let area = (px1 - px0 + 1) * (py1 - py0 + 1);
        if area <= 32 {
            let mut occluded = true;
            'outer: for py in py0..=py1 {
                let row = &self.depth[py * WIDTH..py * WIDTH + WIDTH];
                for &d in &row[px0..=px1] {
                    if d <= biased_near {
                        occluded = false;
                        break 'outer;
                    }
                }
            }
            if occluded {
                return Visibility::Occluded;
            }
        }
        Visibility::Partial
    }

    /// Convenience predicate for single-instance occlusion tests.
    #[inline]
    pub fn is_occluded(&self, lo: Vec3, hi: Vec3) -> bool {
        self.test_aabb(lo, hi) == Visibility::Occluded
    }

    /// Rasterizes the 1..=3 front-facing faces of an OBB into `self.depth`.
    fn rasterize_box(&mut self, occ: &OccluderBox, cam: Vec3) -> bool {
        let d = cam - occ.center;
        let mut inside = true;
        for a in &occ.axes {
            let len_sq = a.length_squared();
            let len = len_sq.sqrt();
            if d.dot(*a).abs() > len_sq + 0.5 * len {
                inside = false;
                break;
            }
        }
        if inside {
            return false;
        }

        let vp = self.view_proj;
        let cc = vp * occ.center.extend(1.0);
        let cx = vp * occ.axes[0].extend(0.0);
        let cy = vp * occ.axes[1].extend(0.0);
        let cz = vp * occ.axes[2].extend(0.0);

        let c = [
            cc - cx - cy - cz,
            cc + cx - cy - cz,
            cc - cx + cy - cz,
            cc + cx + cy - cz,
            cc - cx - cy + cz,
            cc + cx - cy + cz,
            cc - cx + cy + cz,
            cc + cx + cy + cz,
        ];

        const FACES: [(usize, f32, [usize; 4]); 6] = [
            (0, 1.0, [1, 3, 7, 5]),  // +X
            (0, -1.0, [0, 4, 6, 2]), // -X
            (1, 1.0, [2, 6, 7, 3]),  // +Y
            (1, -1.0, [0, 1, 5, 4]), // -Y
            (2, 1.0, [4, 5, 7, 6]),  // +Z
            (2, -1.0, [0, 2, 3, 1]), // -Z
        ];

        let mut drew = false;
        for &(axis_i, sign, idx) in &FACES {
            let face_axis = occ.axes[axis_i] * sign;
            let face_center = occ.center + face_axis;
            if (cam - face_center).dot(face_axis) <= 0.0 {
                continue;
            }
            let quad = [c[idx[0]], c[idx[1]], c[idx[2]], c[idx[3]]];
            if self.rasterize_clip_quad(quad) {
                drew = true;
            }
        }
        drew
    }

    /// Clips a convex quad against `clip.w >= NEAR_CLIP_W` and rasterizes it
    /// inner-conservatively into `self.depth`.
    fn rasterize_clip_quad(&mut self, quad: [Vec4; 4]) -> bool {
        let mut clipped = [Vec4::ZERO; 6];
        let mut n = 0usize;
        for i in 0..4 {
            let a = quad[i];
            let b = quad[(i + 1) & 3];
            let a_in = a.w >= NEAR_CLIP_W;
            let b_in = b.w >= NEAR_CLIP_W;
            if a_in {
                clipped[n] = a;
                n += 1;
            }
            if a_in != b_in {
                let t = (NEAR_CLIP_W - a.w) / (b.w - a.w);
                clipped[n] = a + (b - a) * t;
                n += 1;
            }
        }
        if n < 3 {
            return false;
        }

        let mut sx = [0.0f32; 6];
        let mut sy = [0.0f32; 6];
        let mut iw = [0.0f32; 6];
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut min_iw = f32::INFINITY;
        let mut max_iw = 0.0f32;

        for i in 0..n {
            let inv_w = 1.0 / clipped[i].w;
            let x = (clipped[i].x * inv_w * 0.5 + 0.5) * WIDTH as f32;
            let y = (0.5 - clipped[i].y * inv_w * 0.5) * HEIGHT as f32;
            sx[i] = x;
            sy[i] = y;
            iw[i] = inv_w;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            min_iw = min_iw.min(inv_w);
            max_iw = max_iw.max(inv_w);
        }

        if max_x <= 0.0 || min_x >= WIDTH as f32 || max_y <= 0.0 || min_y >= HEIGHT as f32 {
            return false;
        }

        let mut area2 = 0.0f32;
        for i in 0..n {
            let j = (i + 1) % n;
            area2 += sx[i] * sy[j] - sx[j] * sy[i];
        }
        if area2.abs() < 2.0 {
            return false;
        }
        if area2 < 0.0 {
            sx[..n].reverse();
            sy[..n].reverse();
            iw[..n].reverse();
        }

        let mut best_k = 1usize;
        let mut best_tri = 0.0f32;
        for k in 1..n - 1 {
            let cross = (sx[k] - sx[0]) * (sy[k + 1] - sy[0])
                - (sy[k] - sy[0]) * (sx[k + 1] - sx[0]);
            if cross.abs() > best_tri {
                best_tri = cross.abs();
                best_k = k;
            }
        }
        if best_tri < 1e-4 {
            return false;
        }
        let (x0, y0, z0) = (sx[0], sy[0], iw[0]);
        let (dx1, dy1, dz1) = (sx[best_k] - x0, sy[best_k] - y0, iw[best_k] - z0);
        let (dx2, dy2, dz2) = (sx[best_k + 1] - x0, sy[best_k + 1] - y0, iw[best_k + 1] - z0);
        let inv_det = 1.0 / (dx1 * dy2 - dx2 * dy1);
        let gx = (dz1 * dy2 - dz2 * dy1) * inv_det;
        let gy = (dx1 * dz2 - dx2 * dz1) * inv_det;
        let z_cell_pad = 0.5 * (gx.abs() + gy.abs());
        let z_base = z0 - gx * x0 - gy * y0 - z_cell_pad;

        let mut ea = [0.0f32; 6];
        let mut eb = [0.0f32; 6];
        let mut ec = [0.0f32; 6];
        for i in 0..n {
            let j = (i + 1) % n;
            let a = -(sy[j] - sy[i]);
            let b = sx[j] - sx[i];
            let c = -a * sx[i] - b * sy[i];
            ea[i] = a;
            eb[i] = b;
            ec[i] = c - 0.5 * (a.abs() + b.abs());
        }

        let py0 = (min_y.floor() as i32).max(0) as usize;
        let py1 = (max_y.ceil() as i32).min(HEIGHT as i32 - 1) as usize;
        if py0 > py1 {
            return false;
        }

        let mut wrote_any = false;
        for py in py0..=py1 {
            let yc = py as f32 + 0.5;
            let mut x_lo = 0.0f32;
            let mut x_hi = (WIDTH - 1) as f32;
            let mut valid = true;
            for i in 0..n {
                let rhs = eb[i] * yc + ec[i];
                let a = ea[i];
                if a.abs() < 1e-7 {
                    if rhs < 0.0 {
                        valid = false;
                        break;
                    }
                } else {
                    let bound = -rhs / a - 0.5;
                    if a > 0.0 {
                        x_lo = x_lo.max(bound);
                    } else {
                        x_hi = x_hi.min(bound);
                    }
                }
            }
            if !valid {
                continue;
            }
            let px0 = x_lo.ceil().max(0.0) as i32;
            let px1 = x_hi.floor().min((WIDTH - 1) as f32) as i32;
            if px0 > px1 {
                continue;
            }
            let px0 = px0 as usize;
            let px1 = px1 as usize;
            wrote_any = true;
            let row_offset = py * WIDTH;
            let row = &mut self.depth[row_offset..row_offset + WIDTH];
            let mut z = z_base + gy * yc + gx * (px0 as f32 + 0.5);
            for dst in &mut row[px0..=px1] {
                let zc = z.clamp(min_iw, max_iw);
                if zc > *dst {
                    *dst = zc;
                }
                z += gx;
            }
        }
        wrote_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_view(cam: Vec3, dir: Vec3) -> (Mat4, [Vec4; 5]) {
        let proj = Mat4::perspective_rh(70.0_f32.to_radians(), 16.0 / 9.0, 0.1, 10_000.0);
        let view = Mat4::look_to_rh(cam, dir, Vec3::Y);
        let vp = proj * view;
        let (r0, r1, r2, r3) = (vp.row(0), vp.row(1), vp.row(2), vp.row(3));
        let n = |p: Vec4| p / p.truncate().length().max(1e-6);
        (vp, [n(r3 + r0), n(r3 - r0), n(r3 + r1), n(r3 - r1), n(r2)])
    }

    fn axis_box(pos: Vec3, size: Vec3) -> OccluderBox {
        OccluderBox::from_part(
            0,
            [1.0, 0.0, 0.0, pos.x],
            [0.0, 1.0, 0.0, pos.y],
            [0.0, 0.0, 1.0, pos.z],
            size.to_array(),
            Vec3::splat(-0.5),
            Vec3::splat(0.5),
        )
        .unwrap()
    }

    #[test]
    fn large_wall_occludes_parts_behind_it_and_not_in_front() {
        let cam = Vec3::ZERO;
        let (vp, planes) = test_view(cam, -Vec3::Z);
        let mut walls = vec![axis_box(Vec3::new(0.0, 0.0, -25.0), Vec3::new(100.0, 100.0, 4.0))];
        let cells = build_occluder_cells(&mut walls, 128.0);
        let mut buf = OcclusionBuffer::new();
        buf.build(vp, cam, &planes, 1600.0, &walls, &cells, |_| true, &[]);
        assert!(buf.is_active());

        assert_eq!(
            buf.test_aabb(Vec3::new(-5.0, -5.0, -60.0), Vec3::new(5.0, 5.0, -50.0)),
            Visibility::Occluded
        );
        assert_eq!(
            buf.test_aabb(Vec3::new(-2.0, -2.0, -15.0), Vec3::new(2.0, 2.0, -10.0)),
            Visibility::Visible
        );
        assert_ne!(
            buf.test_aabb(Vec3::new(-4.0, -4.0, -23.01), Vec3::new(4.0, 4.0, -22.99)),
            Visibility::Occluded
        );
        assert_eq!(
            buf.test_aabb(Vec3::new(-50.0, -50.0, -100.0), Vec3::new(50.0, 50.0, 20.0)),
            Visibility::Partial
        );
    }

    #[test]
    fn peeking_object_next_to_wall_is_never_falsely_occluded() {
        let cam = Vec3::ZERO;
        let (vp, planes) = test_view(cam, -Vec3::Z);
        let mut walls = vec![axis_box(Vec3::new(-10.0, 0.0, -25.0), Vec3::new(20.0, 40.0, 2.0))];
        let cells = build_occluder_cells(&mut walls, 128.0);
        let mut buf = OcclusionBuffer::new();
        buf.build(vp, cam, &planes, 1600.0, &walls, &cells, |_| true, &[]);

        assert_ne!(
            buf.test_aabb(Vec3::new(1.0, -2.0, -50.0), Vec3::new(5.0, 2.0, -45.0)),
            Visibility::Occluded
        );
        assert_ne!(
            buf.test_aabb(Vec3::new(-5.0, -2.0, -50.0), Vec3::new(5.0, 2.0, -45.0)),
            Visibility::Occluded
        );
        assert_eq!(
            buf.test_aabb(Vec3::new(-15.0, -2.0, -50.0), Vec3::new(-5.0, 2.0, -45.0)),
            Visibility::Occluded
        );
    }

    #[test]
    fn camera_inside_occluder_does_not_occlude_anything() {
        let cam = Vec3::ZERO;
        let (vp, planes) = test_view(cam, -Vec3::Z);
        let mut boxes = vec![axis_box(Vec3::ZERO, Vec3::splat(20.0))];
        let cells = build_occluder_cells(&mut boxes, 128.0);
        let mut buf = OcclusionBuffer::new();
        buf.build(vp, cam, &planes, 1600.0, &boxes, &cells, |_| true, &[]);
        assert!(!buf.is_active());
        assert_eq!(
            buf.test_aabb(Vec3::new(-5.0, -5.0, -60.0), Vec3::new(5.0, 5.0, -50.0)),
            Visibility::Visible
        );
    }
}
