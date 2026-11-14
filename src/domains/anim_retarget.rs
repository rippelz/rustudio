//! Animation retargeting (Workspace.Retargeting) as Studio 0.744 does it:
//! the legacy R15ToR15Retargeter's FK correction plus its hip-translation
//! scale, between the clip's authoring rig (its AnimationRigData) and the
//! rig Studio builds from the animated character. Spec with every native
//! address and the measurements: checks/retargeting/SPEC.md.
//!
//! In short (addresses in Studio of Studio's Studio 0.744):
//! - The gate (Workspace Studio): Enabled, or Default while
//!   DFFlagWorkspaceAnimatorRetargetingEnabledDefault (on); Disabled is off.
//! - A clip retargets when it carries an R15 AnimationRigData and the
//!   character is an R15 Humanoid rig (factory Studio; the
//!   RobustRetargeter only runs when a rig holds a HumanoidRigDescription,
//!   Studio).
//! - The character rig (Studio, the same
//!   path AnimationRigData:LoadFromModel takes): a node per part, joint frame
//!   = Part0's rest frame ∘ C0, pre = parent frame⁻¹ ∘ frame, post =
//!   identity, labels by part name, then the TPoser v2
//! gives each arm's shoulder and elbow T.
//! - initialize builds a correction (A, B) per
//!   label 1..21; RetargetFK maps a track's local pose
//!   q → A·q·B, t → A·t·B; the hip step scales the root's
//!   translation by the leg-length ratio.

use std::collections::HashMap;

use glam::{Mat3, Quat, Vec3};
use rbx_dom_weak::types::{Ref, Variant};

use crate::doc::Document;
use crate::math::Xform;

/// FInt AnimatorRetargetInterpolateFKCorrectionMin/MaxAngleDeg (values at
/// Studio; GetFastInt in Studio: 10 and 35).
const FK_MIN_DEG: f32 = 10.0;
const FK_MAX_DEG: f32 = 35.0;
/// FInt TPoseBlockyElbowBendMin (Studio; measured 10).
const TPOSE_BLOCKY_ELBOW_BEND_MIN_DEG: f32 = 10.0;
/// FInt TPoseBlockyMeshAlignedAdjustmentPercent (Studio; measured 64).
const TPOSE_BLOCKY_ADJUST_PERCENT: f32 = 64.0;

// ------------------------------------------------------- transform algebra

/// Studio's rig transform: a quaternion and a position (7 floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tq {
    pub q: Quat,
    pub p: Vec3,
}

pub const TQ_ID: Tq = Tq { q: Quat::IDENTITY, p: Vec3::ZERO };

/// Hamilton product as Studio writes it.
fn qmul(a: Quat, b: Quat) -> Quat {
    Quat::from_xyzw(
        a.x * b.w + b.x * a.w + (a.y * b.z - a.z * b.y),
        a.y * b.w + b.y * a.w + (a.z * b.x - a.x * b.z),
        a.z * b.w + b.z * a.w + (a.x * b.y - a.y * b.x),
        b.w * a.w - (a.y * b.y + a.x * b.x + a.z * b.z),
    )
}

/// conj / |q|² (Studio's inverses).
fn qinv(q: Quat) -> Quat {
    let n = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
    Quat::from_xyzw(-q.x / n, -q.y / n, -q.z / n, q.w / n)
}

/// v rotated by q with the 2·cross form Studio uses.
fn qrot(q: Quat, v: Vec3) -> Vec3 {
    let t = Vec3::new(q.y * v.z - q.z * v.y, q.z * v.x - q.x * v.z, q.x * v.y - q.y * v.x) * 2.0;
    v + t * q.w + Vec3::new(q.y * t.z - q.z * t.y, q.z * t.x - q.x * t.z, q.x * t.y - q.y * t.x)
}

/// a ∘ b.
fn comp(a: Tq, b: Tq) -> Tq {
    Tq { q: qmul(a.q, b.q), p: a.p + qrot(a.q, b.p) }
}

/// The inverse (Studio: conjugate, −conj·p).
fn tinv(a: Tq) -> Tq {
    let c = Quat::from_xyzw(-a.q.x, -a.q.y, -a.q.z, a.q.w);
    Tq { q: c, p: -qrot(c, a.p) }
}

fn rot_only(q: Quat) -> Tq {
    Tq { q, p: Vec3::ZERO }
}

/// Unit rotation from a to b (Studio, k = |a||b|).
fn quat_between(a: Vec3, b: Vec3, k: f32) -> Quat {
    let w = b.x * a.x + b.y * a.y + b.z * a.z + k;
    let q = if w < k * 1e-5 {
        if a.x.abs() <= a.y.abs() {
            Quat::from_xyzw(0.0, -a.z, a.y, 0.0)
        } else {
            Quat::from_xyzw(-a.z, 0.0, a.x, 0.0)
        }
    } else {
        Quat::from_xyzw(b.z * a.y - a.z * b.y, a.z * b.x - b.z * a.x, a.x * b.y - b.x * a.y, w)
    };
    let n = 1.0 / (q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w).sqrt();
    Quat::from_xyzw(q.x * n, q.y * n, q.z * n, q.w * n)
}

/// Studio: the world rotation from a to b, expressed in frame f
/// (f⁻¹ ∘ rot ∘ f), position dropped; identity when either is ~0.
fn local_rotation_between(a: Vec3, b: Vec3, f: Tq) -> Tq {
    let la = a.x * a.x + a.y * a.y + a.z * a.z;
    let lb = b.x * b.x + b.y * b.y + b.z * b.z;
    if la < 1e-6 || lb < 1e-6 {
        return TQ_ID;
    }
    let q = quat_between(a, b, (lb * la).sqrt());
    let r = comp(comp(tinv(f), rot_only(q)), f);
    rot_only(r.q)
}

/// Studio: the twist about X that turns f's forward (f·(−0,−0,−1),
/// projected on YZ) back onto −Z, in f's frame.
fn twist_fix(f: Tq) -> Tq {
    let fwd = qrot(f.q, Vec3::new(-0.0, -0.0, -1.0));
    let proj = Vec3::new(0.0, fwd.y, fwd.z);
    if (fwd.y * fwd.y + 0.0 + fwd.z * fwd.z).sqrt() > 1e-6 {
        local_rotation_between(proj, Vec3::new(-0.0, -0.0, -1.0), f)
    } else {
        TQ_ID
    }
}

/// Studio: unit a slerped toward unit b by t.
fn vec_slerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    let d = (b.x * a.x + b.y * a.y + b.z * a.z).clamp(-1.0, 1.0);
    let th = d.acos();
    let o = b - a * d;
    let inv = 1.0 / (o.x * o.x + o.y * o.y + o.z * o.z).sqrt();
    let (c, s) = ((th * t).cos(), (th * t).sin());
    Vec3::new(o.x * inv * s + a.x * c, o.y * inv * s + a.y * c, o.z * inv * s + a.z * c)
}

/// Studio: the angle between two vectors (atan2 of |a×b|, a·b).
fn vec_angle(a: Vec3, b: Vec3) -> f32 {
    let c = Vec3::new(b.z * a.y - b.y * a.z, b.x * a.z - a.x * b.z, a.x * b.y - b.x * a.y);
    (c.x * c.x + c.y * c.y + c.z * c.z).sqrt().atan2(a.x * b.x + b.y * a.y + a.z * b.z)
}

fn normalize(v: Vec3) -> Vec3 {
    v * (1.0 / (v.x * v.x + v.y * v.y + v.z * v.z).sqrt())
}

/// Studio: slerp with copysign on the dot, lerp + normalize past
/// 0.99981.
fn slerp(a: Quat, b: Quat, t: f32) -> Quat {
    if t == 0.0 {
        return a;
    }
    if t == 1.0 {
        return b;
    }
    let dot = b.x * a.x + b.y * a.y + b.z * a.z + b.w * a.w;
    let sign = 1.0f32.copysign(dot);
    let d = dot.abs().clamp(0.0, 1.0);
    if d <= 0.999_809_98 {
        let s = (1.0 - d * d).sqrt();
        let th = d.acos();
        let k0 = ((1.0 - t) * th).sin() * (1.0 / s);
        let k1 = (th * t).sin() * (1.0 / s) * sign;
        Quat::from_xyzw(a.x * k0 + b.x * k1, a.y * k0 + b.y * k1, a.z * k0 + b.z * k1, a.w * k0 + b.w * k1)
    } else {
        let k1 = sign * t;
        let k0 = 1.0 - t;
        let r = Quat::from_xyzw(a.x * k0 + b.x * k1, a.y * k0 + b.y * k1, a.z * k0 + b.z * k1, a.w * k0 + b.w * k1);
        let n = 1.0 / (r.x * r.x + r.y * r.y + r.z * r.z + r.w * r.w).sqrt();
        Quat::from_xyzw(r.x * n, r.y * n, r.z * n, r.w * n)
    }
}

/// The rotation angle Studio measures: 2·acos(w) for −1 < w < 1,
/// 2π at w ≤ −1, 0 at w ≥ 1, then |·|.
fn corr_angle(q: Quat) -> f32 {
    let a = if q.w < 1.0 {
        if -1.0 < q.w {
            2.0 * q.w.acos()
        } else {
            std::f32::consts::TAU
        }
    } else {
        0.0
    };
    a.abs()
}

fn tq_of(x: &Xform) -> Tq {
    Tq { q: Quat::from_mat3(&x.rot), p: x.pos }
}

// ------------------------------------------------------------------ rigs

/// One node of Studio's AnimationRig (a 0x80-byte record at rig:
/// pre, post, transform, label).
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub label: u32,
    pub parent: Option<usize>,
    pub pre: Tq,
    pub post: Tq,
    pub t: Tq,
}

#[derive(Clone, Debug, Default)]
pub struct Rig {
    pub nodes: Vec<Node>,
}

impl Rig {
    /// The node with a label (the legacy label → node map, Studio).
    pub fn by_label(&self, label: u32) -> Option<usize> {
        self.nodes.iter().position(|n| n.label == label)
    }

    /// The frame after the node's post (Studio:
    /// parent's ∘ pre ∘ T ∘ post).
    pub fn world_part(&self, i: usize) -> Tq {
        let n = &self.nodes[i];
        let base = n.parent.map(|p| self.world_part(p)).unwrap_or(TQ_ID);
        comp(comp(comp(base, n.pre), n.t), n.post)
    }

    /// The joint's pivot frame (Studio: parent's ∘ pre).
    pub fn world_pivot(&self, i: usize) -> Tq {
        let n = &self.nodes[i];
        let base = n.parent.map(|p| self.world_part(p)).unwrap_or(TQ_ID);
        comp(base, n.pre)
    }

    /// A node's position in the rig's rest pose (Studio over the pose
    /// Studio makes: parent's ∘ parent post ∘ pre ∘ T).
    fn rest_position(&self, i: usize) -> Vec3 {
        let n = &self.nodes[i];
        let base = match n.parent {
            Some(p) => comp(self.rest_frame(p), self.nodes[p].post),
            None => TQ_ID,
        };
        comp(base, n.pre).p
    }

    fn rest_frame(&self, i: usize) -> Tq {
        let n = &self.nodes[i];
        let base = match n.parent {
            Some(p) => comp(self.rest_frame(p), self.nodes[p].post),
            None => TQ_ID,
        };
        comp(comp(base, n.pre), n.t)
    }
}

/// AnimationRigData's binary properties (label u32s, parent u16s, names as
/// lengths then bytes, transforms as 12 floats R00..R22 X Y Z), each after a
/// u32 version and a u32 count. Entries with label 0, no name and no parent
/// past the first are unused slots.
pub fn parse_rig_data(doc: &Document, r: Ref) -> Option<Rig> {
    let bytes = |k: &str| -> Option<Vec<u8>> {
        match doc.prop(r, k) {
            Some(Variant::BinaryString(b)) => Some(AsRef::<[u8]>::as_ref(&b).to_vec()),
            Some(Variant::String(s)) => Some(s.into_bytes()),
            _ => None,
        }
    };
    let u32_at = |b: &[u8], o: usize| -> Option<u32> { b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])) };
    let label = bytes("label")?;
    let parent = bytes("parent")?;
    let name = bytes("name").unwrap_or_default();
    let (pre, post, tf) = (bytes("preTransform")?, bytes("postTransform")?, bytes("transform")?);
    let n = u32_at(&label, 4)? as usize;
    if n == 0 || n > 4096 {
        return None;
    }
    let cf = |b: &[u8], i: usize| -> Option<Tq> {
        let o = 8 + 48 * i;
        let f: Vec<f32> = (0..12).map(|k| u32_at(b, o + 4 * k).map(f32::from_bits)).collect::<Option<_>>()?;
        let rot = Mat3::from_cols(Vec3::new(f[0], f[3], f[6]), Vec3::new(f[1], f[4], f[7]), Vec3::new(f[2], f[5], f[8]));
        Some(Tq { q: Quat::from_mat3(&rot), p: Vec3::new(f[9], f[10], f[11]) })
    };
    let mut names = Vec::with_capacity(n);
    if name.len() >= 8 + 4 * n {
        let mut o = 8 + 4 * n;
        for i in 0..n {
            let l = u32_at(&name, 8 + 4 * i)? as usize;
            names.push(String::from_utf8_lossy(name.get(o..o + l)?).into_owned());
            o += l;
        }
    } else {
        names.resize(n, String::new());
    }
    let mut keep = vec![usize::MAX; n];
    let mut raw = Vec::new();
    for i in 0..n {
        let lab = u32_at(&label, 8 + 4 * i)?;
        let par = parent.get(8 + 2 * i..10 + 2 * i).map(|s| u16::from_le_bytes([s[0], s[1]]))?;
        if i > 0 && lab == 0 && par == 0 && names[i].is_empty() {
            continue;
        }
        keep[i] = raw.len();
        raw.push((i, lab, par, cf(&pre, i)?, cf(&post, i)?, cf(&tf, i)?));
    }
    let nodes = raw
        .into_iter()
        .map(|(i, lab, par, pre, post, t)| Node {
            name: names[i].clone(),
            label: lab,
            // 1-based parent indices, 0 = none.
            parent: (par as usize).checked_sub(1).and_then(|p| keep.get(p).copied()).filter(|&p| p != usize::MAX),
            pre,
            post,
            t,
        })
        .collect();
    Some(Rig { nodes })
}

/// A clip's source rig as Studio loads it: the AnimationRigData parsed,
/// then the same post-load fixup the character rig gets (Studio →
/// TPoser Studio, current Studio 63eb6055): the clip loaders
/// Studio take an AnimationRigData
/// child's rig into the clip and run that fixup on it. A clip rig stored
/// with an older T-pose (Locomotion_Cartoony_Idle's AnimationRigDataBlocky:
/// shoulders Rz(∓57.1°), elbows Rz(∓32.9°) on blocky arms) is re-posed with
/// the blocky adjustment, so on a blocky avatar it needs no correction.
pub fn clip_rig(doc: &Document, r: Ref) -> Option<Rig> {
    let mut rig = parse_rig_data(doc, r)?;
    tposer(&mut rig);
    Some(rig)
}

/// Part name → rig label (Studio's table; RigLabel values).
pub fn label_of_part(name: &str) -> u32 {
    match name {
        "LowerTorso" => 2,
        "LeftUpperLeg" => 3,
        "LeftLowerLeg" => 4,
        "LeftFoot" => 5,
        "RightUpperLeg" => 7,
        "RightLowerLeg" => 8,
        "RightFoot" => 9,
        "UpperTorso" => 11,
        "LeftUpperArm" => 12,
        "LeftLowerArm" => 13,
        "LeftHand" => 14,
        "RightUpperArm" => 16,
        "RightLowerArm" => 17,
        "RightHand" => 18,
        "Head" => 20,
        _ => 0,
    }
}

/// The rig Studio builds from a character model: a node per part reached
/// from the root part through Motor6Ds / AnimationConstraints (the root at
/// the origin), joint frame = Part0's rest frame ∘ C0, pre = parent's frame⁻¹
/// ∘ frame, post and T identity, then the TPoser. Live Transforms don't
/// enter (measured: LoadFromModel with a posed LeftShoulder gives the same
/// pre).
pub fn build_character_rig(doc: &Document, model: Ref, root_part: Ref) -> Rig {
    let mut joints: Vec<(Ref, Ref, Xform, Xform)> = Vec::new();
    for i in doc.dom.descendants_of(model) {
        if !crate::rig::is_animated_joint(&i.class) {
            continue;
        }
        if let Some((p0, p1, c0, c1)) = crate::rig::joint_frames(doc, i.referent()) {
            joints.push((p0, p1, c0, c1));
        }
    }
    let name = |r: Ref| doc.dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default();
    let mut nodes = vec![Node { name: name(root_part), label: label_of_part(&name(root_part)), parent: None, pre: TQ_ID, post: TQ_ID, t: TQ_ID }];
    // (part, node index, part rest frame, joint frame)
    let mut placed: Vec<(Ref, usize, Tq, Tq)> = vec![(root_part, 0, TQ_ID, TQ_ID)];
    let mut k = 0;
    while k < placed.len() {
        let (part, idx, rest, frame) = placed[k];
        k += 1;
        for &(p0, p1, c0, c1) in &joints {
            if p0 != part || placed.iter().any(|e| e.0 == p1) {
                continue;
            }
            let jf = comp(rest, tq_of(&c0));
            let p1_rest = comp(jf, tinv(tq_of(&c1)));
            nodes.push(Node {
                name: name(p1),
                label: label_of_part(&name(p1)),
                parent: Some(idx),
                pre: comp(tinv(frame), jf),
                post: TQ_ID,
                t: TQ_ID,
            });
            placed.push((p1, nodes.len() - 1, p1_rest, jf));
        }
    }
    let mut rig = Rig { nodes };
    tposer(&mut rig);
    rig
}

/// The TPoser (Studio with TPoserBlockyAdjustment2 and FixBentElbows
/// on, TPoseAdjusterDefaultVersion 2 → solver Studio): with
/// FixRetargetingMissingLimbs off (measured false) both arms' shoulder,
/// elbow and wrist must exist or nothing is posed; then, left arm first,
/// both T's reset to identity and the solver gives shoulder and elbow T.
pub fn tposer(rig: &mut Rig) {
    let arm = |r: &Rig, a: u32, b: u32, c: u32| Some((r.by_label(a)?, r.by_label(b)?, r.by_label(c)?));
    if arm(rig, 12, 13, 14).is_none() || arm(rig, 16, 17, 18).is_none() {
        return;
    }
    for (sh, el, wr, left) in [(12u32, 13u32, 14u32, true), (16, 17, 18, false)] {
        let Some((s, e, w)) = arm(rig, sh, el, wr) else { continue };
        rig.nodes[s].t = TQ_ID;
        rig.nodes[e].t = TQ_ID;
        let (ts, te) = tpose_arm(rig.world_pivot(s), rig.world_pivot(e), rig.world_pivot(w), left);
        rig.nodes[s].t = ts;
        rig.nodes[e].t = te;
    }
}

/// Studio: (shoulder T, elbow T) that lay the arm along ∓X, with the
/// blocky-mesh adjustment when the elbow bends in the arm's plane.
fn tpose_arm(sh: Tq, el: Tq, wr: Tq, left: bool) -> (Tq, Tq) {
    let l1 = comp(tinv(sh), el);
    let l2 = comp(tinv(el), wr);
    let d = el.p - sh.p;
    let dir = if left { Vec3::new(-1.0, -0.0, -0.0) } else { Vec3::new(1.0, 0.0, 0.0) };
    let mut upper_target = dir;
    let mut fore_target = dir;
    let n1 = normalize(l1.p);
    let lw = comp(tinv(sh), wr);
    let n2 = normalize(lw.p - l1.p);
    // n2 pointing straight down (relative tolerance 0.025, Studio).
    let down = if n2.y != -1.0 {
        let s = n2.y.abs() + 1.0;
        let tol = if s == f32::INFINITY { 0.025 } else { s * 0.025 };
        tol >= (n2.y - -1.0).abs()
    } else {
        true
    };
    let flat = n1.z.abs() <= 0.025 && down;
    let bend = vec_angle(n1, n2);
    if TPOSE_BLOCKY_ELBOW_BEND_MIN_DEG * std::f32::consts::PI / 180.0 < bend && flat {
        let p = if left {
            let (a, b) = (n1.y, -n1.x);
            let inv = 1.0 / (b * b + a * a + 0.0).sqrt();
            Vec3::new(a * inv, b * inv, inv * 0.0)
        } else {
            let (a, b) = (-n1.y, n1.x);
            let inv = 1.0 / (n1.x * n1.x + a * a + 0.0).sqrt();
            Vec3::new(a * inv, b * inv, inv * 0.0)
        };
        upper_target = vec_slerp(dir, p, TPOSE_BLOCKY_ADJUST_PERCENT / 100.0);
        // Studio: the rotation p → upper target, applied to dir.
        let r = Mat3::from_quat(quat_between(normalize(p), normalize(upper_target), 1.0));
        fore_target = r * dir;
    }
    let r1 = local_rotation_between(d, upper_target, sh);
    let r1 = comp(r1, twist_fix(comp(sh, r1)));
    let e2 = comp(comp(sh, r1), l1);
    let w2 = comp(e2, l2);
    let v = w2.p - e2.p;
    let lv = v.x * v.x + v.y * v.y + v.z * v.z;
    let lt = fore_target.x * fore_target.x + fore_target.y * fore_target.y + fore_target.z * fore_target.z;
    let mut r2 = TQ_ID;
    if lv >= 1e-6 && lt >= 1e-6 {
        let q = quat_between(v, fore_target, (lv * lt).sqrt());
        r2 = rot_only(comp(comp(tinv(e2), rot_only(q)), e2).q);
    }
    let r2 = comp(r2, twist_fix(comp(e2, r2)));
    (r1, r2)
}

// ------------------------------------------------------------- retargeter

#[derive(Clone, Copy, Debug)]
pub struct Correction {
    pub a: Quat,
    pub b: Quat,
    /// Applied (the bool Studio stores: min < max(angle A, angle B)).
    pub on: bool,
}

/// An R15ToR15Retargeter between a clip rig (source) and a character rig.
#[derive(Clone, Debug, Default)]
pub struct Retargeter {
    pub corrections: HashMap<u32, Correction>,
    /// The root translation scale (this), 1 when unset.
    pub root_scale: f32,
}

/// initialize: a correction per label 1..21
/// found in both rigs, and the hip scale.
pub fn initialize(src: &Rig, tgt: &Rig) -> Retargeter {
    let mut d: HashMap<u32, Quat> = HashMap::new();
    let mut corrections = HashMap::new();
    let lo = FK_MIN_DEG * std::f32::consts::PI / 180.0;
    let hi = FK_MAX_DEG * std::f32::consts::PI / 180.0;
    for label in 1u32..=21 {
        let (Some(i1), Some(i2)) = (src.by_label(label), tgt.by_label(label)) else { continue };
        let q1 = src.world_part(i1).q;
        let q2 = tgt.world_part(i2).q;
        let dl = qmul(qinv(q1), q2);
        d.insert(label, dl);
        let Some(p1) = src.nodes[i1].parent else { continue };
        let parent_label = src.nodes[p1].label;
        // A label not seen yet reads identity (std::map default, Studio).
        let dp = if label != 2 { *d.entry(parent_label).or_insert(Quat::IDENTITY) } else { Quat::IDENTITY };
        let mut a = qmul(qmul(qinv(tgt.nodes[i2].pre.q), qinv(dp)), src.nodes[i1].pre.q);
        let mut b = qmul(qmul(src.nodes[i1].post.q, dl), qinv(tgt.nodes[i2].post.q));
        let (aa, ab) = (corr_angle(a), corr_angle(b));
        if hi - lo > 0.0 {
            let ta = ((aa - lo) / (hi - lo)).clamp(0.0, 1.0);
            let tb = ((ab - lo) / (hi - lo)).clamp(0.0, 1.0);
            let ta = if (aa - lo) / (hi - lo) > 0.0 { ta } else { 0.0 };
            let tb = if (ab - lo) / (hi - lo) > 0.0 { tb } else { 0.0 };
            a = slerp(Quat::IDENTITY, a, ta);
            b = slerp(Quat::IDENTITY, b, tb);
        }
        corrections.insert(label, Correction { a, b, on: lo < aa.max(ab) });
    }
    Retargeter { corrections, root_scale: hip_scale(src, tgt) }
}

/// Hip translation scale (Studio with RemovePrebuiltR15AnimationRig):
/// the target's root and both legs' hip, knee and ankle in both rigs; the
/// scale is the mean target leg length over the mean source one, where a
/// leg is |knee − hip| + |ankle − knee| in the rest pose. Non-finite stays 1.
fn hip_scale(src: &Rig, tgt: &Rig) -> f32 {
    if tgt.by_label(2).is_none() {
        return 1.0;
    }
    let legs = |r: &Rig| -> Option<(f32, f32)> {
        let len = |h: u32, k: u32, a: u32| -> Option<f32> {
            let (h, k, a) = (r.rest_position(r.by_label(h)?), r.rest_position(r.by_label(k)?), r.rest_position(r.by_label(a)?));
            Some((h - k).length() + (k - a).length())
        };
        Some((len(3, 4, 5)?, len(7, 8, 9)?))
    };
    let (Some((sl, sr)), Some((tl, tr))) = (legs(src), legs(tgt)) else { return 1.0 };
    let s = (sr + sl) * 0.5;
    let t = (tr + tl) * 0.5;
    if 0.0 < s && 0.0 < t {
        let k = t / s;
        if k.is_finite() {
            return k;
        }
    }
    1.0
}

impl Retargeter {
    /// RetargetFK for one joint of a track's pose, then the
    /// hip step on the root (label 2).
    pub fn apply(&self, label: u32, xf: Xform) -> Xform {
        let mut out = xf;
        if let Some(c) = self.corrections.get(&label) {
            if c.on {
                let q = Quat::from_mat3(&xf.rot);
                let rot = qmul(qmul(c.a, q), c.b);
                let t = Quat::from_xyzw(xf.pos.x, xf.pos.y, xf.pos.z, 0.0);
                let tp = qmul(qmul(c.a, t), c.b);
                out = Xform { pos: Vec3::new(tp.x, tp.y, tp.z), rot: Mat3::from_quat(rot) };
            }
        }
        if label == 2 && self.root_scale != 1.0 {
            out.pos *= self.root_scale;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tq(axis: Vec3, ang: f32, p: Vec3) -> Tq {
        Tq { q: Quat::from_axis_angle(axis, ang), p }
    }

    /// The character Studio's Play spawned in the Laser Tag template, as its
    /// AnimationRigData:LoadFromModel dump gives it (checks/retargeting).
    fn studio_character() -> Rig {
        let mut r = Rig::default();
        let mut add = |name: &str, label: u32, parent: Option<usize>, p: Vec3| {
            r.nodes.push(Node { name: name.into(), label, parent, pre: Tq { q: Quat::IDENTITY, p }, post: TQ_ID, t: TQ_ID });
            r.nodes.len() - 1
        };
        let hrp = add("HumanoidRootPart", 0, None, Vec3::ZERO);
        let lt = add("LowerTorso", 2, Some(hrp), Vec3::new(0.0, -1.0, 0.0));
        let ut = add("UpperTorso", 11, Some(lt), Vec3::new(0.0, 0.4, 0.0));
        add("Head", 20, Some(ut), Vec3::new(0.0, 1.698, 0.0));
        let lua = add("LeftUpperArm", 12, Some(ut), Vec3::new(-0.9716, 1.4465, 0.0));
        let lla = add("LeftLowerArm", 13, Some(lua), Vec3::new(-0.5001, -0.7738, 0.0));
        add("LeftHand", 14, Some(lla), Vec3::new(0.0, -0.807, 0.0));
        let rua = add("RightUpperArm", 16, Some(ut), Vec3::new(0.9716, 1.4465, 0.0));
        let rla = add("RightLowerArm", 17, Some(rua), Vec3::new(0.5005, -0.7739, 0.0));
        add("RightHand", 18, Some(rla), Vec3::new(0.0, -0.8069, 0.0));
        let lul = add("LeftUpperLeg", 3, Some(lt), Vec3::new(-0.5, 0.0, 0.0));
        let lll = add("LeftLowerLeg", 4, Some(lul), Vec3::new(0.0, -0.9207, 0.0));
        add("LeftFoot", 5, Some(lll), Vec3::new(0.0, -1.0097, 0.0));
        let rul = add("RightUpperLeg", 7, Some(lt), Vec3::new(0.5, 0.0, 0.0));
        let rll = add("RightLowerLeg", 8, Some(rul), Vec3::new(0.0, -0.9206, 0.0));
        add("RightFoot", 9, Some(rll), Vec3::new(0.0, -1.0098, 0.0));
        r
    }

    /// The AnimationRigData of the Laser Tag character clips (assets
    /// 17747384020 / 17747381445 / 17747385966, identical): label, parent,
    /// pre, post, T as (qx qy qz qw px py pz).
    fn laser_tag_clip_rig() -> Rig {
        type N = (u32, Option<usize>, [f32; 7], [f32; 7], [f32; 7]);
        const NODES: [N; 21] = [
            (0, None, [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (2, Some(0), [0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0], [0.0, 0.0, 0.0, 1.0, 1e-06, 0.328173, 0.020128], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (11, Some(1), [0.0, 0.0, 0.0, 1.0, -1e-06, 0.071824, -0.020128], [0.0, 0.0, 0.0, 1.0, -1e-06, 0.650381, -0.019249], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (20, Some(2), [0.0, 0.0, 0.0, 1.0, 0.0, 0.601472, 0.0], [0.0, 0.0, 0.0, 1.0, 4e-06, 0.585757, -0.001178], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (21, Some(3), [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (3, Some(1), [0.0, 0.0, 0.0, 1.0, -0.500001, -0.328174, -0.020128], [0.0, 0.0, 0.0, 1.0, 0.109618, -0.435645, 0.010279], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (4, Some(5), [0.0, 0.0, 0.0, 1.0, -0.109618, -0.522849, -0.010279], [0.0, 0.0, 0.0, 1.0, 0.079929, -0.283395, 0.010279], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (5, Some(6), [0.0, 0.0, 0.0, 1.0, -0.07993, -0.284938, -0.010283], [0.0, 0.0, 0.0, 1.0, 0.079928, -0.061168, -0.184421], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (6, Some(7), [0.0, 0.0, 0.0, 1.0, 0.0, -0.41003, -0.310751], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (7, Some(1), [0.0, 0.0, 0.0, 1.0, 0.499999, -0.328174, -0.020148], [0.0, 0.0, 0.0, 1.0, -0.109618, -0.435641, 0.010273], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (8, Some(9), [0.0, 0.0, 0.0, 1.0, 0.109618, -0.522712, -0.010273], [0.0, 0.0, 0.0, 1.0, -0.079929, -0.283525, 0.010232], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (9, Some(10), [0.0, 0.0, 0.0, 1.0, 0.07993, -0.284935, -0.01022], [0.0, 0.0, 0.0, 1.0, -0.079929, -0.061179, -0.184511], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (10, Some(11), [0.0, 0.0, 0.0, 1.0, 0.0, -0.410036, -0.310757], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (12, Some(2), [0.0, 0.0, 0.0, 1.0, -0.916025, 0.603152, 0.019248], [0.0, 0.0, 0.0, 1.0, -0.231646, -0.338694, 0.07464], [0.0, -0.0, -0.49726, 0.867602, 0.0, 0.0, 0.0]),
            (13, Some(13), [0.0, 0.0, 0.0, 1.0, -0.10683, -0.239097, -0.074639], [0.0, 0.0, 0.0, 1.0, -0.1392, -0.295419, -0.000861], [-0.118824, 0.050349, -0.127689, 0.983382, 0.0, 0.0, 0.0]),
            (14, Some(14), [0.0, 0.0, 0.0, 1.0, -0.068576, -0.450478, -0.204142], [0.0, 0.0, 0.0, 1.0, -0.044037, -0.2336, -0.124126], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (15, Some(15), [0.0, 0.0, 0.0, 1.0, -0.073118, -0.387864, -0.206096], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (16, Some(2), [0.0, 0.0, 0.0, 1.0, 0.916001, 0.603152, 0.019248], [0.0, 0.0, 0.0, 1.0, 0.231771, -0.338694, 0.074639], [0.0, 0.0, 0.497595, 0.86741, 0.0, 0.0, 0.0]),
            (17, Some(17), [0.0, 0.0, 0.0, 1.0, 0.106228, -0.239305, -0.074638], [0.0, 0.0, 0.0, 1.0, 0.139703, -0.29521, -0.000862], [-0.118814, -0.050308, 0.127191, 0.983451, 0.0, 0.0, 0.0]),
            (18, Some(18), [0.0, 0.0, 0.0, 1.0, 0.068297, -0.45079, -0.204141], [0.0, 0.0, 0.0, 1.0, 0.044315, -0.233288, -0.124126], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            (19, Some(19), [0.0, 0.0, 0.0, 1.0, 0.073641, -0.387673, -0.20627], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
        ];
        let t = |a: [f32; 7]| Tq { q: Quat::from_xyzw(a[0], a[1], a[2], a[3]), p: Vec3::new(a[4], a[5], a[6]) };
        Rig {
            nodes: NODES
                .iter()
                .map(|&(label, parent, pre, post, tf)| Node { name: String::new(), label, parent, pre: t(pre), post: t(post), t: t(tf) })
                .collect(),
        }
    }

    fn aa(q: Quat) -> Vec3 {
        let q = if q.w < 0.0 { -q } else { q };
        let (axis, angle) = q.to_axis_angle();
        axis * angle
    }

    /// The clip-load fixup (clip_rig) leaves the Laser Tag clips' rig as
    /// stored: its arms aren't the flat blocky kind, so the TPoser gives
    /// back the same T (the measured Laser Tag poses stay matched).
    #[test]
    fn clip_fixup_keeps_laser_tag_rig() {
        let raw = laser_tag_clip_rig();
        let mut fixed = raw.clone();
        tposer(&mut fixed);
        for (a, b) in raw.nodes.iter().zip(&fixed.nodes) {
            assert!(a.t.q.dot(b.t.q).abs() > 0.99999, "{}: {:?} vs {:?}", a.name, a.t.q, b.t.q);
        }
    }

    #[test]
    fn laser_tag_idle_retargets_as_studio() {
        // checks/retargeting/studio.txt, rig "Default" (the avatar Studio's
        // Play spawns), Idle at 0.6 s: Disabled → Default.
        let mut tgt = studio_character();
        tposer(&mut tgt);
        let rt = initialize(&laser_tag_clip_rig(), &tgt);
        let pose = |v: Vec3, p: Vec3| Xform { pos: p, rot: Mat3::from_quat(Quat::from_scaled_axis(v)) };
        let cases = [
            (17, Vec3::new(1.78174, 0.0, 0.0), Vec3::new(1.83222, -0.10318, -0.05518)),
            (12, Vec3::new(1.13592, -0.80754, 0.26370), Vec3::new(1.17627, -0.74215, 0.17181)),
        ];
        for (label, input, want) in cases {
            let got = aa(Quat::from_mat3(&rt.apply(label, pose(input, Vec3::ZERO)).rot));
            assert!((got - want).abs().max_element() < 2e-4, "label {label}: {got:?} vs {want:?}");
        }
        // RootMove at 0.6 s: the root's translation × the leg-length ratio.
        let root = rt.apply(2, pose(Vec3::new(0.19999, 0.0, 0.0), Vec3::new(0.1, 0.48, 0.39999)));
        assert!((root.pos - Vec3::new(0.12643, 0.60688, 0.50572)).abs().max_element() < 2e-4, "{:?}", root.pos);
        // Labels without a correction pass through.
        let neck = rt.apply(20, pose(Vec3::new(0.1, 0.2, 0.0), Vec3::ZERO));
        assert!(aa(Quat::from_mat3(&neck.rot)).distance(Vec3::new(0.1, 0.2, 0.0)) < 1e-5);
    }

    #[test]
    fn tposer_matches_studio_dump() {
        // LoadFromModel on Studio's spawned character: upper arms T =
        // Rz(∓78.16°) (cos 0.2051), elbows identity.
        let mut r = studio_character();
        tposer(&mut r);
        let m = Mat3::from_quat(r.nodes[r.by_label(12).unwrap()].t.q);
        assert!((m.x_axis.x - 0.2051).abs() < 2e-4, "{m:?}");
        assert!((m.y_axis.x - 0.9787).abs() < 2e-4, "{m:?}");
        let e = r.nodes[r.by_label(13).unwrap()].t.q;
        assert!(e.angle_between(Quat::IDENTITY) < 1e-3, "{e:?}");
        let m = Mat3::from_quat(r.nodes[r.by_label(16).unwrap()].t.q);
        assert!((m.y_axis.x + 0.9787).abs() < 2e-4, "{m:?}");
    }

    #[test]
    fn rotated_shoulder_attachments_match_studio() {
        // Studio dump B (checks/retargeting/studio/rot-B): RightShoulder's
        // attachments turned by Rz(0.5): pre Rz(0.5) at the shoulder, the
        // elbow's pre Rz(−0.5) at (0.0682, −0.9192), T shoulder cos 0.5431,
        // T elbow cos 0.8397.
        let mut r = studio_character();
        let s = r.by_label(16).unwrap();
        let e = r.by_label(17).unwrap();
        r.nodes[s].pre = tq(Vec3::Z, 0.5, Vec3::new(0.9716, 1.4465, 0.0));
        r.nodes[e].pre = tq(Vec3::Z, -0.5, Vec3::new(0.0682, -0.9192, 0.0));
        tposer(&mut r);
        let ms = Mat3::from_quat(r.nodes[s].t.q);
        let me = Mat3::from_quat(r.nodes[e].t.q);
        assert!((ms.x_axis.x - 0.5431).abs() < 5e-4 && (ms.y_axis.x + 0.8397).abs() < 5e-4, "{ms:?}");
        assert!((me.x_axis.x - 0.8397).abs() < 5e-4 && (me.y_axis.x + 0.5431).abs() < 5e-4, "{me:?}");
    }
}
