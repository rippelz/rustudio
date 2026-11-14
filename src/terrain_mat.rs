//! Studio's terrain material table (round 21): per SmoothGrid material its
//! texture pack for the top, side and bottom faces, with tiling, detiling,
//! rotation and the height-blend range, and the colour table (default
//! MaterialColors and each pack's average albedo). Read from
//! assets/terrain-materials.json, a copy of the fast flags
//! FStringTerrainMaterialTablePre2022 / FStringTerrainMaterialTable2022 /
//! FStringTerrainColorTable of an offline Studio (0.744).
//!
//! How Studio's terrain shaders use it (SmoothCluster VS / FS HQ;
//! docs/MECHANICS.md "Round 21"):
//! - every triangle carries three corner materials; each corner's texture
//!   coordinate is the world position projected on one of 18 axes, times the
//!   face's `tiling` / 4, turned by a random angle (× `rotation`) and moved by a
//!   random offset (/ `detiling`), the randoms fixed per corner (its cell's hash);
//! - the three samples blend by barycentric weight sharpened by each
//!   texture's height, remapped by `blend {min, max, scale}`;
//! - the albedo is recoloured: its luma scaled and its chroma turned and
//!   scaled so the pack's average albedo comes out as the MaterialColor.

use std::sync::OnceLock;

use serde_json::Value;

/// Studs per voxel: the material table's `tiling` is per voxel.
pub const VOXEL_STUDS: f32 = 4.0;

/// CrackedLava's emissive strength: the light its texel's emissive mask (the
/// KTX2 specular map's blue) adds before the albedo multiplies it (FS HQ:
/// Σ bary × CB5 emissive × mask). Studio's static material defaults
/// (Studio, filled by Studio) give every material tint white and
/// strength 0 except CrackedLava (entry 15): strength 2.0 (Studio); CB5
/// carries tint × strength (Studio).
pub const CRACKED_LAVA_EMISSIVE: f32 = 2.0;

/// A terrain palette entry's emissive strength (tint white), in the free
/// `.w` of its parameter row 1 (the shader reads it there).
pub fn set_emissive(params: &mut [[f32; 4]], entry: usize, material: u8) {
    params[entry + 1][3] = if material == 15 { CRACKED_LAVA_EMISSIVE } else { 0.0 };
}

/// One texture of a material (a face variant).
#[derive(Clone, Debug, Default)]
pub struct Tex {
    pub pack: u64,
    /// Texture repeats per stud along the projection (the table's per-voxel
    /// tiling / VOXEL_STUDS).
    pub tiling: f32,
    /// Random offset scale: 1 / the table's "detiling" (Studio's CB2.y).
    pub detiling: f32,
    /// Random rotation scale (0 = none, 1 = up to ±90°).
    pub rotation: f32,
    /// Height blend: remap height h to (h − min) / (max − min); sharpness.
    pub blend_min: f32,
    pub blend_max: f32,
    pub blend_scale: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Material {
    #[cfg_attr(not(test), allow(dead_code))]
    pub name: String,
    /// Top, side, bottom.
    pub faces: [Option<Tex>; 3],
    /// Brick, WoodPlanks: cube mapping (six projection axes only).
    pub cube: bool,
    /// How the material bends the surface (mesher).
    pub shape: crate::terrain::Shape,
    pub default_color: [u8; 3],
    pub avg_albedo: [u8; 3],
}

struct Tables {
    pre2022: Vec<Material>,
    v2022: Vec<Material>,
}

fn tex(v: &Value) -> Option<Tex> {
    let pack = v
        .get("texturePackId")?
        .as_str()?
        .trim_start_matches("rbxassetid://")
        .parse()
        .ok()?;
    let f = |k: &str, d: f64| v.get(k).and_then(Value::as_f64).unwrap_or(d) as f32;
    let b = v.get("blend");
    let bf = |k: &str, d: f64| b.and_then(|b| b.get(k)).and_then(Value::as_f64).unwrap_or(d) as f32;
    Some(Tex {
        pack,
        // The table's tiling counts repeats per voxel (4 studs): Studio's
        // Brick (0.8) repeats every 5 studs, fitted on the textured the offline Studio
        // close-ups (300 px a tile, 57 px a stud at 8 studs, FOV 70).
        tiling: f("tiling", 0.5) / VOXEL_STUDS,
        // Studio's CB2.y is 1 / detiling: the random offset
        // (r, ⌊2.665 r⌋) × CB2.y in texture units (Studio).
        detiling: { let d = f("detiling", 0.0); if d > 0.0 { 1.0 / d } else { 0.0 } },
        rotation: f("rotation", 0.0),
        blend_min: bf("min", 0.0),
        blend_max: bf("max", 1.0),
        blend_scale: bf("scale", 0.1),
    })
}

/// A material entry's deform mode: of shift, cubify, quantize (Studio also
/// knows barrel and water), the last key present.
fn deform_mode(m: &Value) -> Option<&'static str> {
    ["shift", "cubify", "quantize"].into_iter().filter(|k| m.get(*k).is_some()).last()
}

fn rgb(v: Option<&Value>) -> [u8; 3] {
    let a = v.and_then(Value::as_array);
    let c = |i: usize| a.and_then(|a| a.get(i)).and_then(Value::as_u64).unwrap_or(0) as u8;
    [c(0), c(1), c(2)]
}

fn parse(list: &Value, colors: &Value, base_key: &str, avg_key: &str) -> Vec<Material> {
    let empty = Vec::new();
    let list = list.as_array().unwrap_or(&empty);
    list.iter()
        .enumerate()
        .map(|(i, m)| {
            let all = m.get("texture").and_then(tex);
            let top = m.get("texture_top").and_then(tex).or_else(|| all.clone());
            let side = m.get("texture_side").and_then(tex).or_else(|| all.clone());
            // A material without its own bottom shows its top below
            // (Studio's loader Studio parses the top entry again).
            let bottom = m
                .get("texture_bottom")
                .and_then(tex)
                .or_else(|| top.clone());
            Material {
                name: m.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                faces: [top, side, bottom],
                cube: m.get("mapping").and_then(Value::as_str) == Some("cube"),
                // One deform mode: the last of shift, cubify, quantize the
                // entry has (Studio's loader Studio overwrites the mode).
                shape: crate::terrain::Shape {
                    shift: if deform_mode(m) == Some("shift") { m["shift"].as_f64().unwrap_or(0.0) as f32 } else { 0.0 },
                    quantize: if deform_mode(m) == Some("quantize") { m["quantize"].as_f64().unwrap_or(0.0) as f32 } else { 0.0 },
                    cubify: deform_mode(m) == Some("cubify") && m["cubify"].as_f64().unwrap_or(0.0) > 0.0,
                    faceted: matches!(m.get("type").and_then(Value::as_str), Some("hard" | "hardsoft")),
                    hardsoft: m.get("type").and_then(Value::as_str) == Some("hardsoft"),
                },
                default_color: rgb(colors.get(base_key).and_then(|c| c.get(i))),
                avg_albedo: rgb(colors.get(avg_key).and_then(|c| c.get(i))),
            }
        })
        .collect()
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let j: Value =
            serde_json::from_str(include_str!("../assets/terrain-materials.json")).unwrap_or(Value::Null);
        let colors = j.get("colors").cloned().unwrap_or(Value::Null);
        Tables {
            pre2022: parse(&j["pre2022"], &colors, "baseColorsPre2022", "avgAlbedoPre2022"),
            v2022: parse(&j["2022"], &colors, "baseColors2022", "avgAlbedo2022"),
        }
    })
}

/// Whether `pack` is one of the pre-2022 table's: Studio's KTX2 of those
/// holds its albedo as YCoCg (the built-in texture path); the 2022 packs'
/// albedo is plain RGB with the tint mask in alpha.
pub fn pre2022_pack(pack: u64) -> bool {
    tables().pre2022.iter().flat_map(|m| m.faces.iter().flatten()).any(|t| t.pack == pack)
}

/// The table entry of SmoothGrid material `m` (MaterialService.Use2022Materials picks the table).
pub fn material(m: u8, use2022: bool) -> Option<&'static Material> {
    let t = tables();
    if use2022 { &t.v2022 } else { &t.pre2022 }.get(m as usize)
}

/// Studio's recolour of a terrain albedo for MaterialColors (CB4, built by
/// Studio): the default MaterialColor `default` and the
/// current `color` as HSV straight from their sRGB bytes / 255;
/// x = clamp(V / V₀, 0, 2), s = clamp(S / S₀, 0, 2), θ = H − H₀; the shader
/// scales luma by x and turns chroma (Co, Cg) by θ and scales it by s · x:
/// (x, cos θ · s · x, sin θ · s · x). A near-grey texture can only take a
/// faint tint (Concrete at 220,40,70 comes out pale pink).
pub fn recolor(default: [f32; 3], color: [f32; 3]) -> [f32; 3] {
    let hsv = |c: [f32; 3]| {
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        let d = max - min;
        let h = if d <= 0.0 {
            0.0
        } else if max == c[0] {
            ((c[1] - c[2]) / d).rem_euclid(6.0) / 6.0
        } else if max == c[1] {
            ((c[2] - c[0]) / d + 2.0) / 6.0
        } else {
            ((c[0] - c[1]) / d + 4.0) / 6.0
        };
        let s = if max > 0.0 { d / max } else { 0.0 };
        (h, s, max)
    };
    let (h0, s0, v0) = hsv(default);
    let (h1, s1, v1) = hsv(color);
    let ratio = |a: f32, b: f32| if b > 0.0 { (a / b).clamp(0.0, 2.0) } else if a > 0.0 { 2.0 } else { 1.0 };
    let x = ratio(v1, v0);
    let s = ratio(s1, s0);
    let theta = (h1 - h0) * std::f32::consts::TAU;
    [x, theta.cos() * s * x, theta.sin() * s * x]
}

/// A palette entry whose layer isn't one of Studio's built-in YCoCg
/// (pre-2022) textures: a 2022 pack or MaterialService's override for its
/// material. CB4 then takes the tint form (MaterialColor / 255, .w 1),
/// always, not only once recoloured (Studio uploads it whenever the
/// texture manager isn't in its built-in mode): albedo × mix(1, colour,
/// tint mask) (the shader reads row 2 .w as the flag).
pub fn set_override_tint(params: &mut [[f32; 4]], entry: usize, doc: &crate::doc::Document, m: u8, use2022: bool, rgb: [u8; 3]) {
    if (use2022 && m >= 2) || terrain_override(doc, m, use2022).is_some() {
        params[entry + 2] = [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_material_tables_are_complete_and_faces_are_valid() {
        for modern in [false, true] {
            for slot in 2..23 {
                let m = material(slot, modern).unwrap();
                assert_eq!(m.name, crate::terrain::material_name(slot));
                for face in m.faces.iter().flatten() {
                    assert!(face.pack > 0);
                    assert!(face.tiling > 0.0);
                    assert!(face.blend_max > face.blend_min);
                }
                let coefficients = recolor(m.default_color.map(|x| x as f32 / 255.0),
                    m.default_color.map(|x| x as f32 / 255.0));
                assert!((coefficients[0] - 1.0).abs() < 1e-5);
                assert!((coefficients[1] - 1.0).abs() < 1e-5);
                assert!(coefficients[2].abs() < 1e-5);
            }
        }
    }

    #[test]
    fn table_tiling_is_per_voxel_and_detiling_inverted() {
        // Brick (pre-2022): tiling 0.8 per 4-stud voxel, detiling 5 (CB2.y = 1/5).
        let b = material(5, false).unwrap().faces[0].clone().unwrap();
        assert!((b.tiling - 0.2).abs() < 1e-6);
        assert!((b.detiling - 0.2).abs() < 1e-6);
        // A material without its own bottom shows its top below (pre-2022
        // LeafyGrass has a top and a side only).
        let leafy = material(19, false).unwrap();
        assert_eq!(leafy.faces[2].as_ref().unwrap().pack, 7546664715);
        assert_eq!(leafy.faces[1].as_ref().unwrap().pack, 7546666342);
    }

    #[test]
    fn cracked_lava_alone_glows() {
        let mut p = vec![[0.0f32; 4]; 8];
        set_emissive(&mut p, 0, 15);
        set_emissive(&mut p, 4, 2);
        assert_eq!(p[1][3], 2.0);
        assert_eq!(p[5][3], 0.0);
    }

    #[test]
    fn grass_has_three_faces_and_its_colours() {
        let g = material(2, false).expect("grass");
        assert_eq!(g.name, "Grass");
        assert!(g.faces.iter().all(|f| f.is_some()));
        assert_eq!(g.faces[0].as_ref().unwrap().pack, 7547336797);
        assert_eq!(g.default_color, [106, 127, 63]);
        assert!(material(5, false).unwrap().cube, "Brick maps as a cube");
    }

    #[test]
    fn recolor_follows_studios_hsv_ratios() {
        // Concrete (pre-2022): default 127,102,63 → 220,40,70 (Studio's CB4
        // for this pair, re-terrain-color.md: ≈ (1.732, 1.94, −2.04)).
        let u = |c: [u8; 3]| c.map(|x| x as f32 / 255.0);
        let [x, c, s] = recolor(u([127, 102, 63]), u([220, 40, 70]));
        assert!((x - 1.732).abs() < 0.01, "{x}");
        assert!((c - 1.94).abs() < 0.03 && (s + 2.04).abs() < 0.03, "{c} {s}");
        // The default colour is the identity; ratios clamp at 2.
        let [x, c, s] = recolor(u([127, 102, 63]), u([127, 102, 63]));
        assert!((x - 1.0).abs() < 1e-6 && (c - 1.0).abs() < 1e-6 && s.abs() < 1e-6);
        let [x, ..] = recolor(u([30, 30, 37]), u([255, 255, 255]));
        assert_eq!(x, 2.0);
    }
}

/// Include the actual palette in the upload generation. Live runtime writes can
/// precede the scene journal's mesh rebuild; palette uploads must see them now.
pub fn palette_generation(doc: &crate::doc::Document, packs: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    packs.hash(&mut h);
    crate::scene::use_2022_materials(doc).hash(&mut h);
    let parent = doc.workspace().unwrap_or(doc.dom.root_ref());
    if let Some(terrain) = doc.find_child(parent, "Terrain") {
        match doc.prop(terrain, "MaterialColors") {
            Some(rbx_dom_weak::types::Variant::BinaryString(bytes)) => AsRef::<[u8]>::as_ref(&bytes).hash(&mut h),
            Some(rbx_dom_weak::types::Variant::MaterialColors(colors)) => colors.encode().hash(&mut h),
            _ => {},
        }
    }
    for m in 2..23 {
        if let Some(tex) = terrain_override(doc, m, crate::scene::use_2022_materials(doc)) {
            m.hash(&mut h); tex.pack.hash(&mut h); tex.tiling.to_bits().hash(&mut h);
        }
    }
    h.finish()
}

#[cfg(test)]
mod live_palette_tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, types::Variant};
    #[test]
    fn live_color_invalidates_without_mesh_change() {
        let mut doc = crate::doc::Document::empty();
        let terrain = doc.dom.insert(doc.dom.root_ref(), InstanceBuilder::new("Terrain")
            .with_property("MaterialColors", Variant::BinaryString(vec![0;69].into())));
        let initial = palette_generation(&doc, 5);
        assert_eq!(initial, palette_generation(&doc, 5));
        let mut colors = vec![0;69]; colors[12..15].copy_from_slice(&[220,40,70]);
        doc.set_prop_live(terrain, rbx_dom_weak::ustr("MaterialColors"), Variant::BinaryString(colors.into()));
        let binary_generation = palette_generation(&doc, 5);
        let mut typed = rbx_dom_weak::types::MaterialColors::default();
        typed.set_color(rbx_dom_weak::types::TerrainMaterials::Grass, rbx_dom_weak::types::Color3uint8::new(180,65,45));
        doc.set_prop_live(terrain, rbx_dom_weak::ustr("MaterialColors"), Variant::MaterialColors(typed));
        assert_ne!(binary_generation, palette_generation(&doc, 5));
        assert_ne!(initial, palette_generation(&doc, 5));
        assert_ne!(palette_generation(&doc, 5), palette_generation(&doc, 6));
    }
}

/// Resolve MaterialService's terrain override with the same base-material guard
/// used for parts. An invalid name leaves the built-in terrain material active.
pub fn terrain_override(doc: &crate::doc::Document, m: u8, use2022: bool) -> Option<Tex> {
    use rbx_dom_weak::types::Variant;
    let base = material(m, use2022)?;
    let ms = doc.find_child(doc.dom.root_ref(), "MaterialService")?;
    let Variant::String(name) = doc.prop(ms, &format!("{}Name", base.name))? else { return None };
    if name.is_empty() { return None; }
    let expected = crate::ltypes::LEnumItem::from_name("Material", &base.name)?.value;
    let v = doc.dom.descendants_of(ms).find(|v| v.class == "MaterialVariant" && v.name == name
        && matches!(doc.prop(v.referent(), "BaseMaterial"), Some(Variant::Enum(e)) if e.to_u32() == expected))?.referent();
    let id = |key: &str| {
        let uri = crate::scene::content_string(doc.prop(v, key))
            .or_else(|| crate::scene::content_string(doc.prop(v, &format!("{key}Content"))))?;
        uri.strip_prefix("rbxassetid://").unwrap_or(&uri).parse::<u64>().ok()
    };
    let studs = match doc.prop(v, "StudsPerTile") { Some(Variant::Float32(v)) => v,
        Some(Variant::Float64(v)) => v as f32, _ => 10.0 };
    // Studio draws a published variant on terrain from its hidden
    // `TexturePack` (a texturepack XML asset naming the same four maps, e.g.
    // Laser Tag's Walkway → 13749139929), loaded like the built-in packs:
    // its KTX2 representations (matpack::build_acr), else the pack's maps.
    // A variant without one (not yet published) registers its loose maps.
    let pack = match id("TexturePack") {
        Some(pack) => pack,
        None => crate::matpack::register_terrain_variant(crate::matpack::Pack {
            color: Some(id("ColorMap")?), normal: id("NormalMap"), roughness: id("RoughnessMap"),
            metalness: id("MetalnessMap"), tiling: 1.0, ..Default::default() }),
    };
    Some(Tex { pack, tiling: 1.0 / studs.max(0.01), blend_max: 1.0, blend_scale: 0.1, ..Default::default() })
}

#[cfg(test)]
mod override_tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, types::{Variant, Enum}};
    #[test]
    fn override_lookup_requires_name_and_base_material_and_tracks_changes() {
        let mut doc = crate::doc::Document::empty();
        let ms = doc.dom.insert(doc.dom.root_ref(), InstanceBuilder::new("MaterialService")
            .with_property("ConcreteName", "Offline"));
        let v = doc.dom.insert(ms, InstanceBuilder::new("MaterialVariant").with_name("Offline")
            .with_property("BaseMaterial", Variant::Enum(Enum::from_u32(816)))
            .with_property("ColorMap", "rbxassetid://7547307376")
            .with_property("StudsPerTile", 8.0_f32));
        let tex = terrain_override(&doc, 4, false).unwrap();
        assert_eq!(tex.tiling, 0.125);
        assert_eq!(tex.detiling, 0.0);
        let initial = palette_generation(&doc, 0);
        doc.set_prop_live(v, rbx_dom_weak::ustr("StudsPerTile"), Variant::Float32(4.0));
        assert_ne!(initial, palette_generation(&doc, 0));
        assert_eq!(terrain_override(&doc, 4, false).unwrap().tiling, 0.25);
        // The override layer takes CB4's tint form: albedo × MaterialColor.
        let mut params = vec![[0.0f32; 4]; 4];
        set_override_tint(&mut params, 0, &doc, 4, false, [127, 102, 63]);
        assert_eq!(params[2], [127.0 / 255.0, 102.0 / 255.0, 63.0 / 255.0, 1.0]);
        doc.set_prop_live(v, rbx_dom_weak::ustr("BaseMaterial"), Variant::Enum(Enum::from_u32(800)));
        assert!(terrain_override(&doc, 4, false).is_none());
        let mut params = vec![[0.0f32; 4]; 4];
        set_override_tint(&mut params, 0, &doc, 4, false, [127, 102, 63]);
        assert_eq!(params[2][3], 0.0);
    }
}
