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
/// Probe cube: 128 px for PBR, 90-degree faces, far 3000
///.
pub const SIZE: u32 = 128;
pub const FAR: f32 = 3000.0;
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

/// One GPU step of a capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    /// Draw cube face `0..6` of the scene at `centre` into the next slot.
    Face(u32),
    /// Build the FastConvDownsample source pyramid from the captured faces.
    Pyramid,
    /// FastConvolution of mip `mip` (1 or 2), faces `first..first+count`.
    Convolve { mip: u32, first: u32, count: u32 },
}

/// The capture task list, one entry a frame: six faces, then the
/// convolution (Studio: one face a call for mip 1, two for mips 2..3).
pub fn capture_tasks() -> Vec<Task> {
    let mut t: Vec<Task> = (0..6).map(Task::Face).collect();
    t.push(Task::Pyramid);
    t.extend((0..6).map(|f| Task::Convolve { mip: 1, first: f, count: 1 }));
    t.extend((0..3).map(|f| Task::Convolve { mip: 2, first: f * 2, count: 2 }));
    t
}

/// What the renderer does with the probe this frame.
#[derive(Clone, Debug, Default)]
pub struct ProbeFrame {
    /// The capture position (centre of the probe box).
    pub centre: Vec3,
    /// GPU work this frame, in order.
    pub tasks: Vec<Task>,
    /// CB0[32].w: how far the shader blends from the current slot to the next.
    pub blend: f32,
    /// The next slot becomes the current one before drawing.
    pub swap: bool,
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
pub fn world_ray(d: &crate::doc::Document, scene: &crate::scene::Scene, o: Vec3, dir: Vec3, len: f32) -> Option<f32> {
    use rbx_dom_weak::types::Variant;
    let ray = crate::math::Ray { origin: o, dir };
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
        let Some(xf) = crate::scene::xform_of(d, sp.r) else { continue };
        let size = sp.size;
        let mesh_class = class == "MeshPart" || crate::doc::is_a(&class, "PartOperation");
        let t = if mesh_class {
            let fidelity = crate::physics_mesh::fidelity(d, sp.r);
            let pieces = match crate::physics_mesh::of_part_id(d, sp.r) {
                Some((crate::physics_mesh::Geometry::Pieces(_), id)) if fidelity != Some(crate::physics_mesh::FIDELITY_BOX) => Some(id),
                _ => None,
            };
            if let Some(id) = pieces {
                let initial = crate::scene::vec3_of(d.prop(sp.r, "InitialSize"))
                    .or_else(|| crate::scene::vec3_of(d.prop(sp.r, "MeshSize")))
                    .filter(|v| v.max_element() > 0.0)
                    .unwrap_or(size);
                let scale = crate::physics_mesh::scale(initial, size);
                if ray.hit_obb(&xf, size).is_none() {
                    continue;
                }
                crate::physics_mesh::pieces(id).and_then(|p| crate::physics_mesh::ray(&p, &xf, scale, o, dir, len)).map(|h| h.0)
            } else {
                let id = if class == "MeshPart" {
                    crate::scene::content_string(d.prop(sp.r, "MeshId"))
                } else {
                    crate::scene::union_mesh(d, sp.r).map(|m| m.id)
                };
                let gxf = match &id {
                    Some(id) => crate::math::Xform { pos: xf.pos + xf.rot * (crate::mesh_bvh::union_offset(id) * size), rot: xf.rot },
                    None => xf,
                };
                if fidelity == Some(crate::physics_mesh::FIDELITY_BOX) {
                    ray.enter_obb(&gxf, size)
                } else {
                    if ray.hit_obb(&gxf, size).is_none() {
                        continue;
                    }
                    match id.as_deref().filter(|i| !i.is_empty()).and_then(crate::mesh_bvh::for_mesh) {
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
            let kind = crate::query_shapes::kind_of(d, sp.r, &class);
            if kind == crate::query_shapes::BOX {
                ray.enter_obb(&xf, size)
            } else {
                crate::query_shapes::ray_part(&xf, size, kind, o, dir).map(|h| h.0)
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

/// The face's view basis (forward, up), cube order +X, -X, +Y, -Y, +Z, -Z
/// (Studio; D3D / wgpu cube faces).
pub fn face_basis(face: u32) -> (Vec3, Vec3) {
    match face {
        0 => (Vec3::X, Vec3::Y),
        1 => (-Vec3::X, Vec3::Y),
        2 => (Vec3::Y, -Vec3::Z),
        3 => (-Vec3::Y, Vec3::Z),
        4 => (Vec3::Z, Vec3::Y),
        _ => (-Vec3::Z, Vec3::Y),
    }
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

/// FastConvolutionPS CB4: the 960 bytes Studio uploads (copied from
/// .rdata Studio..Studio; checks/laser-tag-parity/render/
/// native-traced/convolution-cb4.json). Per level (mip 1, mip 2): 30 vec4,
/// tangent / bitangent / normal weights, source lod, tap weight (6 each).
pub const CONVOLUTION: [[f32; 4]; 60] = [
    [-0.047896601259708405, 0.0022376899141818285, 0.13721300661563873, -0.14268499612808228],
    [0.11079200357198715, -0.048012200742959976, -0.004811530001461506, 0.02609799988567829],
    [-0.04865869879722595, -0.03231469914317131, 0.12428499758243561, -0.08737579733133316],
    [0.09280289709568024, -0.0064116897992789745, -0.04859580099582672, 0.03342840075492859],
    [-0.02262910082936287, -0.06591200083494186, 0.10839200019836426, -0.09438200294971466],
    [0.10435599833726883, -0.029181700199842453, -0.0050018299371004105, 0.006752519868314266],
    [-0.03361539915204048, -0.03066210076212883, 0.014751099981367588, 0.017846299335360527],
    [-0.0310651995241642, 0.011960700154304504, 0.006071330048143864, 0.01514929998666048],
    [0.012825800105929375, -0.0040247500874102116, 0.016707399860024452, -0.01684579998254776],
    [-0.05792130157351494, 0.021790800616145134, -0.007918530143797398, 0.0215901006013155],
    [-0.03485039994120598, -0.0032204100862145424, -0.028062399476766586, -0.016979800537228584],
    [-0.02272370085120201, 0.018890999257564545, 5.3588599257636815e-05, 0.037955399602651596],
    [0.996815025806427, 0.994388997554779, 0.9810929894447327, 0.9833130240440369],
    [0.9915739893913269, 0.9951609969139099, 0.999879002571106, 0.9983410239219666],
    [0.9976639747619629, 0.9984059929847717, 0.9793969988822937, 0.9896960258483887],
    [0.9916139841079712, 0.9988129734992981, 0.9976040124893188, 0.9975969791412354],
    [0.9971839785575867, 0.9959679841995239, 0.9866899847984314, 0.9871580004692078],
    [0.993815004825592, 0.9975690245628357, 0.9999669790267944, 0.9984369874000549],
    [2.672990083694458, 3.6537699699401855, 2.87103009223938, 2.8844099044799805],
    [3.0511300563812256, 2.3980400562286377, 1.9938299655914307, 2.415450096130371],
    [2.810889959335327, 3.819659948348999, 2.9271399974823, 2.921689987182617],
    [3.2918100357055664, 2.4230000972747803, 1.9989500045776367, 2.137350082397461],
    [2.3872199058532715, 3.312659978866577, 2.9524900913238525, 3.029860019683838],
    [2.9255900382995605, 2.4142000675201416, 1.9309899806976318, 2.0014500617980957],
    [0.5689849853515625, 0.3531000018119812, 0.973406970500946, 0.8651319742202759],
    [0.7326409816741943, 1.384950041770935, 1.5516899824142456, 1.457900047302246],
    [1.127020001411438, 0.7165160179138184, 0.9421409964561462, 0.9906229972839355],
    [0.9753149747848511, 1.7410099506378174, 1.442829966545105, 1.4955799579620361],
    [0.7084370255470276, 0.556538999080658, 1.0517100095748901, 0.7899979948997498],
    [0.9671270251274109, 1.3909800052642822, 1.573349952697754, 1.2821600437164307],
    [-0.0694521963596344, -0.054684098809957504, 0.26948198676109314, -0.48583298921585083],
    [0.3975369930267334, -0.05444389954209328, 0.08300969749689102, -0.20476700365543365],
    [-0.07158230245113373, -0.07419479638338089, 0.2483379989862442, -0.48693400621414185],
    [0.355663001537323, -0.025955699384212494, 0.054478999227285385, -0.19293299317359924],
    [-0.06279610097408295, -0.055908799171447754, 0.2013150006532669, -0.42486900091171265],
    [0.42616501450538635, 0.010062700137495995, 0.0889727994799614, -0.22596800327301025],
    [0.054569099098443985, -0.2913289964199066, 0.13726100325584412, -0.004152089823037386],
    [-0.16085900366306305, 0.28030699491500854, -0.06945829838514328, -0.13830700516700745],
    [0.07551079988479614, -0.2936210036277771, 0.10127600282430649, -0.06112419813871384],
    [-0.14244399964809418, 0.2934190034866333, -0.07422740012407303, -0.09664910286664963],
    [0.04261770099401474, -0.26699700951576233, 0.08656840026378632, -0.02063559927046299],
    [-0.030395999550819397, 0.30966299772262573, -0.08135200291872025, -0.13887600600719452],
    [0.9949929714202881, 0.9509490132331848, 0.9523249864578247, 0.8693649768829346],
    [0.8976889848709106, 0.9541260004043579, 0.9930970072746277, 0.9621040225028992],
    [0.9941409826278687, 0.9483960270881653, 0.9630360007286072, 0.8638520240783691],
    [0.9198269844055176, 0.9513630270957947, 0.9941009879112244, 0.9704750180244446],
    [0.9959679841995239, 0.9590579867362976, 0.9744129776954651, 0.9016939997673035],
    [0.8975729942321777, 0.945268988609314, 0.9916499853134155, 0.9569230079650879],
    [3.491489887237549, 3.9926199913024902, 4.155479907989502, 4.767980098724365],
    [5.026559829711914, 4.073729991912842, 3.4915199279785156, 3.8488099575042725],
    [3.4601500034332275, 3.6936099529266357, 4.029660224914551, 4.756750106811523],
    [4.919229984283447, 4.336880207061768, 3.511280059814453, 3.899319887161255],
    [3.5485899448394775, 3.955549955368042, 4.2572197914123535, 5.208270072937012],
    [4.674950122833252, 4.198890209197998, 3.345679998397827, 3.716559886932373],
    [1.5162999629974365, 0.7468050122261047, 0.9939900040626526, 0.7289569973945618],
    [0.7255610227584839, 0.7039139866828918, 1.3796299695968628, 0.7575200200080872],
    [1.5814599990844727, 0.6280959844589233, 1.0032299757003784, 0.756367027759552],
    [1.0983500480651855, 1.1569299697875977, 1.5953700542449951, 1.0459599494934082],
    [1.8529499769210815, 0.8183349967002869, 1.0174599885940552, 0.7971770167350769],
    [0.9702339768409729, 0.9149870276451111, 1.0250999927520752, 0.5558300018310547],
];
