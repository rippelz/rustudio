//! Studio's scene environment probe: the indoor reflection cube that
//! DefaultUnifiedTiledFS samples as PrefilteredEnvIndoorTexture and blends
//! toward PrefilteredEnvBlendTargetTexture by CB0[32].w.
//!
//! Every step is ported from the Studio binary (0.744) and checked
//! against RenderDoc captures of the Laser Tag template
//! (checks/laser-tag-parity/render/findings.md):
//!
//! * Region (Studio tracker, Studio trigger, Studio step,
//!   Studio ray, Studio filter, Studio commit, Studio
//!   sight check): 32 rays on a Fibonacci sphere from the camera, one a
//!   frame, 256 studs; at least 85 % must hit; hits further than 1.5 x IQR
//!   outside the 25th..75th distance percentiles are dropped; the box of the
//!   rest and the seed, merged with the current box when that's smaller.
//! * Capture (Studio faces, Studio FastConvolution): the box
//!   centre, one 90-degree face a frame, then the convolution of mips 1 and
//!   2 (one face a frame for mip 1, two for mip 2). Mips 3+ keep the default
//!   indoor cube both slots start with.
//! * Blend: CB0[32].w climbs by min(0.05 x turn + 0.05 x
//!   move, 0.2) + 0.05 x dt a frame; at 1 the slots swap.

use glam::Vec3;

// The probe frame's types and sizes are render's (`env_probe_gpu`): the
// renderer consumes them, so they cannot point up at this module.
use rustudio_render::render::env_probe_gpu::{capture_tasks, ProbeFrame, Task};

/// FInt Studio: rays per region pass.
pub const RAYS: usize = 32;
/// FInt Studio: ray length, studs.
pub const RAY_LENGTH: f32 = 256.0;
/// FInt Studio: percent of the rays that must hit (Studio = 100).
pub const MIN_HIT_PERCENT: f32 = 85.0;
/// Studio: distance quantiles; Studio: fence.
pub const Q_LO: f64 = 0.25;
pub const Q_HI: f64 = 0.75;
pub const FENCE: f32 = 1.5;
/// Studio: the golden angle stepping the ray directions.
pub const GOLDEN: f32 = 2.39996;
/// FInt Studio: studs the camera moves before a new region pass.
pub const MOVE_STUDS: f32 = 8.0;
/// FInt Studio: seconds between passes while the camera is outside the box.
pub const RETRY_SECONDS: f64 = 5.0;
/// Blend rates (Studio x FInt Studio).
pub const BLEND_MOTION: f32 = 0.05;
pub const BLEND_MOTION_MAX: f32 = 0.2;
pub const BLEND_PER_SECOND: f32 = 0.01 * 5.0;
/// Studio: the world-revision check runs every 15th frame.
pub const REVISION_FRAMES: u64 = 15;

/// The ray directions: y from 1 to -1, sin / cos (Studio
/// = sinf, Studio = cosf) of i x the golden angle around it.
pub fn directions() -> [Vec3; RAYS] {
    let mut out = [Vec3::ZERO; RAYS];
    for (i, d) in out.iter_mut().enumerate() {
        let y = 1.0 - 2.0 * (i as f32 / (RAYS - 1) as f32);
        let r = (1.0 - y * y).max(0.0).sqrt();
        let a = i as f32 * GOLDEN;
        *d = Vec3::new(a.sin() * r, y, a.cos() * r);
    }
    out
}

#[derive(Default)]
struct Region {
    /// Rays cast so far this pass (tracker +4).
    count: usize,
    /// Where the rays start (tracker): the camera when the pass began.
    seed: Vec3,
    /// The seed of the last finished pass (tracker).
    last_seed: Option<Vec3>,
    /// When the last pass finished (tracker +8).
    last_time: f64,
    hits: Vec<Vec3>,
    /// 2 in the tracker: a new box waits for the probe (tracker..0x84).
    ready: Option<(Vec3, Vec3)>,
}

pub struct EnvProbe {
    dirs: [Vec3; RAYS],
    region: Region,
    /// The probe's box (probe min, max); inverted until the first pass.
    min: Vec3,
    max: Vec3,
    tasks: std::collections::VecDeque<Task>,
    /// A capture finished: CB0[32].w climbs (probe,).
    blending: Option<f32>,
    last_cam: Option<(Vec3, Vec3)>,
    frame: u64,
    revision: Option<u64>,
}

impl Default for EnvProbe {
    fn default() -> Self {
        EnvProbe {
            dirs: directions(),
            region: Region::default(),
            min: Vec3::splat(f32::MAX),
            max: Vec3::splat(-f32::MAX),
            tasks: Default::default(),
            blending: None,
            last_cam: None,
            frame: 0,
            revision: None,
        }
    }
}

/// Studio: drop hits whose distance from the seed falls outside the
/// quartile fences.
pub fn filter(hits: &[Vec3], seed: Vec3) -> Vec<Vec3> {
    let mut d: Vec<f32> = hits.iter().map(|h| h.distance(seed)).collect();
    d.sort_by(|a, b| a.total_cmp(b));
    let n = d.len();
    if n == 0 {
        return Vec::new();
    }
    let lo = d[((n as f64 * Q_LO) as usize).min(n - 1)];
    let hi = d[((n as f64 * Q_HI) as usize).min(n - 1)];
    let fence = (hi - lo) * FENCE;
    hits.iter().copied().filter(|h| {
        let x = h.distance(seed);
        lo - fence <= x && x <= fence + hi
    }).collect()
}

fn volume(min: Vec3, max: Vec3) -> f32 {
    let e = max - min;
    e.x * e.y * e.z
}

impl EnvProbe {
    pub fn centre(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn has_box(&self) -> bool {
        self.min.x <= self.max.x
    }

    /// One frame. `ray(origin, dir, len)` is the world ray (Studio's raycast
    /// at Studio): the distance to the first part hit. `revision` is
    /// the scene's content generation (Studio's world revision).
    pub fn update(
        &mut self,
        cam: Vec3,
        cam_dir: Vec3,
        dt: f32,
        now: f64,
        revision: u64,
        ray: &dyn Fn(Vec3, Vec3, f32) -> Option<f32>,
    ) -> ProbeFrame {
        self.frame += 1;
        let mut out = ProbeFrame { centre: self.centre(), ..Default::default() };

        // Studio: start (or continue) a region pass.
        if self.region.ready.is_none() && !self.dirs.is_empty() {
            let start = if self.region.count == 0 && self.region.hits.is_empty() {
                let moved = self.region.last_seed.map_or(f32::MAX, |s| s.distance(cam));
                let inside = cam.cmpge(self.min).all() && cam.cmple(self.max).all();
                !(moved < MOVE_STUDS && (now - self.region.last_time < RETRY_SECONDS || inside))
            } else {
                true
            };
            if start {
                self.region_step(cam, now, ray);
            }
        }
        // Studio: a new box starts a capture.
        if let Some((min, max)) = self.region.ready {
            if self.tasks.is_empty() && self.blending.is_none() {
                self.min = min;
                self.max = max;
                self.tasks.extend(capture_tasks());
                out.centre = self.centre();
            }
        }
        // Studio: every 15th frame, a newer world revision recaptures.
        if self.frame % REVISION_FRAMES == 0 {
            let newer = self.revision.is_some_and(|r| revision > r);
            if self.revision.is_none() || newer {
                self.revision = Some(revision);
            }
            if newer && self.has_box() && self.tasks.is_empty() && self.blending.is_none() {
                self.tasks.extend(capture_tasks());
            }
        }
        // One task a frame (the queue runner in Studio).
        if let Some(t) = self.tasks.pop_front() {
            out.tasks.push(t);
            if self.tasks.is_empty() {
                self.blending = Some(0.0);
            }
        } else if let Some(t) = self.blending {
            // Studio: turn (acos of the view directions' dot) and move
            // speed the blend up, at most 0.2 a frame, plus 0.05 a second.
            let (pp, pd) = self.last_cam.unwrap_or((cam, cam_dir));
            let turn = cam_dir.dot(pd).min(1.0).acos();
            let motion = (turn * BLEND_MOTION + cam.distance(pp) * BLEND_MOTION).min(BLEND_MOTION_MAX);
            let t = (t + motion + dt * BLEND_PER_SECOND).min(1.0);
            if t < 1.0 {
                self.blending = Some(t);
                out.blend = t;
            } else {
                self.blending = None;
                out.swap = true;
                // The tracker is released once the blend completes.
                if self.region.ready.take().is_some() {
                    self.region.count = 0;
                    self.region.hits.clear();
                }
            }
        }
        self.last_cam = Some((cam, cam_dir));
        out.centre = self.centre();
        out
    }

    /// Studio: one ray a call; the call after the last finishes the pass.
    fn region_step(&mut self, cam: Vec3, now: f64, ray: &dyn Fn(Vec3, Vec3, f32) -> Option<f32>) {
        let r = &mut self.region;
        if r.count < RAYS {
            if r.hits.is_empty() {
                r.seed = cam;
            }
            // Studio: the direction (normalised) times the ray length.
            let d = self.dirs[r.count].normalize_or_zero();
            if let Some(t) = ray(r.seed, d, RAY_LENGTH) {
                r.hits.push(r.seed + d * t);
            }
            r.count += 1;
            return;
        }
        r.last_time = now;
        let seed = r.seed;
        let mut committed = None;
        if (MIN_HIT_PERCENT * RAYS as f32) / 100.0 <= r.hits.len() as f32 {
            let kept = filter(&r.hits, seed);
            if !kept.is_empty() {
                let (mut lo, mut hi) = (kept[0], kept[0]);
                for p in &kept[1..] {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
                lo = lo.min(seed);
                hi = hi.max(seed);
                committed = commit((self.min, self.max), (lo, hi), seed, ray);
            }
        }
        r.last_seed = Some(seed);
        match committed {
            Some(b) => r.ready = Some(b),
            None => {
                r.count = 0;
                r.hits.clear();
            }
        }
    }
}

/// Studio: the box centre must be in sight of the seed.
fn in_sight(seed: Vec3, at: Vec3, ray: &dyn Fn(Vec3, Vec3, f32) -> Option<f32>) -> bool {
    let d = at - seed;
    let len = d.length();
    len < 1e-6 || ray(seed, d / len, len).is_none()
}

/// Studio: nothing new inside the current box; else the union when
/// it's smaller than both boxes together, else the new box.
fn commit(
    cur: (Vec3, Vec3),
    new: (Vec3, Vec3),
    seed: Vec3,
    ray: &dyn Fn(Vec3, Vec3, f32) -> Option<f32>,
) -> Option<(Vec3, Vec3)> {
    if cur.0.cmple(new.0).all() && new.1.cmple(cur.1).all() {
        return None;
    }
    if !in_sight(seed, (new.0 + new.1) * 0.5, ray) {
        return None;
    }
    let u = (cur.0.min(new.0), cur.1.max(new.1));
    if volume(u.0, u.1) < volume(cur.0, cur.1) + volume(new.0, new.1) && in_sight(seed, (u.0 + u.1) * 0.5, ray) {
        return Some(u);
    }
    Some(new)
}

/// Studio's world ray for the probe (Studio, the region's 0x700
/// filter) over the editor scene, with the runtime's ray rules
/// (runtime_ext.rs cast_swept, plain rays): CanQuery only counts once
/// CanCollide is off; stored collision pieces, then the mesh, then the box
/// for MeshParts and unions; exact balls, cylinders and wedges.
pub fn world_ray(d: &rustudio_core::doc::Document, scene: &rustudio_render::scene::Scene, o: Vec3, dir: Vec3, len: f32) -> Option<f32> {
    use rbx_dom_weak::types::Variant;
    let ray = rustudio_core::math::Ray { origin: o, dir };
    let mut best: Option<f32> = None;
    for (sp, b) in scene.parts.iter().zip(&scene.bounds) {
        let oc = b.truncate() - o;
        let along = oc.dot(dir);
        if oc.length_squared() - along * along > b.w * b.w || along < -b.w || along - b.w > best.unwrap_or(len) {
            continue;
        }
        let collide = !matches!(d.prop(sp.r, "CanCollide"), Some(Variant::Bool(false)));
        let query = !matches!(d.prop(sp.r, "CanQuery"), Some(Variant::Bool(false)));
        if !query && !collide {
            continue;
        }
        let Some(class) = d.class_of(sp.r) else { continue };
        let Some(xf) = rustudio_render::scene::xform_of(d, sp.r) else { continue };
        let size = sp.size;
        let mesh_class = class == "MeshPart" || rustudio_core::doc::is_a(&class, "PartOperation");
        let t = if mesh_class {
            let fidelity = rustudio_physics::physics_mesh::fidelity(d, sp.r);
            let pieces = match rustudio_physics::physics_mesh::of_part_id(d, sp.r) {
                Some((rustudio_physics::physics_mesh::Geometry::Pieces(_), id)) if fidelity != Some(rustudio_physics::physics_mesh::FIDELITY_BOX) => Some(id),
                _ => None,
            };
            if let Some(id) = pieces {
                let initial = rustudio_render::scene::vec3_of(d.prop(sp.r, "InitialSize"))
                    .or_else(|| rustudio_render::scene::vec3_of(d.prop(sp.r, "MeshSize")))
                    .filter(|v| v.max_element() > 0.0)
                    .unwrap_or(size);
                let scale = rustudio_physics::physics_mesh::scale(initial, size);
                if ray.hit_obb(&xf, size).is_none() {
                    continue;
                }
                rustudio_physics::physics_mesh::pieces(id).and_then(|p| rustudio_physics::physics_mesh::ray(&p, &xf, scale, o, dir, len)).map(|h| h.0)
            } else {
                let id = if class == "MeshPart" {
                    rustudio_render::scene::content_string(d.prop(sp.r, "MeshId"))
                } else {
                    rustudio_core::mesh::union_mesh(d, sp.r).map(|m| m.id)
                };
                let gxf = match &id {
                    Some(id) => rustudio_core::math::Xform { pos: xf.pos + xf.rot * (rustudio_core::mesh_bvh::union_offset(id) * size), rot: xf.rot },
                    None => xf,
                };
                if fidelity == Some(rustudio_physics::physics_mesh::FIDELITY_BOX) {
                    ray.enter_obb(&gxf, size)
                } else {
                    if ray.hit_obb(&gxf, size).is_none() {
                        continue;
                    }
                    match id.as_deref().filter(|i| !i.is_empty()).and_then(rustudio_core::mesh_bvh::for_mesh) {
                        Some(bvh) => {
                            let inv = gxf.rot.transpose();
                            let s = size.max(Vec3::splat(1e-4));
                            bvh.ray_normal((inv * (o - gxf.pos)) / s, (inv * dir) / s).map(|h| h.0)
                        }
                        None => ray.enter_obb(&gxf, size),
                    }
                }
            }
        } else {
            let kind = rustudio_physics::query_shapes::kind_of(d, sp.r, &class);
            if kind == rustudio_physics::query_shapes::BOX {
                ray.enter_obb(&xf, size)
            } else {
                rustudio_physics::query_shapes::ray_part(&xf, size, kind, o, dir).map(|h| h.0)
            }
        };
        if let Some(t) = t.filter(|t| *t >= 0.0 && *t <= len) {
            if best.is_none_or(|b| t < b) {
                best = Some(t);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_directions_are_unit_and_span_the_sphere() {
        let d = directions();
        assert!((d[0] - Vec3::Y).length() < 1e-6);
        assert!((d[RAYS - 1] + Vec3::Y).length() < 1e-5);
        assert!(d.iter().all(|v| (v.length() - 1.0).abs() < 1e-5));
        // i = 1: y = 1 - 2/31, angle 2.39996.
        let y = 1.0 - 2.0 / 31.0;
        let r = (1.0f32 - y * y).sqrt();
        assert!((d[1] - Vec3::new(2.39996f32.sin() * r, y, 2.39996f32.cos() * r)).length() < 1e-6);
    }

    #[test]
    fn quartile_fences_drop_far_hits() {
        let mut hits: Vec<Vec3> = (0..30).map(|i| Vec3::new(10.0 + i as f32 * 0.1, 0.0, 0.0)).collect();
        hits.push(Vec3::new(250.0, 0.0, 0.0));
        let kept = filter(&hits, Vec3::ZERO);
        assert_eq!(kept.len(), 30);
    }

    #[test]
    fn room_detection_boxes_a_closed_room() {
        // A 40 x 20 x 30 room around the origin: every ray hits a wall.
        let ray = |o: Vec3, d: Vec3, len: f32| -> Option<f32> {
            let half = Vec3::new(20.0, 10.0, 15.0);
            let mut best = f32::MAX;
            for k in 0..3 {
                if d[k].abs() > 1e-6 {
                    let t = ((half[k] * d[k].signum()) - o[k]) / d[k];
                    if t > 0.0 { best = best.min(t); }
                }
            }
            (best <= len).then_some(best)
        };
        let mut p = EnvProbe::default();
        let mut frame = ProbeFrame::default();
        for i in 0..40 {
            frame = p.update(Vec3::new(1.0, 2.0, 3.0), Vec3::NEG_Z, 1.0 / 60.0, i as f64 / 60.0, 0, &ray);
            if !frame.tasks.is_empty() { break; }
        }
        assert_eq!(frame.tasks, vec![Task::Face(0)]);
        assert!(p.has_box());
        assert!(p.min.cmple(Vec3::new(-15.0, -9.0, -12.0)).all() && p.max.cmpge(Vec3::new(15.0, 9.0, 12.0)).all(), "{:?} {:?}", p.min, p.max);
    }

    #[test]
    fn capture_then_blend_then_swap() {
        let ray = |_: Vec3, _: Vec3, len: f32| -> Option<f32> { (5.0 <= len).then_some(5.0) };
        let mut p = EnvProbe::default();
        let mut tasks = Vec::new();
        let mut swapped = false;
        for i in 0..2000 {
            let f = p.update(Vec3::ZERO, Vec3::NEG_Z, 1.0 / 60.0, i as f64 / 60.0, 0, &ray);
            tasks.extend(f.tasks);
            if f.swap { swapped = true; break; }
        }
        assert_eq!(tasks, capture_tasks());
        assert!(swapped);
    }
}
