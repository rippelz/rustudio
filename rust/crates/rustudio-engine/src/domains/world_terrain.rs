//! Terrain's legacy cell API on smooth voxels, as Studio 0.705 maps it
//! (measured in an offline Studio):
//! - SetCell / SetCells write a full voxel of the CellMaterial's smooth
//!   material (Granite → Slate, Iron / Aluminum / Gold / CinderBlock /
//!   Cement → Concrete, WoodPlank / WoodLog → WoodPlanks, Gravel → Asphalt,
//!   MossyStone → Pavement, RedPlastic → Sandstone, BluePlastic → Ice,
//!   Empty → Air); the CellBlock sets the occupancy (Solid 1, the wedges ½,
//!   CornerWedge ⅓, InverseCornerWedge ⅔; reads 1 / 0.5 / 0.336 / 0.668);
//!   the orientation is ignored. SetCells' Region3int16 is inclusive.
//! - GetCell maps back (Slate → Granite, Concrete → Cement, WoodPlanks →
//!   WoodPlank, Pavement → MossyStone, Sandstone → RedPlastic, Ice →
//!   BluePlastic, Water → Water, Air → Empty, anything else → Grass) and
//!   always answers block Solid, orientation NegZ.
//! - SetWaterCell writes a full Water voxel; GetWaterCell answers (is
//!   water, WaterForce.None, WaterDirection.NegX) — force and direction
//!   aren't kept by smooth terrain.
//! - CountCells = non-Air voxels. AutowedgeCell is false and AutowedgeCells
//!   does nothing (smooth terrain has no wedges to add).
//! - Material slots 0…63: 0…22 are the smooth materials (variant "", the
//!   terrain's MaterialColors), 23…63 empty; slots 0 / 1 (Air, Water) can't
//!   be set or reset; others take a terrain material, a variant name and a
//!   colour. Messages are Studio's.
//! - Read / Write / Modify / Iterate / ClearVoxelsAsync_beta: Studio
//!   refuses them all ("Beta is not enabled", before any argument check),
//!   so their operation objects (TerrainIterate/Modify/WriteOperation) are
//!   never reached by a script.

use std::cell::RefCell;
use std::collections::HashMap;

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::Ref;

use super::util::*;
use crate::ltypes::*;
use crate::runtime::*;
use rustudio_core::terrain::{self, Voxels};

const CELL_TO_SMOOTH: &[(&str, &str)] = &[
    ("Empty", "Air"),
    ("Grass", "Grass"),
    ("Sand", "Sand"),
    ("Brick", "Brick"),
    ("Granite", "Slate"),
    ("Asphalt", "Asphalt"),
    ("Iron", "Concrete"),
    ("Aluminum", "Concrete"),
    ("Gold", "Concrete"),
    ("WoodPlank", "WoodPlanks"),
    ("WoodLog", "WoodPlanks"),
    ("Gravel", "Asphalt"),
    ("CinderBlock", "Concrete"),
    ("MossyStone", "Pavement"),
    ("Cement", "Concrete"),
    ("RedPlastic", "Sandstone"),
    ("BluePlastic", "Ice"),
    ("Water", "Water"),
];

const SMOOTH_TO_CELL: &[(&str, &str)] = &[
    ("Air", "Empty"),
    ("Grass", "Grass"),
    ("Sand", "Sand"),
    ("Brick", "Brick"),
    ("Slate", "Granite"),
    ("Asphalt", "Asphalt"),
    ("Concrete", "Cement"),
    ("WoodPlanks", "WoodPlank"),
    ("Pavement", "MossyStone"),
    ("Sandstone", "RedPlastic"),
    ("Ice", "BluePlastic"),
    ("Water", "Water"),
];

/// CellBlock → stored share (bytes 255 / 127 / 85 / 170, as Studio's).
fn block_share(b: u32) -> f32 {
    match b {
        1 | 4 => 127.0 / 255.0,
        2 => 85.0 / 255.0,
        3 => 170.0 / 255.0,
        _ => 1.0,
    }
}

thread_local! {
    /// (document id, terrain) → slot → (material, variant, colour).
    static SLOTS: RefCell<HashMap<(u64, Ref), HashMap<i64, (u8, String, [f32; 3])>>> = RefCell::new(HashMap::new());
}

/// The voxels being edited this frame, else the saved grid (as runtime_ext).
fn voxels(lua: &Lua, this: Ref) -> mlua::Result<Voxels> {
    let mut w = world(lua)?;
    Ok(match w.terrain_edit.take() {
        Some((r, v)) if r == this => v,
        Some(other) => {
            w.terrain_edit = Some(other);
            match Voxels::from_dom(&w.d().dom) {
                Some(Ok(v)) => v,
                _ => Voxels::empty(),
            }
        }
        None => match Voxels::from_dom(&w.d().dom) {
            Some(Ok(v)) => v,
            _ => Voxels::empty(),
        },
    })
}

fn put_back(lua: &Lua, this: Ref, v: Voxels) -> mlua::Result<()> {
    world(lua)?.terrain_edit = Some((this, v));
    Ok(())
}

/// Integer arguments 1..=n; Studio names the last missing one.
fn ints(args: &MultiValue, n: usize) -> mlua::Result<Vec<i32>> {
    if let Some(i) = (1..=n).rev().find(|i| num_arg(args, *i).is_none()) {
        return Err(missing(i));
    }
    Ok((1..=n).map(|i| num_arg(args, i).unwrap_or(0.0) as i32).collect())
}

fn cell_material(lua: &Lua, m: u8) -> mlua::Result<Value> {
    let smooth = terrain::material_name(m);
    let cell = SMOOTH_TO_CELL.iter().find(|(s, _)| *s == smooth).map(|(_, c)| *c).unwrap_or("Grass");
    enum_item(lua, "CellMaterial", cell)
}

pub fn call(lua: &Lua, name: &str, this: Ref, class: &str, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if class == "TerrainRegion" {
        return match name {
            "ApplyTransform" | "ApplyTransformSubregion" => Some(region_transform_call(lua, name, this, args)),
            _ => None,
        };
    }
    if class != "Terrain" {
        return None;
    }
    Some(match name {
        "GetCell" => (|| {
            let c = ints(args, 3)?;
            let v = voxels(lua, this)?;
            let (m, _) = v.get(c[0], c[1], c[2]);
            put_back(lua, this, v)?;
            Ok(MultiValue::from_iter([
                cell_material(lua, m)?,
                enum_item(lua, "CellBlock", "Solid")?,
                enum_item(lua, "CellOrientation", "NegZ")?,
            ]))
        })(),
        "GetWaterCell" => (|| {
            let c = ints(args, 3)?;
            let v = voxels(lua, this)?;
            let (m, _) = v.get(c[0], c[1], c[2]);
            put_back(lua, this, v)?;
            Ok(MultiValue::from_iter([
                Value::Boolean(terrain::is_water(m)),
                enum_item(lua, "WaterForce", "None")?,
                enum_item(lua, "WaterDirection", "NegX")?,
            ]))
        })(),
        "SetWaterCell" => (|| {
            if arg(args, 5).is_nil() {
                return Err(missing(5));
            }
            if arg(args, 4).is_nil() {
                return Err(missing(4));
            }
            let c = ints(args, 3)?;
            let mut v = voxels(lua, this)?;
            v.set([c[0], c[1], c[2]], terrain::WATER, 1.0);
            put_back(lua, this, v)?;
            none()
        })(),
        "SetCell" => (|| {
            for i in (4..=6).rev() {
                if arg(args, i).is_nil() {
                    return Err(missing(i));
                }
            }
            let c = ints(args, 3)?;
            let cm = enum_of(&arg(args, 4), "CellMaterial").ok_or_else(|| rt_err("Unable to cast value to CellMaterial"))?;
            let block = enum_of(&arg(args, 5), "CellBlock").unwrap_or(0);
            let mut v = voxels(lua, this)?;
            v.set([c[0], c[1], c[2]], smooth_of(cm), block_share(block));
            put_back(lua, this, v)?;
            none()
        })(),
        "SetCells" => (|| {
            for i in (1..=4).rev() {
                if arg(args, i).is_nil() {
                    return Err(missing(i));
                }
            }
            let reg = ud::<LRegion3int16>(&arg(args, 1)).ok_or_else(|| rt_err("Unable to cast value to Region3int16"))?;
            let cm = enum_of(&arg(args, 2), "CellMaterial").ok_or_else(|| rt_err("Unable to cast value to CellMaterial"))?;
            let block = enum_of(&arg(args, 3), "CellBlock").unwrap_or(0);
            let mat = smooth_of(cm);
            let (lo, hi) = (reg.min.0, reg.max.0);
            let mut cells = Vec::new();
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        cells.push(([x as i32, y as i32, z as i32], mat, block_share(block)));
                    }
                }
            }
            let mut v = voxels(lua, this)?;
            v.set_many(cells);
            put_back(lua, this, v)?;
            none()
        })(),
        "CountCells" => (|| {
            let v = voxels(lua, this)?;
            let n = v.count();
            put_back(lua, this, v)?;
            one(Value::Number(n as f64))
        })(),
        "AutowedgeCell" => (|| {
            ints(args, 3)?;
            one(Value::Boolean(false))
        })(),
        "AutowedgeCells" => {
            if arg(args, 1).is_nil() { Err(missing(1)) } else { none() }
        }
        "GetMaterialSlot" | "SetMaterialSlot" | "ResetMaterialSlot" => slot_call(lua, name, this, args),
        "IsMaterialSlotInvalid" => (|| {
            let i = num_arg(args, 1).ok_or_else(|| missing(1))? as i64;
            one(Value::Boolean(!(0..64).contains(&i)))
        })(),
        "CanSmoothVoxelsBeUpgraded" => one(Value::Boolean(false)),
        "GetTerrainWireframe" => (|| {
            let _cf = ud::<LCFrame>(&arg(args, 1)).ok_or_else(|| rt_err("Terrain:GetTerrainWireframe: CFrame expected"))?;
            let _sz = vec3_arg(args, 2)?;
            one(Value::Table(lua.create_table()?))
        })(),
        "GenerateWaterFlowMap" | "ResetWaterFlowMap" => none(),
        "ReplaceMaterialInTransform"
        | "ReplaceMaterialInTransformSubregion"
        | "ReplaceMaterialInTransformSubregionSlot"
        | "SetMaterialInTransform"
        | "SetMaterialInTransformSubregion"
        | "SetMaterialInTransformSubregionSlot" => transform_material_call(lua, name, this, args),
        "FillBallSlot" | "FillBlockSlot" | "FillCylinderSlot" | "FillRegionSlot" | "FillWedgeSlot" => {
            fill_slot_call(lua, name, this, args)
        }
        "SmoothRegion" | "SmoothRegionMaterialSlots" => smooth_region_call(lua, name, this, args),
        "ReadVoxelsAsync_beta" | "WriteVoxelsAsync_beta" | "ModifyVoxelsAsync_beta" | "IterateVoxelsAsync_beta"
        | "ClearVoxelsAsync_beta" => Err(rt_err("Beta is not enabled")),
        _ => return None,
    })
}

fn region_transform_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let rot = ud::<LCFrame>(&arg(args, 1))
        .ok_or_else(|| rt_err(format!("TerrainRegion:{name}: CFrame expected")))?
        .0
        .rot;
    let size = vec3_arg(args, 2)?;
    let sub = if name == "ApplyTransformSubregion" {
        let rg = ud::<LRegion3int16>(&arg(args, 3))
            .ok_or_else(|| rt_err("TerrainRegion:ApplyTransformSubregion: Region3int16 expected"))?;
        Some((rg.min.0.map(i32::from), rg.max.0.map(i32::from)))
    } else {
        None
    };
    let src = {
        let w = world(lua)?;
        let d = w.d();
        let ext = |k: &str| match d.prop(this, k) {
            Some(rbx_dom_weak::types::Variant::Vector3int16(v)) => [v.x as i32, v.y as i32, v.z as i32],
            _ => [0; 3],
        };
        let (min, max) = (ext("ExtentsMin"), ext("ExtentsMax"));
        let grid = match d.prop(this, "SmoothGrid") {
            Some(rbx_dom_weak::types::Variant::BinaryString(b)) => AsRef::<[u8]>::as_ref(&b).to_vec(),
            _ => Vec::new(),
        };
        rustudio_core::terrain_region::Region::from_smooth_grid(&grid, min, max)
            .map_err(|e| rt_err(format!("TerrainRegion:{name}: {e}")))?
    };
    let out = src.apply_transform(rot, size, sub);
    let v3 = |c: [i32; 3]| {
        rbx_dom_weak::types::Variant::Vector3int16(rbx_dom_weak::types::Vector3int16::new(
            c[0] as i16,
            c[1] as i16,
            c[2] as i16,
        ))
    };
    let max_ext = [0, 1, 2].map(|k| out.size[k] - 1);
    let grid_bytes = out.to_smooth_grid([0; 3]);
    if name == "ApplyTransformSubregion" {
        let r = {
            let mut w = world(lua)?;
            let r = w.dm().dom.insert(
                Ref::none(),
                rbx_dom_weak::InstanceBuilder::new("TerrainRegion").with_name("TerrainRegion"),
            );
            w.orphans.insert(r);
            if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                i.properties.insert(rbx_dom_weak::ustr("ExtentsMin"), v3([0; 3]));
                i.properties.insert(rbx_dom_weak::ustr("ExtentsMax"), v3(max_ext));
                i.properties.insert(rbx_dom_weak::ustr("SmoothGrid"), rbx_dom_weak::types::Variant::BinaryString(grid_bytes.into()));
            }
            r
        };
        one(inst_value(lua, r)?)
    } else {
        let mut w = world(lua)?;
        if let Some(i) = w.dm().dom.get_by_ref_mut(this) {
            i.properties.insert(rbx_dom_weak::ustr("ExtentsMin"), v3([0; 3]));
            i.properties.insert(rbx_dom_weak::ustr("ExtentsMax"), v3(max_ext));
            i.properties.insert(rbx_dom_weak::ustr("SmoothGrid"), rbx_dom_weak::types::Variant::BinaryString(grid_bytes.into()));
        }
        none()
    }
}

fn slot_to_mat(lua: &Lua, this: Ref, slot: i64) -> mlua::Result<u8> {
    let doc_id = world(lua)?.d().id;
    let custom = SLOTS.with(|s| s.borrow().get(&(doc_id, this)).and_then(|m| m.get(&slot)).map(|x| x.0));
    Ok(custom.unwrap_or_else(|| if (0..23).contains(&slot) { slot as u8 } else { terrain::material_index("Grass").unwrap_or(2) }))
}

fn mat_from_arg(v: &Value) -> mlua::Result<u8> {
    let e = ud::<LEnumItem>(v)
        .filter(|e| e.enum_type == "Material")
        .ok_or_else(|| rt_err("Unable to cast value to Material"))?;
    Ok(terrain::material_index(&e.name).unwrap_or(0))
}

fn subregion_bounds(v: &Value) -> Option<([i32; 3], [i32; 3])> {
    if let Some(r) = ud::<LRegion3int16>(v) {
        return Some((r.min.0.map(i32::from), r.max.0.map(i32::from)));
    }
    if let Some(r) = ud::<LRegion3>(v) {
        let lo = (r.min / terrain::VOXEL).floor().as_ivec3().to_array();
        let hi = ((r.max / terrain::VOXEL).ceil().as_ivec3() - glam::IVec3::ONE).to_array();
        return Some((lo, hi));
    }
    None
}

fn transform_material_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let xf = ud::<LCFrame>(&arg(args, 1)).ok_or_else(|| rt_err(format!("Terrain:{name}: CFrame expected")))?.0;
    let h = vec3_arg(args, 2)?.abs() * 0.5;
    let (src_mat, dst_mat, sub) = match name {
        "ReplaceMaterialInTransform" => (Some(mat_from_arg(&arg(args, 3))?), mat_from_arg(&arg(args, 4))?, None),
        "ReplaceMaterialInTransformSubregion" => (
            Some(mat_from_arg(&arg(args, 3))?),
            mat_from_arg(&arg(args, 4))?,
            subregion_bounds(&arg(args, 5)),
        ),
        "ReplaceMaterialInTransformSubregionSlot" => (
            Some(slot_to_mat(lua, this, num_arg(args, 3).unwrap_or(0.0) as i64)?),
            slot_to_mat(lua, this, num_arg(args, 4).unwrap_or(2.0) as i64)?,
            subregion_bounds(&arg(args, 5)),
        ),
        "SetMaterialInTransform" => (None, mat_from_arg(&arg(args, 3))?, None),
        "SetMaterialInTransformSubregion" => (None, mat_from_arg(&arg(args, 3))?, subregion_bounds(&arg(args, 4))),
        _ => (
            None,
            slot_to_mat(lua, this, num_arg(args, 3).unwrap_or(2.0) as i64)?,
            subregion_bounds(&arg(args, 4)),
        ),
    };
    let e = xf.rot.x_axis.abs() * h.x + xf.rot.y_axis.abs() * h.y + xf.rot.z_axis.abs() * h.z;
    let base_lo = ((xf.pos - e) / terrain::VOXEL).floor().as_ivec3().to_array();
    let base_hi = ((xf.pos + e) / terrain::VOXEL).ceil().as_ivec3().to_array();
    let (lo, hi) = if let Some((rlo, rhi)) = sub {
        // SetMaterialInTransformSubregion / ReplaceMaterialInTransformSubregion take
        // `targetRegion` as a half-open Region3int16 voxel offset from the transform's
        // minimum bounding cell (TerrainEditor's CreateSubregions tiles [0..size/4]).
        (
            [0, 1, 2].map(|k| base_lo[k] + rlo[k]),
            [0, 1, 2].map(|k| (base_lo[k] + rhi[k] - 1).min(base_hi[k])),
        )
    } else {
        (base_lo, base_hi)
    };
    let mut v = voxels(lua, this)?;
    let mut edits = Vec::new();
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let c = glam::Vec3::from(terrain::voxel_center([x, y, z]));
                let local = xf.rot.transpose() * (c - xf.pos);
                if local.x.abs() > h.x + 1e-3 || local.y.abs() > h.y + 1e-3 || local.z.abs() > h.z + 1e-3 {
                    continue;
                }
                let (m, occ) = v.get(x, y, z);
                let new_occ = match src_mat {
                    None => {
                        if dst_mat == 0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                    Some(0) => {
                        if m != 0 && occ > 0.0 {
                            continue;
                        }
                        if dst_mat == 0 {
                            0.0
                        } else {
                            1.0
                        }
                    }
                    Some(src) => {
                        if m != src || occ <= 0.0 {
                            continue;
                        }
                        if dst_mat == 0 {
                            0.0
                        } else {
                            occ
                        }
                    }
                };
                edits.push(([x, y, z], dst_mat, new_occ));
            }
        }
    }
    if !edits.is_empty() {
        v.edit(edits);
    }
    put_back(lua, this, v)?;
    none()
}

fn fill_slot_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let mut v = voxels(lua, this)?;
    match name {
        "FillBallSlot" => {
            let center = vec3_arg(args, 1)?;
            let r = num_arg(args, 2).ok_or_else(|| missing(2))?.abs() as f32;
            let mat = slot_to_mat(lua, this, num_arg(args, 3).unwrap_or(2.0) as i64)?;
            let lo = (center - glam::Vec3::splat(r)).to_array();
            let hi = (center + glam::Vec3::splat(r)).to_array();
            v.fill(lo, hi, mat, |c| glam::Vec3::from(c).distance(center) - r);
        }
        "FillBlockSlot" | "FillWedgeSlot" => {
            let xf = ud::<LCFrame>(&arg(args, 1)).ok_or_else(|| rt_err(format!("Terrain:{name}: CFrame expected")))?.0;
            let h = vec3_arg(args, 2)?.abs() * 0.5;
            let mat = slot_to_mat(lua, this, num_arg(args, 3).unwrap_or(2.0) as i64)?;
            let e = xf.rot.x_axis.abs() * h.x + xf.rot.y_axis.abs() * h.y + xf.rot.z_axis.abs() * h.z;
            let wedge = name == "FillWedgeSlot";
            v.fill((xf.pos - e).to_array(), (xf.pos + e).to_array(), mat, |c| {
                let p = xf.rot.transpose() * (glam::Vec3::from(c) - xf.pos);
                let q = p.abs() - h;
                let d = q.max(glam::Vec3::ZERO).length() + q.max_element().min(0.0);
                if wedge {
                    let n = glam::Vec3::new(0.0, h.z, -h.y).normalize_or_zero();
                    d.max(n.dot(p))
                } else {
                    d
                }
            });
        }
        "FillCylinderSlot" => {
            let xf = ud::<LCFrame>(&arg(args, 1)).ok_or_else(|| rt_err("Terrain:FillCylinderSlot: CFrame expected"))?.0;
            let height = num_arg(args, 2).ok_or_else(|| missing(2))?.abs() as f32;
            let r = num_arg(args, 3).ok_or_else(|| missing(3))?.abs() as f32;
            let mat = slot_to_mat(lua, this, num_arg(args, 4).unwrap_or(2.0) as i64)?;
            let h = glam::Vec3::new(r, height * 0.5, r);
            let e = xf.rot.x_axis.abs() * h.x + xf.rot.y_axis.abs() * h.y + xf.rot.z_axis.abs() * h.z;
            v.fill((xf.pos - e).to_array(), (xf.pos + e).to_array(), mat, |c| {
                let p = xf.rot.transpose() * (glam::Vec3::from(c) - xf.pos);
                let q = glam::Vec2::new(glam::Vec2::new(p.x, p.z).length() - r, p.y.abs() - height * 0.5);
                q.max(glam::Vec2::ZERO).length() + q.max_element().min(0.0)
            });
        }
        "FillRegionSlot" => {
            let rg = ud::<LRegion3>(&arg(args, 1)).ok_or_else(|| rt_err("Terrain:FillRegionSlot: Region3 expected"))?;
            let mat = slot_to_mat(lua, this, num_arg(args, 3).unwrap_or(2.0) as i64)?;
            let (lo, hi) = (rg.min, rg.max);
            v.fill(lo.to_array(), hi.to_array(), mat, |c| {
                let c = glam::Vec3::from(c);
                if c.cmpge(lo).all() && c.cmple(hi).all() { -terrain::VOXEL } else { terrain::VOXEL }
            });
        }
        _ => {}
    }
    put_back(lua, this, v)?;
    none()
}

fn smooth_region_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let rg = ud::<LRegion3>(&arg(args, 1)).ok_or_else(|| rt_err(format!("Terrain:{name}: Region3 expected")))?;
    let strength = (num_arg(args, 3).unwrap_or(0.5) as f32).clamp(0.0, 1.0);
    let a = (rg.min / terrain::VOXEL).round().as_ivec3().to_array();
    let b = (rg.max / terrain::VOXEL).round().as_ivec3().to_array();
    let dims = [0, 1, 2].map(|k| (b[k] - a[k]).max(0) as usize);
    let v = voxels(lua, this)?;
    let mats = lua.create_table()?;
    let occs = lua.create_table()?;
    let size = LVector3(glam::Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32)).into_value();
    mats.set("Size", size.clone())?;
    occs.set("Size", size)?;
    let use_slots = name == "SmoothRegionMaterialSlots";
    for ix in 0..dims[0] {
        let mx = lua.create_table()?;
        let ox = lua.create_table()?;
        for iy in 0..dims[1] {
            let my = lua.create_table()?;
            let oy = lua.create_table()?;
            for iz in 0..dims[2] {
                let (cx, cy, cz) = (a[0] + ix as i32, a[1] + iy as i32, a[2] + iz as i32);
                let (m0, o0) = v.get(cx, cy, cz);
                let mut sum = 0.0f32;
                let mut best_m = m0;
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            let (mn, on) = v.get(cx + dx, cy + dy, cz + dz);
                            sum += on;
                            if best_m == 0 && mn != 0 && on > 0.0 {
                                best_m = mn;
                            }
                        }
                    }
                }
                let avg = sum / 27.0;
                let occ = (o0 + (avg - o0) * strength).clamp(0.0, 1.0);
                let m_out = if occ <= 0.0 { 0 } else { best_m };
                if use_slots {
                    my.set(iz + 1, m_out as i64)?;
                } else {
                    my.set(iz + 1, enum_item(lua, "Material", terrain::material_name(m_out))?)?;
                }
                oy.set(iz + 1, occ as f64)?;
            }
            mx.set(iy + 1, my)?;
            ox.set(iy + 1, oy)?;
        }
        mats.set(ix + 1, mx)?;
        occs.set(ix + 1, ox)?;
    }
    put_back(lua, this, v)?;
    Ok(MultiValue::from_iter([Value::Table(mats), Value::Table(occs)]))
}

/// The smooth material index a CellMaterial value writes.
fn smooth_of(cm: u32) -> u8 {
    LEnumItem::from_value("CellMaterial", cm)
        .and_then(|e| CELL_TO_SMOOTH.iter().find(|(c, _)| *c == e.name).map(|(_, s)| *s))
        .and_then(terrain::material_index)
        .unwrap_or(0)
}

fn slot_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let i = match arg(args, 1) {
        Value::Nil => return Err(missing(1)),
        v => crate::ltypes::num(&v).ok_or_else(|| {
            let what = ud::<LEnumItem>(&v).map(|e| e.enum_type).unwrap_or_else(|| v.type_name().to_string());
            rt_err(format!("Unable to cast {what} to int"))
        })? as i64,
    };
    if !(0..64).contains(&i) {
        return Err(rt_err("slotIndex is out of range"));
    }
    let doc_id = world(lua)?.d().id;
    match name {
        "GetMaterialSlot" => {
            let set = SLOTS.with(|s| s.borrow().get(&(doc_id, this)).and_then(|m| m.get(&i)).cloned());
            let (m, variant, color) = match set {
                Some(x) => x,
                None if i < 23 => {
                    let m = i as u8;
                    // Terrain.MaterialColors (as GetMaterialColor reads it).
                    let color = if m < 2 {
                        [0.0; 3]
                    } else {
                        let w = world(lua)?;
                        let rgb = match w
                            .d()
                            .prop(this, "MaterialColors")
                            .as_ref()
                            .and_then(rustudio_core::terrain_mat::material_colors_bytes)
                        {
                            Some(b) => [b[m as usize * 3], b[m as usize * 3 + 1], b[m as usize * 3 + 2]],
                            None => terrain::material_color(m),
                        };
                        rgb.map(|x| x as f32 / 255.0)
                    };
                    (m, String::new(), color)
                }
                None => return none(),
            };
            Ok(MultiValue::from_iter([
                enum_item(lua, "Material", terrain::material_name(m))?,
                Value::String(lua.create_string(&variant)?),
                Value::UserData(lua.create_userdata(LColor3(color))?),
            ]))
        }
        "SetMaterialSlot" => {
            for k in (2..=4).rev() {
                if arg(args, k).is_nil() {
                    return Err(missing(k));
                }
            }
            if i < 2 {
                return Err(rt_err("Air and water slots cannot be set"));
            }
            let mat = ud::<LEnumItem>(&arg(args, 2))
                .filter(|e| e.enum_type == "Material")
                .ok_or_else(|| rt_err("Unable to cast value to Material"))?;
            let m = terrain::material_index(&mat.name)
                .filter(|m| *m >= 2)
                .ok_or_else(|| rt_err("baseMaterial is not a terrain material"))?;
            let variant = match arg(args, 3) {
                Value::String(s) => s.to_str()?.to_string(),
                v => return Err(rt_err(format!("Unable to cast {} to string", v.type_name()))),
            };
            let color = ud::<LColor3>(&arg(args, 4)).ok_or_else(|| rt_err("Unable to cast value to Color3"))?.0;
            SLOTS.with(|s| s.borrow_mut().entry((doc_id, this)).or_default().insert(i, (m, variant, color)));
            none()
        }
        _ => {
            if i < 2 {
                return Err(rt_err("Air and water slots cannot be reset"));
            }
            SLOTS.with(|s| {
                if let Some(m) = s.borrow_mut().get_mut(&(doc_id, this)) {
                    m.remove(&i);
                }
            });
            none()
        }
    }
}
