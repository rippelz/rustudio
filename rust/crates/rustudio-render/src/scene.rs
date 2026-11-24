//! Turns the document into renderable parts and lighting.

use std::collections::{HashMap, HashSet};

use glam::{Mat3, Vec3, Vec4};
use rbx_dom_weak::types::{BrickColor, Ref, Variant};
use rbx_dom_weak::{Instance, Ustr};

use rustudio_core::doc::{self, DocKind, Document};
use rustudio_core::math::Xform;
use rustudio_core::mech;
use crate::render::{LineVertex, MeshKind, PartDraw};

pub use rustudio_core::doc::{content_string, enum_u32, use_2022_materials};

/// A CharacterMesh's scale on a limb of `size`: the part's Size over the
/// standard R6 limb (`standard`), per axis.
pub fn character_mesh_scale(size: Vec3, standard: Vec3) -> Vec3 {
    size / standard
}

// Unions are solved in core (`mesh::union_mesh`): physics collides them.
pub use rustudio_core::mesh::{union_mesh, MeshAsset};

/// A part in the scene, with the ref it came from.
#[derive(Debug, Clone)]
pub struct ScenePart {
    pub r: Ref,
    pub draw: PartDraw,
    /// The part's own Size (picking and gizmos use it, not the drawn shape).
    pub size: Vec3,
    /// Mesh geometry this part would like drawn, if it can be loaded.
    pub mesh_asset: Option<MeshAsset>,
    /// MeshPart TextureID, SurfaceAppearance ColorMap or SpecialMesh TextureId.
    pub texture_id: Option<String>,
    /// How that texture covers the part (Opaque, Over, Cutout).
    pub tex_kind: crate::render::TexMode,
    /// SurfaceAppearance emission: mask id and EmissiveTint² × EmissiveStrength.
    pub emissive: Option<(String, [f32; 3])>,
    /// Decal / Texture children (and a T-shirt graphic on the torso).
    pub decals: Vec<DecalDesc>,
    /// Classic clothing on a character body part: (part name, shirt id, pants id).
    pub clothing: Option<(String, Option<String>, Option<String>)>,
    /// An R15 body MeshPart under that clothing: its character's atlas
    /// colours (assets::r15_composite).
    pub r15_look: Option<rustudio_core::assets::R15Look>,
    /// Locked parts can't be clicked in the viewport.
    pub locked: bool,
    /// A MaterialVariant replaces the material's own texture pack.
    pub variant: bool,
    /// That variant as a registered material pack (matpack.rs) and its
    /// StudsPerTile. The viewport draws it through the part material shader
    /// (Studio: uvScale = Size / StudsPerTile, uvOffset 0, face UVs from the
    /// primitive's own faces, including wedge slopes); the six ColorMap
    /// decals in `decals` are only the fallback while it can't load.
    pub variant_pack: Option<(u64, f32)>,
    /// The box decals sit on: the part's Size, or a Head mesh's drawn size.
    pub decal_size: Vec3,
    /// Where that box sits in the part (a legacy mesh's Offset), part space.
    pub decal_offset: Vec3,
    /// Children / siblings whose changes alter this part's look (meshes,
    /// decals, clothing).
    pub deps: Vec<Ref>,
    /// A MeshPart with Bones / FaceControls (deps from skin.rs): redone
    /// when the rig changes.
    pub skinned: bool,
    /// Offset of the drawn shape from the part (mesh Offset), in part space.
    pub draw_offset: Vec3,
    /// Extra rotation of the drawn shape (vertical cylinder meshes).
    pub draw_rot: Mat3,
    /// The part's own CFrame (picking, boxes and gizmos use it with `size`).
    pub xf: Xform,
    /// The part as Studio's light grid voxelizes it (lightgrid.rs); None
    /// when it doesn't (CastShadow off, fully transparent, file meshes).
    pub light: Option<crate::lightgrid::Prim>,
    /// The ViewportFrame it is drawn in (`Scene::viewport_frames`), None
    /// in the 3D world.
    pub layer: Option<Ref>,
}

pub(crate) fn glass_alpha(alpha: f32) -> bool {
    alpha > mech::FX_GLASS_MIN_ALPHA && alpha < 0.999
}

/// A Decal or Texture on one face of a part.
#[derive(Debug, Clone)]
pub struct DecalDesc {
    /// Enum.NormalId: Right 0, Top 1, Back 2, Left 3, Bottom 4, Front 5.
    pub face: u32,
    pub id: String,
    /// Linear RGB tint (Color3).
    pub color: [f32; 3],
    pub transparency: f32,
    /// Generated base material layers inherit the host opacity; authored decals do not.
    pub base_surface: bool,
    /// Texture tiling: studs per tile (u, v) and offset studs (u, v).
    pub tiling: Option<[f32; 4]>,
}

/// A SurfaceAppearance saved with a TexturePack (newer Studio packs its maps
/// into one asset) draws as if absent while material packs are off: the
/// offline Studio reads the pack instead of ColorMap, can't fetch it
/// ("Failed to fetch TexturePack in …") and draws the part's plain Color
/// (round 26: Place B Newer's sky domes, orange and pale in Studio, clouds in
/// rustudio). With packs on rustudio keeps the ColorMap, the online look.
fn sa_unloaded(doc: &Document, sa: Ref) -> bool {
    !crate::matpack::enabled()
        && (content_string(doc.prop(sa, "TexturePack")).is_some()
            || content_string(doc.prop(sa, "TexturePackContent")).is_some())
}

/// Separate PBR maps, read as linear data (never sRGB).
pub fn surface_maps(doc: &Document, r: Ref) -> Option<[Option<String>; 3]> {
    let inst = doc.dom.get_by_ref(r)?;
    let sa = inst.children().iter().copied().find(|c| doc.class_of(*c).is_some_and(|k| k == "SurfaceAppearance"))?;
    if sa_unloaded(doc, sa) { return None; }
    let maps = ["NormalMap", "RoughnessMap", "MetalnessMap"].map(|n|
        content_string(doc.prop(sa, n)).or_else(|| content_string(doc.prop(sa, &format!("{n}Content")))));
    maps.iter().any(Option::is_some).then_some(maps)
}

/// The saved texture-pack asset, when this part has a SurfaceAppearance.
pub fn surface_appearance_pack(doc: &Document, part: Ref) -> Option<u64> {
    if !crate::matpack::enabled() {
        return None;
    }
    let sa = doc.dom.get_by_ref(part)?.children().iter().copied()
        .find(|c| doc.class_of(*c).is_some_and(|k| k == "SurfaceAppearance"))?;
    let uri = content_string(doc.prop(sa, "TexturePack"))
        .or_else(|| content_string(doc.prop(sa, "TexturePackContent")))?;
    rustudio_core::assets::asset_ref(&uri)
        .filter(|asset| asset.id != 0 && asset.version.is_none())
        .map(|asset| asset.id)
}

/// A SurfaceAppearance's emission: EmissiveMaskContent with
/// EmissiveTint (squared) × EmissiveStrength.
fn surface_emissive(doc: &Document, inst: &Instance) -> Option<(String, [f32; 3])> {
    let sa = inst
        .children()
        .iter()
        .copied()
        .find(|c| doc.class_of(*c).is_some_and(|k| k == "SurfaceAppearance"))?;
    if sa_unloaded(doc, sa) {
        return None;
    }
    let mask = content_string(doc.prop(sa, "EmissiveMaskContent"))
        .or_else(|| content_string(doc.prop(sa, "EmissiveMask")))?;
    let strength = f32_of(doc.prop(sa, "EmissiveStrength")).unwrap_or(1.0);
    if strength <= 0.0 {
        return None;
    }
    let tint = color_of(doc.prop(sa, "EmissiveTint")).unwrap_or([255, 255, 255]);
    Some((
        mask,
        [
            srgb_to_linear(tint[0]) * strength,
            srgb_to_linear(tint[1]) * strength,
            srgb_to_linear(tint[2]) * strength,
        ],
    ))
}

/// A SurfaceAppearance's Color as its shader takes it: sRGB, each channel
/// clamped to 0..1 (Studio 0.747: Color 2 or 3 draws as 1, -1 as 0; default
/// white).
fn surface_tint(doc: &Document, sa: Ref) -> [f32; 3] {
    match doc.prop(sa, "Color") {
        Some(Variant::Color3(c)) => [c.r, c.g, c.b].map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 }),
        Some(Variant::Color3uint8(c)) => [c.r, c.g, c.b].map(|v| v as f32 / 255.0),
        _ => [1.0; 3],
    }
}

/// The Color of the SurfaceAppearance a part is drawn with (the last one
/// with a ColorMap, as [`surface_details`] picks it); None without one.
fn part_surface_tint(doc: &Document, part: Ref) -> Option<[f32; 3]> {
    let mut tint = None;
    for c in doc.dom.get_by_ref(part)?.children() {
        if doc.class_of(*c).is_some_and(|k| k == "SurfaceAppearance") && !sa_unloaded(doc, *c) {
            tint = (content_string(doc.prop(*c, "ColorMap")).is_some()
                || content_string(doc.prop(*c, "ColorMapContent")).is_some())
            .then(|| surface_tint(doc, *c));
        }
    }
    tint
}

/// Decals, Textures and SurfaceAppearance of a part: the SurfaceAppearance's
/// ColorMap (how it covers the part, its Color) and the decals.
fn surface_details(
    doc: &Document,
    inst: &Instance,
) -> (Option<(String, crate::render::TexMode, [f32; 3])>, Vec<DecalDesc>) {
    let mut texture = None;
    let mut decals = Vec::new();
    for c in inst.children() {
        let Some(child) = doc.dom.get_by_ref(*c) else {
            continue;
        };
        match child.class.as_str() {
            "SurfaceAppearance" if sa_unloaded(doc, *c) => {}
            "SurfaceAppearance" => {
                // AlphaMode: Overlay 0 (alpha blends over Color), Transparency 1,
                // TintMask 2 (alpha says where Color tints).
                let kind = match enum_u32(doc.prop(*c, "AlphaMode")) {
                    Some(1) => crate::render::TexMode::Cutout,
                    Some(2) => crate::render::TexMode::TintMask,
                    _ => crate::render::TexMode::Over,
                };
                // Without a ColorMap the SurfaceAppearance changes nothing:
                // its Color tints neither TextureID nor the part (Studio).
                texture = content_string(doc.prop(*c, "ColorMap"))
                    .or_else(|| content_string(doc.prop(*c, "ColorMapContent")))
                    .map(|t| (t, kind, surface_tint(doc, *c)));
            }
            "Decal" | "Texture" => {
                let Some(id) = content_string(doc.prop(*c, "Texture"))
                    .or_else(|| content_string(doc.prop(*c, "TextureContent")))
                else {
                    continue;
                };
                let tint = match doc.prop(*c, "Color3") {
                    Some(Variant::Color3(col)) => [col.r * col.r, col.g * col.g, col.b * col.b],
                    _ => [1.0, 1.0, 1.0],
                };
                let tiling = (child.class == "Texture").then(|| {
                    [
                        f32_of(doc.prop(*c, "StudsPerTileU")).unwrap_or(2.0),
                        f32_of(doc.prop(*c, "StudsPerTileV")).unwrap_or(2.0),
                        f32_of(doc.prop(*c, "OffsetStudsU")).unwrap_or(0.0),
                        f32_of(doc.prop(*c, "OffsetStudsV")).unwrap_or(0.0),
                    ]
                });
                decals.push(DecalDesc {
                    face: enum_u32(doc.prop(*c, "Face")).unwrap_or(5),
                    id,
                    color: tint,
                    transparency: f32_of(doc.prop(*c, "Transparency")).unwrap_or(0.0),
                    base_surface: false,
                    tiling,
                });
            }
            _ => {}
        }
    }
    (texture, decals)
}

/// Shirt / Pants templates (and the T-shirt graphic) on the part's character model.
fn character_clothing(
    doc: &Document,
    inst: &Instance,
) -> (
    Option<(String, Option<String>, Option<String>)>,
    Option<String>,
) {
    if rustudio_core::assets::clothing_role(&inst.name).is_none() {
        return (None, None);
    }
    let Some(model) = doc.dom.get_by_ref(inst.parent()) else {
        return (None, None);
    };
    let mut shirt = None;
    let mut pants = None;
    let mut graphic = None;
    for c in model.children() {
        match doc.class_of(*c).map(|s| s.to_string()).as_deref() {
            Some("Shirt") => {
                shirt = content_string(doc.prop(*c, "ShirtTemplate"))
                    .or_else(|| content_string(doc.prop(*c, "ShirtTemplateContent")))
            }
            Some("Pants") => {
                pants = content_string(doc.prop(*c, "PantsTemplate"))
                    .or_else(|| content_string(doc.prop(*c, "PantsTemplateContent")))
            }
            Some("ShirtGraphic") => {
                graphic = content_string(doc.prop(*c, "Graphic"))
                    .or_else(|| content_string(doc.prop(*c, "GraphicContent")))
            }
            _ => {}
        }
    }
    let torso = inst.name == "Torso" || inst.name == "UpperTorso";
    let clothing = (shirt.is_some() || pants.is_some()).then(|| (inst.name.clone(), shirt, pants));
    (clothing, if torso { graphic } else { None })
}

/// An R15 body MeshPart's character as Studio composites its atlas
///: each cell's colour from the first part of its group that
/// is a MeshPart with a TextureID, else the group's first
/// part present; the Shirt / Pants Color3.
fn r15_look(doc: &Document, inst: &Instance) -> Option<rustudio_core::assets::R15Look> {
    if inst.class != "MeshPart" || rustudio_core::assets::r15_cell(&inst.name).is_none() {
        return None;
    }
    let model = doc.dom.get_by_ref(inst.parent())?;
    let child = |name: &str| {
        model.children().iter().copied().find(|c| {
            doc.dom.get_by_ref(*c).is_some_and(|i| i.name == name && doc::is_a(&i.class, "BasePart"))
        })
    };
    let colour = |r: Ref| color_of(doc.prop(r, "Color")).or_else(|| color_of(doc.prop(r, "BrickColor")));
    let mut cell_colors = [[0u8; 3]; 5];
    for (k, group) in rustudio_core::assets::R15_CELL_GROUPS.iter().enumerate() {
        let parts: Vec<Ref> = group.iter().filter_map(|n| child(n)).collect();
        let textured = parts.iter().copied().find(|p| {
            doc.class_of(*p).is_some_and(|c| c == "MeshPart")
                && (content_string(doc.prop(*p, "TextureID")).is_some()
                    || content_string(doc.prop(*p, "TextureContent")).is_some())
        });
        let pick = textured.or_else(|| parts.first().copied());
        cell_colors[k] = pick.and_then(colour).unwrap_or([255, 255, 255]);
    }
    let cloth = |class: &str| {
        model
            .children()
            .iter()
            .copied()
            .find(|c| doc.class_of(*c).is_some_and(|k| k == class))
            .and_then(|c| match doc.prop(c, "Color3") {
                Some(Variant::Color3(c)) => Some([c.r, c.g, c.b]),
                Some(Variant::Color3uint8(c)) => Some([c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]),
                _ => None,
            })
            .unwrap_or([1.0, 1.0, 1.0])
    };
    Some(rustudio_core::assets::R15Look { cell_colors, shirt_color: cloth("Shirt"), pants_color: cloth("Pants") })
}

fn material_variant(doc: &Document, part: Ref, material: u32) -> Option<(String, f32)> {
    let v = material_variant_ref(doc, part, material)?;
    let map = content_string(doc.prop(v, "ColorMap"))
        .or_else(|| content_string(doc.prop(v, "ColorMapContent")))?;
    Some((map, variant_studs(doc, v)))
}

fn variant_studs(doc: &Document, v: Ref) -> f32 {
    f32_of(doc.prop(v, "StudsPerTile")).unwrap_or(10.0).max(0.01)
}

/// The part's MaterialVariant as a material pack (its four maps), registered
/// with matpack like terrain variants, and its StudsPerTile.
fn material_variant_pack(doc: &Document, part: Ref, material: u32) -> Option<(u64, f32)> {
    let v = material_variant_ref(doc, part, material)?;
    let id = |key: &str| {
        let uri = content_string(doc.prop(v, key))
            .or_else(|| content_string(doc.prop(v, &format!("{key}Content"))))?;
        uri.strip_prefix("rbxassetid://").unwrap_or(&uri).trim().parse::<u64>().ok()
    };
    let pack = rustudio_core::texture_pack::Pack {
        color: Some(id("ColorMap")?),
        normal: id("NormalMap"),
        roughness: id("RoughnessMap"),
        metalness: id("MetalnessMap"),
        tiling: 1.0,
        ..Default::default()
    };
    Some((rustudio_core::texture_pack::register_terrain_variant(pack), variant_studs(doc, v)))
}

fn material_variant_ref(doc: &Document, part: Ref, material: u32) -> Option<Ref> {
    let ms = doc.find_child(doc.dom.root_ref(), "MaterialService")?;
    let mat_name = rustudio_core::doc::enum_item_name("Material", material)?;
    let name = match doc.prop(part, "MaterialVariant") {
        Some(Variant::String(s)) if !s.is_empty() => s,
        _ => match doc.prop(ms, &format!("{mat_name}Name")) {
            Some(Variant::String(s)) if !s.is_empty() => s,
            _ => return None,
        },
    };
    let v = doc
        .dom
        .descendants_of(ms)
        .find(|i| {
            i.class == "MaterialVariant"
                && i.name == name
                && enum_u32(doc.prop(i.referent(), "BaseMaterial")).unwrap_or(256) == material
        })?
        .referent();
    Some(v)
}

/// What changed in the scene since the viewport last asked.
#[derive(Default)]
pub struct SceneChanges {
    /// Everything was rebuilt.
    pub full: bool,
    /// Parts added or updated.
    pub changed: Vec<Ref>,
    /// Parts that only moved (CFrame changed).
    pub moved: Vec<Ref>,
    /// Parts whose only changes were to how they're tinted (Transparency,
    /// Color, Reflectance, CastShadow …): re-read here, and their draws
    /// patched in place by the viewport rather than rebuilt.
    pub restyled: Vec<Ref>,
    pub removed: Vec<Ref>,
}

/// Part properties that only change a part's colour, opacity, reflectance
/// or shadow casting: the drawn instance keeps its shape, mesh, textures and
/// batch (Studio's renderer updates them in the instance data).
fn style_prop(name: &str) -> bool {
    matches!(
        name,
        "Transparency" | "LocalTransparencyModifier" | "Color" | "Color3uint8" | "BrickColor" | "Reflectance" | "CastShadow"
    )
}

/// The parts to draw, kept up to date from the document's change journal so
/// only changed parts are re-read each frame.
#[derive(Default)]
pub struct Scene {
    /// Document the parts came from (0 = none yet).
    pub doc_id: u64,
    pub(crate) glass_generation: u64,
    pub parts: Vec<ScenePart>,
    /// Part → its slot in `parts` (Fx hashing: looked up per part per
    /// frame by the editor's outlines and picking; never iterated).
    pub index: rustc_hash::FxHashMap<Ref, usize>,
    /// Bounding sphere per part (centre, radius), parallel to `parts`.
    pub bounds: Vec<Vec4>,
    is_part: HashMap<Ustr, bool>,
    /// Instance → parts whose look depends on it.
    owners: HashMap<Ref, Vec<Ref>>,
    changes: SceneChanges,
    /// The voxel terrain needs re-meshing.
    pub terrain_dirty: bool,
    /// Some: this is the parts inside these drawn ViewportFrames (each part
    /// in its nearest ViewportFrame's layer), not the 3D world. The world
    /// leaves out everything inside a ViewportFrame: Studio draws a
    /// ViewportFrame's parts only in the frame, wherever it is (a SurfaceGui
    /// on a Workspace part too).
    vf_roots: Option<HashSet<Ref>>,
}

/// A colour channel in linear light the way Studio's shaders take it:
/// squared (gamma 2.0; docs/MECHANICS.md "Lighting").
pub fn srgb_to_linear(c: u8) -> f32 {
    (c as f32 / 255.0).powi(2)
}

fn color_of(v: Option<Variant>) -> Option<[u8; 3]> {
    match v? {
        Variant::Color3uint8(c) => Some([c.r, c.g, c.b]),
        Variant::Color3(c) => {
            let c: rbx_dom_weak::types::Color3uint8 = c.into();
            Some([c.r, c.g, c.b])
        }
        Variant::BrickColor(b) => {
            let c = b.to_color3uint8();
            Some([c.r, c.g, c.b])
        }
        _ => None,
    }
}

// The property readers are core's (`doc`): physics reads them too.
pub use rustudio_core::doc::{f32_of, vec3_of, xform_of};

impl Scene {
    pub fn is_part_class(&mut self, class: Ustr) -> bool {
        *self
            .is_part
            .entry(class)
            .or_insert_with(|| doc::is_a(&class, "BasePart") && class != "Terrain")
    }

    fn root(doc: &Document) -> Option<Ref> {
        match doc.kind {
            DocKind::Place => doc.workspace(),
            DocKind::Model => Some(doc.dom.root_ref()),
        }
    }

    /// A scene of the parts inside the given ViewportFrames (see `vf_roots`).
    pub fn viewport_frames() -> Self {
        Scene { vf_roots: Some(HashSet::new()), ..Default::default() }
    }

    /// The ViewportFrames drawn (a ViewportFrame scene only): parts of
    /// frames no longer drawn go, those of new ones come in.
    pub fn set_viewport_frames(&mut self, doc: &Document, roots: &HashSet<Ref>) {
        let Some(old) = self.vf_roots.as_ref() else { return };
        if old == roots {
            return;
        }
        let gone: Vec<Ref> = old.difference(roots).copied().collect();
        let added: Vec<Ref> = roots.difference(old).copied().collect();
        self.vf_roots = Some(roots.clone());
        if self.doc_id != doc.id {
            // (the next sync rebuilds)
            return;
        }
        let drop: Vec<Ref> = self.parts.iter().filter(|p| p.layer.is_some_and(|l| gone.contains(&l))).map(|p| p.r).collect();
        for r in drop {
            if self.remove(r) {
                self.changes.removed.push(r);
            }
        }
        for vf in added {
            // GUI roots come from the previous frame. A frame may already
            // have been deleted or moved out of this document.
            if !doc.exists(vf) {
                continue;
            }
            let refs: Vec<Ref> = doc.dom.descendants_of(vf).map(|i| i.referent()).collect();
            for r in refs {
                self.add(doc, r);
            }
        }
    }

    /// Whether `r` (a part) is in this scene: Some(layer) (None = the 3D
    /// world, Some(frame) = in that ViewportFrame), or None.
    fn membership(&self, doc: &Document, root: Option<Ref>, r: Ref) -> Option<Option<Ref>> {
        let mut vf = None;
        let mut in_root = false;
        let mut cur = doc.dom.get_by_ref(r)?.parent();
        while let Some(i) = doc.dom.get_by_ref(cur) {
            if vf.is_none() && i.class == "ViewportFrame" {
                vf = Some(cur);
                if self.vf_roots.is_some() {
                    break;
                }
            }
            if Some(cur) == root {
                in_root = true;
                break;
            }
            cur = i.parent();
        }
        match (&self.vf_roots, vf) {
            (Some(roots), Some(v)) if roots.contains(&v) => Some(Some(v)),
            (None, None) if in_root => Some(None),
            _ => None,
        }
    }

    /// Reads part `r` in if it belongs here (rebuild / new frame).
    fn add(&mut self, doc: &Document, r: Ref) {
        let Some(inst) = doc.dom.get_by_ref(r) else { return };
        if !self.is_part_class(inst.class) {
            return;
        }
        let root = Self::root(doc);
        let Some(layer) = self.membership(doc, root, r) else { return };
        if let Some(mut p) = part_draw(doc, inst) {
            p.layer = layer;
            self.insert(p);
            self.changes.changed.push(r);
        }
    }

    /// Brings the part list up to date with the document.
    pub fn sync(&mut self, doc: &mut Document) {
        // A ViewportFrame scene with no frame drawn reads no marks (it
        // rebuilds from scratch when a frame comes back).
        if self.vf_roots.as_ref().is_some_and(|r| r.is_empty()) && self.parts.is_empty() {
            self.doc_id = 0;
            return;
        }
        let marks = doc.marks_for(if self.vf_roots.is_some() { doc::Reader::VfScene } else { doc::Reader::Scene });
        let marks = if self.doc_id != doc.id {
            doc::Marks::All
        } else {
            marks
        };
        match marks {
            doc::Marks::All => self.rebuild(doc),
            doc::Marks::Some(m) if m.is_empty() => {}
            doc::Marks::Some(m) => self.apply(doc, &m),
        }
    }

    /// Parts removed since `take_changes`, without taking them.
    pub fn peek_removed(&self) -> Vec<Ref> {
        self.changes.removed.clone()
    }

    /// Changes since the last call.
    pub fn take_changes(&mut self) -> SceneChanges {
        std::mem::take(&mut self.changes)
    }

    fn rebuild(&mut self, doc: &Document) {
        crate::skin::rig_changed();
        self.doc_id = doc.id;
        self.glass_generation = self.glass_generation.wrapping_add(1);
        self.parts.clear();
        self.index.clear();
        self.bounds.clear();
        self.owners.clear();
        self.changes = SceneChanges {
            full: true,
            ..Default::default()
        };
        self.terrain_dirty = self.vf_roots.is_none();
        let roots: Vec<Ref> = match &self.vf_roots {
            Some(v) => v.iter().copied().filter(|r| doc.exists(*r)).collect(),
            None => Self::root(doc).into_iter().collect(),
        };
        for root in roots {
            let refs: Vec<Ref> = doc.dom.descendants_of(root).map(|i| i.referent()).collect();
            for r in refs {
                self.add(doc, r);
            }
        }
        // (a rebuild hands over everything as `full`)
        self.changes.changed.clear();
    }

    fn insert(&mut self, p: ScenePart) {
        for d in &p.deps {
            let o = self.owners.entry(*d).or_default();
            if !o.contains(&p.r) {
                o.push(p.r);
            }
        }
        let b = bound_of(&p);
        match self.index.get(&p.r) {
            Some(&i) => {
                if glass_alpha(self.parts[i].draw.alpha) != glass_alpha(p.draw.alpha) {
                    self.glass_generation = self.glass_generation.wrapping_add(1);
                }
                self.parts[i] = p;
                self.bounds[i] = b;
            }
            None => {
                self.index.insert(p.r, self.parts.len());
                self.parts.push(p);
                self.bounds.push(b);
            }
        }
    }

    fn remove(&mut self, r: Ref) -> bool {
        let Some(i) = self.index.remove(&r) else {
            return false;
        };
        // A swap changes the indices held by the effects glass list.
        self.glass_generation = self.glass_generation.wrapping_add(1);
        self.parts.swap_remove(i);
        self.bounds.swap_remove(i);
        if let Some(moved) = self.parts.get(i) {
            self.index.insert(moved.r, i);
        }
        true
    }

    /// Applies journal marks: re-reads only the parts they touch.
    fn apply(&mut self, doc: &Document, marks: &[doc::Mark]) {
        let root = Self::root(doc);
        if root.is_none() && self.vf_roots.is_none() {
            return;
        }
        let mut full: std::collections::HashSet<Ref> = Default::default();
        let mut moved: std::collections::HashSet<Ref> = Default::default();
        let mut restyled: std::collections::HashSet<Ref> = Default::default();
        // Parts whose SurfaceAppearance Color changed: only the tint is re-read.
        let mut retinted: std::collections::HashSet<Ref> = Default::default();
        let cframe = rbx_dom_weak::ustr("CFrame");
        // A Bone / joint / FaceControls added, removed, renamed or re-pointed:
        // skinned parts look their Bones up again and are redrawn.
        let mut rig = false;
        let marks_scope = rustudio_core::bench::scope("ss_marks");
        for m in marks {
            match *m {
                doc::Mark::Prop(r, name)
                    if matches!(
                        name.as_str(),
                        "SmoothGrid" | "MaterialColors" | "WaterColor" | "WaterTransparency" | "WaterReflectance"
                    ) =>
                {
                    if doc.class_of(r).is_some_and(|c| c == "Terrain") {
                        self.terrain_dirty = true;
                    }
                }
                doc::Mark::Tree(r) if doc.class_of(r).is_some_and(|c| c == "Terrain") => {
                    self.terrain_dirty = true;
                }
                doc::Mark::Prop(r, name) => {
                    if matches!(name.as_str(), "Name" | "Part0" | "Part1" | "Enabled" | "C0" | "C1" | "Attachment0" | "Attachment1")
                        && doc.class_of(r).is_some_and(|c| crate::skin::rig_class(&c))
                    {
                        rig = true;
                    }
                    if self.index.contains_key(&r) {
                        if name == cframe {
                            moved.insert(r);
                        } else if style_prop(name.as_str()) {
                            restyled.insert(r);
                        } else {
                            full.insert(r);
                        }
                    } else if doc.class_of(r).is_some_and(|c| self.is_part_class(c)) {
                        full.insert(r);
                    }
                    if let Some(o) = self.owners.get(&r) {
                        // A SurfaceAppearance's Color is instance data in Studio
                        // (a per-frame tint re-reads no textures or meshes).
                        if name.as_str() == "Color" && doc.class_of(r).is_some_and(|c| c == "SurfaceAppearance") {
                            retinted.extend(o.iter().copied());
                        } else {
                            full.extend(o.iter().copied());
                        }
                    }
                }
                doc::Mark::Tree(r) => {
                    for d in doc.subtree(r) {
                        if !rig && doc.class_any(d).is_some_and(|c| crate::skin::rig_class(&c)) {
                            rig = true;
                        }
                        if self.index.contains_key(&d)
                            || doc.class_of(d).is_some_and(|c| self.is_part_class(c))
                        {
                            full.insert(d);
                        }
                        if let Some(o) = self.owners.get(&d) {
                            full.extend(o.iter().copied());
                        }
                    }
                    // A new decal / mesh / clothing item changes its parent part
                    // or the body parts beside it.
                    if let Some(parent) = doc.dom.get_by_ref(r).map(|i| i.parent()) {
                        if self.index.contains_key(&parent) {
                            full.insert(parent);
                        }
                        let clothing = doc.class_of(r).is_some_and(|c| {
                            matches!(c.as_str(), "Shirt" | "Pants" | "ShirtGraphic")
                        });
                        if clothing {
                            for c in doc
                                .dom
                                .get_by_ref(parent)
                                .map(|i| i.children().to_vec())
                                .unwrap_or_default()
                            {
                                if self.index.contains_key(&c) {
                                    full.insert(c);
                                }
                            }
                        }
                    }
                }
            }
        }
        drop(marks_scope);
        if rig {
            crate::skin::rig_changed();
            for p in &self.parts {
                if p.skinned {
                    full.insert(p.r);
                }
            }
        }
        let _draws_scope = rustudio_core::bench::scope("ss_part_draw");
        rustudio_core::bench::add("ss_full_count", 0.0, full.len() as u64);
        for r in full.iter().copied() {
            let layer = doc
                .dom
                .get_by_ref(r)
                .filter(|i| self.is_part_class(i.class))
                .and_then(|_| self.membership(doc, root, r));
            let p = layer.and_then(|l| part_draw(doc, doc.dom.get_by_ref(r)?).map(|mut p| {
                p.layer = l;
                p
            }));
            match p {
                Some(p) => {
                    self.insert(p);
                    self.changes.changed.push(r);
                }
                None => {
                    if self.remove(r) {
                        self.changes.removed.push(r);
                    }
                }
            }
        }
        for r in restyled.iter().copied() {
            if full.contains(&r) {
                continue;
            }
            // (still a part under the root: a removal comes as a Tree mark)
            let layer = self.get(r).and_then(|p| p.layer);
            let p = doc.dom.get_by_ref(r).and_then(|inst| part_draw(doc, inst)).map(|mut p| {
                p.layer = layer;
                p
            });
            match p {
                Some(p) => {
                    self.insert(p);
                    self.changes.restyled.push(r);
                }
                None => {
                    if self.remove(r) {
                        self.changes.removed.push(r);
                    }
                }
            }
        }
        for r in retinted {
            if full.contains(&r) || restyled.contains(&r) {
                continue;
            }
            let Some(&i) = self.index.get(&r) else { continue };
            let tint = part_surface_tint(doc, r);
            let p = &mut self.parts[i];
            if p.draw.surface_tint != tint {
                p.draw.surface_tint = tint;
                self.changes.restyled.push(r);
            }
        }
        for r in moved {
            if full.contains(&r) {
                continue;
            }
            let (Some(&i), Some(xf)) = (self.index.get(&r), xform_of(doc, r)) else {
                continue;
            };
            let p = &mut self.parts[i];
            p.xf = xf;
            p.draw.xf = Xform {
                pos: xf.pos + xf.rot * p.draw_offset,
                rot: xf.rot * p.draw_rot,
            };
            self.bounds[i] = bound_of(p);
            self.changes.moved.push(r);
        }
    }

    pub fn get(&self, r: Ref) -> Option<&ScenePart> {
        self.index.get(&r).map(|&i| &self.parts[i])
    }

    /// Recomputes a MeshPart's skinning dependencies after its skinned mesh
    /// base has been parsed (so BasePart / Bone deps are registered in `owners`).
    pub fn refresh_skin_deps(&mut self, doc: &Document, r: Ref) {
        let Some(&i) = self.index.get(&r) else { return };
        let mesh_id = self.parts[i].mesh_asset.as_ref().map(|a| a.id.as_str());
        let skin_deps = crate::skin::deps_of(doc, r, mesh_id);
        if skin_deps.is_empty() {
            return;
        }
        let p = &mut self.parts[i];
        p.skinned = true;
        for d in skin_deps {
            if !p.deps.contains(&d) {
                p.deps.push(d);
            }
            let o = self.owners.entry(d).or_default();
            if !o.contains(&r) {
                o.push(r);
            }
        }
    }
}

fn bound_of(p: &ScenePart) -> Vec4 {
    let r = p.size.max(p.draw.size).length() * 0.5 + p.draw_offset.length();
    p.xf.pos.extend(r)
}

/// The six SurfaceTypes packed 3 bits per face (Enum.NormalId order) as
/// bands of the studs atlas: Studs 1, Weld and Glue 2, Inlet 3, Universal 4.
fn surfaces_of(doc: &Document, r: Ref) -> u32 {
    let mut out = 0;
    for (i, face) in [
        "RightSurface",
        "TopSurface",
        "BackSurface",
        "LeftSurface",
        "BottomSurface",
        "FrontSurface",
    ]
    .iter()
    .enumerate()
    {
        // Enum.SurfaceType: Smooth 0, Glue 1, Weld 2, Studs 3, Inlet 4, Universal 5.
        let band = match enum_u32(doc.prop(r, face)) {
            Some(3) => 1,
            Some(1) | Some(2) => 2,
            Some(4) => 3,
            Some(5) => 4,
            _ => 0,
        };
        out |= band << (3 * i);
    }
    out
}

pub fn part_draw(doc: &Document, inst: &Instance) -> Option<ScenePart> {
    let r = inst.referent();
    let xf = xform_of(doc, r)?;
    let size = vec3_of(doc.prop(r, "Size")).unwrap_or(Vec3::new(4.0, 1.0, 2.0));
    let rgb = color_of(doc.prop(r, "Color"))
        .or_else(|| color_of(doc.prop(r, "BrickColor")))
        .unwrap_or(mech::NEW_PART_COLOR);
    // LocalTransparencyModifier (client-side) fades the part further:
    // shown transparency = 1 - (1 - Transparency) × (1 - modifier).
    let modifier = f32_of(doc.prop(r, "LocalTransparencyModifier"))
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let transparency = 1.0
        - (1.0
            - f32_of(doc.prop(r, "Transparency"))
                .unwrap_or(0.0)
                .clamp(0.0, 1.0))
            * (1.0 - modifier);
    let mut alpha = 1.0 - transparency;
    // The light grid takes Transparency itself (the physics primitive's).
    let light_transparency = f32_of(doc.prop(r, "Transparency")).unwrap_or(0.0);
    // Reflectance mixes in the sky's reflection (fs_part, round 26).
    let reflectance = f32_of(doc.prop(r, "Reflectance")).unwrap_or(0.0).clamp(0.0, 1.0);
    let material_id = enum_u32(doc.prop(r, "Material")).unwrap_or(256);
    let class = inst.class.as_str();
    let mut mesh = match class {
        "WedgePart" => MeshKind::Wedge,
        "CornerWedgePart" => MeshKind::CornerWedge,
        // Part.Shape (Part and its subclasses such as SpawnLocation, Seat).
        _ if doc::is_a(class, "Part") => match enum_u32(doc.prop(r, "Shape")) {
            Some(0) => MeshKind::Ball,
            Some(2) => MeshKind::Cylinder,
            Some(3) => MeshKind::Wedge,
            Some(4) => MeshKind::CornerWedge,
            _ => MeshKind::Block,
        },
        _ => MeshKind::Block,
    };
    let mut placeholder = matches!(
        class,
        "MeshPart" | "UnionOperation" | "IntersectOperation" | "NegateOperation" | "PartOperation"
    );
    let part_class = doc::is_a(class, "Part");
    let mut draw_size = size;
    // Extra rotation of the drawn shape (vertical cylinders) and mesh Offset.
    let mut draw_rot = Mat3::IDENTITY;
    let mut draw_offset = Vec3::ZERO;
    let mut ellipsoid = false;
    let mut head_mesh = false;
    // Legacy shape meshes (Brick, Wedge, Sphere, … SpecialMesh; BlockMesh,
    // CylinderMesh) carry their part's Decals on the stretched shape: Pirate
    // Island's rear sail is a 0.47-stud part whose Brick mesh is scaled to 37 studs.
    let mut legacy_shape = false;
    let mut deps = Vec::new();

    // A SpecialMesh child changes the drawn shape; file meshes become placeholders.
    let (sa_texture, mut decals) = surface_details(doc, inst);
    let sa_emissive = surface_emissive(doc, inst);
    let has_active_sa = sa_texture.is_some() || sa_emissive.is_some() || surface_maps(doc, r).is_some();
    // Studio's shader dispatch picks SurfaceAppearanceFS whenever a MeshPart
    // has an active SurfaceAppearance, overriding Material (Neon / Glass /
    // ForceField) and using the part's raw Color in VARYING2 without the
    // untextured material table's fallback_albedo darkening.
    let albedo = if has_active_sa {
        [1.0; 3]
    } else {
        crate::matpack::fallback_albedo(material_id, use_2022_materials(doc))
    };
    let material = if has_active_sa {
        0.0
    } else {
        match material_id {
            288 => 1.0,  // Neon
            272 => 2.0,  // SmoothPlastic
            1568 => 3.0, // Glass
            1584 => {
                alpha *= 0.5; // ForceField
                3.0
            }
            _ => 0.0,
        }
    };
    // Mesh textures cover the part outright, except on characters, where
    // transparent texels show the body Color (skin).
    let character_part = rustudio_core::assets::clothing_role(&inst.name).is_some()
        || (inst.name == "Head"
            && doc.dom.get_by_ref(inst.parent()).is_some_and(|m| {
                m.children()
                    .iter()
                    .any(|c| doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
            }));
    let mesh_kind = if character_part {
        crate::render::TexMode::Over
    } else {
        crate::render::TexMode::Opaque
    };
    let mut tex_kind = sa_texture.as_ref().map(|t| t.1).unwrap_or(mesh_kind);
    let surface_tint = sa_texture.as_ref().map(|t| t.2);
    let mut texture_id = sa_texture.map(|t| t.0);
    let mut texture_tint = None;
    if class == "MeshPart" && texture_id.is_none() {
        texture_id = content_string(doc.prop(r, "TextureID"))
            .or_else(|| content_string(doc.prop(r, "TextureContent")));
        tex_kind = mesh_kind;
    }
    // A MaterialVariant's ColorMap covers every face, tiled every StudsPerTile,
    // except when a SurfaceAppearance or MeshPart TextureID owns the surface.
    let variant = if has_active_sa || texture_id.is_some() {
        None
    } else {
        material_variant(doc, r, material_id)
    };
    let has_variant = variant.is_some();
    if let Some((map, studs)) = variant {
        let tint = [
            srgb_to_linear(rgb[0]),
            srgb_to_linear(rgb[1]),
            srgb_to_linear(rgb[2]),
        ];
        // The variant is the base surface. Authored Decals/Textures must
        // follow it in the decal pass (e.g. SpawnLocation's star over Grid).
        for face in 0..6 {
            decals.insert(face as usize, DecalDesc {
                face,
                id: map.clone(),
                color: tint,
                transparency: 0.0,
                base_surface: true,
                tiling: Some([studs, studs, 0.0, 0.0]),
            });
        }
    }
    let (clothing, tshirt) = character_clothing(doc, inst);
    let r15_look = clothing.as_ref().and_then(|_| r15_look(doc, inst));
    if let Some(g) = tshirt {
        decals.push(DecalDesc {
            face: 5,
            id: g,
            color: [1.0, 1.0, 1.0],
            transparency: 0.0,
            base_surface: false,
            tiling: None,
        });
    }
    let mut mesh_asset = if class == "MeshPart" {
        content_string(doc.prop(r, "MeshId"))
            .or_else(|| content_string(doc.prop(r, "MeshContent")))
            .map(|id| MeshAsset {
                id,
                fit_to_part: true,
                scale: Vec3::ONE,
                offset: Vec3::ZERO,
                vertex_colors: true, origin: false,
            })
    } else if matches!(class, "UnionOperation" | "IntersectOperation" | "NegateOperation" | "PartOperation") {
        union_mesh(doc, r)
    } else {
        None
    };
    if mesh_asset.is_some() && class.ends_with("Operation") {
        placeholder = false;
        // GeometryService unions keep the main part's CFrame: their geometry
        // sits off-centre in the Size box (mesh_bvh::union_offset).
        if let Some(a) = &mesh_asset {
            draw_offset = rustudio_core::mesh_bvh::union_offset(&a.id) * size;
        }
    }
    // GeometryService's MeshParts (fragments) are off-centre the same way.
    if class == "MeshPart" {
        if let Some(a) = &mesh_asset {
            draw_offset = rustudio_core::mesh_bvh::union_offset(&a.id) * size;
        }
    }
    // CharacterMesh (R6 package bodies): the model's CharacterMesh for this
    // limb replaces its box (BodyPart Torso 1, LeftArm 2, RightArm 3,
    // LeftLeg 4, RightLeg 5), scaled by the part's Size over the standard
    // limb (Torso 2×2×1, arms and legs 1×2×1) and placed by the mesh's own
    // origin. Studio 0.747: the Boy torso on Torso
    // boxes of 2×2×1, 2.93×2.93×1.46, 4×2×1 and 2×4×1 follows the box each
    // time; a game's 1.46× rig drew the package at 1× with floating hands.
    if class == "Part" {
        let body_part = match inst.name.as_str() {
            "Torso" => Some(1),
            "Left Arm" => Some(2),
            "Right Arm" => Some(3),
            "Left Leg" => Some(4),
            "Right Leg" => Some(5),
            _ => None,
        };
        if let (Some(want), Some(model)) = (body_part, doc.dom.get_by_ref(inst.parent())) {
            for c in model.children() {
                if doc.class_of(*c).is_none_or(|k| k != "CharacterMesh") {
                    continue;
                }
                deps.push(*c);
                if enum_u32(doc.prop(*c, "BodyPart")) != Some(want) {
                    continue;
                }
                let id = character_mesh_asset_id(doc, *c);
                if let Some(id) = id {
                    let standard = if want == 1 { Vec3::new(2.0, 2.0, 1.0) } else { Vec3::new(1.0, 2.0, 1.0) };
                    let scale = character_mesh_scale(size, standard);
                    mesh_asset = Some(MeshAsset { id, fit_to_part: false, scale, offset: Vec3::ZERO, vertex_colors: true, origin: true });
                    placeholder = true;
                }
            }
        }
    }
    if class == "TrussPart" {
        mesh_asset = Some(MeshAsset {
            id: format!("rustudio-truss:{:08x}:{:08x}:{:08x}:{}", size.x.to_bits(), size.y.to_bits(), size.z.to_bits(), enum_u32(doc.prop(r, "Style").or_else(|| doc.prop(r, "style"))).unwrap_or(0)),
            fit_to_part: true,
            scale: Vec3::ONE,
            offset: Vec3::ZERO,
            vertex_colors: true, origin: false,
        });
    }
    // Default R6 body geometry comes from Studio's shipped avatar meshes.
    // Explicit CharacterMesh and DataModelMesh children take precedence.
    // (The limb name is checked first: the Humanoid look scans the parent's
    // children, which for a plain Part in a 10k-part Folder made map loads
    // quadratic.)
    let builtin = match inst.name.as_str() {
        "Torso" => Some("torso"),
        "Left Arm" => Some("leftarm"),
        "Right Arm" => Some("rightarm"),
        "Left Leg" => Some("leftleg"),
        "Right Leg" => Some("rightleg"),
        _ => None,
    };
    if class == "Part" && mesh_asset.is_none() && builtin.is_some()
        && doc.dom.get_by_ref(inst.parent()).is_some_and(|model| {
            model.children().iter().any(|c| doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
        })
        && !inst.children().iter().any(|c| {
            doc.class_of(*c).is_some_and(|k| matches!(k.as_str(), "SpecialMesh" | "BlockMesh" | "CylinderMesh"))
        })
    {
        if let Some(id) = builtin.and_then(|name| studio_avatar_mesh(&format!("meshes/{name}.mesh"))) {
            mesh_asset = Some(MeshAsset {
                id,
                fit_to_part: true,
                scale: Vec3::ONE,
                offset: Vec3::ZERO,
                vertex_colors: true, origin: false,
            });
            placeholder = true;
        }
    }
    // A vertical (Y-axis) cylinder: the X-axis cylinder turned onto Y.
    let vertical = |s: Vec3| {
        (
            Mat3::from_rotation_z(std::f32::consts::FRAC_PI_2),
            Vec3::new(s.y, s.x, s.z),
        )
    };
    // The light grid's view of the mesh children (FUN_Studio): the
    // SpecialMesh type, or a BlockMesh / CylinderMesh.
    let mut light_special: Option<u32> = None;
    let mut light_legacy: Option<bool> = None;
    for c in inst.children() {
        let Some(child) = doc.dom.get_by_ref(*c) else {
            continue;
        };
        if !(class == "MeshPart" || class.ends_with("Operation")) {
            match child.class.as_str() {
                "SpecialMesh" => light_special = Some(enum_u32(doc.prop(*c, "MeshType")).unwrap_or(5)),
                "BlockMesh" => light_legacy = Some(false),
                "CylinderMesh" => light_legacy = Some(true),
                _ => {}
            }
        }
        if matches!(
            child.class.as_str(),
            "Decal"
                | "Texture"
                | "SurfaceAppearance"
                | "SpecialMesh"
                | "BlockMesh"
                | "CylinderMesh"
        ) {
            deps.push(*c);
        }
        // A MeshPart (or union) keeps its own shape: Roblox ignores
        // SpecialMesh / BlockMesh / CylinderMesh under it (Place B has a
        // MeshPart whose empty SpecialMesh hid its mesh).
        if class == "MeshPart" || class.ends_with("Operation") {
            continue;
        }
        if matches!(child.class.as_str(), "BlockMesh" | "CylinderMesh") {
            // Legacy meshes: stretch with Scale, shift by Offset.
            let scale = vec3_of(doc.prop(*c, "Scale")).unwrap_or(Vec3::ONE);
            draw_offset = vec3_of(doc.prop(*c, "Offset")).unwrap_or(Vec3::ZERO);
            draw_size = size * scale;
            legacy_shape = true;
            if child.class == "CylinderMesh" {
                (draw_rot, draw_size) = vertical(draw_size);
                mesh = MeshKind::Cylinder;
                ellipsoid = true;
            } else {
                mesh = MeshKind::Block;
            }
        } else if child.class == "SpecialMesh" {
            let scale = vec3_of(doc.prop(*c, "Scale")).unwrap_or(Vec3::ONE);
            let mesh_type = enum_u32(doc.prop(*c, "MeshType")).unwrap_or(5);
            if mesh_type != 5 {
                draw_offset = vec3_of(doc.prop(*c, "Offset")).unwrap_or(Vec3::ZERO);
                draw_size = size * scale;
                legacy_shape = mesh_type != 0;
            }
            match mesh_type {
                // Head: Studio's own head mesh (content/avatar/heads/head.mesh),
                // round across the part's smaller horizontal side.
                0 => {
                    head_mesh = true;
                    let m = size.x.min(size.z);
                    draw_size = Vec3::new(m, size.y, m) * scale;
                    if let Some(head) = studio_avatar_mesh("heads/head.mesh") {
                        mesh_asset = Some(MeshAsset {
                            id: head,
                            fit_to_part: true,
                            scale: Vec3::ONE,
                            offset: Vec3::ZERO,
                            vertex_colors: true, origin: false,
                        });
                        placeholder = true;
                    } else {
                        (draw_rot, draw_size) = vertical(draw_size);
                        mesh = MeshKind::Cylinder;
                        ellipsoid = true;
                    }
                }
                1 | 6 => mesh = MeshKind::Block,
                2 => mesh = MeshKind::Wedge,
                3 => {
                    mesh = MeshKind::Ball;
                    ellipsoid = true;
                }
                4 => {
                    mesh = MeshKind::Cylinder;
                    ellipsoid = true;
                }
                11 => mesh = MeshKind::CornerWedge,
                5 => {
                    placeholder = true;
                    if texture_id.is_none() {
                        texture_id = content_string(doc.prop(*c, "TextureId"))
                            .or_else(|| content_string(doc.prop(*c, "TextureContent")));
                        tex_kind = mesh_kind;
                        if texture_id.is_some() && mesh_kind == crate::render::TexMode::Opaque {
                            // DefaultUnifiedFlatOpaqueFS multiplies diffuse texels by
                            // vertex colour before squaring into linear light.
                            let tint = vec3_of(doc.prop(*c, "VertexColor")).unwrap_or(Vec3::ONE);
                            texture_tint = Some(tint * tint);
                            tex_kind = crate::render::TexMode::OpaqueTinted;
                        }
                    }
                    mesh_asset = content_string(doc.prop(*c, "MeshId"))
                        .or_else(|| content_string(doc.prop(*c, "MeshContent")))
                        .map(|id| MeshAsset {
                            id,
                            fit_to_part: false,
                            scale: vec3_of(doc.prop(*c, "Scale")).unwrap_or(Vec3::ONE),
                            offset: vec3_of(doc.prop(*c, "Offset")).unwrap_or(Vec3::ZERO),
                            // Studio draws a SpecialMesh without its mesh's vertex
                            // colours. TEMPORARY EXCEPTION (MeshCache::get_colored):
                            // a local mesh file in a format Studio can't load as a
                            // MeshId at all (OBJ) keeps them, for a port that uses
                            // local OBJ scenery with light baked into the colours;
                            // remove it once that port has moved to MeshParts.
                            vertex_colors: false, origin: false,
                        });
                }
                // Prism, Pyramid, ramps: no shape of their own yet.
                _ => mesh = MeshKind::Block,
            }
        }
    }
    if rustudio_core::assets::clothing_role(&inst.name).is_some() {
        if let Some(model) = doc.dom.get_by_ref(inst.parent()) {
            deps.extend(model.children().iter().copied().filter(|c| {
                doc.class_of(*c)
                    .is_some_and(|k| matches!(k.as_str(), "Shirt" | "Pants" | "ShirtGraphic"))
            }));
            // The R15 atlas takes its cell colours from the other body parts.
            if r15_look.is_some() {
                deps.extend(model.children().iter().copied().filter(|c| {
                    *c != r && doc.dom.get_by_ref(*c).is_some_and(|i| rustudio_core::assets::r15_cell(&i.name).is_some())
                }));
            }
        }
    }
    if class == "NegateOperation" {
        alpha = alpha.min(0.5);
    }
    // Part balls and cylinders keep round ends (the shortest side wins);
    // mesh ellipsoids stretch to the size.
    match mesh {
        MeshKind::Ball if !ellipsoid => draw_size = Vec3::splat(draw_size.min_element()),
        MeshKind::Cylinder if !ellipsoid => {
            let m = draw_size.y.min(draw_size.z);
            draw_size = Vec3::new(draw_size.x, m, m);
        }
        _ => {}
    }
    if placeholder {
        mesh = MeshKind::Block;
    }
    let cast_shadow = !matches!(doc.prop(r, "CastShadow"), Some(Variant::Bool(false)));
    // MeshParts and unions: Studio fills a blob per convex piece of their
    // physics (record type 9); the viewport supplies cached convex-piece
    // bounds, with the part box as a fallback (lightgrid.rs Kind::Mesh).
    let mesh_like = class == "MeshPart" || class.ends_with("Operation");
    let light = (cast_shadow && light_transparency < 1.0 && class != "NegateOperation")
        .then(|| {
            if mesh_like {
                return Some(crate::lightgrid::Kind::Mesh);
            }
            let shape = match class {
                "WedgePart" => MeshKind::Wedge,
                "CornerWedgePart" => MeshKind::CornerWedge,
                _ if part_class => match enum_u32(doc.prop(r, "Shape")) {
                    Some(0) => MeshKind::Ball,
                    Some(2) => MeshKind::Cylinder,
                    Some(3) => MeshKind::Wedge,
                    Some(4) => MeshKind::CornerWedge,
                    _ => MeshKind::Block,
                },
                _ => MeshKind::Block,
            };
            crate::lightgrid::kind_of(light_special, light_legacy, shape)
        })
        .flatten()
        // A truss lets most light through (geometry 3: Transparency × 0.25 + 0.75).
        .map(|kind| {
            let t = if class == "TrussPart" { light_transparency * 0.25 + 0.75 } else { light_transparency };
            crate::lightgrid::Prim::new(kind, size, &xf, t)
        });
    let locked = matches!(doc.prop(r, "Locked"), Some(Variant::Bool(true)));
    // Only the Plastic shader samples StudsMapTexture. SmoothPlastic and
    // textured material shaders do not (DefaultUnifiedPlasticFS vs Flat/Detiled).
    // This applies to both built-in material tables.
    let surfaces = if part_class
        && mesh == MeshKind::Block
        && material_id == 256
        && material_variant(doc, r, material_id).is_none()
        && !placeholder
        && mesh_asset.is_none()
        && draw_size == size
    {
        surfaces_of(doc, r)
    } else {
        0
    };
    let skin_deps = (class == "MeshPart")
        .then(|| crate::skin::deps_of(doc, r, mesh_asset.as_ref().map(|a| a.id.as_str())));
    Some(ScenePart {
        layer: None,
        r,
        xf,
        size,
        mesh_asset,
        texture_id,
        tex_kind,
        emissive: sa_emissive,
        decals,
        clothing,
        r15_look,
        locked,
        variant: has_variant,
        variant_pack: if has_variant { material_variant_pack(doc, r, material_id) } else { None },
        decal_size: if head_mesh || legacy_shape { draw_size } else { size },
        decal_offset: if legacy_shape { draw_offset } else { Vec3::ZERO },
        // A skinned MeshPart redraws when a Bone or its FaceControls changes (skin.rs).
        skinned: skin_deps.as_ref().is_some_and(|d| !d.is_empty()),
        deps: if class == "MeshPart" {
            let mut deps = deps;
            deps.extend(skin_deps.unwrap_or_default());
            // The body part that draws a fitted garment redraws when the
            // other body parts move (the garment is skinned to them, wrap.rs).
            if crate::wrap::draws_garment(doc, inst.parent(), r) {
                if let Some(m) = doc.dom.get_by_ref(inst.parent()) {
                    for c in m.children() {
                        if *c != r && doc.dom.get_by_ref(*c).is_some_and(|i| i.children().iter().any(|k| doc.class_of(*k).is_some_and(|x| x == "WrapTarget"))) {
                            deps.push(*c);
                        }
                    }
                }
            }
            deps
        } else {
            deps
        },
        draw_offset,
        draw_rot,
        light,
        draw: PartDraw {
            xf: Xform {
                pos: xf.pos + xf.rot * draw_offset,
                rot: xf.rot * draw_rot,
            },
            size: draw_size,
            // Textured meshes: Studio's mesh shaders square texel × vertex /
            // instance colour only; the per-Material darkening (measured on
            // untextured cubes) doesn't apply (Map 2 in an offline Studio: a
            // CorrodedMetal MeshPart reads exactly mean(texel²), 2026-11-08).
            color: texture_tint.map(|t| t.to_array()).unwrap_or([
                srgb_to_linear(rgb[0]) * albedo[0],
                srgb_to_linear(rgb[1]) * albedo[1],
                srgb_to_linear(rgb[2]) * albedo[2],
            ]),
            alpha,
            mesh,
            material,
            placeholder,
            hover: false,
            reflectance,
            custom_mesh: None,
            texture: None,
            tex_mode: crate::render::TexMode::None,
            uvt: PartDraw::NO_UVT,
            emissive: [0.0; 3],
            tex_albedo: 1.0,
            surfaces,
            material_id,
            mat_layer: -1,
            surface_tint,
            cast_shadow,
        },
    })
}

/// The ground grid, drawn just above the baseplate top.
// ---------------------------------------------------------------- decals
// (drawn by the viewport's parts and by skin.rs's skinned meshes)

/// Where a face's Decal / Texture image lands on a part, in its unit box
/// (Enum.NormalId: Right 0, Top 1, Back 2, Left 3, Bottom 4, Front 5),
/// as Studio 0.747 maps it: seen from outside, the image upright and not
/// mirrored (checks/mesh-deform/tools/decalprobe.py, a grid image on all
/// six faces of a Part: Front u runs toward −X, Back toward +X, Top's u
/// toward −X and v toward −Z).
pub fn decal_face_uv(face: u32, v: Vec3) -> glam::Vec2 {
    match face {
        0 => glam::Vec2::new(0.5 - v.z, 0.5 - v.y),
        1 => glam::Vec2::new(0.5 - v.x, 0.5 - v.z),
        2 => glam::Vec2::new(v.x + 0.5, 0.5 - v.y),
        3 => glam::Vec2::new(v.z + 0.5, 0.5 - v.y),
        4 => glam::Vec2::new(v.x + 0.5, 0.5 - v.z),
        _ => glam::Vec2::new(0.5 - v.x, 0.5 - v.y),
    }
}

/// The face whose Decal a mesh triangle takes: its geometric normal's
/// largest axis (mesh space). Studio 0.747: a quad turned 44° from Front
/// takes the Front image, 46° doesn't (about either axis, skinned or not,
/// Size × (1, 1, 3) the same); a triangle's vertex normals don't count.
pub fn decal_face_of(n: Vec3) -> Option<u32> {
    let a = n.abs();
    if a.max_element() <= 0.0 || !n.is_finite() {
        return None;
    }
    Some(if a.x >= a.y && a.x >= a.z {
        if n.x > 0.0 { 0 } else { 3 }
    } else if a.y >= a.z {
        if n.y > 0.0 { 1 } else { 4 }
    } else if n.z > 0.0 {
        2
    } else {
        5
    })
}

pub fn grid_lines(out: &mut Vec<LineVertex>, step: f32) {
    let ext = mech::GRID_EXTENT;
    let step = if step > 0.0 { step } else { mech::GRID_SPACING };
    let n = (ext / step) as i32;
    let y = 0.1;
    for i in -n..=n {
        let t = i as f32 * step;
        let major = i % 4 == 0;
        let c = if major {
            Vec4::new(1.0, 1.0, 1.0, 0.32)
        } else {
            Vec4::new(1.0, 1.0, 1.0, 0.14)
        };
        for (a, b) in [
            (Vec3::new(t, y, -ext), Vec3::new(t, y, ext)),
            (Vec3::new(-ext, y, t), Vec3::new(ext, y, t)),
        ] {
            out.push(LineVertex {
                pos: a.to_array(),
                color: c.to_array(),
            });
            out.push(LineVertex {
                pos: b.to_array(),
                color: c.to_array(),
            });
        }
    }
}

/// BrickColor lookup for the properties panel swatch.
pub fn brick_rgb(b: BrickColor) -> [u8; 3] {
    let c = b.to_color3uint8();
    [c.r, c.g, c.b]
}

#[cfg(test)]
mod tint_tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, WeakDom};

    #[test]
    fn serialized_material_generation_precedes_api_default() {
        for (property, value) in [("Use2022MaterialsXml", true), ("Use2022MaterialsXml", false), ("Use2022Materials", true)] {
            let service = InstanceBuilder::new("MaterialService").with_property(property, value);
            let doc = Document::from_play_dom(WeakDom::new(
                InstanceBuilder::new("DataModel").with_child(service)), "materials".into());
            assert_eq!(use_2022_materials(&doc), value, "{property}");
        }
    }

    #[test]
    fn file_mesh_texture_uses_vertex_color() {
        let mesh = InstanceBuilder::new("SpecialMesh")
            .with_property("MeshType", rbx_dom_weak::types::Enum::from_u32(5))
            .with_property("MeshId", rbx_dom_weak::types::ContentId::from("rbxassetid://1"))
            .with_property("TextureId", rbx_dom_weak::types::ContentId::from("rbxassetid://2"))
            .with_property("VertexColor", rbx_dom_weak::types::Vector3::new(0.5, 1.0, 0.25));
        let part = InstanceBuilder::new("Part").with_child(mesh);
        let r = part.referent();
        let doc = Document::from_play_dom(WeakDom::new(
            InstanceBuilder::new("DataModel").with_child(part)), "tint".into());
        let p = part_draw(&doc, doc.dom.get_by_ref(r).unwrap()).unwrap();
        assert_eq!(p.tex_kind, crate::render::TexMode::OpaqueTinted);
        assert!((p.draw.color[0] / p.draw.color[1] - 0.25).abs() < 1e-6);
        assert!((p.draw.color[2] / p.draw.color[1] - 0.0625).abs() < 1e-6);
    }

    /// A CharacterMesh scales with its limb over the standard R6 size and
    /// sits by its own origin (Studio 0.747: the Boy
    /// package on a game's 1.46× rig fills the limbs; rustudio drew it at 1×).
    #[test]
    fn character_mesh_scales_with_its_limb() {
        let size = |x: f32, y: f32, z: f32| rbx_dom_weak::types::Vector3::new(x, y, z);
        let cm = |part: u32, id: i64| InstanceBuilder::new("CharacterMesh")
            .with_property("BodyPart", rbx_dom_weak::types::Enum::from_u32(part))
            .with_property("MeshId", id);
        let torso = InstanceBuilder::new("Part").with_name("Torso").with_property("Size", size(2.93, 2.93, 1.46));
        let arm = InstanceBuilder::new("Part").with_name("Left Arm").with_property("Size", size(1.46, 2.93, 1.46));
        let (tr, ar) = (torso.referent(), arm.referent());
        let model = InstanceBuilder::new("Model")
            .with_child(InstanceBuilder::new("Humanoid"))
            .with_child(torso)
            .with_child(arm)
            .with_child(cm(1, 376169183))
            .with_child(cm(2, 279174949));
        let doc = Document::from_play_dom(WeakDom::new(
            InstanceBuilder::new("DataModel").with_child(model)), "charmesh".into());
        for (r, id, scale) in [(tr, "rbxassetid://376169183", Vec3::new(1.465, 1.465, 1.46)), (ar, "rbxassetid://279174949", Vec3::new(1.46, 1.465, 1.46))] {
            let p = part_draw(&doc, doc.dom.get_by_ref(r).unwrap()).unwrap();
            let a = p.mesh_asset.expect("the CharacterMesh");
            assert_eq!(a.id, id);
            assert!(!a.fit_to_part && a.origin);
            assert!((a.scale - scale).length() < 1e-5, "{:?}", a.scale);
        }
    }

    #[test]
    fn character_mesh_negative_i32_id_unwraps_to_u32() {
        let size = |x: f32, y: f32, z: f32| rbx_dom_weak::types::Vector3::new(x, y, z);
        let cm = InstanceBuilder::new("CharacterMesh")
            .with_property("BodyPart", rbx_dom_weak::types::Enum::from_u32(3))
            .with_property("MeshId", -1709562747i64);
        let arm = InstanceBuilder::new("Part")
            .with_name("Right Arm")
            .with_property("Size", size(1.0, 2.0, 1.0));
        let ar = arm.referent();
        let model = InstanceBuilder::new("Model")
            .with_child(InstanceBuilder::new("Humanoid"))
            .with_child(arm)
            .with_child(cm);
        let doc = Document::from_play_dom(
            WeakDom::new(InstanceBuilder::new("DataModel").with_child(model)),
            "charmesh-neg".into(),
        );
        let p = part_draw(&doc, doc.dom.get_by_ref(ar).unwrap()).unwrap();
        let a = p.mesh_asset.expect("the unwrapped CharacterMesh");
        assert_eq!(a.id, "rbxassetid://2585404549");
    }
}

/// Resolves a CharacterMesh's asset URI. Legacy XML files serialized MeshId
/// as a signed 32-bit `<int>`, wrapping asset IDs above `i32::MAX` into
/// negative numbers (`-1709562747` → `2585404549`); reinterpret those as u32.
fn character_mesh_asset_id(doc: &Document, r: Ref) -> Option<String> {
    let from_signed = |n: i64| -> Option<String> {
        if n > 0 {
            Some(format!("rbxassetid://{n}"))
        } else if n < 0 && n >= i32::MIN as i64 {
            Some(format!("rbxassetid://{}", (n as i32) as u32))
        } else {
            None
        }
    };
    match doc.prop(r, "MeshId") {
        Some(Variant::Int64(n)) if n != 0 => {
            if let Some(id) = from_signed(n) {
                return Some(id);
            }
        }
        Some(Variant::Int32(n)) if n != 0 => {
            return Some(format!("rbxassetid://{}", n as u32));
        }
        _ => {}
    }
    let content = content_string(doc.prop(r, "MeshContent"))?;
    if let Some(rest) = content.trim().strip_prefix("rbxassetid://") {
        if let Ok(n) = rest.parse::<i64>() {
            if let Some(id) = from_signed(n) {
                return Some(id);
            }
        }
    }
    Some(content)
}

/// A mesh of the Studio install's content/avatar (the classic head, R6
/// limbs) as an engine-generated id: the engine picks these files, not the
/// place, so they still draw while scripts run, when local paths aren't
/// place content (assets::place_content_allowed). Read once per file.
fn studio_avatar_mesh(rel: &str) -> Option<String> {
    static IDS: std::sync::Mutex<Option<std::collections::HashMap<String, Option<String>>>> =
        std::sync::Mutex::new(None);
    let mut g = IDS.lock().ok()?;
    g.get_or_insert_with(Default::default)
        .entry(rel.to_string())
        .or_insert_with(|| {
            let bytes = std::fs::read(rustudio_core::default_scripts::studio_dir()?.join("content/avatar").join(rel)).ok()?;
            let id = format!("rbxtemp://rustudio/avatar/{rel}");
            rustudio_core::assets::register_generated(&id, bytes);
            Some(id)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::InstanceBuilder;

    #[test]
    fn viewport_scene_handles_a_frame_removed_since_gui_draw() {
        let mut doc = Document::new_place();
        let root = doc.default_parent();
        let removed = doc.insert(root, InstanceBuilder::new("ViewportFrame"));
        let live = doc.insert(root, InstanceBuilder::new("ViewportFrame"));
        let part = doc.insert(live, InstanceBuilder::new("Part"));

        let mut scene = Scene::viewport_frames();
        scene.set_viewport_frames(&doc, &HashSet::from([live]));
        scene.sync(&mut doc);
        let last_drawn = HashSet::from([removed, live]);
        assert_eq!(doc.remove(&[removed]), 1);
        scene.set_viewport_frames(&doc, &last_drawn);
        scene.sync(&mut doc);

        assert_eq!(scene.parts.len(), 1);
        assert_eq!(scene.parts[0].r, part);
        assert_eq!(scene.parts[0].layer, Some(live));
    }
}
