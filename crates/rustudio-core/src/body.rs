//! Body parts, rigid joints and humanoid states: the little that the physics
//! simulation, the renderer's skinning and the Engine's rigs all read. Part
//! names are Roblox's, joints hold two parts by Part0 / Part1 (or by
//! attachments), and a Humanoid is in one state at a time.

use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{Ref, Variant};

use crate::doc::{self, Document};
use crate::math::Xform;

/// Which body colour a part takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limb {
    Head,
    Torso,
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
    Root,
}

/// Which body colour an R6 / R15 part name takes.
/// A character body part's name (R6 or R15, Head included).
pub fn is_body_part_name(name: &str) -> bool {
    limb_of(name).is_some()
}

pub fn limb_of(name: &str) -> Option<Limb> {
    Some(match name {
        "Head" => Limb::Head,
        "Torso" | "UpperTorso" | "LowerTorso" => Limb::Torso,
        "Left Arm" | "LeftUpperArm" | "LeftLowerArm" | "LeftHand" => Limb::LeftArm,
        "Right Arm" | "RightUpperArm" | "RightLowerArm" | "RightHand" => Limb::RightArm,
        "Left Leg" | "LeftUpperLeg" | "LeftLowerLeg" | "LeftFoot" => Limb::LeftLeg,
        "Right Leg" | "RightUpperLeg" | "RightLowerLeg" | "RightFoot" => Limb::RightLeg,
        _ => return None,
    })
}

/// Joint classes that hold two parts rigidly together (one assembly).
pub fn is_rigid_joint(class: &str) -> bool {
    matches!(
        class,
        "RigidConstraint"
            | "WeldConstraint"
            | "Weld"
            | "ManualWeld"
            | "Motor6D"
            | "Motor"
            | "AnimationConstraint"
            | "Snap"
            | "Glue"
            | "ManualGlue"
    )
}

/// Whether a joint or constraint is on. WeldConstraint never saves or
/// replicates `Enabled`: a file and the Team Create wire carry `State`
/// (Studio saves 3 for an enabled weld and 0 for a disabled one; the live
/// wire sends 1), so bit 0 is Enabled. A script's write lands in `Enabled`.
pub fn joint_enabled(doc: &Document, j: Ref) -> bool {
    let Some(i) = doc.dom.get_by_ref(j) else { return false };
    match i.properties.get(&crate::ukey!("Enabled")) {
        Some(Variant::Bool(b)) => return *b,
        _ => {}
    }
    match i.properties.get(&crate::ukey!("State")) {
        Some(Variant::Int32(s)) if i.class == "WeldConstraint" => s & 1 != 0,
        _ => !matches!(doc.prop(j, "Enabled"), Some(Variant::Bool(false))),
    }
}

/// The part an attachment belongs to (its nearest BasePart ancestor).
pub fn attachment_part(doc: &Document, a: Ref) -> Option<Ref> {
    let mut cur = doc.dom.get_by_ref(a)?.parent();
    while let Some(i) = doc.dom.get_by_ref(cur) {
        if doc::is_a(&i.class, "BasePart") {
            return Some(cur);
        }
        cur = i.parent();
    }
    None
}

/// The two parts a rigid joint holds: Part0 / Part1 (alias-aware: files and
/// the live wire keep a WeldConstraint's as Part0Internal / Part1Internal),
/// a RigidConstraint's attachments' parts, or an AnimationConstraint's
/// (`joint_frames`: its Part0 / Part1 are derived from its attachments and
/// neither saved nor replicated, api-dump.json; R15 rigs with
/// StarterPlayer.AvatarJointUpgrade are jointed with them). None while
/// either part is not in the DOM (a reference whose instance was removed,
/// or hasn't arrived): such a joint holds nothing.
pub fn joint_parts(doc: &Document, j: Ref) -> Option<(Ref, Ref)> {
    let class = doc.class_of(j)?;
    let pr = |k: rbx_dom_weak::Ustr| match doc.prop_key(j, k) {
        Some(Variant::Ref(x)) if x.is_some() && doc.dom.get_by_ref(x).is_some() => Some(x),
        _ => None,
    };
    if class == "RigidConstraint" {
        return Some((
            attachment_part(doc, pr(crate::ukey!("Attachment0"))?)?,
            attachment_part(doc, pr(crate::ukey!("Attachment1"))?)?,
        ));
    }
    if class == "AnimationConstraint" {
        return joint_frames(doc, j).map(|(p0, p1, _, _)| (p0, p1));
    }
    Some((pr(crate::ukey!("Part0"))?, pr(crate::ukey!("Part1"))?))
}

/// A joint's two parts and frames, for every class that holds parts by a
/// C0 / C1 pair: JointInstances by Part0 / Part1 / C0 / C1, an
/// AnimationConstraint by its attachments (Studio derives its Part0 /
/// Part1 / C0 / C1 from them).
pub fn joint_frames(doc: &Document, r: Ref) -> Option<(Ref, Ref, Xform, Xform)> {
    joint_frames_for(doc, doc.dom.get_by_ref(r)?)
}

/// Resolve joint frames when the joint instance is already available.
pub fn joint_frames_for(doc: &Document, i: &rbx_dom_weak::Instance) -> Option<(Ref, Ref, Xform, Xform)> {
    let refp = |k: rbx_dom_weak::Ustr| match i.properties.get(&k) {
        Some(Variant::Ref(x)) if x.is_some() && doc.dom.get_by_ref(*x).is_some() => Some(*x),
        _ => None,
    };
    let cf_of = |x: &rbx_dom_weak::Instance, k: rbx_dom_weak::Ustr| match x.properties.get(&k) {
        Some(Variant::CFrame(c)) => Xform::from_cframe(c),
        _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
    };
    if i.class == "AnimationConstraint" {
        if let (Some(a0), Some(a1)) = (refp(crate::ukey!("Attachment0")), refp(crate::ukey!("Attachment1"))) {
            let (i0, i1) = (doc.dom.get_by_ref(a0)?, doc.dom.get_by_ref(a1)?);
            let (p0, p1) = (i0.parent(), i1.parent());
            let part = |p: Ref| doc.class_of(p).is_some_and(|k| doc::is_a(&k, "BasePart"));
            if part(p0) && part(p1) {
                let cframe = crate::ukey!("CFrame");
                return Some((p0, p1, cf_of(i0, cframe), cf_of(i1, cframe)));
            }
            return None;
        }
    }
    Some((refp(crate::ukey!("Part0"))?, refp(crate::ukey!("Part1"))?, cf_of(i, crate::ukey!("C0")), cf_of(i, crate::ukey!("C1"))))
}

/// Humanoid states this simulation produces (Enum.HumanoidStateType names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Jumping,
    Freefall,
    Landed,
    Dead,
    Seated,
    Climbing,
    Swimming,
    FallingDown,
    GettingUp,
    PlatformStanding,
    Ragdoll,
    Physics,
}

impl State {
    /// From an Enum.HumanoidStateType name (the states this simulation has).
    pub fn from_name(n: &str) -> Option<State> {
        Some(match n {
            "Running" | "RunningNoPhysics" => State::Running,
            "Jumping" => State::Jumping,
            "Freefall" => State::Freefall,
            "Landed" => State::Landed,
            "Dead" => State::Dead,
            "Seated" => State::Seated,
            "Climbing" => State::Climbing,
            "Swimming" => State::Swimming,
            "FallingDown" => State::FallingDown,
            "GettingUp" => State::GettingUp,
            "PlatformStanding" => State::PlatformStanding,
            "Ragdoll" => State::Ragdoll,
            "Physics" => State::Physics,
            _ => return None,
        })
    }

    pub fn enum_name(self) -> &'static str {
        match self {
            State::Running => "Running",
            State::Jumping => "Jumping",
            State::Freefall => "Freefall",
            State::Landed => "Landed",
            State::Dead => "Dead",
            State::Seated => "Seated",
            State::Climbing => "Climbing",
            State::Swimming => "Swimming",
            State::FallingDown => "FallingDown",
            State::GettingUp => "GettingUp",
            State::PlatformStanding => "PlatformStanding",
            State::Ragdoll => "Ragdoll",
            State::Physics => "Physics",
        }
    }
}
