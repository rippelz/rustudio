//! Plugin:ImportFbxAnimation(rig, isR15): the picked FBX file's animation
//! (crates/rustudio-core/src/fbx_anim.rs) as a KeyframeSequence, as Studio 0.747 makes it
//! (measured: captures/fbxanim-studio.txt, studio_fbxanim.py). The dialog,
//! its "No file selected" cancel and the yield are api_studio_service.rs's.
//!
//! - The sequence: named "<file stem>_<stack name>", Loop true, Priority
//!   Action, AuthoredHipHeight 2, parented to Workspace (an ordinary
//!   change: no waypoint of its own, undone with whatever the next undo
//!   takes back; the selection is left alone).
//! - Its children: a Keyframe per frame (fbx_anim.rs: Times from 0 at the
//!   file's frame rate), named "Keyframe", the last "End"; then, with
//!   isR15, an AnimationRigData (below).
//! - Each Keyframe holds every kept node's Pose (the file's node tree and
//!   names, not the rig's): Weight 1, EasingStyle Linear, EasingDirection
//!   In, in file order.
//! - The rig argument is only checked to be a Model (after the dialog:
//!   "A Model rig must be passed as the first argument", also when the
//!   picked file is unreadable: a Part or the Camera with a text, ASCII or
//!   animation-less file gives this, not "Import failed"); its joints,
//!   names, C0 / C1 and Humanoid don't change the result.
//! - The AnimationRigData Studio writes is the same two-node stub for every
//!   file and rig (no labels, identity transforms, nodes "" and
//!   HumanoidRootPart); only one float of its Generic blob varies:
//!   -(LowerTorso's rest height) - 1, or -1 without a LowerTorso that has
//!   a NodeAttribute. A file with no NodeAttribute at all (no
//!   DefaultAttributeIndex anywhere) gets a one-node stub (the "" node
//!   only) and one Pose per Keyframe named after the file, the 180° turn.

use rbx_dom_weak::types::{BinaryString, CFrame, Enum, Ref, Variant};
use rbx_dom_weak::InstanceBuilder;

use super::*;
use rustudio_core::fbx_anim::{Clip, Xf};
use rustudio_core::math::Xform;

fn enum_value(e: &str, name: &str) -> Variant {
    let v = rustudio_engine::ltypes::LEnumItem::from_name(e, name).map(|i| i.value as u32).unwrap_or(0);
    Variant::Enum(Enum::from_u32(v))
}

fn cframe(x: &Xf) -> CFrame {
    Xform { pos: x.pos.as_vec3(), rot: x.rot.as_mat3() }.to_cframe()
}

/// The AnimationRigData Studio puts in an R15 import (binary properties as
/// it serializes them: a version, a count, then per node). `one`: the
/// one-node stub of a file without NodeAttributes (no HumanoidRootPart).
fn rig_data(lower_torso_height: Option<f64>, one: bool) -> InstanceBuilder {
    let mut b = InstanceBuilder::new("AnimationRigData").with_name("AnimationRigData");
    let le = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let count = if one { 1 } else { 2 };
    let (label, mut name, mut parent) = if one {
        (le(&[2, 1, 0]), le(&[1, 1, 0]), le(&[1, 1]))
    } else {
        (le(&[2, 2, 0, 0]), le(&[1, 2, 0, 16]), le(&[1, 2]))
    };
    if one {
        parent.extend_from_slice(&[0, 0]);
    } else {
        name.extend_from_slice(b"HumanoidRootPart");
        parent.extend_from_slice(&[0, 0, 1, 0]);
    }
    let mut xf = le(&[2, count]);
    for _ in 0..count {
        for f in [1.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0] {
            xf.extend_from_slice(&f.to_le_bytes());
        }
    }
    let floor = lower_torso_height.map(|h| -(h as f32) - 1.0).unwrap_or(-1.0);
    let mut generic = le(&[3]);
    for f in [floor, 2.0, 2.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        generic.extend_from_slice(&f.to_le_bytes());
    }
    generic.extend_from_slice(&[0; 14]);
    for (k, v) in [
        ("label", label),
        ("name", name),
        ("parent", parent),
        ("transform", xf.clone()),
        ("preTransform", xf.clone()),
        ("postTransform", xf),
        ("Generic", generic),
    ] {
        b = b.with_property(k, Variant::BinaryString(BinaryString::from(v)));
    }
    b
}

/// Builds the KeyframeSequence (parentless; the caller parents it).
pub fn build(lua: &Lua, clip: &Clip, stem: &str, r15: bool) -> Result<Ref, String> {
    let mut w = world(lua).map_err(|e| e.to_string())?;
    let d = w.dm();
    let ease = (enum_value("PoseEasingStyle", "Linear"), enum_value("PoseEasingDirection", "In"));
    let mut seq = InstanceBuilder::new("KeyframeSequence")
        .with_name(format!("{stem}_{}", clip.name))
        .with_property("Loop", Variant::Bool(true))
        .with_property("Priority", enum_value("AnimationPriority", "Action"))
        .with_property("AuthoredHipHeight", Variant::Float32(2.0));
    fn pose(clip: &Clip, i: usize, frame: usize, ease: &(Variant, Variant)) -> InstanceBuilder {
        let b = &clip.bones[i];
        let mut p = InstanceBuilder::new("Pose")
            .with_name(b.name.clone())
            .with_property("CFrame", Variant::CFrame(cframe(&b.poses[frame])))
            .with_property("Weight", Variant::Float32(1.0))
            .with_property("EasingStyle", ease.0.clone())
            .with_property("EasingDirection", ease.1.clone());
        for (k, c) in clip.bones.iter().enumerate() {
            if c.parent == Some(i) {
                p = p.with_child(pose(clip, k, frame, ease));
            }
        }
        p
    }
    let last = clip.times.len().saturating_sub(1);
    for (f, t) in clip.times.iter().enumerate() {
        let mut kf = InstanceBuilder::new("Keyframe")
            .with_name(if f == last { "End" } else { "Keyframe" })
            .with_property("Time", Variant::Float32(*t as f32));
        for (i, b) in clip.bones.iter().enumerate() {
            if b.parent.is_none() {
                kf = kf.with_child(pose(clip, i, f, &ease));
            }
        }
        if clip.file_root {
            let turn = Xf { rot: glam::DMat3::from_diagonal(glam::DVec3::new(-1.0, 1.0, -1.0)), pos: glam::DVec3::ZERO };
            kf = kf.with_child(
                InstanceBuilder::new("Pose")
                    .with_name(stem)
                    .with_property("CFrame", Variant::CFrame(cframe(&turn)))
                    .with_property("Weight", Variant::Float32(1.0))
                    .with_property("EasingStyle", ease.0.clone())
                    .with_property("EasingDirection", ease.1.clone()),
            );
        }
        seq = seq.with_child(kf);
    }
    if r15 {
        seq = seq.with_child(rig_data(clip.lower_torso_height, clip.file_root));
    }
    Ok(d.dom.insert(Ref::none(), seq))
}
