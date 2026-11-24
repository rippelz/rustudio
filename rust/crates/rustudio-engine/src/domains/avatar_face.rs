//! FaceControls: the 50 FACS properties really drive the face. Values are
//! clamped to [0, 1] and kept on the instance (Clone copies them, Changed
//! fires), as in Studio. When the FaceControls sits in a head MeshPart whose
//! mesh carries FACS data (mesh v4/v5 trailing block, or the v6/v7 FACS
//! chunk), every frame with a change poses the head's Bones the way Roblox
//! does: each bone's offset is the sum over controls of control value ×
//! that control's pose column, plus the two- and three-control correctives
//! weighted by the product of their controls; the result is written to
//! Bone.Transform (position in studs at the part's scale, rotation from the
//! XYZ Euler degrees in the data). Meshes are fetched once per id, off the
//! script thread. Mesh skinning in the renderer is outside this domain: the
//! Bones move (scripts and Bone.TransformedCFrame see them); the drawn
//! head deforms once the renderer skins meshes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use mlua::{Lua, Value};
use rbx_dom_weak::types::{Ref, Variant};

// FACS rigs, parsing and posing are core's (`mesh`): the renderer's skinning
// reads them as well as this domain.
use rustudio_core::mesh::{parse_mesh_facs, pose, Facs};

use crate::runtime::*;

pub const CONTROLS: &[&str] = &[
    "ChinRaiser", "ChinRaiserUpperLip", "Corrugator", "EyesLookDown", "EyesLookLeft", "EyesLookRight",
    "EyesLookUp", "FlatPucker", "Funneler", "JawDrop", "JawLeft", "JawRight", "LeftBrowLowerer",
    "LeftCheekPuff", "LeftCheekRaiser", "LeftDimpler", "LeftEyeClosed", "LeftEyeUpperLidRaiser",
    "LeftInnerBrowRaiser", "LeftLipCornerDown", "LeftLipCornerPuller", "LeftLipStretcher",
    "LeftLowerLipDepressor", "LeftNoseWrinkler", "LeftOuterBrowRaiser", "LeftUpperLipRaiser", "LipPresser",
    "LipsTogether", "LowerLipSuck", "MouthLeft", "MouthRight", "Pucker", "RightBrowLowerer", "RightCheekPuff",
    "RightCheekRaiser", "RightDimpler", "RightEyeClosed", "RightEyeUpperLidRaiser", "RightInnerBrowRaiser",
    "RightLipCornerDown", "RightLipCornerPuller", "RightLipStretcher", "RightLowerLipDepressor",
    "RightNoseWrinkler", "RightOuterBrowRaiser", "RightUpperLipRaiser", "TongueDown", "TongueOut", "TongueUp",
    "UpperLipSuck",
];

/// Mesh FACS rigs by asset id: None while loading, Some(None) without FACS.
static RIGS: std::sync::LazyLock<Mutex<HashMap<u64, Option<Option<Arc<Facs>>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn rig_for(id: u64) -> Option<Arc<Facs>> {
    let mut g = RIGS.lock().unwrap_or_else(|e| e.into_inner());
    match g.get(&id) {
        Some(Some(r)) => return r.clone(),
        Some(None) => return None,
        None => {}
    }
    g.insert(id, None);
    std::thread::spawn(move || {
        let r = rustudio_core::assets::public_cached(id)
            .ok()
            .and_then(|b| parse_mesh_facs(&b))
            .map(Arc::new);
        if let Ok(mut g) = RIGS.lock() {
            g.insert(id, Some(r));
        }
    });
    None
}

/// FaceControls whose values changed (or that wait for their mesh).
#[derive(Default)]
pub struct Dirty(pub HashSet<Ref>);

pub fn is_control(key: &str) -> bool {
    CONTROLS.contains(&key)
}

pub fn prop_get(lua: &Lua, r: Ref, key: &str) -> mlua::Result<Option<Value>> {
    if !is_control(key) {
        return Ok(None);
    }
    let v = match world(lua)?.d().prop(r, key) {
        Some(Variant::Float32(f)) => f as f64,
        Some(Variant::Float64(f)) => f,
        _ => 0.0,
    };
    Ok(Some(Value::Number(v)))
}

pub fn newindex(lua: &Lua, r: Ref, key: &str, value: &Value) -> mlua::Result<bool> {
    if !is_control(key) {
        return Ok(false);
    }
    let n = num(value).ok_or_else(|| {
        rt_err(format!("Unable to assign property {key}. float expected, got {}", value.type_name()))
    })?;
    store_prop(lua, r, key, Variant::Float32((n as f32).clamp(0.0, 1.0)))?;
    if let Some(mut d) = lua.app_data_mut::<Dirty>() {
        d.0.insert(r);
    }
    Ok(true)
}

fn mesh_id(lua: &Lua, part: Ref) -> mlua::Result<Option<u64>> {
    let w = world(lua)?;
    let d = w.d();
    let s = ["MeshContent", "MeshId"].iter().find_map(|k| match d.prop(part, k) {
        Some(Variant::Content(c)) => c.as_uri().map(str::to_string),
        Some(Variant::ContentId(c)) => Some(c.as_str().to_string()),
        Some(Variant::String(s)) => Some(s),
        _ => None,
    });
    Ok(s.and_then(|s| rustudio_core::assets::asset_ref(&s)).map(|a| a.id))
}

fn v3(d: &rustudio_core::doc::Document, r: Ref, k: &str) -> Option<glam::Vec3> {
    match d.prop(r, k) {
        Some(Variant::Vector3(v)) => Some(glam::Vec3::new(v.x, v.y, v.z)),
        _ => None,
    }
}

/// Poses the bones of every changed FaceControls whose mesh has FACS.
pub fn step(lua: &Lua) -> mlua::Result<()> {
    let pending: Vec<Ref> = match lua.app_data_mut::<Dirty>() {
        Some(mut d) if !d.0.is_empty() => d.0.drain().collect(),
        _ => return Ok(()),
    };
    let mut again = Vec::new();
    for fc in pending {
        let part = {
            let w = world(lua)?;
            let d = w.d();
            let Some(p) = d.dom.get_by_ref(fc).map(|i| i.parent()) else { continue };
            if !d.class_of(p).is_some_and(|c| c == "MeshPart") {
                continue;
            }
            p
        };
        let Some(id) = mesh_id(lua, part)? else { continue };
        let Some(rig) = rig_for(id) else {
            // Still loading (or no FACS): look again next frame while loading.
            if RIGS.lock().map(|g| matches!(g.get(&id), Some(None))).unwrap_or(false) {
                again.push(fc);
            }
            continue;
        };
        let (values, scale, bones) = {
            let w = world(lua)?;
            let d = w.d();
            let values: HashMap<&str, f32> = CONTROLS
                .iter()
                .map(|c| (*c, match d.prop(fc, c) { Some(Variant::Float32(f)) => f, _ => 0.0 }))
                .collect();
            let scale = match (v3(d, part, "Size"), v3(d, part, "MeshSize").or_else(|| v3(d, part, "InitialSize"))) {
                (Some(s), Some(m)) if m.x > 0.0 && m.y > 0.0 && m.z > 0.0 => s / m,
                _ => glam::Vec3::ONE,
            };
            let bones: HashMap<String, Ref> = d
                .dom
                .descendants_of(part)
                .filter(|i| i.class == "Bone")
                .map(|i| (i.name.to_string(), i.referent()))
                .collect();
            (values, scale, bones)
        };
        for (name, p, r) in pose(&rig, &|c| values.get(c).copied().unwrap_or(0.0)) {
            let Some(&b) = bones.get(&name) else { continue };
            let rot = glam::Mat3::from_euler(glam::EulerRot::XYZ, r[0].to_radians(), r[1].to_radians(), r[2].to_radians());
            let xf = rustudio_core::math::Xform { pos: glam::Vec3::from(p) * scale, rot };
            store_prop(lua, b, "Transform", Variant::CFrame(xf.to_cframe()))?;
        }
    }
    if !again.is_empty() {
        if let Some(mut d) = lua.app_data_mut::<Dirty>() {
            d.0.extend(again);
        }
    }
    Ok(())
}
