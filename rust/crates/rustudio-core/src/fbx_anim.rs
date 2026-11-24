//! FBX animation reading for `Plugin:ImportFbxAnimation` (binary FBX 7.x,
//! the files fbx.rs reads), as Studio 0.747 imports it. Every rule below was
//! measured in Studio with the files checks/plugins/services/fixtures/
//! fbxanim/variants.py writes (captures/fbxanim-studio.txt; driver
//! studio_fbxanim.py, probe probes/SvcM_fbxanim_measure.lua) and a real
//! third-party walk cycle (1612 poses, not in the repo); plugin_host/
//! api_fbx_anim.rs builds the KeyframeSequence from what this returns.
//!
//! - The file: binary FBX only (ASCII, text, a file without Definitions or
//!   an AnimationStack: "Import failed").
//! - The take: the file's first AnimationStack. Its time range is the
//!   Takes entry of the stack's name (LocalTime) when there is one, else the
//!   stack's LocalStart / LocalStop (GlobalSettings TimeSpan is not used).
//! - Frames: the TimeMode's rate (TimeMode 6: 30 fps, 11: 24, 9: 29.97;
//!   14, custom: CustomFrameRate, 12.5 when it is below 1 or missing);
//!   `round(length / period) + 1` frames with the period a float (7.5 fps
//!   makes 7 a second, 12.5 makes 13), sampled every 1 / rate seconds from
//!   the range start but given Times spread evenly over the range in
//!   floats (k × length / n): 7.5 fps over 1 s samples 0.9333 s as Time 1.
//! - Layers: every unmuted layer of the stack, in order. The base layer's
//!   curve node gives a channel's value (its curves, else its d|X/Y/Z; 0 when
//!   only later layers animate the channel, scale too); each later layer
//!   then blends in by its BlendMode and Weight: translation additive
//!   (+ w × value), override (w × value) or passthrough (a weighted mix),
//!   a root node's later-layer translations turned 180° about Y (x, z
//!   negated); scale × value^w (multiply), + w × value
//!   (ScaleAccumulationMode additive), value^w (override) or geometric
//!   (passthrough); rotations as matrices in XYZ, base × slerp(I, layer, w)
//!   (additive), the layer whatever the weight (override) or slerp(base,
//!   layer, w) (passthrough), with the angles negated before and after for a
//!   node whose RotationOrder parity is odd (XZY, YXZ, ZYX), the order
//!   counting for that when RotationActive is set or the node has a parent
//!   Model; RotationAccumulationMode by channel adds the angles. The base
//!   layer's Weight and every Solo change nothing; a muted base layer is no
//!   motion. Measured with checks/plugins/services/fixtures/fbxanim/lab.py
//!   (lab_*, lab2_*, lab3: isolated axes, orders, weights and modes).
//! - Nodes: a Model with curves on Lcl Translation / Rotation / Scaling in
//!   any layer and a NodeAttribute (DefaultAttributeIndex set: without it a
//!   node never is a Pose). With isR15 (the default) only skeleton nodes (a
//!   Skeleton attribute); without it any. Nodes left out hang their
//!   children on the nearest kept ancestor: one with a NodeAttribute (a
//!   Null with isR15) adds no motion, one without moves with its curves and
//!   rests at its static transform (Lcl Rotation included). Nothing is
//!   matched to the rig and names are kept ("mixamorig:Hips" stays). A file
//!   without any NodeAttribute gives one Pose named after the file.
//! - A node's transform: Lcl Translation (or its curves); rotation only
//!   from curves (an axis without a curve takes the curve node's d|X/Y/Z;
//!   a node with no rotation curve node has none: the static Lcl Rotation
//!   never counts); PreRotation, PostRotation and RotationOrder only with
//!   RotationActive; RotationPivot / RotationOffset / ScalingPivot /
//!   ScalingOffset always (FBX: T Roff Rp Rpre R Rpost⁻¹ Rp⁻¹ Soff Sp S
//!   Sp⁻¹; pivots.fbx); scaling scales the offsets of the node's
//!   descendants per axis (no shear) and nothing else, a zero in it making
//!   the node's rotation and its children's Poses NaN as in Studio. The
//!   BindPose is not read: a node's rest is that transform with no rotation
//!   and its static scaling.
//! - Units and axes: file units are studs (UnitScaleFactor and UpAxis are
//!   not applied); a Pose is the node's change from its rest in its own
//!   frame, turned 180° about Y (x, z negated).
//! - With isR15, a node named exactly "Root" is renamed HumanoidRootPart,
//!   and its children's changes are taken in the file's frame (its motion
//!   moves to them); a top-level HumanoidRootPart's Pose is its animated
//!   transform (rest not taken off) times the 180° turn.

use std::collections::HashMap;

use glam::{DMat3, DVec3};

use crate::fbx::{self, Node, Prop};

/// FBX time units per second.
pub const KTIME: i64 = 46_186_158_000;

/// Studio's error for every file it can't import.
pub const IMPORT_FAILED: &str = "Import failed";

/// A rigid transform (rotation, position in studs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xf {
    pub rot: DMat3,
    pub pos: DVec3,
}

impl Xf {
    pub const IDENTITY: Xf = Xf { rot: DMat3::IDENTITY, pos: DVec3::ZERO };

    fn mul(self, o: Xf) -> Xf {
        Xf { rot: self.rot * o.rot, pos: self.pos + self.rot * o.pos }
    }

    fn inverse(self) -> Xf {
        let r = self.rot.transpose();
        Xf { rot: r, pos: -(r * self.pos) }
    }
}

/// One kept node: its Pose name, its parent among the kept nodes, and its
/// Pose CFrame at each frame.
#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub poses: Vec<Xf>,
}

#[derive(Debug, Clone)]
pub struct Clip {
    /// The animation stack's name.
    pub name: String,
    /// Keyframe times (seconds from the range start).
    pub times: Vec<f64>,
    /// Kept nodes in file order (a parent before its children).
    pub bones: Vec<Bone>,
    /// The rest height of the file's "LowerTorso" node (file units), if it
    /// has a NodeAttribute: Studio writes `-height - 1` into the R15
    /// AnimationRigData.
    pub lower_torso_height: Option<f64>,
    /// No node of the file has a NodeAttribute (none has a
    /// DefaultAttributeIndex): Studio makes one Pose named after the file
    /// (the 180° turn at every frame) and a one-node AnimationRigData.
    pub file_root: bool,
}

#[derive(Debug, Clone)]
struct Curve {
    times: Vec<i64>,
    values: Vec<f64>,
    /// Per key: (flags, right slope, next-left slope).
    attrs: Vec<(i64, f64, f64)>,
}

const INTERP_CONSTANT: i64 = 0x2;
const INTERP_LINEAR: i64 = 0x4;
const INTERP_CUBIC: i64 = 0x8;
const CONSTANT_NEXT: i64 = 0x100;

impl Curve {
    fn read(n: &Node) -> Option<Curve> {
        let ints = |name: &str| match n.child(name).and_then(|c| c.props.first()) {
            Some(Prop::Ints(v)) => v.clone(),
            Some(p) => p.as_i64().into_iter().collect(),
            None => Vec::new(),
        };
        let floats = |name: &str| match n.child(name).and_then(|c| c.props.first()) {
            Some(Prop::Floats(v)) => v.clone(),
            Some(p) => p.as_f64().into_iter().collect(),
            None => Vec::new(),
        };
        let times = ints("KeyTime");
        let values = floats("KeyValueFloat");
        if times.is_empty() || times.len() != values.len() {
            return None;
        }
        let flags = ints("KeyAttrFlags");
        let data = floats("KeyAttrDataFloat");
        let counts = ints("KeyAttrRefCount");
        let mut attrs = Vec::with_capacity(times.len());
        for (j, count) in counts.iter().enumerate() {
            let f = flags.get(j).copied().unwrap_or(INTERP_LINEAR);
            let rs = data.get(j * 4).copied().unwrap_or(0.0);
            let nl = data.get(j * 4 + 1).copied().unwrap_or(0.0);
            for _ in 0..(*count).max(0) {
                attrs.push((f, rs, nl));
            }
        }
        // Files that leave the attribute runs out: linear keys.
        let last = attrs.last().copied().unwrap_or((INTERP_LINEAR, 0.0, 0.0));
        attrs.resize(times.len(), last);
        Some(Curve { times, values, attrs })
    }

    /// The curve's value at `t` (ticks, file time): before the first key
    /// its value, after the last the last.
    fn at(&self, t: i64) -> f64 {
        let n = self.times.len();
        if t <= self.times[0] {
            return self.values[0];
        }
        for i in 0..n - 1 {
            let (t0, t1) = (self.times[i], self.times[i + 1]);
            if t < t1 {
                let (v0, v1) = (self.values[i], self.values[i + 1]);
                if t1 <= t0 {
                    return v0;
                }
                let f = self.attrs[i].0;
                let u = (t - t0) as f64 / (t1 - t0) as f64;
                if f & INTERP_CONSTANT != 0 {
                    return if f & CONSTANT_NEXT != 0 { v1 } else { v0 };
                }
                if f & INTERP_CUBIC != 0 {
                    return hermite(v0, self.attrs[i].1, v1, self.attrs[i].2, seconds(t1 - t0), u);
                }
                return v0 + (v1 - v0) * u;
            }
        }
        self.values[n - 1]
    }
}

pub fn seconds(tick: i64) -> f64 {
    tick as f64 / KTIME as f64
}

/// Cubic Hermite between two keys `dt` seconds apart, slopes per second.
pub fn hermite(v0: f64, s0: f64, v1: f64, s1: f64, dt: f64, u: f64) -> f64 {
    let (u2, u3) = (u * u, u * u * u);
    (2.0 * u3 - 3.0 * u2 + 1.0) * v0 + (u3 - 2.0 * u2 + u) * dt * s0 + (-2.0 * u3 + 3.0 * u2) * v1 + (u3 - u2) * dt * s1
}

/// Frames per second of an FBX GlobalSettings TimeMode (FbxTime::EMode).
fn frame_rate(mode: i64, custom: f64) -> f64 {
    match mode {
        1 => 120.0,
        2 => 100.0,
        3 => 60.0,
        4 => 50.0,
        5 => 48.0,
        6 | 7 => 30.0,
        8 | 9 => 29.97,
        10 => 25.0,
        11 => 24.0,
        12 => 1000.0,
        13 => 23.976,
        // A custom rate below 1 (or none): 12.5 (measured: 0, 0.6, 0.99,
        // -5 and a missing CustomFrameRate all give 12.5).
        14 if custom >= 1.0 => custom,
        14 => 12.5,
        15 => 96.0,
        16 => 72.0,
        17 => 59.94,
        18 => 119.88,
        _ => 30.0,
    }
}

fn obj_name(n: &Node) -> String {
    let raw = n.props.get(1).and_then(|p| p.as_str()).unwrap_or_default();
    let s = raw.split(|b| *b == 0).next().unwrap_or_default();
    String::from_utf8_lossy(s).into_owned()
}

fn ktime(p: &HashMap<String, Vec<Prop>>, name: &str) -> Option<i64> {
    p.get(name).and_then(|v| v.first()?.as_i64())
}

/// One curve node of a Model: its layer, channel (0 T, 1 R, 2 S), the
/// d|X/Y/Z values and the axis curves.
struct Channel {
    layer: usize,
    kind: usize,
    defaults: [Option<f64>; 3],
    axes: [Option<Curve>; 3],
}

impl Channel {
    /// The curve node's value at `tick`: an axis's curve, else its d|X/Y/Z,
    /// else `fill`.
    fn at(&self, tick: i64, fill: f64) -> DVec3 {
        let mut v = DVec3::splat(fill);
        for a in 0..3 {
            if let Some(x) = self.axes[a].as_ref().map(|k| k.at(tick)).or(self.defaults[a]) {
                v[a] = x;
            }
        }
        v
    }
}

/// An AnimationLayer's blending properties (FbxAnimLayer's).
#[derive(Debug, Clone, Copy)]
struct Layer {
    /// Weight / 100.
    weight: f64,
    /// BlendMode: 0 additive, 1 override, 2 override passthrough.
    mode: i64,
    /// RotationAccumulationMode 1: by channel (Euler angles add).
    by_channel: bool,
    /// ScaleAccumulationMode 1: additive (else multiply).
    scale_add: bool,
    mute: bool,
}

impl Layer {
    fn read(n: Option<&Node>) -> Layer {
        let p = n.map(|n| n.props70()).unwrap_or_default();
        let num = |k: &str| p.get(k).and_then(|v| v.first()?.as_f64());
        let int = |k: &str| p.get(k).and_then(|v| v.first()?.as_i64()).unwrap_or(0);
        Layer {
            weight: num("Weight").unwrap_or(100.0) / 100.0,
            mode: int("BlendMode"),
            by_channel: int("RotationAccumulationMode") == 1,
            scale_add: int("ScaleAccumulationMode") == 1,
            mute: int("Mute") != 0,
        }
    }
}

struct Model {
    name: String,
    props: HashMap<String, Vec<Prop>>,
    parent: Option<usize>,
    /// The node has a NodeAttribute (DefaultAttributeIndex set): without
    /// one it is never a Pose.
    attr: bool,
    skeleton: bool,
    chans: Vec<Channel>,
}

/// XYZ Euler angles (degrees) of a rotation matrix (FBX XYZ: X first).
fn euler_xyz(m: DMat3) -> DVec3 {
    let r = m.transpose().to_cols_array_2d(); // r[row][col]
    let y = (-r[2][0]).clamp(-1.0, 1.0).asin();
    let (x, z) = if r[2][0].abs() < 1.0 - 1e-12 {
        (r[2][1].atan2(r[2][2]), r[1][0].atan2(r[0][0]))
    } else {
        ((-r[1][2]).atan2(r[1][1]), 0.0)
    };
    DVec3::new(x, y, z) * (180.0 / std::f64::consts::PI)
}

fn rot_xyz(d: DVec3) -> DMat3 {
    DMat3::from_mat4(fbx::euler_ordered(d, 0))
}

/// From `a` to `b` by `w` along the shortest arc.
fn slerp(a: DMat3, b: DMat3, w: f64) -> DMat3 {
    let (qa, mut qb) = (glam::DQuat::from_mat3(&a), glam::DQuat::from_mat3(&b));
    if qa.dot(qb) < 0.0 {
        qb = -qb;
    }
    DMat3::from_quat(qa.slerp(qb, w).normalize())
}

impl Model {
    fn active(&self) -> bool {
        self.props.get("RotationActive").and_then(|v| v.first()?.as_i64()).unwrap_or(0) != 0
    }

    fn order(&self) -> i64 {
        self.props.get("RotationOrder").and_then(|v| v.first()?.as_i64()).unwrap_or(0)
    }

    fn vec(&self, k: &str, d: DVec3) -> DVec3 {
        fbx::vec3_prop(&self.props, k, d)
    }

    /// The channel's value blended over the layers (None: no layer animates
    /// it). Measured in Studio (captures/fbxanim-studio.txt, lab* cases):
    /// the base layer's curve node (its curves, else its d|X/Y/Z; nothing
    /// when it has none: 0, also for scale) then each later unmuted layer
    /// in turn, additive (+ weight × value; scale × value^weight, or
    /// + weight × value with ScaleAccumulationMode additive), override
    /// (weight × value; scale value^weight) or passthrough (a weighted
    /// mix; scale geometric). A root node's later-layer translations are
    /// turned 180° about Y (x, z negated). The base layer's Weight and
    /// Solo change nothing; a muted base layer counts as no motion
    /// (scale 1).
    fn blend(&self, kind: usize, tick: i64, layers: &[Layer], root: bool) -> Option<DVec3> {
        let live = |c: &&Channel| c.kind == kind && !layers[c.layer].mute;
        if !self.chans.iter().any(|c| live(&c)) {
            return None;
        }
        let one = if kind == 2 { 1.0 } else { 0.0 };
        let mut v = if layers[0].mute {
            DVec3::splat(one)
        } else {
            self.chans.iter().find(|c| c.kind == kind && c.layer == 0).map(|c| c.at(tick, 0.0)).unwrap_or(DVec3::ZERO)
        };
        for c in self.chans.iter().filter(|c| live(c) && c.layer > 0) {
            let ly = layers[c.layer];
            let w = ly.weight;
            let mut l = c.at(tick, one);
            if kind == 0 && root {
                l = DVec3::new(-l.x, l.y, -l.z);
            }
            v = match (kind, ly.mode) {
                (2, 1) => l.powf(w),
                (2, 2) => v.powf(1.0 - w) * l.powf(w),
                (2, _) if ly.scale_add => v + l * w,
                (2, _) => v * l.powf(w),
                (_, 1) => l * w,
                (_, 2) => v * (1.0 - w) + l * w,
                _ => v + l * w,
            };
        }
        Some(v)
    }

    /// The node's rotation curves blended over the layers, as Euler angles
    /// in the node's order (measured): a later layer combines with the
    /// result so far as rotations (XYZ matrices base × layer; for a node
    /// whose RotationOrder parity is odd (XZY, YXZ, ZYX) the angles are
    /// negated before and after, which is base × layer in that order's
    /// sense), additive by slerp from identity by the weight, override
    /// replacing it whatever the weight, passthrough slerping by the
    /// weight; with RotationAccumulationMode by channel the angles add.
    /// The order counts for the parity when RotationActive is set or the
    /// node has a parent Model (an inactive root composes in XYZ).
    fn rotation(&self, tick: i64, layers: &[Layer], parity_order: i64) -> DVec3 {
        let live = |c: &&Channel| c.kind == 1 && !layers[c.layer].mute;
        let mut e = if layers[0].mute {
            DVec3::ZERO
        } else {
            self.chans.iter().find(|c| c.kind == 1 && c.layer == 0).map(|c| c.at(tick, 0.0)).unwrap_or(DVec3::ZERO)
        };
        let sign = if matches!(parity_order, 1 | 3 | 5) { -1.0 } else { 1.0 };
        for c in self.chans.iter().filter(|c| live(c) && c.layer > 0) {
            let ly = layers[c.layer];
            let l = c.at(tick, 0.0);
            if ly.by_channel {
                e = match ly.mode {
                    1 => l * ly.weight,
                    2 => e * (1.0 - ly.weight) + l * ly.weight,
                    _ => e + l * ly.weight,
                };
                continue;
            }
            let (x, lm) = (rot_xyz(e * sign), rot_xyz(l * sign));
            let m = match ly.mode {
                1 => lm,
                2 => slerp(x, lm, ly.weight),
                _ => x * slerp(DMat3::IDENTITY, lm, ly.weight),
            };
            e = euler_xyz(m) * sign;
        }
        e
    }

    /// (translation, rotation, scale) at `tick`, or at rest (`None`). The
    /// translation is the local offset with the node's rotation / scaling
    /// pivots and offsets (FBX: T Roff Rp Rpre R Rpost⁻¹ Rp⁻¹ Soff Sp S
    /// Sp⁻¹, pivots and offsets with or without RotationActive; measured
    /// with pivots.fbx), before the ancestors' scale.
    ///
    /// `kept`: the node is a Pose. Measured (rest_* cases): a node that is
    /// no Pose but has a NodeAttribute (a Null with isR15) doesn't move
    /// (its static transform at every frame); one without a NodeAttribute
    /// moves with its curves and rests at its static transform, Lcl
    /// Rotation included (a Pose's rest leaves the Lcl Rotation out).
    fn trs(&self, tick: Option<i64>, layers: &[Layer], root: bool, kept: bool) -> (DVec3, DMat3, DVec3) {
        let static_t = self.vec("Lcl Translation", DVec3::ZERO);
        let static_s = self.vec("Lcl Scaling", DVec3::ONE);
        let static_r = if kept { DVec3::ZERO } else { self.vec("Lcl Rotation", DVec3::ZERO) };
        let tick = if !kept && self.attr { None } else { tick };
        let (t, r, s) = match tick {
            None => (static_t, static_r, static_s),
            Some(tick) => {
                let po = if self.active() || !root { self.order() } else { 0 };
                let has_r = self.chans.iter().any(|c| c.kind == 1 && !layers[c.layer].mute);
                (
                    self.blend(0, tick, layers, root).unwrap_or(static_t),
                    if has_r { self.rotation(tick, layers, po) } else { static_r },
                    self.blend(2, tick, layers, root).unwrap_or(static_s),
                )
            }
        };
        let (pre, post, order) = if self.active() {
            (self.vec("PreRotation", DVec3::ZERO), self.vec("PostRotation", DVec3::ZERO), self.order())
        } else {
            (DVec3::ZERO, DVec3::ZERO, 0)
        };
        let m3 = |d: DVec3, o: i64| DMat3::from_mat4(fbx::euler_ordered(d, o));
        let rot = m3(pre, 0) * m3(r, order) * m3(post, 0).transpose();
        let (rp, roff) = (self.vec("RotationPivot", DVec3::ZERO), self.vec("RotationOffset", DVec3::ZERO));
        let (sp, soff) = (self.vec("ScalingPivot", DVec3::ZERO), self.vec("ScalingOffset", DVec3::ZERO));
        (t + roff + rp + rot * (-rp + soff + sp - s * sp), rot, s)
    }
}

/// The 180° turn about Y between the file's frame and Roblox's.
fn turn(x: Xf) -> Xf {
    let f = DMat3::from_diagonal(DVec3::new(-1.0, 1.0, -1.0));
    Xf { rot: f * x.rot * f, pos: f * x.pos }
}

/// Reads the file's animation for ImportFbxAnimation(rig, isR15).
pub fn read(bytes: &[u8], r15: bool) -> Result<Clip, String> {
    let fail = || IMPORT_FAILED.to_string();
    let (_, top) = fbx::parse(bytes).map_err(|_| fail())?;
    let find = |name: &str| top.iter().find(|n| n.name == name);
    let objects = find("Objects").ok_or_else(fail)?;
    if find("Definitions").is_none() {
        return Err(fail());
    }
    let gs = find("GlobalSettings").map(|g| g.props70()).unwrap_or_default();
    let fps = frame_rate(
        gs.get("TimeMode").and_then(|v| v.first()?.as_i64()).unwrap_or(0),
        gs.get("CustomFrameRate").and_then(|v| v.first()?.as_f64()).unwrap_or(0.0),
    );

    let mut by_id: HashMap<i64, &Node> = HashMap::new();
    for o in &objects.kids {
        if let Some(id) = o.props.first().and_then(|p| p.as_i64()) {
            by_id.insert(id, o);
        }
    }
    let mut links: Vec<(i64, i64, Option<String>)> = Vec::new();
    if let Some(c) = find("Connections") {
        for l in c.children("C") {
            let (Some(a), Some(b)) = (l.props.get(1).and_then(|p| p.as_i64()), l.props.get(2).and_then(|p| p.as_i64())) else {
                continue;
            };
            let prop = l.props.get(3).and_then(|p| p.as_str()).map(|s| String::from_utf8_lossy(s).into_owned());
            links.push((a, b, prop));
        }
    }
    let class = |id: i64| by_id.get(&id).map(|n| n.name.as_str()).unwrap_or("");

    let stack = objects.kids.iter().find(|n| n.name == "AnimationStack").ok_or_else(fail)?;
    let stack_id = stack.props.first().and_then(|p| p.as_i64()).ok_or_else(fail)?;
    let stack_name = obj_name(stack);
    let layers: Vec<i64> =
        links.iter().filter(|(a, b, p)| *b == stack_id && p.is_none() && class(*a) == "AnimationLayer").map(|l| l.0).collect();
    let mut blend: Vec<Layer> = layers.iter().map(|id| Layer::read(by_id.get(id).copied())).collect();
    if blend.is_empty() {
        blend.push(Layer::read(None));
    }

    // Models in file order.
    let model_nodes: Vec<&Node> = objects.kids.iter().filter(|n| n.name == "Model").collect();
    let index: HashMap<i64, usize> =
        model_nodes.iter().enumerate().filter_map(|(i, n)| Some((n.props.first()?.as_i64()?, i))).collect();
    let mut models: Vec<Model> = model_nodes
        .iter()
        .map(|n| Model { name: obj_name(n), props: n.props70(), parent: None, attr: false, skeleton: false, chans: Vec::new() })
        .collect();
    for (a, b, p) in &links {
        let (Some(&ai), None) = (index.get(a), p) else { continue };
        if let Some(&bi) = index.get(b) {
            models[ai].parent = Some(bi);
        }
    }
    for (a, b, p) in &links {
        if p.is_some() || class(*a) != "NodeAttribute" {
            continue;
        }
        let Some(&bi) = index.get(b) else { continue };
        let skeleton = by_id[a].child("TypeFlags").and_then(|t| t.props.first()?.as_str()).is_some_and(|s| s == b"Skeleton");
        // The FBX SDK gives a node no attribute unless DefaultAttributeIndex says which.
        let indexed = models[bi].props.get("DefaultAttributeIndex").and_then(|v| v.first()?.as_i64()).is_some_and(|i| i >= 0);
        models[bi].skeleton |= skeleton && indexed;
        models[bi].attr |= indexed;
    }
    for (li, layer) in layers.iter().enumerate() {
        for (cn, l, _) in &links {
            if l != layer || class(*cn) != "AnimationCurveNode" {
                continue;
            }
            for (c2, m, prop) in &links {
                let (true, Some(prop), Some(&mi)) = (c2 == cn, prop.as_deref(), index.get(m)) else { continue };
                let kind = match prop {
                    "Lcl Translation" => 0,
                    "Lcl Rotation" => 1,
                    "Lcl Scaling" => 2,
                    _ => continue,
                };
                let cnp = by_id[cn].props70();
                let d = |k: &str| cnp.get(k).and_then(|v| v.first()?.as_f64());
                let mut ch = Channel { layer: li, kind, defaults: [d("d|X"), d("d|Y"), d("d|Z")], axes: [None, None, None] };
                for (c3, n3, pa) in &links {
                    if n3 != cn || class(*c3) != "AnimationCurve" {
                        continue;
                    }
                    let axis = match pa.as_deref() {
                        Some("d|X") => 0,
                        Some("d|Y") => 1,
                        Some("d|Z") => 2,
                        _ => continue,
                    };
                    ch.axes[axis] = Curve::read(by_id[c3]);
                }
                if ch.axes.iter().any(Option::is_some) {
                    models[mi].chans.push(ch);
                }
            }
        }
    }

    // The time range: the take of the stack's name, else the stack's own.
    let take = find("Takes").and_then(|t| {
        t.children("Take").find(|k| k.props.first().and_then(|p| p.as_str()).is_some_and(|s| s == stack_name.as_bytes()))
    });
    let take_range = take.and_then(|k| {
        let lt = k.child("LocalTime")?;
        Some((lt.props.first()?.as_i64()?, lt.props.get(1)?.as_i64()?))
    });
    let sp = stack.props70();
    let (start, stop) = match take_range {
        Some(r) => r,
        None => {
            let times = models.iter().flat_map(|m| &m.chans).flat_map(|c| c.axes.iter().flatten()).flat_map(|c| c.times.iter().copied());
            let (lo, hi) = times.fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
            let start = ktime(&sp, "LocalStart").unwrap_or(if lo == i64::MAX { 0 } else { lo });
            (start, ktime(&sp, "LocalStop").unwrap_or(hi.max(start)))
        }
    };
    // round(length / period) + 1 frames with the period a float (1 / 7.5 s
    // gives 7 a second, 1 / 12.5 s 13), sampled every 1 / rate seconds from
    // the start, their Times spread evenly over the range (measured: 7.5
    // fps over 1 s samples 0, 0.1333 .. 0.9333 s as Times 0, 1/7 .. 1;
    // 30 fps over 1.04 s samples k/30 as Times k × 1.04/31).
    let len = (stop - start).max(0);
    let n = ((len as f64 / KTIME as f64) / ((1.0 / fps) as f32 as f64)).round().max(0.0) as i64;
    let frames: Vec<(i64, f64)> = (0..=n)
        .map(|k| {
            // (in floats: k × length / n, as Studio's Times read)
            let secs = (len as f64 / KTIME as f64) as f32;
            let label = if n == 0 { 0.0 } else { (k as f32 * secs / n as f32) as f64 };
            (start + (k as f64 * KTIME as f64 / fps).round() as i64, label)
        })
        .collect();

    // Kept nodes, their kept parents and Pose names.
    let keep: Vec<bool> = models.iter().map(|m| !m.chans.is_empty() && if r15 { m.skeleton } else { m.attr }).collect();
    let kept: Vec<usize> = (0..models.len()).filter(|&i| keep[i]).collect();
    let slot: HashMap<usize, usize> = kept.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    let kept_parent = |mut i: usize| {
        let mut guard = 0;
        while let Some(p) = models[i].parent {
            if keep[p] {
                return Some(p);
            }
            i = p;
            guard += 1;
            if guard > 4096 {
                break;
            }
        }
        None
    };
    let renamed: Vec<bool> = kept.iter().map(|&i| r15 && models[i].name == "Root").collect();

    // World transforms: offsets scale with the ancestors' scale (per axis),
    // rotations don't. Also the node's own world scale (a zero in it: Studio
    // gives NaN rotations).
    let world_s = |i: usize, tick: Option<i64>| {
        let mut chain = vec![i];
        let mut c = i;
        while let Some(p) = models[c].parent {
            if chain.len() > 4096 {
                break;
            }
            chain.push(p);
            c = p;
        }
        let (mut pos, mut rot, mut sacc) = (DVec3::ZERO, DMat3::IDENTITY, DVec3::ONE);
        for &m in chain.iter().rev() {
            let (t, r, s) = models[m].trs(tick, &blend, models[m].parent.is_none(), keep[m]);
            pos += rot * (sacc * t);
            rot *= r;
            sacc *= s;
        }
        (Xf { rot, pos }, sacc)
    };
    let world = |i: usize, tick: Option<i64>| world_s(i, tick).0;
    let rest: Vec<Xf> = kept.iter().map(|&i| world(i, None)).collect();
    let mut bones: Vec<Bone> = kept
        .iter()
        .enumerate()
        .map(|(k, &i)| Bone {
            name: if renamed[k] { "HumanoidRootPart".into() } else { models[i].name.clone() },
            parent: kept_parent(i).map(|p| slot[&p]),
            poses: Vec::with_capacity(frames.len()),
        })
        .collect();
    for &(tick, _) in &frames {
        let now_s: Vec<(Xf, DVec3)> = kept.iter().map(|&i| world_s(i, Some(tick))).collect();
        let now: Vec<Xf> = now_s.iter().map(|n| n.0).collect();
        let flat = |s: DVec3| s.x == 0.0 || s.y == 0.0 || s.z == 0.0;
        for k in 0..kept.len() {
            let (la, lr) = match bones[k].parent {
                Some(p) => {
                    let frame = if renamed[p] { Xf::IDENTITY } else { now[p] };
                    (frame.inverse().mul(now[k]), rest[p].inverse().mul(rest[k]))
                }
                None => (now[k], rest[k]),
            };
            let pose = if r15 && bones[k].parent.is_none() && bones[k].name == "HumanoidRootPart" {
                Xf { rot: la.rot * DMat3::from_diagonal(DVec3::new(-1.0, 1.0, -1.0)), pos: la.pos }
            } else {
                turn(lr.inverse().mul(la))
            };
            // A zero world scale: Studio's rotation of the node is NaN, and
            // all of its children's poses (measured: lab_def sp / sc).
            let nan = DMat3::from_cols_array(&[f64::NAN; 9]);
            let pose = match bones[k].parent {
                Some(p) if flat(now_s[p].1) => Xf { rot: nan, pos: DVec3::splat(f64::NAN) },
                _ if flat(now_s[k].1) => Xf { rot: nan, ..pose },
                _ => pose,
            };
            bones[k].poses.push(pose);
        }
    }
    let lower_torso_height = models.iter().position(|m| m.name == "LowerTorso" && m.attr).map(|i| world(i, None).pos.y);
    let file_root = !models.iter().any(|m| m.attr);
    Ok(Clip { name: stack_name, times: frames.iter().map(|f| f.1).collect(), bones, lower_torso_height, file_root })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAVE: &[u8] = include_bytes!("../../../checks/plugins/services/fixtures/fbxanim/wave.fbx");

    fn pose(c: &Clip, name: &str, frame: usize) -> Xf {
        c.bones.iter().find(|b| b.name == name).unwrap().poses[frame]
    }

    fn close(a: Xf, pos: [f64; 3], rot: [f64; 9]) -> bool {
        let r = a.rot.transpose().to_cols_array(); // row-major
        (a.pos - DVec3::from_array(pos)).length() < 2e-4 && r.iter().zip(rot).all(|(x, y)| (x - y).abs() < 2e-4)
    }

    /// Studio's poses for wave.fbx (captures/fbxanim-studio.txt, `wave`).
    #[test]
    fn wave_matches_studio() {
        let c = read(WAVE, true).unwrap();
        assert_eq!(c.name, "Wave");
        assert_eq!(c.times.len(), 31);
        assert!((c.times[30] - 1.0).abs() < 1e-12);
        let names: Vec<(&str, Option<usize>)> = c.bones.iter().map(|b| (b.name.as_str(), b.parent)).collect();
        assert_eq!(names, [("LowerTorso", None), ("UpperTorso", Some(0)), ("RightUpperArm", Some(1)), ("Antenna", Some(1))]);
        assert!(close(pose(&c, "LowerTorso", 15), [-5.0, 10.0, 0.0], [1., 0., 0., 0., 1., 0., 0., 0., 1.]));
        assert!(close(
            pose(&c, "UpperTorso", 15),
            [0.0; 3],
            [0.852868, 0.040288, 0.520569, -0.150384, 0.973723, 0.171021, -0.5, -0.224144, 0.836516]
        ));
        assert!(close(pose(&c, "Antenna", 15), [0.0; 3], [0.939693, 0.342020, 0., -0.342020, 0.939693, 0., 0., 0., 1.]));
        assert!(close(pose(&c, "RightUpperArm", 30), [0.0; 3], [1., 0., 0., 0., 0., 1., 0., -1., 0.]));
        assert_eq!(c.lower_torso_height, Some(100.0));
        // isR15 false keeps the animated Null too.
        let f = read(WAVE, false).unwrap();
        assert!(close(pose(&f, "Prop", 15), [-25.0, 0.0, 0.0], [1., 0., 0., 0., 1., 0., 0., 0., 1.]));
    }

    #[test]
    fn hermite_uses_slopes_per_second() {
        assert!((hermite(0.0, 0.0, 1.0, 0.0, 1.0, 0.5) - 0.5).abs() < 1e-12);
        assert!((hermite(0.0, 0.0, 1.0, 0.0, 1.0, 0.25) - 0.15625).abs() < 1e-12);
        for u in [0.1, 0.3, 0.7] {
            assert!((hermite(0.0, 1.0, 2.0, 1.0, 2.0, u) - 2.0 * u).abs() < 1e-12);
        }
    }

    #[test]
    fn curve_interpolation_modes() {
        let c = |flags: i64| Curve { times: vec![0, KTIME], values: vec![0.0, 10.0], attrs: vec![(flags, 0.0, 0.0); 2] };
        assert_eq!(c(INTERP_LINEAR).at(KTIME / 4), 2.5);
        assert_eq!(c(INTERP_CONSTANT).at(KTIME / 4), 0.0);
        assert_eq!(c(INTERP_CONSTANT | CONSTANT_NEXT).at(KTIME / 4), 10.0);
        assert!((c(INTERP_CUBIC).at(KTIME / 4) - 1.5625).abs() < 1e-12);
        assert_eq!(c(INTERP_LINEAR).at(-5), 0.0);
        assert_eq!(c(INTERP_LINEAR).at(KTIME * 3), 10.0);
    }

    #[test]
    fn unreadable_files_fail_as_studio() {
        // A mesh-only file has no AnimationStack; not an FBX at all.
        assert_eq!(read(&crate::fbx::sample([100.0, 100.0, 100.0], [0.0, 0.0, 0.0]), true).unwrap_err(), IMPORT_FAILED);
        assert_eq!(read(b"hello", true).unwrap_err(), IMPORT_FAILED);
        assert_eq!(read(b"; FBX 7.4.0 project file\n", false).unwrap_err(), IMPORT_FAILED);
    }

    #[test]
    fn euler_xyz_inverts_rot_xyz() {
        for d in [DVec3::new(30.0, -50.0, 20.0), DVec3::new(-120.0, 40.0, 170.0), DVec3::new(10.0, 89.0, -5.0)] {
            let back = rot_xyz(euler_xyz(rot_xyz(d)));
            assert!(back.abs_diff_eq(rot_xyz(d), 1e-9));
        }
    }

    /// The lab_def capture's layered rotations at t = 1 (Layer2 additive,
    /// weight 1): rxz (base R.x 60, layer R.z 40) is Rx(60)·Rz(40); rord
    /// (RotationActive, ZYX: odd parity) is the layer first, Rz(40)·Rx(30)·Ry(60).
    #[test]
    fn layered_rotation_parity() {
        let m = |b: [f64; 3], l: [f64; 3], order: i64, po: i64| {
            let curve = |v: f64| Curve { times: vec![0], values: vec![v], attrs: vec![(INTERP_LINEAR, 0.0, 0.0)] };
            let chan = |layer: usize, v: [f64; 3]| Channel {
                layer,
                kind: 1,
                defaults: [Some(0.0); 3],
                axes: [Some(curve(v[0])), Some(curve(v[1])), Some(curve(v[2]))],
            };
            let mut props = HashMap::new();
            props.insert("RotationOrder".to_string(), vec![Prop::Int(order)]);
            let model = Model { name: String::new(), props, parent: None, attr: true, skeleton: true, chans: vec![chan(0, b), chan(1, l)] };
            let ly = Layer { weight: 1.0, mode: 0, by_channel: false, scale_add: false, mute: false };
            DMat3::from_mat4(fbx::euler_ordered(model.rotation(0, &[ly, ly], po), order))
        };
        let rx = |a: f64| DMat3::from_rotation_x(a.to_radians());
        let ry = |a: f64| DMat3::from_rotation_y(a.to_radians());
        let rz = |a: f64| DMat3::from_rotation_z(a.to_radians());
        assert!(m([60.0, 0.0, 0.0], [0.0, 0.0, 40.0], 0, 0).abs_diff_eq(rx(60.0) * rz(40.0), 1e-9));
        assert!(m([30.0, 60.0, 0.0], [0.0, 0.0, 40.0], 5, 5).abs_diff_eq(rz(40.0) * rx(30.0) * ry(60.0), 1e-9));
    }

    #[test]
    fn custom_frame_rates() {
        assert_eq!(frame_rate(14, 7.5), 7.5);
        assert_eq!(frame_rate(14, 0.99), 12.5);
        assert_eq!(frame_rate(14, 0.0), 12.5);
        assert_eq!(frame_rate(11, 12.5), 24.0);
    }

    #[test]
    fn rotation_orders_match_fbx() {
        // ZYX (5): Z first, then Y, then X.
        let d = DVec3::new(10.0, 20.0, 30.0) * (std::f64::consts::PI / 180.0);
        let want = glam::DMat4::from_rotation_x(d.x) * glam::DMat4::from_rotation_y(d.y) * glam::DMat4::from_rotation_z(d.z);
        assert!(fbx::euler_ordered(DVec3::new(10.0, 20.0, 30.0), 5).abs_diff_eq(want, 1e-12));
    }
}
