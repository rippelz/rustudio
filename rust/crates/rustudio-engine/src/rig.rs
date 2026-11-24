//! Humanoid characters: R6 and R15 rigs with the documented joint and
//! attachment names (Motor6D + RigAttachments + HumanoidRootPart), posing
//! through Motor6D C0/C1/Transform, accessories attached by matching
//! attachment names, and a simple procedural walk / idle / jump animation.
//!
//! The default body is rustudio's own blocky design made of plain parts;
//! sizes and joint positions are estimates (see docs/MECHANICS.md).

use std::collections::HashMap;

use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{CFrame, Color3uint8, Enum, Ref, Variant, Vector3};
use rbx_dom_weak::{ustr, InstanceBuilder};

use rustudio_core::body::{joint_frames, limb_of, Limb};
use rustudio_core::doc::{self, Document};
use crate::ltypes::{cf_inv, cf_mul};
use rustudio_core::math::Xform;
use rustudio_core::mech;
use rustudio_render::scene::{f32_of, vec3_of};

pub use rustudio_core::rig_type::RigType;

/// Whether a character part collides on Studio's spawned character.
pub fn studio_collides(name: &str, rig: RigType) -> bool {
    match rig {
        RigType::R15 => matches!(name, "HumanoidRootPart" | "UpperTorso" | "LowerTorso" | "Head"),
        RigType::R6 => matches!(name, "Torso" | "Head"),
    }
}

/// Body colours (Roblox BodyColors), as RGB.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyColors {
    pub head: [u8; 3],
    pub torso: [u8; 3],
    pub left_arm: [u8; 3],
    pub right_arm: [u8; 3],
    pub left_leg: [u8; 3],
    pub right_leg: [u8; 3],
}

impl Default for BodyColors {
    fn default() -> Self {
        let skin = mech::CHARACTER_SKIN;
        BodyColors {
            head: skin,
            torso: mech::CHARACTER_SHIRT,
            left_arm: skin,
            right_arm: skin,
            left_leg: mech::CHARACTER_PANTS,
            right_leg: mech::CHARACTER_PANTS,
        }
    }
}

/// R15 body scales (HumanoidDescription / avatar scales).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scales {
    pub height: f32,
    pub width: f32,
    pub depth: f32,
    pub head: f32,
    /// BodyTypeScale / ProportionScale: how far Classic parts go toward the
    /// ProportionsNormal / ProportionsSlender shapes.
    pub body_type: f32,
    pub proportion: f32,
}

impl Default for Scales {
    /// A default HumanoidDescription's (Studio's own Play character when no
    /// avatar is loaded): BodyTypeScale 0.3, ProportionScale 1.
    fn default() -> Self {
        Scales {
            height: 1.0,
            width: 1.0,
            depth: 1.0,
            head: 1.0,
            body_type: 0.3,
            proportion: 1.0,
        }
    }
}

/// Which R15 body-type proportion row a part takes.
pub(crate) fn part_kind(name: &str) -> Option<usize> {
    Some(match name {
        "UpperTorso" => 0,
        "LowerTorso" => 1,
        "Head" => 2,
        "LeftUpperArm" | "LeftLowerArm" | "RightUpperArm" | "RightLowerArm" => 3,
        "LeftHand" | "RightHand" => 4,
        "LeftUpperLeg" | "RightUpperLeg" => 5,
        "LeftLowerLeg" | "RightLowerLeg" => 6,
        "LeftFoot" | "RightFoot" => 7,
        _ => return None,
    })
}

/// Studio's R15 body-type factors relative to Classic (measured,
/// domains/avatar_apply.luau): BodyType 1 with Proportion 0
/// (ProportionsNormal) and with Proportion 1 (ProportionsSlender).
pub(crate) const PROPORTIONS_NORMAL: [[f32; 3]; 8] = [
    [1.033, 1.309, 1.14],
    [1.033, 1.309, 1.14],
    [0.942, 0.942, 0.942],
    [1.129, 1.342, 1.132],
    [1.066, 1.174, 1.231],
    [1.023, 1.506, 1.023],
    [1.023, 1.506, 1.023],
    [1.079, 1.267, 1.129],
];
pub(crate) const PROPORTIONS_SLENDER: [[f32; 3]; 8] = [
    [0.905346, 1.204232, 1.013333],
    [0.985687, 1.004605, 1.013333],
    [0.896289, 0.942, 0.896289],
    [1.003556, 1.207921, 1.006222],
    [0.947556, 1.174, 1.094222],
    [0.976145, 1.40093, 0.909333],
    [0.976145, 1.300518, 0.909333],
    [1.02958, 1.133274, 1.003556],
];

/// The proportions a body type / proportion pair asks of a part, relative
/// to Classic: lerp(1, lerp(Normal, Slender, proportion), body_type).
pub(crate) fn proportion_target(name: &str, proportion: f32, body_type: f32) -> Vec3 {
    let Some(k) = part_kind(name) else { return Vec3::ONE };
    let n = Vec3::from(PROPORTIONS_NORMAL[k]);
    let sl = Vec3::from(PROPORTIONS_SLENDER[k]);
    Vec3::ONE.lerp(n.lerp(sl, proportion), body_type)
}


/// A body part of the default rig: name, size, centre (relative to the
/// HumanoidRootPart centre), colour slot.
struct PartSpec {
    name: &'static str,
    size: [f32; 3],
    center: [f32; 3],
    limb: Limb,
}

/// A Motor6D: name, Part0, Part1, joint position (relative to the root).
struct JointSpec {
    name: &'static str,
    part0: &'static str,
    part1: &'static str,
    at: [f32; 3],
    /// R15 RigAttachment base name (e.g. "LeftShoulder"); None for R6.
    rig_attachment: Option<&'static str>,
}

const R15_PARTS: &[PartSpec] = &[
    PartSpec {
        name: "HumanoidRootPart",
        size: [2.0, 2.0, 1.0],
        center: [0.0, 0.0, 0.0],
        limb: Limb::Root,
    },
    PartSpec {
        name: "LowerTorso",
        size: [2.0, 0.4, 1.0],
        center: [0.0, -0.2, 0.0],
        limb: Limb::Torso,
    },
    PartSpec {
        name: "UpperTorso",
        size: [2.0, 1.6, 1.0],
        center: [0.0, 0.8, 0.0],
        limb: Limb::Torso,
    },
    PartSpec {
        name: "Head",
        size: [1.2, 1.2, 1.2],
        center: [0.0, 2.2, 0.0],
        limb: Limb::Head,
    },
    PartSpec {
        name: "LeftUpperArm",
        size: [1.0, 1.0, 1.0],
        center: [-1.5, 1.1, 0.0],
        limb: Limb::LeftArm,
    },
    PartSpec {
        name: "LeftLowerArm",
        size: [1.0, 0.95, 1.0],
        center: [-1.5, 0.125, 0.0],
        limb: Limb::LeftArm,
    },
    PartSpec {
        name: "LeftHand",
        size: [1.0, 0.3, 1.0],
        center: [-1.5, -0.5, 0.0],
        limb: Limb::LeftArm,
    },
    PartSpec {
        name: "RightUpperArm",
        size: [1.0, 1.0, 1.0],
        center: [1.5, 1.1, 0.0],
        limb: Limb::RightArm,
    },
    PartSpec {
        name: "RightLowerArm",
        size: [1.0, 0.95, 1.0],
        center: [1.5, 0.125, 0.0],
        limb: Limb::RightArm,
    },
    PartSpec {
        name: "RightHand",
        size: [1.0, 0.3, 1.0],
        center: [1.5, -0.5, 0.0],
        limb: Limb::RightArm,
    },
    PartSpec {
        name: "LeftUpperLeg",
        size: [1.0, 1.15, 1.0],
        center: [-0.5, -0.975, 0.0],
        limb: Limb::LeftLeg,
    },
    PartSpec {
        name: "LeftLowerLeg",
        size: [1.0, 1.15, 1.0],
        center: [-0.5, -2.125, 0.0],
        limb: Limb::LeftLeg,
    },
    PartSpec {
        name: "LeftFoot",
        size: [1.0, 0.3, 1.0],
        center: [-0.5, -2.85, 0.0],
        limb: Limb::LeftLeg,
    },
    PartSpec {
        name: "RightUpperLeg",
        size: [1.0, 1.15, 1.0],
        center: [0.5, -0.975, 0.0],
        limb: Limb::RightLeg,
    },
    PartSpec {
        name: "RightLowerLeg",
        size: [1.0, 1.15, 1.0],
        center: [0.5, -2.125, 0.0],
        limb: Limb::RightLeg,
    },
    PartSpec {
        name: "RightFoot",
        size: [1.0, 0.3, 1.0],
        center: [0.5, -2.85, 0.0],
        limb: Limb::RightLeg,
    },
];

const R15_JOINTS: &[JointSpec] = &[
    JointSpec {
        name: "Root",
        part0: "HumanoidRootPart",
        part1: "LowerTorso",
        at: [0.0, -0.2, 0.0],
        rig_attachment: Some("Root"),
    },
    JointSpec {
        name: "Waist",
        part0: "LowerTorso",
        part1: "UpperTorso",
        at: [0.0, 0.0, 0.0],
        rig_attachment: Some("Waist"),
    },
    JointSpec {
        name: "Neck",
        part0: "UpperTorso",
        part1: "Head",
        at: [0.0, 1.6, 0.0],
        rig_attachment: Some("Neck"),
    },
    JointSpec {
        name: "LeftShoulder",
        part0: "UpperTorso",
        part1: "LeftUpperArm",
        at: [-1.5, 1.4, 0.0],
        rig_attachment: Some("LeftShoulder"),
    },
    JointSpec {
        name: "LeftElbow",
        part0: "LeftUpperArm",
        part1: "LeftLowerArm",
        at: [-1.5, 0.6, 0.0],
        rig_attachment: Some("LeftElbow"),
    },
    JointSpec {
        name: "LeftWrist",
        part0: "LeftLowerArm",
        part1: "LeftHand",
        at: [-1.5, -0.35, 0.0],
        rig_attachment: Some("LeftWrist"),
    },
    JointSpec {
        name: "RightShoulder",
        part0: "UpperTorso",
        part1: "RightUpperArm",
        at: [1.5, 1.4, 0.0],
        rig_attachment: Some("RightShoulder"),
    },
    JointSpec {
        name: "RightElbow",
        part0: "RightUpperArm",
        part1: "RightLowerArm",
        at: [1.5, 0.6, 0.0],
        rig_attachment: Some("RightElbow"),
    },
    JointSpec {
        name: "RightWrist",
        part0: "RightLowerArm",
        part1: "RightHand",
        at: [1.5, -0.35, 0.0],
        rig_attachment: Some("RightWrist"),
    },
    JointSpec {
        name: "LeftHip",
        part0: "LowerTorso",
        part1: "LeftUpperLeg",
        at: [-0.5, -0.4, 0.0],
        rig_attachment: Some("LeftHip"),
    },
    JointSpec {
        name: "LeftKnee",
        part0: "LeftUpperLeg",
        part1: "LeftLowerLeg",
        at: [-0.5, -1.55, 0.0],
        rig_attachment: Some("LeftKnee"),
    },
    JointSpec {
        name: "LeftAnkle",
        part0: "LeftLowerLeg",
        part1: "LeftFoot",
        at: [-0.5, -2.7, 0.0],
        rig_attachment: Some("LeftAnkle"),
    },
    JointSpec {
        name: "RightHip",
        part0: "LowerTorso",
        part1: "RightUpperLeg",
        at: [0.5, -0.4, 0.0],
        rig_attachment: Some("RightHip"),
    },
    JointSpec {
        name: "RightKnee",
        part0: "RightUpperLeg",
        part1: "RightLowerLeg",
        at: [0.5, -1.55, 0.0],
        rig_attachment: Some("RightKnee"),
    },
    JointSpec {
        name: "RightAnkle",
        part0: "RightLowerLeg",
        part1: "RightFoot",
        at: [0.5, -2.7, 0.0],
        rig_attachment: Some("RightAnkle"),
    },
];

const R6_PARTS: &[PartSpec] = &[
    PartSpec {
        name: "HumanoidRootPart",
        size: [2.0, 2.0, 1.0],
        center: [0.0, 0.0, 0.0],
        limb: Limb::Root,
    },
    PartSpec {
        name: "Torso",
        size: [2.0, 2.0, 1.0],
        center: [0.0, 0.0, 0.0],
        limb: Limb::Torso,
    },
    PartSpec {
        name: "Head",
        size: [1.2, 1.2, 1.2],
        center: [0.0, 1.6, 0.0],
        limb: Limb::Head,
    },
    PartSpec {
        name: "Left Arm",
        size: [1.0, 2.0, 1.0],
        center: [-1.5, 0.0, 0.0],
        limb: Limb::LeftArm,
    },
    PartSpec {
        name: "Right Arm",
        size: [1.0, 2.0, 1.0],
        center: [1.5, 0.0, 0.0],
        limb: Limb::RightArm,
    },
    PartSpec {
        name: "Left Leg",
        size: [1.0, 2.0, 1.0],
        center: [-0.5, -2.0, 0.0],
        limb: Limb::LeftLeg,
    },
    PartSpec {
        name: "Right Leg",
        size: [1.0, 2.0, 1.0],
        center: [0.5, -2.0, 0.0],
        limb: Limb::RightLeg,
    },
];

const R6_JOINTS: &[JointSpec] = &[
    JointSpec {
        name: "RootJoint",
        part0: "HumanoidRootPart",
        part1: "Torso",
        at: [0.0, 0.0, 0.0],
        rig_attachment: None,
    },
    JointSpec {
        name: "Neck",
        part0: "Torso",
        part1: "Head",
        at: [0.0, 1.0, 0.0],
        rig_attachment: None,
    },
    JointSpec {
        name: "Left Shoulder",
        part0: "Torso",
        part1: "Left Arm",
        at: [-1.5, 0.5, 0.0],
        rig_attachment: None,
    },
    JointSpec {
        name: "Right Shoulder",
        part0: "Torso",
        part1: "Right Arm",
        at: [1.5, 0.5, 0.0],
        rig_attachment: None,
    },
    JointSpec {
        name: "Left Hip",
        part0: "Torso",
        part1: "Left Leg",
        at: [-0.5, -1.0, 0.0],
        rig_attachment: None,
    },
    JointSpec {
        name: "Right Hip",
        part0: "Torso",
        part1: "Right Leg",
        at: [0.5, -1.0, 0.0],
        rig_attachment: None,
    },
];

/// Accessory attachments (documented names) on the default body: (part, name, offset from the part centre).
const ACCESSORY_ATTACHMENTS: &[(&str, &str, [f32; 3])] = &[
    ("Head", "HatAttachment", [0.0, 0.6, 0.0]),
    ("Head", "HairAttachment", [0.0, 0.6, 0.0]),
    ("Head", "FaceFrontAttachment", [0.0, 0.0, -0.6]),
    ("Head", "FaceCenterAttachment", [0.0, 0.0, 0.0]),
    ("UpperTorso", "NeckAttachment", [0.0, 0.8, 0.0]),
    ("UpperTorso", "BodyFrontAttachment", [0.0, 0.2, -0.5]),
    ("UpperTorso", "BodyBackAttachment", [0.0, 0.2, 0.5]),
    ("UpperTorso", "LeftCollarAttachment", [-1.0, 0.8, 0.0]),
    ("UpperTorso", "RightCollarAttachment", [1.0, 0.8, 0.0]),
    ("LowerTorso", "WaistFrontAttachment", [0.0, 0.0, -0.5]),
    ("LowerTorso", "WaistBackAttachment", [0.0, 0.0, 0.5]),
    ("LowerTorso", "WaistCenterAttachment", [0.0, 0.0, 0.0]),
    ("LeftUpperArm", "LeftShoulderAttachment", [0.0, 0.5, 0.0]),
    ("RightUpperArm", "RightShoulderAttachment", [0.0, 0.5, 0.0]),
    ("LeftHand", "LeftGripAttachment", [0.0, -0.15, 0.0]),
    ("RightHand", "RightGripAttachment", [0.0, -0.15, 0.0]),
    ("LeftFoot", "LeftFootAttachment", [0.0, -0.15, 0.0]),
    ("RightFoot", "RightFootAttachment", [0.0, -0.15, 0.0]),
    ("HumanoidRootPart", "RootAttachment", [0.0, 0.0, 0.0]),
    // R6 equivalents
    ("Torso", "NeckAttachment", [0.0, 1.0, 0.0]),
    ("Torso", "BodyFrontAttachment", [0.0, 0.0, -0.5]),
    ("Torso", "BodyBackAttachment", [0.0, 0.0, 0.5]),
    ("Torso", "WaistFrontAttachment", [0.0, -1.0, -0.5]),
    ("Torso", "WaistBackAttachment", [0.0, -1.0, 0.5]),
    ("Left Arm", "LeftShoulderAttachment", [0.0, 1.0, 0.0]),
    ("Right Arm", "RightShoulderAttachment", [0.0, 1.0, 0.0]),
    ("Left Arm", "LeftGripAttachment", [0.0, -1.0, 0.0]),
    ("Right Arm", "RightGripAttachment", [0.0, -1.0, 0.0]),
];

fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::from(a)
}

fn rv(v: Vec3) -> Vector3 {
    Vector3::new(v.x, v.y, v.z)
}

fn cf_at(p: Vec3) -> CFrame {
    Xform {
        pos: p,
        rot: Mat3::IDENTITY,
    }
    .to_cframe()
}

/// Scales a vector in character space (R15 scaling; the head uses its own scale).
pub(crate) fn scale_vec(v: Vec3, s: &Scales, head: bool) -> Vec3 {
    if head {
        v * s.head
    } else {
        Vec3::new(v.x * s.width, v.y * s.height, v.z * s.depth)
    }
}

/// Builds the default blocky character Model (HumanoidRootPart, body parts,
/// Motor6Ds, RigAttachments, accessory attachments, Humanoid, BodyColors).
/// `humanoid` carries WalkSpeed / jump settings taken from StarterPlayer.
pub fn build_default(
    name: &str,
    rig: RigType,
    colors: &BodyColors,
    scales: &Scales,
    humanoid: InstanceBuilder,
) -> InstanceBuilder {
    let (parts, joints) = match rig {
        RigType::R6 => (R6_PARTS, R6_JOINTS),
        RigType::R15 => (R15_PARTS, R15_JOINTS),
    };
    let s = if rig == RigType::R15 {
        *scales
    } else {
        Scales::default()
    };
    let head_part = |n: &str| n == "Head";
    // Joint positions scale with the parts they join; the neck sits at the torso top.
    let joint_pos = |j: &JointSpec| -> Vec3 { scale_vec(v3(j.at), &s, false) };
    let refs: HashMap<&str, Ref> = parts.iter().map(|p| (p.name, rbx_dom_weak::new_ref())).collect();
    let mut builders: HashMap<&str, InstanceBuilder> = HashMap::new();
    for p in parts {
        let color = match p.limb {
            Limb::Head => colors.head,
            Limb::Torso | Limb::Root => colors.torso,
            Limb::LeftArm => colors.left_arm,
            Limb::RightArm => colors.right_arm,
            Limb::LeftLeg => colors.left_leg,
            Limb::RightLeg => colors.right_leg,
        };
        let size = scale_vec(v3(p.size), &s, head_part(p.name));
        let center = if head_part(p.name) && rig == RigType::R15 {
            // The head rests on the neck whatever its scale.
            let neck = scale_vec(Vec3::new(0.0, 1.6, 0.0), &s, false);
            neck + Vec3::Y * size.y * 0.5
        } else {
            scale_vec(v3(p.center), &s, false)
        };
        let mut b = InstanceBuilder::new("Part")
            .with_referent(refs[p.name])
            .with_name(p.name)
            .with_property("Anchored", false)
            .with_property("CanCollide", false)
            .with_property("Size", rv(size))
            .with_property("CFrame", cf_at(center))
            .with_property("Color", Color3uint8::new(color[0], color[1], color[2]))
            .with_property("Material", Enum::from_u32(272))
            // Smooth like an avatar's parts (a bare Part's are a Studs top
            // and an Inlet bottom: doc::constructor_default).
            .with_property("TopSurface", Enum::from_u32(0))
            .with_property("BottomSurface", Enum::from_u32(0));
        if p.limb == Limb::Root {
            b.add_property("Transparency", 1.0f32);
        }
        if p.name == "Head" {
            b.add_property("Shape", Enum::from_u32(0));
        }
        for (part, att, off) in ACCESSORY_ATTACHMENTS {
            if *part == p.name {
                let off = scale_vec(v3(*off), &s, head_part(p.name));
                b.add_child(
                    InstanceBuilder::new("Attachment")
                        .with_name(*att)
                        .with_property("CFrame", cf_at(off)),
                );
            }
        }
        builders.insert(p.name, b);
    }
    let center_of = |name: &str| -> Vec3 {
        parts
            .iter()
            .find(|p| p.name == name)
            .map(|p| {
                if head_part(name) && rig == RigType::R15 {
                    scale_vec(Vec3::new(0.0, 1.6, 0.0), &s, false)
                        + Vec3::Y * scale_vec(v3(p.size), &s, true).y * 0.5
                } else {
                    scale_vec(v3(p.center), &s, false)
                }
            })
            .unwrap_or(Vec3::ZERO)
    };
    for j in joints {
        let at = joint_pos(j);
        let c0 = at - center_of(j.part0);
        let c1 = at - center_of(j.part1);
        if let Some(base) = j.rig_attachment {
            let att = format!("{base}RigAttachment");
            if let Some(b) = builders.get_mut(j.part0) {
                b.add_child(
                    InstanceBuilder::new("Attachment")
                        .with_name(att.clone())
                        .with_property("CFrame", cf_at(c0)),
                );
            }
            if let Some(b) = builders.get_mut(j.part1) {
                b.add_child(
                    InstanceBuilder::new("Attachment")
                        .with_name(att)
                        .with_property("CFrame", cf_at(c1)),
                );
            }
        }
        let motor = InstanceBuilder::new("Motor6D")
            .with_name(j.name)
            .with_property("Part0", Variant::Ref(refs[j.part0]))
            .with_property("Part1", Variant::Ref(refs[j.part1]))
            .with_property("C0", cf_at(c0))
            .with_property("C1", cf_at(c1));
        // R15 motors live in Part1; R6 motors in the Torso (RootJoint in the root part).
        let host = match rig {
            RigType::R15 => j.part1,
            RigType::R6 if j.name == "RootJoint" => "HumanoidRootPart",
            RigType::R6 => "Torso",
        };
        if let Some(b) = builders.get_mut(host) {
            b.add_child(motor);
        }
    }
    let hip_height = match rig {
        RigType::R6 => 0.0,
        RigType::R15 => mech::R15_HIP_HEIGHT * s.height,
    };
    let rig_type = match rig {
        RigType::R6 => 0,
        RigType::R15 => 1,
    };
    let humanoid = humanoid
        .with_property("HipHeight", hip_height)
        .with_property("RigType", Enum::from_u32(rig_type));
    let body_colors = InstanceBuilder::new("BodyColors")
        .with_name("Body Colors")
        .with_property(
            "HeadColor3",
            Color3uint8::new(colors.head[0], colors.head[1], colors.head[2]),
        )
        .with_property(
            "TorsoColor3",
            Color3uint8::new(colors.torso[0], colors.torso[1], colors.torso[2]),
        )
        .with_property(
            "LeftArmColor3",
            Color3uint8::new(colors.left_arm[0], colors.left_arm[1], colors.left_arm[2]),
        )
        .with_property(
            "RightArmColor3",
            Color3uint8::new(
                colors.right_arm[0],
                colors.right_arm[1],
                colors.right_arm[2],
            ),
        )
        .with_property(
            "LeftLegColor3",
            Color3uint8::new(colors.left_leg[0], colors.left_leg[1], colors.left_leg[2]),
        )
        .with_property(
            "RightLegColor3",
            Color3uint8::new(
                colors.right_leg[0],
                colors.right_leg[1],
                colors.right_leg[2],
            ),
        );
    let mut model = InstanceBuilder::new("Model")
        .with_name(name)
        .with_property("PrimaryPart", Variant::Ref(refs["HumanoidRootPart"]))
        .with_child(humanoid)
        .with_child(body_colors);
    for p in parts {
        if let Some(b) = builders.remove(p.name) {
            model.add_child(b);
        }
    }
    model
}

/// Roblox's classic R6 joints (the Humanoid builds them on character.rbxm's
/// loose parts): name, Part0, Part1, C0, C1 as CFrame components (x, y, z,
/// R00..R22). The rotated frames are what R6 animations are authored in.
const R6_CLASSIC_JOINTS: &[(&str, &str, &str, [f32; 12], [f32; 12])] = &[
    (
        "RootJoint",
        "HumanoidRootPart",
        "Torso",
        [0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
    ),
    (
        "Neck",
        "Torso",
        "Head",
        [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
        [0.0, -0.5, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
    ),
    (
        "Right Shoulder",
        "Torso",
        "Right Arm",
        [1.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0],
        [-0.5, 0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0],
    ),
    (
        "Left Shoulder",
        "Torso",
        "Left Arm",
        [-1.0, 0.5, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
        [0.5, 0.5, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
    ),
    (
        "Right Hip",
        "Torso",
        "Right Leg",
        [1.0, -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0],
        [0.5, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0],
    ),
    (
        "Left Hip",
        "Torso",
        "Left Leg",
        [
            -1.0, -1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0,
        ],
        [-0.5, 1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
    ),
];

fn cframe12(c: [f32; 12]) -> CFrame {
    use rbx_dom_weak::types::Matrix3;
    CFrame::new(
        Vector3::new(c[0], c[1], c[2]),
        Matrix3::new(
            Vector3::new(c[3], c[4], c[5]),
            Vector3::new(c[6], c[7], c[8]),
            Vector3::new(c[9], c[10], c[11]),
        ),
    )
}

/// The Studio install's default character file for a rig type
/// (content/avatar, read only).
pub fn studio_character_file(rig: RigType) -> Option<std::path::PathBuf> {
    let dir = rustudio_core::default_scripts::studio_dir()?.join("content/avatar");
    let p = match rig {
        RigType::R15 => dir.join("characterR15DynamicHeadV2.rbxm"),
        RigType::R6 => dir.join("character.rbxm"),
    };
    p.exists().then_some(p)
}

fn dom_child(dom: &rbx_dom_weak::WeakDom, parent: Ref, name: &str) -> Option<Ref> {
    dom.get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| dom.get_by_ref(*c).is_some_and(|i| i.name == name))
}

fn dom_class_child(dom: &rbx_dom_weak::WeakDom, parent: Ref, class: &str) -> Option<Ref> {
    dom.get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| dom.get_by_ref(*c).is_some_and(|i| i.class == class))
}

fn set(dom: &mut rbx_dom_weak::WeakDom, r: Ref, key: &str, v: impl Into<Variant>) {
    if let Some(i) = dom.get_by_ref_mut(r) {
        i.properties.insert(ustr(key), v.into());
    }
}


/// Inserts a character Model under `parent`: Roblox's own default body from
/// the Studio install when there is one (real body-part meshes, cages,
/// attachments and joints), else rustudio's blocky stand-in. `humanoid`
/// replaces the file's Humanoid (it carries StarterPlayer's settings); the
/// file's HipHeight and RigType are kept.
pub fn insert_character(
    dom: &mut rbx_dom_weak::WeakDom,
    parent: Ref,
    name: &str,
    rig: RigType,
    colors: &BodyColors,
    scales: &Scales,
    humanoid: InstanceBuilder,
) -> Ref {
    match studio_character(dom, parent, name, rig, colors, scales, humanoid) {
        Ok(m) => m,
        Err(humanoid) => dom.insert(parent, build_default(name, rig, colors, scales, humanoid)),
    }
}

/// Reads a model file from the Studio install and moves its first Model (or
/// MeshPart) under `parent`.
fn take_from_file(
    path: &std::path::Path,
    dom: &mut rbx_dom_weak::WeakDom,
    parent: Ref,
    class: &str,
) -> Option<Ref> {
    let bytes = std::fs::read(path).ok()?;
    let mut file = Document::decode(&bytes).ok()?;
    let root = file.root_ref();
    let top = dom_class_child(&file, root, class)?;
    // A model made for a script starts without a parent; rbx_dom_weak only
    // moves into an instance, so it waits in a parentless holder.
    let parent = if parent.is_some() {
        parent
    } else {
        dom.insert(
            Ref::none(),
            InstanceBuilder::new("Folder").with_name("CharacterHolder"),
        )
    };
    file.transfer(top, dom, parent);
    Some(top)
}

fn studio_character(
    dom: &mut rbx_dom_weak::WeakDom,
    parent: Ref,
    name: &str,
    rig: RigType,
    colors: &BodyColors,
    scales: &Scales,
    humanoid: InstanceBuilder,
) -> Result<Ref, InstanceBuilder> {
    let Some(path) = studio_character_file(rig) else {
        return Err(humanoid);
    };
    let Some(m) = take_from_file(&path, dom, parent, "Model") else {
        return Err(humanoid);
    };
    if let Some(i) = dom.get_by_ref_mut(m) {
        i.name = name.to_string();
    }
    let Some(root) = dom_child(dom, m, "HumanoidRootPart") else {
        dom.destroy(m);
        return Err(humanoid);
    };
    set(dom, m, "PrimaryPart", Variant::Ref(root));
    // The file's Humanoid gives way to ours (with its HipHeight / RigType).
    let old_h = dom_class_child(dom, m, "Humanoid");
    let hip = old_h
        .and_then(
            |h| match dom.get_by_ref(h)?.properties.get(&ustr("HipHeight")) {
                Some(Variant::Float32(v)) => Some(*v),
                _ => None,
            },
        )
        .unwrap_or(match rig {
            RigType::R6 => 0.0,
            RigType::R15 => mech::R15_HIP_HEIGHT,
        });
    if let Some(h) = old_h {
        dom.destroy(h);
    }
    let rig_type = match rig {
        RigType::R6 => 0,
        RigType::R15 => 1,
    };
    let s = if rig == RigType::R15 {
        *scales
    } else {
        Scales::default()
    };
    dom.insert(
        m,
        humanoid
            .with_property("HipHeight", hip * s.height)
            .with_property("RigType", Enum::from_u32(rig_type)),
    );
    match rig {
        RigType::R15 => {
            // The V2 head's mesh (and cage) are served only to signed-in
            // accounts: kept when the session (or the cache) can supply
            // them; otherwise Studio's defaultDynamicHead (public mesh and
            // face texture) takes its place on the same neck.
            let head_file = path.with_file_name("defaultDynamicHead.rbxm");
            let head_mesh = dom_child(dom, m, "Head")
                .and_then(
                    |h| match dom.get_by_ref(h)?.properties.get(&ustr("MeshContent")) {
                        Some(Variant::Content(c)) => c.as_uri().map(str::to_string),
                        _ => None,
                    },
                )
                .and_then(|s| rustudio_core::assets::asset_ref(&s));
            let keep_v2 = head_mesh.is_some_and(|a| rustudio_core::assets::reachable(a.id));
            if let Some(old) = dom_child(dom, m, "Head").filter(|_| !keep_v2) {
                if let Some(new) = take_from_file(&head_file, dom, m, "MeshPart") {
                    if let Some(neck) = dom_child(dom, old, "Neck") {
                        dom.transfer_within(neck, new);
                        set(dom, neck, "Part1", Variant::Ref(new));
                    }
                    let cf = dom
                        .get_by_ref(old)
                        .and_then(|i| i.properties.get(&ustr("CFrame")).cloned());
                    if let Some(cf) = cf {
                        set(dom, new, "CFrame", cf);
                    }
                    dom.destroy(old);
                }
            }
            apply_r15_scales(dom, m, &s);
        }
        RigType::R6 => {
            for (jn, p0, p1, c0, c1) in R6_CLASSIC_JOINTS {
                let (Some(a), Some(b)) = (dom_child(dom, m, p0), dom_child(dom, m, p1)) else {
                    continue;
                };
                let host = if *jn == "RootJoint" {
                    a
                } else {
                    dom_child(dom, m, "Torso").unwrap_or(a)
                };
                dom.insert(
                    host,
                    InstanceBuilder::new("Motor6D")
                        .with_name(*jn)
                        .with_property("Part0", Variant::Ref(a))
                        .with_property("Part1", Variant::Ref(b))
                        .with_property("C0", cframe12(*c0))
                        .with_property("C1", cframe12(*c1))
                        .with_property("MaxVelocity", 0.1f32),
                );
            }
        }
    }
    // Body colours, and no part collisions of their own (the Humanoid's
    // capsule walks the character).
    let parts: Vec<Ref> = dom
        .get_by_ref(m)
        .map(|i| i.children().to_vec())
        .unwrap_or_default();
    for p in parts {
        let Some(i) = dom.get_by_ref(p) else { continue };
        if !matches!(i.class.as_str(), "Part" | "MeshPart") {
            continue;
        }
        let limb = limb_of(&i.name);
        set(dom, p, "CanCollide", false);
        set(dom, p, "Anchored", false);
        if let Some(l) = limb {
            let c = match l {
                Limb::Head => colors.head,
                Limb::Torso | Limb::Root => colors.torso,
                Limb::LeftArm => colors.left_arm,
                Limb::RightArm => colors.right_arm,
                Limb::LeftLeg => colors.left_leg,
                Limb::RightLeg => colors.right_leg,
            };
            set(dom, p, "Color", Color3uint8::new(c[0], c[1], c[2]));
        }
    }
    let bc = |c: [u8; 3]| Color3uint8::new(c[0], c[1], c[2]);
    dom.insert(
        m,
        InstanceBuilder::new("BodyColors")
            .with_name("Body Colors")
            .with_property("HeadColor3", bc(colors.head))
            .with_property("TorsoColor3", bc(colors.torso))
            .with_property("LeftArmColor3", bc(colors.left_arm))
            .with_property("RightArmColor3", bc(colors.right_arm))
            .with_property("LeftLegColor3", bc(colors.left_leg))
            .with_property("RightLegColor3", bc(colors.right_leg)),
    );
    Ok(m)
}

/// R15 body scaling the way the avatar scale values work on Classic parts:
/// every part's OriginalSize and every attachment's OriginalPosition (both
/// kept in the file) times the body type's proportions for that part
/// (`proportion_target`) and (width, height, depth) — the head by its own
/// scale — and each Motor6D re-seated on its two RigAttachments. With a
/// default HumanoidDescription (BodyTypeScale 0.3, ProportionScale 1) this
/// gives Studio's own Play character's sizes (mech::R15_DEFAULT_PART_SIZES);
/// an avatar with BodyTypeScale 0 keeps the file's sizes.
fn apply_r15_scales(dom: &mut rbx_dom_weak::WeakDom, m: Ref, s: &Scales) {
    let v3_of = |dom: &rbx_dom_weak::WeakDom, r: Ref, k: &str| match dom
        .get_by_ref(r)
        .and_then(|i| i.properties.get(&ustr(k)))
    {
        Some(Variant::Vector3(v)) => Some(Vec3::new(v.x, v.y, v.z)),
        _ => None,
    };
    let parts: Vec<Ref> = dom
        .get_by_ref(m)
        .map(|i| i.children().to_vec())
        .unwrap_or_default();
    let unit_hwd = s.height == 1.0 && s.width == 1.0 && s.depth == 1.0 && s.head == 1.0;
    for p in &parts {
        let name = dom.get_by_ref(*p).map(|i| i.name.clone()).unwrap_or_default();
        let head = name == "Head";
        let orig = dom_child(dom, *p, "OriginalSize").and_then(|o| v3_of(dom, o, "Value"));
        // A part the body type doesn't shape (the HumanoidRootPart) keeps
        // its file CFrames at unit scales: its RootRigAttachment's
        // OriginalPosition in the body file is stale (-0.35 against the
        // real -1.0).
        if part_kind(&name).is_none() && unit_hwd {
            continue;
        }
        let ratio = proportion_target(&name, s.proportion, s.body_type);
        let scale = |v: Vec3| scale_vec(v * ratio, s, head);
        if let Some(orig) = orig {
            set(dom, *p, "Size", rv(scale(orig)));
        }
        let kids: Vec<Ref> = dom
            .get_by_ref(*p)
            .map(|i| i.children().to_vec())
            .unwrap_or_default();
        for a in kids {
            if dom.get_by_ref(a).is_some_and(|i| i.class == "Attachment") {
                if let Some(orig) =
                    dom_child(dom, a, "OriginalPosition").and_then(|o| v3_of(dom, o, "Value"))
                {
                    set(dom, a, "CFrame", cf_at(scale(orig)));
                }
            }
        }
    }
    reseat_r15_joints(dom, m);
}

/// Re-seats each R15 Motor6D on its two RigAttachments and sets the
/// Humanoid's HipHeight from the legs (after body parts change size).
pub fn reseat_r15_joints(dom: &mut rbx_dom_weak::WeakDom, m: Ref) {
    // The HumanoidRootPart's RootRigAttachment follows the legs' parts
    // (Studio 0.747, CreateHumanoidModelFromDescription: default body
    // -1.0, a 1.05 height -1.05, a package body -0.9541): at (0, y, 0) with
    // y = LowerTorso's RootRigAttachment − its LeftHipRigAttachment − half
    // the root's height, so the soles meet the floor when the root stands
    // HipHeight + half its height above it. A package's parts keep the
    // body file's stale -0.35 otherwise (the avatar stood 0.6 low).
    let y_of = |dom: &rbx_dom_weak::WeakDom, r: Ref, k: &str| match dom.get_by_ref(r)?.properties.get(&ustr(k)) {
        Some(Variant::CFrame(c)) => Some(c.position.y),
        Some(Variant::Vector3(v)) => Some(v.y),
        _ => None,
    };
    let root_att = (|| {
        let root = dom_child(dom, m, "HumanoidRootPart")?;
        let lt = dom_child(dom, m, "LowerTorso")?;
        let att = dom_child(dom, root, "RootRigAttachment")?;
        let y = y_of(dom, dom_child(dom, lt, "RootRigAttachment")?, "CFrame")?
            - y_of(dom, dom_child(dom, lt, "LeftHipRigAttachment")?, "CFrame")?
            - y_of(dom, root, "Size")? * 0.5;
        let Some(Variant::CFrame(cf)) = dom.get_by_ref(att)?.properties.get(&ustr("CFrame")).cloned() else { return None };
        Some((att, rbx_dom_weak::types::CFrame::new(rbx_dom_weak::types::Vector3::new(0.0, y, 0.0), cf.orientation)))
    })();
    if let Some((att, cf)) = root_att {
        set(dom, att, "CFrame", cf);
    }
    let parts: Vec<Ref> = dom
        .get_by_ref(m)
        .map(|i| i.children().to_vec())
        .unwrap_or_default();
    for p in &parts {
        let kids: Vec<Ref> = dom
            .get_by_ref(*p)
            .map(|i| i.children().to_vec())
            .unwrap_or_default();
        for j in kids {
            let Some(i) = dom.get_by_ref(j) else { continue };
            if i.class != "Motor6D" {
                continue;
            }
            let att = format!("{}RigAttachment", i.name);
            let ends = ["Part0", "Part1"].map(|k| match i.properties.get(&ustr(k)) {
                Some(Variant::Ref(r)) => *r,
                _ => Ref::none(),
            });
            for (k, part) in ["C0", "C1"].iter().zip(ends) {
                let cf = dom_child(dom, part, &att)
                    .and_then(|a| dom.get_by_ref(a)?.properties.get(&ustr("CFrame")).cloned());
                if let Some(cf) = cf {
                    set(dom, j, k, cf);
                }
            }
        }
    }
    // HipHeight from the legs, as Roblox's automatic scaling sets it: the
    // left leg's length from its hip attachment down to the sole.
    if let Some(hip) = r15_hip_height(dom, m) {
        if let Some(h) = dom_class_child(dom, m, "Humanoid") {
            set(dom, h, "HipHeight", hip);
        }
    }
}

/// An R15 model's HipHeight: the left leg's length, hip attachment → knee
/// → ankle → sole (Studio 0.747 CreateHumanoidModelFromDescription: 2.1924
/// default body, 1.9670 a package body, 2.1 at HeightScale 1.05, each the
/// left leg exactly). None when a part or attachment is missing.
fn r15_hip_height(dom: &rbx_dom_weak::WeakDom, m: Ref) -> Option<f32> {
    let y_of = |r: Ref, k: &str| match dom.get_by_ref(r)?.properties.get(&ustr(k)) {
        Some(Variant::CFrame(c)) => Some(c.position.y),
        Some(Variant::Vector3(v)) => Some(v.y),
        _ => None,
    };
    let att = |part: &str, name: &str| y_of(dom_child(dom, dom_child(dom, m, part)?, name)?, "CFrame");
    let foot = dom_child(dom, m, "LeftFoot")?;
    Some(
        att("LeftUpperLeg", "LeftHipRigAttachment")? - att("LeftUpperLeg", "LeftKneeRigAttachment")?
            + att("LeftLowerLeg", "LeftKneeRigAttachment")? - att("LeftLowerLeg", "LeftAnkleRigAttachment")?
            + att("LeftFoot", "LeftAnkleRigAttachment")? + y_of(foot, "Size")? * 0.5,
    )
}

/// Distance from the HumanoidRootPart centre down to the floor.
pub fn root_height(doc: &Document, model: Ref) -> f32 {
    let humanoid = doc.dom.get_by_ref(model).and_then(|m| {
        m.children()
            .iter()
            .copied()
            .find(|c| doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
    });
    let root = doc.find_named(model, "HumanoidRootPart");
    let hip = humanoid
        .and_then(|h| f32_of(doc.prop(h, "HipHeight")))
        .unwrap_or(0.0);
    let half = root
        .and_then(|r| vec3_of(doc.prop(r, "Size")))
        .map(|s| s.y * 0.5)
        .unwrap_or(1.0);
    let r6 = humanoid.is_some_and(|h| rustudio_render::scene::enum_u32(doc.prop(h, "RigType")) == Some(0));
    // R6 characters stand on 2-stud legs below a 2-stud torso.
    if r6 {
        half + 2.0
    } else {
        hip + half
    }
}

/// One Motor6D in a character.
#[derive(Debug, Clone)]
pub struct Joint {
    /// The Motor6D (or Weld) itself.
    pub motor: Ref,
    #[allow(dead_code)] // (read by tests and when debugging a rig)
    pub name: String,
    pub part0: Ref,
    pub part1: Ref,
    pub c0: Xform,
    pub c1: Xform,
}

/// Something rigidly attached to a body part (accessory handles).
#[derive(Debug, Clone)]
pub struct Attached {
    pub handle: Ref,
    pub part: Ref,
    pub offset: Xform,
}

/// A character's joint tree, solved from its HumanoidRootPart.
#[derive(Debug, Clone)]
pub struct Rig {
    pub root: Ref,
    pub joints: Vec<Joint>,
    pub attached: Vec<Attached>,
}

fn cf_prop(doc: &Document, r: Ref, name: &str) -> Xform {
    match doc.prop(r, name) {
        Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
        _ => Xform {
            pos: Vec3::ZERO,
            rot: Mat3::IDENTITY,
        },
    }
}

impl Rig {
    /// Reads the Motor6Ds of a character, ordered from the root outward,
    /// and attaches accessories by matching attachment names.
    pub fn read(doc: &Document, model: Ref) -> Rig {
        let root = doc
            .find_named(model, "HumanoidRootPart")
            .unwrap_or_else(Ref::none);
        let mut motors: Vec<Joint> = doc
            .dom
            .descendants_of(model)
            .filter(|i| {
                matches!(i.class.as_str(), "Motor6D" | "Weld" | "ManualWeld" | "AnimationConstraint")
            })
            .filter_map(|i| {
                let r = i.referent();
                let (part0, part1, c0, c1) = joint_frames(doc, r)?;
                Some(Joint { motor: r, name: i.name.clone(), part0, part1, c0, c1 })
            })
            .collect();
        // Order so every joint's Part0 is placed before it is used.
        let mut placed = vec![root];
        let mut ordered = Vec::new();
        while !motors.is_empty() {
            let before = motors.len();
            let mut i = 0;
            while i < motors.len() {
                if placed.contains(&motors[i].part0) {
                    let j = motors.swap_remove(i);
                    placed.push(j.part1);
                    ordered.push(j);
                } else {
                    i += 1;
                }
            }
            if motors.len() == before {
                break;
            }
        }
        let mut rig = Rig {
            root,
            joints: ordered,
            attached: Vec::new(),
        };
        rig.refresh_attached(doc, model);
        rig
    }

    /// Works the accessory attachments out again (accessories put on or
    /// taken off since the rig was read).
    pub fn refresh_accessories(&mut self, doc: &Document, model: Ref) {
        self.attached.clear();
        self.attach_accessories(doc, model);
    }

    /// Accessories, then every other part in the model a game welded to the
    /// body (WeldConstraint, Snap, Glue, RigidConstraint …: Roblox moves them
    /// as one assembly with it), carried from the body part they hang from
    /// at the offset their welds hold (a WeldConstraint's CFrame0, C0 · C1⁻¹,
    /// Attachment0 · Attachment1⁻¹; where they are now only for a weld with
    /// no offset yet). Returns those parts, which the simulation must leave
    /// alone (round 19: Place C welds its ship to the HumanoidRootPart; it
    /// fell away and was destroyed). A game's hunters weld their gowns, masks
    /// and syringes to the limbs: an NPC posed only its Motor6D parts, so
    /// those stayed where the rig was spawned, below the standing body.
    pub fn refresh_attached(&mut self, doc: &Document, model: Ref) -> Vec<Ref> {
        self.refresh_accessories(doc, model);
        let mut placed: std::collections::HashSet<Ref> = self
            .joints
            .iter()
            .flat_map(|j| [j.part0, j.part1])
            .chain([self.root])
            .chain(self.attached.iter().map(|a| a.handle))
            .collect();
        let motors: std::collections::HashSet<Ref> =
            self.joints.iter().map(|j| j.motor).collect();
        // (Part0, Part1, Part0⁻¹ · Part1 as the joint holds it, if it says)
        let mut edges: Vec<(Ref, Ref, Option<Xform>)> = Vec::new();
        for i in doc.dom.descendants_of(model) {
            // (AnimationConstraint is the rig's own; the rest hold parts
            // rigidly. Parts are read alias-aware: a live Team Create place
            // keeps a WeldConstraint's as Part0Internal / Part1Internal.)
            let j = i.referent();
            let rigid = i.class != "AnimationConstraint" && rustudio_core::body::is_rigid_joint(&i.class);
            if !rigid || motors.contains(&j) || !rustudio_core::body::joint_enabled(doc, j) {
                continue;
            }
            if let Some((a, b)) = rustudio_core::body::joint_parts(doc, j) {
                let held = match i.class.as_str() {
                    "WeldConstraint" => crate::runtime::weld_offset(doc, j),
                    "RigidConstraint" => {
                        let att = |k: &str| match doc.prop(j, k) {
                            Some(Variant::Ref(x)) if x.is_some() => Some(cf_prop(doc, x, "CFrame")),
                            _ => None,
                        };
                        att("Attachment0").zip(att("Attachment1")).map(|(a0, a1)| cf_mul(&a0, &cf_inv(&a1)))
                    }
                    _ => joint_frames(doc, j).map(|(_, _, c0, c1)| cf_mul(&c0, &cf_inv(&c1))),
                };
                edges.push((a, b, held));
            }
        }
        // An equipped Tool's parts follow its grip (play_game::tool_pose).
        let in_tool = |r: Ref| {
            let mut cur = doc.dom.get_by_ref(r).map(|i| i.parent());
            while let Some(p) = cur.filter(|p| *p != model && p.is_some()) {
                if doc.class_of(p).is_some_and(|k| doc::is_a(&k, "BackpackItem")) {
                    return true;
                }
                cur = doc.dom.get_by_ref(p).map(|i| i.parent());
            }
            false
        };
        let in_model = |r: Ref| {
            doc.is_ancestor(model, r)
                && doc.class_of(r).is_some_and(|k| doc::is_a(&k, "BasePart"))
                && !in_tool(r)
        };
        // Which body part each carried part hangs from (through welds), and at what offset.
        let mut from: HashMap<Ref, (Ref, Xform)> = HashMap::new();
        let mut added = Vec::new();
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        loop {
            let mut grew = false;
            for (a, b, rel) in &edges {
                // (rel is Part0⁻¹ · Part1: from Part1's side it is inverted)
                for (held, free, step) in [(*a, *b, rel.map(|x| x)), (*b, *a, rel.map(|x| cf_inv(&x)))] {
                    if placed.contains(&held) && !placed.contains(&free) && in_model(free) {
                        let (body, to_held) = from.get(&held).copied().unwrap_or((held, ident));
                        let offset = match step {
                            Some(step) => cf_mul(&to_held, &step),
                            None => cf_mul(
                                &cf_inv(&cf_prop(doc, body, "CFrame")),
                                &cf_prop(doc, free, "CFrame"),
                            ),
                        };
                        self.attached.push(Attached {
                            handle: free,
                            part: body,
                            offset,
                        });
                        from.insert(free, (body, offset));
                        placed.insert(free);
                        added.push(free);
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        added
    }

    /// Accessory handles go where their attachment matches a body
    /// attachment of the same name (as Humanoid:AddAccessory does).
    fn attach_accessories(&mut self, doc: &Document, model: Ref) {
        let body: Vec<Ref> = doc
            .dom
            .get_by_ref(model)
            .map(|m| {
                m.children()
                    .iter()
                    .copied()
                    .filter(|c| doc.class_of(*c).is_some_and(|k| doc::is_a(&k, "BasePart")))
                    .collect()
            })
            .unwrap_or_default();
        let mut body_atts: HashMap<String, (Ref, Xform)> = HashMap::new();
        for p in &body {
            for c in doc
                .dom
                .get_by_ref(*p)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
            {
                if let Some(i) = doc.dom.get_by_ref(c) {
                    if i.class == "Attachment" {
                        body_atts.insert(i.name.clone(), (*p, cf_prop(doc, c, "CFrame")));
                    }
                }
            }
        }
        let accessories: Vec<Ref> = doc
            .dom
            .get_by_ref(model)
            .map(|m| {
                m.children()
                    .iter()
                    .copied()
                    .filter(|c| {
                        doc.class_of(*c)
                            .is_some_and(|k| doc::is_a(&k, "Accoutrement"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for acc in accessories {
            let Some(handle) = doc.find_named(acc, "Handle") else {
                continue;
            };
            let atts: Vec<Ref> = doc
                .dom
                .get_by_ref(handle)
                .map(|h| {
                    h.children()
                        .iter()
                        .copied()
                        .filter(|c| doc.class_of(*c).is_some_and(|k| k == "Attachment"))
                        .collect()
                })
                .unwrap_or_default();
            let found = atts.iter().find_map(|a| {
                let name = doc.dom.get_by_ref(*a)?.name.clone();
                let (part, body_cf) = body_atts.get(&name)?;
                Some((*part, cf_mul(body_cf, &cf_inv(&cf_prop(doc, *a, "CFrame")))))
            });
            // Hats without a matching attachment sit on the head (legacy behaviour).
            let found = found.or_else(|| {
                let head = body
                    .iter()
                    .copied()
                    .find(|p| doc.dom.get_by_ref(*p).is_some_and(|i| i.name == "Head"))?;
                Some((
                    head,
                    Xform {
                        pos: Vec3::Y * 0.6,
                        rot: Mat3::IDENTITY,
                    },
                ))
            });
            if let Some((part, offset)) = found {
                self.attached.push(Attached {
                    handle,
                    part,
                    offset,
                });
            }
        }
    }

    /// World CFrames of every part for a root placement and each joint's
    /// Motor6D.Transform, keyed by the motor (Part1 = Part0 · C0 · Transform · C1⁻¹).
    pub fn solve<S: std::hash::BuildHasher>(&self, root: Xform, transforms: &HashMap<Ref, Xform, S>) -> Vec<(Ref, Xform)> {
        let mut world = rustc_hash::FxHashMap::with_capacity_and_hasher(self.joints.len() + 1, Default::default());
        world.insert(self.root, root);
        let mut out = Vec::with_capacity(self.joints.len() + self.attached.len() + 1);
        out.push((self.root, root));
        for j in &self.joints {
            let Some(p0) = world.get(&j.part0).copied() else {
                continue;
            };
            let t = transforms.get(&j.motor).copied().unwrap_or(Xform {
                pos: Vec3::ZERO,
                rot: Mat3::IDENTITY,
            });
            let xf = cf_mul(&cf_mul(&cf_mul(&p0, &j.c0), &t), &cf_inv(&j.c1));
            world.insert(j.part1, xf);
            out.push((j.part1, xf));
        }
        for a in &self.attached {
            if let Some(p) = world.get(&a.part) {
                out.push((a.handle, cf_mul(p, &a.offset)));
            }
        }
        out
    }
}

/// Shared by native keyframe evaluation, NPC pose solving and its queries.
/// Small groups stay on the caller to avoid waking workers for one avatar.
pub fn character_pool(rigs: usize) -> Option<&'static rapier3d::rayon::ThreadPool> {
    if rigs < 32 {
        return None;
    }
    static POOL: std::sync::OnceLock<Option<rapier3d::rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        if threads == 1 {
            return None;
        }
        rapier3d::rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("rustudio-character-{i}"))
            .build()
            .ok()
    }).as_ref()
}

/// Writes solved part CFrames into a document.
pub fn write_pose(doc: &mut Document, pose: &[(Ref, Xform)], map: Option<&rustudio_core::RefMap>) {
    let key = ustr("CFrame");
    for (r, xf) in pose {
        let target = match map {
            Some(m) => match m.get(r) {
                Some(t) => *t,
                None => continue,
            },
            None => *r,
        };
        if let Some(inst) = doc.dom.get_by_ref_mut(target) {
            let value = Variant::CFrame(xf.to_cframe());
            if inst.properties.get(&key) != Some(&value) {
                inst.properties.insert(key, value);
                doc.mark_prop(target, key);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use rustudio_render::scene::xform_of;

    fn spawn(rig: RigType) -> (Document, Ref) {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let m = d.dom.insert(
            ws,
            build_default(
                "P",
                rig,
                &BodyColors::default(),
                &Scales::default(),
                InstanceBuilder::new("Humanoid").with_name("Humanoid"),
            ),
        );
        (d, m)
    }

    #[test]
    fn r15_rig_solves_to_built_pose() {
        let (d, m) = spawn(RigType::R15);
        let rig = Rig::read(&d, m);
        assert_eq!(rig.joints.len(), 15);
        assert_eq!(
            d.dom
                .descendants_of(m)
                .filter(|i| i.class == "Motor6D")
                .count(),
            15
        );
        let pose = rig.solve(
            Xform {
                pos: Vec3::ZERO,
                rot: Mat3::IDENTITY,
            },
            &HashMap::new(),
        );
        // With no transforms the solved pose equals the built layout.
        for (r, xf) in pose {
            let built = xform_of(&d, r).unwrap();
            assert!(
                (built.pos - xf.pos).length() < 1e-4,
                "{:?}",
                d.dom.get_by_ref(r).map(|i| &i.name)
            );
        }
        assert!((root_height(&d, m) - 3.0).abs() < 1e-4);
    }

    /// Studio's joint upgrade keeps the rig solving the same: 15
    /// AnimationConstraints read from their attachments, 14 ball sockets
    /// (none on Root), no rig Motor6Ds left, and a Transform still turns
    /// its limb.
    #[test]
    fn upgraded_r15_rig_solves_the_same() {
        let (mut d, m) = spawn(RigType::R15);
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        let before = Rig::read(&d, m).solve(ident, &HashMap::new());
        assert_eq!(upgrade_rig(&mut d.dom, m), 15);
        let count = |d: &Document, c: &str| d.dom.descendants_of(m).filter(|i| i.class == c).count();
        assert_eq!(count(&d, "Motor6D"), 0);
        assert_eq!(count(&d, "AnimationConstraint"), 15);
        assert_eq!(count(&d, "BallSocketConstraint"), 14);
        assert_eq!(
            d.dom.descendants_of(m).filter(|i| i.name == "JointRotation").count(),
            4
        );
        let rig = Rig::read(&d, m);
        assert_eq!(rig.joints.len(), 15);
        let after = rig.solve(ident, &HashMap::new());
        for (r, xf) in &after {
            let b = before.iter().find(|(x, _)| x == r).unwrap().1;
            assert!((b.pos - xf.pos).length() < 1e-4);
        }
        let knee = rig.joints.iter().find(|j| j.name == "LeftKnee").unwrap();
        let mut t = HashMap::new();
        t.insert(knee.motor, Xform { pos: Vec3::ZERO, rot: Mat3::from_rotation_x(1.0) });
        let bent = rig.solve(ident, &t);
        let foot = d.find_named(m, "LeftFoot").unwrap();
        let at = |p: &Vec<(Ref, Xform)>| p.iter().find(|(x, _)| *x == foot).unwrap().1.pos;
        assert!((at(&bent) - at(&after)).length() > 0.3);
    }

    #[test]
    fn r6_rig_and_motor_transform() {
        let (d, m) = spawn(RigType::R6);
        let rig = Rig::read(&d, m);
        assert_eq!(rig.joints.len(), 6);
        // A Transform on the Left Hip (what an AnimationTrack writes) swings the leg.
        let hip = rig
            .joints
            .iter()
            .find(|j| j.name == "Left Hip")
            .unwrap()
            .motor;
        let t = HashMap::from([(
            hip,
            Xform {
                pos: Vec3::ZERO,
                rot: Mat3::from_rotation_x(0.5),
            },
        )]);
        let pose = rig.solve(
            Xform {
                pos: Vec3::ZERO,
                rot: Mat3::IDENTITY,
            },
            &t,
        );
        let leg = d.find_named(m, "Left Leg").unwrap();
        let moved = pose.iter().find(|(r, _)| *r == leg).unwrap().1;
        assert!(
            (moved.pos - Vec3::new(-0.5, -2.0, 0.0)).length() > 0.05,
            "leg follows its Motor6D.Transform"
        );
        assert!((root_height(&d, m) - 3.0).abs() < 1e-4);
    }

    /// Parts a game welds to a limb (a game's character: gown stains on
    /// the Torso by WeldConstraint, a syringe on the Right Arm) go with the
    /// limb at the weld's own offset, wherever the parts were left: an NPC
    /// posed only its Motor6D parts, and the stains stayed where it spawned.
    #[test]
    fn welded_parts_follow_their_limb_at_the_weld_offset() {
        let (mut d, m) = spawn(RigType::R6);
        let torso = d.find_named(m, "Torso").unwrap();
        let arm = d.find_named(m, "Right Arm").unwrap();
        let at = |x: f32, y: f32, z: f32| Variant::CFrame(CFrame::new(Vector3::new(x, y, z), rbx_dom_weak::types::Matrix3::identity()));
        // Left 2 studs below where the welds hold them.
        let stain = d.dom.insert(m, InstanceBuilder::new("Part").with_name("Stain1").with_property("CFrame", at(-0.45, -2.0, -0.6)));
        let barrel = d.dom.insert(m, InstanceBuilder::new("Part").with_name("Barrel").with_property("CFrame", at(1.5, -3.0, -1.3)));
        let tip = d.dom.insert(m, InstanceBuilder::new("Part").with_name("Needle").with_property("CFrame", at(9.0, 9.0, 9.0)));
        // Torso → Stain1 by a WeldConstraint that has been active (State 3, CFrame0 kept).
        d.dom.insert(stain, InstanceBuilder::new("WeldConstraint")
            .with_property("Part0", Variant::Ref(torso))
            .with_property("Part1", Variant::Ref(stain))
            .with_property("State", 3i32)
            .with_property("CFrame0", at(-0.45, 0.2, -0.6)));
        // Barrel → Right Arm (written the other way round), then Barrel → Needle.
        d.dom.insert(barrel, InstanceBuilder::new("WeldConstraint")
            .with_property("Part0", Variant::Ref(barrel))
            .with_property("Part1", Variant::Ref(arm))
            .with_property("State", 3i32)
            .with_property("CFrame0", at(0.0, 1.15, 1.3)));
        d.dom.insert(tip, InstanceBuilder::new("WeldConstraint")
            .with_property("Part0", Variant::Ref(barrel))
            .with_property("Part1", Variant::Ref(tip))
            .with_property("State", 3i32)
            .with_property("CFrame0", at(0.0, -0.9, 0.0)));
        let rig = Rig::read(&d, m);
        let root = Xform { pos: Vec3::new(10.0, 20.0, 30.0), rot: Mat3::from_rotation_y(0.7) };
        let shoulder = rig.joints.iter().find(|j| j.name == "Right Shoulder").unwrap().motor;
        let swing = HashMap::from([(shoulder, Xform { pos: Vec3::ZERO, rot: Mat3::from_rotation_x(0.6) })]);
        let pose: HashMap<Ref, Xform> = rig.solve(root, &swing).into_iter().collect();
        let near = |a: Vec3, b: Vec3| (a - b).length() < 1e-4;
        let local = |part: Ref, p: Ref| cf_inv(&pose[&part]).apply(pose[&p].pos);
        assert!(near(local(torso, stain), Vec3::new(-0.45, 0.2, -0.6)), "stain on the torso: {:?}", local(torso, stain));
        // Part1 = Part0 · CFrame0: the arm is 1.15 up and 1.3 back from the barrel.
        assert!(near(local(arm, barrel), Vec3::new(0.0, -1.15, -1.3)), "barrel in the swung hand: {:?}", local(arm, barrel));
        assert!(near(local(arm, tip), Vec3::new(0.0, -2.05, -1.3)), "needle through the barrel: {:?}", local(arm, tip));
    }
}

#[cfg(test)]
mod hip_tests {
    use super::*;

    /// Studio's R15 body file: hip height 2.0 as it stands; with Studio's
    /// default Play sizes, Studio's own Play character measures 2.1924.
    /// A default HumanoidDescription (BodyTypeScale 0.3, ProportionScale 1)
    /// gives Studio's own Play character's part sizes; BodyTypeScale 0 (the
    /// user's avatar in checks/avatar-parity) keeps the body file's sizes,
    /// as Studio's Play character for that avatar measures.
    #[test]
    fn body_type_sizes_match_studio() {
        let Some(path) = studio_character_file(RigType::R15) else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let size = |s: &Scales| {
            let mut file = Document::decode(&bytes).unwrap();
            let root = file.root_ref();
            let m = dom_class_child(&file, root, "Model").unwrap();
            apply_r15_scales(&mut file, m, s);
            let mut out = HashMap::new();
            for p in file.get_by_ref(m).unwrap().children().to_vec() {
                let i = file.get_by_ref(p).unwrap();
                if let Some(Variant::Vector3(v)) = i.properties.get(&ustr("Size")) {
                    out.insert(i.name.clone(), Vec3::new(v.x, v.y, v.z));
                }
            }
            out
        };
        let def = size(&Scales::default());
        for (name, want) in mech::R15_DEFAULT_PART_SIZES {
            if *name == "Head" {
                continue; // the file's head is replaced by the dynamic head
            }
            let got = def[*name];
            assert!((got - Vec3::from(*want)).abs().max_element() < 0.0011, "{name}: {got} vs {want:?}");
        }
        let classic = size(&Scales { body_type: 0.0, proportion: 0.0, ..Scales::default() });
        assert!((classic["LeftUpperArm"] - Vec3::new(1.0, 1.1687, 1.0)).abs().max_element() < 1e-3);
        assert!((classic["LeftHand"] - Vec3::new(1.0, 0.3001, 1.0)).abs().max_element() < 1e-3);
        assert!((classic["UpperTorso"] - Vec3::new(2.0, 1.6, 1.0)).abs().max_element() < 1e-3);
    }

    #[test]
    fn hip_height_from_the_legs() {
        let Some(path) = studio_character_file(RigType::R15) else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let mut file = Document::decode(&bytes).unwrap();
        let root = file.root_ref();
        let m = dom_class_child(&file, root, "Model").unwrap();
        let hip = |f: &rbx_dom_weak::WeakDom| r15_hip_height(f, m).unwrap();
        assert!((hip(&file) - 2.0).abs() < 1e-3, "file hip {}", hip(&file));
        apply_r15_scales(&mut file, m, &Scales::default());
        let scaled = hip(&file);
        assert!(
            (scaled - mech::R15_PLAY_HIP_HEIGHT).abs() < 0.005,
            "scaled hip {scaled}"
        );
    }

    /// A package body (Studio 0.747, the same avatar: HipHeight 1.9670, root
    /// attachment y -0.9541, root 2.9668 over the floor, soles at 0) came out
    /// 1.363 with the root attachment left at the body file's -0.35: the
    /// character stood 0.6 low. Re-seating sets both from the legs.
    fn vec3_of_dom(f: &rbx_dom_weak::WeakDom, r: Ref, k: &str) -> Vec3 {
        match f.get_by_ref(r).unwrap().properties.get(&ustr(k)) {
            Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
            _ => panic!("{k}"),
        }
    }

    #[test]
    fn package_legs_reseat_root_attachment_and_hip_height() {
        let Some(path) = studio_character_file(RigType::R15) else {
            return;
        };
        let bytes = std::fs::read(path).unwrap();
        let mut file = Document::decode(&bytes).unwrap();
        let m = dom_class_child(&file, file.root_ref(), "Model").unwrap();
        apply_r15_scales(&mut file, m, &Scales::default());
        let att = |f: &rbx_dom_weak::WeakDom, part: &str, name: &str| {
            let a = dom_child(f, dom_child(f, m, part).unwrap(), name).unwrap();
            match f.get_by_ref(a).unwrap().properties.get(&ustr("CFrame")) {
                Some(Variant::CFrame(c)) => (a, c.clone()),
                _ => panic!("{part}.{name}"),
            }
        };
        // Studio's default body: -1.0.
        let (_, root_cf) = att(&file, "HumanoidRootPart", "RootRigAttachment");
        assert!((root_cf.position.y + 1.0).abs() < 1e-3, "default root attachment {}", root_cf.position.y);
        // A package: shorter upper legs, the root attachment stale.
        let (ra, rc) = att(&file, "HumanoidRootPart", "RootRigAttachment");
        set(&mut file, ra, "CFrame", rbx_dom_weak::types::CFrame::new(rbx_dom_weak::types::Vector3::new(0.0, -0.35, 0.0), rc.orientation));
        for side in ["Left", "Right"] {
            let (k, kc) = att(&file, &format!("{side}UpperLeg"), &format!("{side}KneeRigAttachment"));
            let mut p = kc.position;
            p.y += 0.2;
            set(&mut file, k, "CFrame", rbx_dom_weak::types::CFrame::new(p, kc.orientation));
        }
        reseat_r15_joints(&mut file, m);
        let y = |part: &str, name: &str| att(&file, part, name).1.position.y;
        let leg = y("LeftUpperLeg", "LeftHipRigAttachment") - y("LeftUpperLeg", "LeftKneeRigAttachment")
            + y("LeftLowerLeg", "LeftKneeRigAttachment") - y("LeftLowerLeg", "LeftAnkleRigAttachment")
            + y("LeftFoot", "LeftAnkleRigAttachment")
            + vec3_of_dom(&file, dom_child(&file, m, "LeftFoot").unwrap(), "Size").y * 0.5;
        let hip = r15_hip_height(&file, m).unwrap();
        assert!((hip - leg).abs() < 1e-5 && (hip - (mech::R15_PLAY_HIP_HEIGHT - 0.2)).abs() < 0.01, "hip {hip} leg {leg}");
        let half = vec3_of_dom(&file, dom_child(&file, m, "HumanoidRootPart").unwrap(), "Size").y * 0.5;
        let root_y = y("HumanoidRootPart", "RootRigAttachment");
        assert!((root_y - (y("LowerTorso", "RootRigAttachment") - y("LowerTorso", "LeftHipRigAttachment") - half)).abs() < 1e-5);
        // Standing (root at HipHeight + half its height), the soles touch y 0.
        let sole = (hip + half) + root_y - y("LowerTorso", "RootRigAttachment") + y("LowerTorso", "LeftHipRigAttachment") - leg;
        assert!(sole.abs() < 1e-4, "soles at {sole}");
        let h = dom_class_child(&file, m, "Humanoid").unwrap();
        assert!(matches!(file.get_by_ref(h).unwrap().properties.get(&ustr("HipHeight")), Some(Variant::Float32(v)) if (v - hip).abs() < 1e-6));
    }
}

/// Humanoid.Touched(otherPart, limb): fired with a touch that starts
/// between a character's body part (a direct child of the Humanoid's
/// model) and anything else, for each side that is one.
/// `find_child(model, "Humanoid")`, remembered per model until the next tree
/// change in the document (`tree_revision`). A touch reaches here for both
/// parts of every touching pair, and a map's parts sit in Models with
/// thousands of children: scanning them was ~6% of a large test place's Play step.
/// (The child count guards against an edit that skipped the journal.)
fn humanoid_child(d: &Document, model: Ref) -> Option<Ref> {
    #[derive(Default)]
    struct Memo {
        doc: u64,
        tree_revision: u64,
        journal_pos: u64,
        map: rustc_hash::FxHashMap<Ref, (Option<Ref>, usize)>,
    }
    thread_local! {
        static MEMO: std::cell::RefCell<Memo> = Default::default();
    }
    let children = d.dom.get_by_ref(model)?.children().len();
    MEMO.with(|m| {
        let mut m = m.borrow_mut();
        if m.doc != d.id || m.map.len() > 4096 {
            m.doc = d.id;
            m.map.clear();
        } else if m.tree_revision != d.tree_revision {
            // A tree change can only change the answer for the parent it
            // lands under (a child added there, or the remembered Humanoid
            // moved / removed: the parent check below). Without the marks
            // (dropped already), start over.
            match d.marks_since(m.journal_pos) {
                Some(marks) => {
                    for mark in marks {
                        if let doc::Mark::Tree(t) = *mark {
                            if let Some(i) = d.dom.get_by_ref(t) {
                                // A child that isn't a Humanoid can't become the
                                // first Humanoid child: only the count moves.
                                let parent = i.parent();
                                if i.class == "Humanoid" {
                                    m.map.remove(&parent);
                                } else if let Some(e) = m.map.get_mut(&parent) {
                                    e.1 = d.dom.get_by_ref(parent).map_or(0, |p| p.children().len());
                                }
                            }
                        }
                    }
                }
                None => m.map.clear(),
            }
        }
        m.tree_revision = d.tree_revision;
        m.journal_pos = d.journal_end();
        if let Some(&(h, n)) = m.map.get(&model) {
            let still = match h {
                Some(h) => d.dom.get_by_ref(h).is_some_and(|i| i.parent() == model && i.class == "Humanoid"),
                None => true,
            };
            if n == children && still {
                return h;
            }
        }
        let h = d.find_child(model, "Humanoid");
        m.map.insert(model, (h, children));
        h
    })
}

pub fn fire_humanoid_touched(rt: &crate::runtime::Runtime, a: Ref, b: Ref) {
    for (limb, other) in [(a, b), (b, a)] {
        let h = rt.world_mut().and_then(|w| {
            let d = w.d();
            let model = d.dom.get_by_ref(limb)?.parent();
            if model.is_none() || d.dom.get_by_ref(model)?.class != "Model" {
                return None;
            }
            humanoid_child(d, model)
        });
        if let Some(h) = h {
            rt.fire(h, "Touched", vec![rt.inst(other), rt.inst(limb)]);
        }
    }
}

// ----------------------------------------------------- avatar joint upgrade

/// StarterPlayer.AvatarJointUpgrade (RolloutState, saved as
/// `AvatarJointUpgrade_SerializedRollout`): Disabled 1 keeps Motor6D rigs,
/// Enabled 2 and Default 0 (or no saved value) upgrade them, Studio 0.746
/// and live Roblox both having FFlagAvatarJointUpgradeDefaultOn = true
/// (clientsettingscdn PCStudioApp / PCDesktopClient, 2026-11-08).
pub fn joint_upgrade(doc: &Document) -> bool {
    let Some(sp) = doc.find_child(doc.dom.root_ref(), "StarterPlayer") else {
        return true;
    };
    for key in ["AvatarJointUpgrade", "AvatarJointUpgrade_SerializedRollout"] {
        if let Some(Variant::Enum(e)) = doc.prop(sp, key) {
            return e.to_u32() != 1;
        }
    }
    true
}

/// One upgraded joint as Studio 0.746 builds it (measured on a spawned
/// R15 character): the BallSocketConstraint's UpperAngle, TwistLowerAngle,
/// TwistUpperAngle, MaxFrictionTorque, and for the shoulders the rotation
/// of the `JointRotation` attachments (Part0 side, Part1 side) it uses
/// instead of the rig attachments. MaxFrictionTorque is Studio's value for
/// its default body (it depends on the body's masses; it only acts when a
/// ball socket is simulated, and rustudio's joints are kinematic while
/// alive and disabled on death, as Studio's are).
pub struct UpgradeSpec {
    pub upper: f32,
    pub twist: (f32, f32),
    pub friction: f32,
    pub rotation: Option<([f32; 9], [f32; 9])>,
}

pub fn upgrade_spec(joint: &str) -> Option<UpgradeSpec> {
    let s = |upper, lo, hi, friction| UpgradeSpec { upper, twist: (lo, hi), friction, rotation: None };
    Some(match joint {
        "LeftWrist" | "RightWrist" => s(30.0, -10.0, 10.0, 16.179_634),
        "LeftElbow" | "RightElbow" => s(20.0, 5.0, 120.0, 55.882_8),
        "LeftShoulder" => UpgradeSpec {
            rotation: Some((
                [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0],
                [0.0, -1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, -1.0],
            )),
            ..s(110.0, -85.0, 85.0, 47.937_03)
        },
        "RightShoulder" => UpgradeSpec {
            rotation: Some((
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
            )),
            ..s(110.0, -85.0, 85.0, 47.937_03)
        },
        "LeftAnkle" | "RightAnkle" => s(10.0, -10.0, 10.0, 25.253_056),
        "LeftKnee" | "RightKnee" => s(5.0, -120.0, -5.0, 67.240_42),
        "Waist" => s(20.0, -40.0, 20.0, 40.519_5),
        "LeftHip" | "RightHip" => s(40.0, -5.0, 80.0, 24.025_192),
        "Neck" => s(45.0, -40.0, 40.0, 37.704_8),
        _ => return None,
    })
}

fn rig_attachment(dom: &rbx_dom_weak::WeakDom, part: Ref, name: &str) -> Option<Ref> {
    dom.get_by_ref(part)?.children().iter().copied().find(|c| {
        dom.get_by_ref(*c)
            .is_some_and(|i| i.class == "Attachment" && i.name == name)
    })
}

fn att_cf(dom: &rbx_dom_weak::WeakDom, a: Ref) -> CFrame {
    match dom.get_by_ref(a).and_then(|i| i.properties.get(&ustr("CFrame"))) {
        Some(Variant::CFrame(c)) => *c,
        _ => CFrame::identity(),
    }
}

/// Builds Studio's upgraded joint `name` between `p0` (attachment `a0`)
/// and `p1` (`a1`): an AnimationConstraint (kinematic, MaxTorque 3000)
/// and, except for Root, a BallSocketConstraint, both parented to `p1`.
/// Returns the AnimationConstraint.
pub fn build_upgraded_joint(
    dom: &mut rbx_dom_weak::WeakDom,
    name: &str,
    p0: Ref,
    a0: Ref,
    p1: Ref,
    a1: Ref,
) -> Ref {
    let ac = dom.insert(
        p1,
        InstanceBuilder::new("AnimationConstraint")
            .with_name(name)
            .with_property("Attachment0", Variant::Ref(a0))
            .with_property("Attachment1", Variant::Ref(a1))
            .with_property("Part0", Variant::Ref(p0))
            .with_property("Part1", Variant::Ref(p1))
            .with_property("C0", Variant::CFrame(att_cf(dom, a0)))
            .with_property("C1", Variant::CFrame(att_cf(dom, a1)))
            .with_property("Transform", Variant::CFrame(CFrame::identity()))
            .with_property("IsKinematic", true)
            .with_property("MaxForce", 0.0f32)
            .with_property("MaxTorque", 3000.0f32)
            .with_property("Enabled", true)
            .with_property("Visible", false),
    );
    if let Some(spec) = upgrade_spec(name) {
        let (b0, b1) = match spec.rotation {
            Some((r0, r1)) => {
                let jr = |dom: &mut rbx_dom_weak::WeakDom, a: Ref, r: [f32; 9]| {
                    let cf = CFrame::new(
                        Vector3::new(0.0, 0.0, 0.0),
                        rbx_dom_weak::types::Matrix3::new(
                            Vector3::new(r[0], r[1], r[2]),
                            Vector3::new(r[3], r[4], r[5]),
                            Vector3::new(r[6], r[7], r[8]),
                        ),
                    );
                    dom.insert(
                        a,
                        InstanceBuilder::new("Attachment")
                            .with_name("JointRotation")
                            .with_property("CFrame", Variant::CFrame(cf))
                            .with_property("Visible", false),
                    )
                };
                (jr(dom, a0, r0), jr(dom, a1, r1))
            }
            None => (a0, a1),
        };
        dom.insert(
            p1,
            InstanceBuilder::new("BallSocketConstraint")
                .with_name(format!("{name}BallSocket"))
                .with_property("Attachment0", Variant::Ref(b0))
                .with_property("Attachment1", Variant::Ref(b1))
                .with_property("LimitsEnabled", true)
                .with_property("UpperAngle", spec.upper)
                .with_property("TwistLimitsEnabled", true)
                .with_property("TwistLowerAngle", spec.twist.0)
                .with_property("TwistUpperAngle", spec.twist.1)
                .with_property("MaxFrictionTorque", spec.friction)
                .with_property("Restitution", 0.0f32)
                .with_property("Radius", 0.15f32)
                .with_property("Enabled", true)
                .with_property("Visible", false),
        );
    }
    ac
}

/// Studio's joint upgrade of an R15 character: each Motor6D that joins two
/// body parts on same-named `<Joint>RigAttachment`s becomes an
/// AnimationConstraint + BallSocketConstraint (other Motor6Ds stay:
/// DFFlagAvatarJointUpgradeOnlyDestroyRigMotor6Ds). R6 rigs are left as
/// they are (measured: an R6 character keeps its Motor6Ds).
pub fn upgrade_rig(dom: &mut rbx_dom_weak::WeakDom, model: Ref) -> usize {
    let motors: Vec<(Ref, String, Ref, Ref)> = dom
        .descendants_of(model)
        .filter(|i| i.class == "Motor6D")
        .filter_map(|i| {
            let part = |k: &str| match i.properties.get(&ustr(k)) {
                Some(Variant::Ref(r)) if r.is_some() => Some(*r),
                _ => None,
            };
            Some((i.referent(), i.name.clone(), part("Part0")?, part("Part1")?))
        })
        .collect();
    let mut n = 0;
    for (m, name, p0, p1) in motors {
        let att = format!("{name}RigAttachment");
        let (Some(a0), Some(a1)) = (rig_attachment(dom, p0, &att), rig_attachment(dom, p1, &att)) else {
            continue;
        };
        let transform = dom
            .get_by_ref(m)
            .and_then(|i| i.properties.get(&ustr("Transform")).cloned());
        let ac = build_upgraded_joint(dom, &name, p0, a0, p1, a1);
        if let (Some(t), Some(i)) = (transform, dom.get_by_ref_mut(ac)) {
            i.properties.insert(ustr("Transform"), t);
        }
        dom.destroy(m);
        n += 1;
    }
    if n > 0 {
        add_r15_no_collisions(dom, model);
    }
    n
}

/// The NoCollisionConstraints Studio's R15 avatar build puts beside the upgraded joints
/// (read from Studio-11-13, CreateHumanoidModelFromDescription(default, R15)):
/// `Part0/<Part1>NoCollision`, Part0 = its parent, all Enabled. They decide which body
/// parts of a dead character push each other apart (solver/CHAR-NOTES, T14): the root
/// part never collides with the UpperTorso or the Head it overlaps.
pub const R15_NO_COLLISIONS: [(&str, &str); 19] = [
    ("LeftUpperArm", "LeftHand"),
    ("RightUpperArm", "RightHand"),
    ("UpperTorso", "HumanoidRootPart"),
    ("UpperTorso", "RightUpperLeg"),
    ("UpperTorso", "LeftUpperLeg"),
    ("UpperTorso", "LeftLowerLeg"),
    ("UpperTorso", "RightLowerLeg"),
    ("UpperTorso", "LeftLowerArm"),
    ("UpperTorso", "RightLowerArm"),
    ("LeftUpperLeg", "RightUpperLeg"),
    ("LeftUpperLeg", "LeftFoot"),
    ("LowerTorso", "LeftUpperArm"),
    ("LowerTorso", "RightUpperArm"),
    ("LowerTorso", "LeftLowerLeg"),
    ("LowerTorso", "RightLowerLeg"),
    ("RightUpperLeg", "RightFoot"),
    ("Head", "HumanoidRootPart"),
    ("Head", "LeftUpperArm"),
    ("Head", "RightUpperArm"),
];

fn add_r15_no_collisions(dom: &mut rbx_dom_weak::WeakDom, model: Ref) {
    for (a, b) in R15_NO_COLLISIONS {
        let (Some(p0), Some(p1)) = (dom_child(dom, model, a), dom_child(dom, model, b)) else { continue };
        let name = format!("{b}NoCollision");
        if dom_child(dom, p0, &name).is_some() {
            continue;
        }
        dom.insert(
            p0,
            InstanceBuilder::new("NoCollisionConstraint")
                .with_name(name)
                .with_property("Part0", Variant::Ref(p0))
                .with_property("Part1", Variant::Ref(p1))
                .with_property("Enabled", true),
        );
    }
}


/// Joint classes posed by an Animator through Transform.
pub fn is_animated_joint(class: &str) -> bool {
    class == "Motor6D" || class == "AnimationConstraint"
}

/// The BallSocketConstraints of an upgraded rig.
pub fn ball_sockets(doc: &Document, model: Ref) -> Vec<Ref> {
    doc.dom
        .descendants_of(model)
        .filter(|i| i.class == "BallSocketConstraint")
        .map(|i| i.referent())
        .collect()
}
