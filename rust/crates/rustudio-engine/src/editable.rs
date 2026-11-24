//! EditableMesh / EditableImage and the Content datatype.
//!
//! The two Editable* objects are unparented DOM instances (as before) whose
//! data lives here, keyed by their Ref, so the renderer can reach it without
//! the Lua state:
//!
//! - an EditableImage is RGBA8 pixels plus a generation that every write
//!   bumps. A `Content` pointing at it resolves (doc.rs `prop`, scene.rs
//!   `content_string`) to `rbxtemp://rustudio/editable/img/<n>@<gen>`; the
//!   texture caches (assets.rs, gui.rs) read the pixels straight from here
//!   and re-upload in place when the generation moves on.
//! - an EditableMesh keeps vertices, normals, UVs, colours and faces under
//!   typed ids (`index * 8 + kind`, so the Batch API can tell them apart).
//!   AssetService:CreateMeshPartAsync snapshots it as a binary 2.00 mesh
//!   registered under `rbxtemp://rustudio/editable/mesh/<n>@<gen>`, so the
//!   MeshPart goes through the ordinary mesh path (drawing, ray tests,
//!   collision).
//!
//! The `Content` global and the `Content` userdata (Content.none, fromUri,
//! fromAssetId, fromObject; SourceType / Uri / Object) are here too.

use std::sync::{Arc, Mutex};

use glam::Vec2;
use rbx_dom_weak::types::ContentType;

use super::*;

// ------------------------------------------------------------------ store

pub struct Image {
    pub n: u64,
    pub w: u32,
    pub h: u32,
    pub px: Arc<Vec<u8>>,
    pub gen: u64,
    /// Instances / properties that showed this image (doc.rs `prop`): marked
    /// changed when its pixels change so parts redraw.
    users: HashSet<(Ref, rbx_dom_weak::Ustr)>,
}

#[path = "editable_mesh.rs"]
pub mod em;
#[path = "editable_project.rs"]
mod project;
pub use em::Mesh;

type SharedMesh = Arc<Mutex<Mesh>>;

fn lock(m: &SharedMesh) -> std::sync::MutexGuard<'_, Mesh> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct Store {
    images: HashMap<Ref, Image>,
    meshes: HashMap<Ref, SharedMesh>,
    next: u64,
    /// Image number → its object (renderer lookups by id).
    image_by_n: HashMap<u64, Ref>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// The store as core (document, asset and texture caches) sees it.
struct Source;
static INSTALLED: Source = Source;

impl rustudio_core::editable_source::EditableSource for Source {
    fn content_uri(&self, obj: Ref, user: Option<(Ref, &str)>) -> Option<String> {
        content_uri(obj, user)
    }
    fn image_pixels(&self, id: &str) -> Option<(u64, rustudio_core::texture::Rgba)> {
        image_pixels(id)
    }
    fn is_live(&self, id: &str) -> bool {
        is_live(id)
    }
    fn live_build(&self, id: &str) -> Option<(Vec<rustudio_core::vertex::Vertex>, Vec3)> {
        live_build(id)
    }
}



fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Store::default))
}

const MESH_PREFIX: &str = "rbxtemp://rustudio/editable/";
/// Content ids of engine-made images Roblox shows as ContentSourceType.Opaque.
pub const OPAQUE_PREFIX: &str = "rbxtemp://rustudio/opaque/";

/// The id a Content pointing at `obj` shows as (an EditableImage's current
/// pixels, an EditableMesh's last MeshPart snapshot); `user` (instance,
/// property) is remembered so a later write redraws it.
pub fn content_uri(obj: Ref, user: Option<(Ref, &str)>) -> Option<String> {
    with(|s| {
        if let Some(img) = s.images.get_mut(&obj) {
            if let Some((r, p)) = user {
                img.users.insert((r, rbx_dom_weak::ustr(p)));
            }
            return Some(format!("{}{}@{}", rustudio_core::editable_source::IMG_PREFIX, img.n, img.gen));
        }
        let m = lock(s.meshes.get(&obj)?);
        (m.gen > 0).then(|| format!("{MESH_PREFIX}mesh/{}@{}", m.n, m.gen))
    })
}

/// The pixels of an editable image id (any generation: the current ones).
pub fn image_pixels(id: &str) -> Option<(u64, rustudio_core::texture::Rgba)> {
    let rest = id.strip_prefix(rustudio_core::editable_source::IMG_PREFIX)?;
    let n: u64 = rest.split('@').next()?.parse().ok()?;
    with(|s| {
        let r = *s.image_by_n.get(&n)?;
        let img = s.images.get(&r)?;
        Some((
            img.gen,
            rustudio_core::texture::Rgba { width: img.w.max(1), height: img.h.max(1), pixels: (*img.px).clone() },
        ))
    })
}

fn is_editable_image(r: Ref) -> bool {
    with(|s| s.images.contains_key(&r))
}

fn is_editable_mesh(r: Ref) -> bool {
    with(|s| s.meshes.contains_key(&r))
}

fn new_image(w: u32, h: u32, px: Vec<u8>) -> Image {
    Image { n: 0, w, h, px: Arc::new(px), gen: 1, users: HashSet::new() }
}

// ------------------------------------------------------------------ Content

/// The Content datatype (typeof "Content").
#[derive(Clone, Debug, PartialEq)]
pub struct LContent(pub Content);

impl LContent {
    pub fn variant(&self) -> Variant {
        Variant::Content(self.0.clone())
    }
}

impl UserData for LContent {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field("__type", "Content");
        f.add_field_method_get("SourceType", |_, this| {
            let v = match this.0.value() {
                ContentType::None => 0,
                // Engine-made images (VideoSampler frames) are Opaque in Roblox.
                ContentType::Uri(u) if u.starts_with(OPAQUE_PREFIX) => 3,
                ContentType::Uri(_) => 1,
                ContentType::Object(_) => 2,
                _ => 3,
            };
            Ok(LEnumItem::from_value("ContentSourceType", v).unwrap_or(LEnumItem {
                enum_type: "ContentSourceType".into(),
                name: ["None", "Uri", "Object", "Opaque"][v as usize].into(),
                value: v,
            }))
        });
        f.add_field_method_get("Uri", |lua, this| match this.0.as_uri() {
            Some(u) if u.starts_with(OPAQUE_PREFIX) => Ok(Value::Nil),
            Some(u) => Ok(Value::String(lua.create_string(u)?)),
            None => Ok(Value::Nil),
        });
        f.add_field_method_get("Object", |lua, this| match this.0.as_object() {
            Some(r) => inst_value(lua, r),
            None => Ok(Value::Nil),
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            // Studio: Content{SourceType=Uri, Uri=rbxassetid://123}.
            Ok(match this.0.value() {
                ContentType::Uri(u) => format!("Content{{SourceType=Uri, Uri={u}}}"),
                ContentType::Object(_) => "Content{SourceType=Object}".to_string(),
                _ => "Content{SourceType=None}".to_string(),
            })
        });
        m.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(match (opt_ud::<LContent>(&a), opt_ud::<LContent>(&b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            })
        });
    }
}

/// A Lua value as a Content (a Content, or a URI string as older code passes).
pub fn content_of(v: &Value) -> Option<Content> {
    match v {
        Value::UserData(_) => opt_ud::<LContent>(v).map(|c| c.0),
        Value::String(s) => Some(Content::from(s.to_str().ok()?.to_string())),
        Value::Nil => Some(Content::none()),
        _ => None,
    }
}

pub fn content_value(lua: &Lua, c: &Content) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(LContent(c.clone()))?))
}

/// Puts the `Content` global in place (over library.luau's string version)
/// and tells core where the store is (the document and the caches read it
/// through `editable_source`).
pub fn install(lua: &Lua) -> mlua::Result<()> {
    rustudio_core::editable_source::install(&INSTALLED);
    let t = lua.create_table()?;
    t.set("none", LContent(Content::none()))?;
    t.set(
        "fromUri",
        lua.create_function(|_, uri: Value| match uri {
            Value::String(s) => Ok(LContent(Content::from(s.to_str()?.to_string()))),
            v => Err(rt_err(format!("Content.fromUri: string expected, got {}", v.type_name()))),
        })?,
    )?;
    t.set(
        "fromAssetId",
        lua.create_function(|_, id: Value| {
            let n = num(&id).ok_or_else(|| rt_err("Content.fromAssetId: number expected"))?;
            Ok(LContent(Content::from(format!("rbxassetid://{}", n as u64))))
        })?,
    )?;
    t.set(
        "fromObject",
        lua.create_function(|_, obj: Value| {
            let r = opt_inst(&obj).filter(|r| is_editable_image(*r) || is_editable_mesh(*r)).ok_or_else(|| {
                rt_err("Content.fromObject: EditableImage or EditableMesh expected")
            })?;
            Ok(LContent(Content::from_referent(r)))
        })?,
    )?;
    lua.globals().set("Content", t)?;
    Ok(())
}

// ------------------------------------------------------------------ methods

pub const METHODS: &[(&str, &str)] = &[
    ("CreateEditableMesh", "AssetService"),
    ("CreateEditableImage", "AssetService"),
    ("CreateEditableMeshAsync", "AssetService"),
    ("CreateEditableImageAsync", "AssetService"),
    ("CreateSurfaceAppearanceAsync", "AssetService"),
    // EditableImage
    ("Destroy", "EditableImage"),
    ("ReadPixelsBuffer", "EditableImage"),
    ("WritePixelsBuffer", "EditableImage"),
    ("DrawRectangle", "EditableImage"),
    ("DrawCircle", "EditableImage"),
    ("DrawLine", "EditableImage"),
    ("DrawTriangle", "EditableImage"),
    ("DrawImage", "EditableImage"),
    ("DrawImageTransformed", "EditableImage"),
    ("DrawImageProjected", "EditableImage"),
    ("SampleImageProjected", "EditableImage"),
    // EditableMesh (editable_mesh.rs, same list as em::METHODS)
    ("Destroy", "EditableMesh"),
    ("AddVertex", "EditableMesh"),
    ("AddNormal", "EditableMesh"),
    ("AddUV", "EditableMesh"),
    ("AddColor", "EditableMesh"),
    ("AddTriangle", "EditableMesh"),
    ("AddFace", "EditableMesh"),
    ("AddBone", "EditableMesh"),
    ("RemoveFace", "EditableMesh"),
    ("RemoveUnused", "EditableMesh"),
    ("RemoveBone", "EditableMesh"),
    ("Clear", "EditableMesh"),
    ("GetVertices", "EditableMesh"),
    ("GetFaces", "EditableMesh"),
    ("GetNormals", "EditableMesh"),
    ("GetUVs", "EditableMesh"),
    ("GetColors", "EditableMesh"),
    ("GetBones", "EditableMesh"),
    ("GetPosition", "EditableMesh"),
    ("SetPosition", "EditableMesh"),
    ("GetNormal", "EditableMesh"),
    ("SetNormal", "EditableMesh"),
    ("ResetNormal", "EditableMesh"),
    ("GetUV", "EditableMesh"),
    ("SetUV", "EditableMesh"),
    ("GetColor", "EditableMesh"),
    ("SetColor", "EditableMesh"),
    ("GetColorAlpha", "EditableMesh"),
    ("SetColorAlpha", "EditableMesh"),
    ("GetFaceVertices", "EditableMesh"),
    ("SetFaceVertices", "EditableMesh"),
    ("GetFaceNormals", "EditableMesh"),
    ("SetFaceNormals", "EditableMesh"),
    ("GetFaceUVs", "EditableMesh"),
    ("SetFaceUVs", "EditableMesh"),
    ("GetFaceColors", "EditableMesh"),
    ("SetFaceColors", "EditableMesh"),
    ("GetVertexFaces", "EditableMesh"),
    ("GetAdjacentVertices", "EditableMesh"),
    ("GetAdjacentFaces", "EditableMesh"),
    ("GetCenter", "EditableMesh"),
    ("GetSize", "EditableMesh"),
    ("RaycastLocal", "EditableMesh"),
    ("FindClosestVertex", "EditableMesh"),
    ("FindVerticesWithinSphere", "EditableMesh"),
    ("FindClosestPointOnSurface", "EditableMesh"),
    ("BatchAdd", "EditableMesh"),
    ("BatchSetValues", "EditableMesh"),
    ("BatchGetValues", "EditableMesh"),
    ("BatchRemove", "EditableMesh"),
    ("BatchGetFaceAttributes", "EditableMesh"),
    ("BatchGetVertexAttributes", "EditableMesh"),
    ("BatchGetVertexFaceAttributes", "EditableMesh"),
    ("BatchSetFaceAttributes", "EditableMesh"),
    ("BatchSetVertexFaceAttributes", "EditableMesh"),
    ("GetBoneByName", "EditableMesh"),
    ("GetBoneCFrame", "EditableMesh"),
    ("GetBoneIsVirtual", "EditableMesh"),
    ("GetBoneName", "EditableMesh"),
    ("GetBoneParent", "EditableMesh"),
    ("SetBoneCFrame", "EditableMesh"),
    ("SetBoneIsVirtual", "EditableMesh"),
    ("SetBoneName", "EditableMesh"),
    ("SetBoneParent", "EditableMesh"),
    ("GetVertexBones", "EditableMesh"),
    ("GetVertexBoneWeights", "EditableMesh"),
    ("SetVertexBones", "EditableMesh"),
    ("SetVertexBoneWeights", "EditableMesh"),
    ("GetFacesWithAttribute", "EditableMesh"),
    ("GetFacesWithColor", "EditableMesh"),
    ("GetFacesWithNormal", "EditableMesh"),
    ("GetFacesWithUV", "EditableMesh"),
    ("GetVerticesWithAttribute", "EditableMesh"),
    ("GetVerticesWithColor", "EditableMesh"),
    ("GetVerticesWithNormal", "EditableMesh"),
    ("GetVerticesWithUV", "EditableMesh"),
    ("GetVertexNormals", "EditableMesh"),
    ("GetVertexUVs", "EditableMesh"),
    ("GetVertexColors", "EditableMesh"),
    ("GetVertexFaceNormal", "EditableMesh"),
    ("GetVertexFaceUV", "EditableMesh"),
    ("GetVertexFaceColor", "EditableMesh"),
    ("SetVertexFaceNormal", "EditableMesh"),
    ("SetVertexFaceUV", "EditableMesh"),
    ("SetVertexFaceColor", "EditableMesh"),
    ("IdDebugString", "EditableMesh"),
    ("MergeVertices", "EditableMesh"),
    ("Triangulate", "EditableMesh"),
    ("GetFacsPose", "EditableMesh"),
    ("GetFacsPoses", "EditableMesh"),
    ("GetFacsCorrectivePose", "EditableMesh"),
    ("GetFacsCorrectivePoses", "EditableMesh"),
    ("SetFacsPose", "EditableMesh"),
    ("SetFacsBonePose", "EditableMesh"),
    ("SetFacsCorrectivePose", "EditableMesh"),
];

fn names() -> &'static HashSet<&'static str> {
    static N: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    N.get_or_init(|| METHODS.iter().map(|(m, _)| *m).collect())
}

/// EditableImage.Size / EditableMesh.FixedSize.
pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    match (class, key) {
        ("EditableImage", "Size") => {
            let s = with(|s| s.images.get(&r).map(|i| (i.w, i.h)));
            Ok(s.map(|(w, h)| LVector2(Vec2::new(w as f32, h as f32))).map(|v| lua.create_userdata(v)).transpose()?.map(Value::UserData))
        }
        ("EditableMesh", "FixedSize") => {
            // Any EditableMesh (one not made through AssetService reads false).
            Ok(Some(Value::Boolean(with(|s| s.meshes.get(&r).map(|m| lock(m).fixed)).unwrap_or(false))))
        }
        _ => Ok(None),
    }
}

fn one(v: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([v]))
}

fn none() -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}


fn v2(v: &Value) -> Option<Vec2> {
    opt_ud::<LVector2>(v).map(|v| v.0)
}

fn field(v: &Value, key: &str) -> Value {
    match v {
        Value::Table(t) => t.get::<Value>(key).unwrap_or(Value::Nil),
        _ => Value::Nil,
    }
}

fn enum_val(v: &Value) -> Option<u32> {
    opt_ud::<LEnumItem>(v).map(|e| e.value)
}





/// The image's users marked changed (they redraw with the new pixels).
fn touched(lua: &Lua, r: Ref) -> mlua::Result<()> {
    let users: Vec<(Ref, rbx_dom_weak::Ustr)> = with(|s| {
        s.images.get_mut(&r).map(|i| {
            i.gen += 1;
            i.users.iter().copied().collect()
        })
    })
    .unwrap_or_default();
    if users.is_empty() {
        return Ok(());
    }
    let mut w = world(lua)?;
    let d = w.dm();
    for (u, p) in users {
        if d.exists(u) {
            d.mark_prop(u, p);
        }
    }
    Ok(())
}

/// The editable-object and AssetService calls; None when `name` isn't one.
pub fn call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if !names().contains(name) {
        return None;
    }
    // (an EditableMesh is known by its entry: no class lookup per call)
    if with(|s| s.meshes.contains_key(&this)) {
        return Some(mesh_call(lua, name, this, args));
    }
    let class = world(lua).ok()?.d().class_of(this)?;
    match class.as_str() {
        "AssetService" => Some(asset_call(lua, name, args)),
        "EditableImage" => Some(image_call(lua, name, this, args)),
        "EditableMesh" => Some(mesh_call(lua, name, this, args)),
        _ => None,
    }
}

fn new_object(lua: &Lua, class: &str) -> mlua::Result<Ref> {
    let r = ext::create_any(lua, class)?;
    if let Some(i) = world(lua)?.dm().dom.get_by_ref_mut(r) {
        i.name = class.to_string();
    }
    Ok(r)
}

fn add_image(lua: &Lua, img: Image) -> mlua::Result<Value> {
    let r = new_object(lua, "EditableImage")?;
    with(|s| {
        s.next += 1;
        let n = s.next;
        s.image_by_n.insert(n, r);
        s.images.insert(r, Image { n, ..img });
    });
    inst_value(lua, r)
}

/// Pixels shown by something other than an EditableImage (a playing
/// video's current frame, media domain), stored under `key` with a new
/// generation per call so the texture caches re-upload; returns its id.
pub fn external_image(key: Ref, w: u32, h: u32, px: Vec<u8>) -> String {
    with(|s| {
        if let Some(img) = s.images.get_mut(&key) {
            img.w = w;
            img.h = h;
            img.px = Arc::new(px);
            img.gen += 1;
        } else {
            s.next += 1;
            let n = s.next;
            s.image_by_n.insert(n, key);
            s.images.insert(key, Image { n, ..new_image(w, h, px) });
        }
        let img = &s.images[&key];
        format!("{}{}@{}", rustudio_core::editable_source::IMG_PREFIX, img.n, img.gen)
    })
}

/// Forgets an [`external_image`].
pub fn drop_external_image(key: Ref) {
    with(|s| {
        if let Some(img) = s.images.remove(&key) {
            s.image_by_n.remove(&img.n);
        }
    })
}

fn add_mesh(lua: &Lua, m: Mesh) -> mlua::Result<Value> {
    let r = new_object(lua, "EditableMesh")?;
    with(|s| {
        s.next += 1;
        let n = s.next;
        let mut m = m;
        m.n = n;
        m.gen = 0;
        s.meshes.insert(r, Arc::new(Mutex::new(m)));
    });
    inst_value(lua, r)
}

fn content_arg(args: &MultiValue, i: usize, what: &str) -> mlua::Result<Content> {
    content_of(&arg(args, i)).ok_or_else(|| rt_err(format!("{what}: Content expected")))
}

fn asset_call(lua: &Lua, name: &str, args: &MultiValue) -> mlua::Result<MultiValue> {
    match name {
        "CreateEditableMesh" => {
            let fixed = field(&arg(args, 1), "FixedSize") == Value::Boolean(true);
            one(add_mesh(lua, { let mut m = Mesh::default(); m.fixed = fixed; m })?)
        }
        "CreateEditableImage" => {
            let size = v2(&field(&arg(args, 1), "Size")).unwrap_or(Vec2::splat(512.0));
            if !(size.x >= 1.0 && size.y >= 1.0 && size.x <= 1024.0 && size.y <= 1024.0) {
                return Err(rt_err("CreateEditableImage: Size must be between 1 and 1024"));
            }
            let (w, h) = (size.x as u32, size.y as u32);
            one(add_image(lua, new_image(w, h, vec![0; (w * h * 4) as usize]))?)
        }
        "CreateEditableMeshAsync" => {
            let c = content_arg(args, 1, name)?;
            let fixed = field(&arg(args, 2), "FixedSize") == Value::Boolean(true);
            let mut m = match c.value() {
                ContentType::Object(r) => with(|s| s.meshes.get(r).map(|m| lock(m).clone()))
                    .ok_or_else(|| rt_err("CreateEditableMeshAsync: the object is not an EditableMesh"))?,
                ContentType::Uri(u) => mesh_from_uri(u)?,
                _ => return Err(rt_err("CreateEditableMeshAsync: empty Content")),
            };
            m.fixed = fixed;
            one(add_mesh(lua, m)?)
        }
        "CreateEditableImageAsync" => {
            let c = content_arg(args, 1, name)?;
            let img = match c.value() {
                ContentType::Object(r) => with(|s| s.images.get(r).map(|i| new_image(i.w, i.h, (*i.px).clone())))
                    .ok_or_else(|| rt_err("CreateEditableImageAsync: the object is not an EditableImage"))?,
                ContentType::Uri(u) => {
                    let bytes = rustudio_core::assets::script_content_blocking(u).map_err(|e| rt_err(format!("CreateEditableImageAsync: {e}")))?;
                    let d = rustudio_core::texture::decode(&bytes).map_err(|e| rt_err(format!("CreateEditableImageAsync: {e}")))?;
                    new_image(d.width, d.height, d.pixels)
                }
                _ => return Err(rt_err("CreateEditableImageAsync: empty Content")),
            };
            one(add_image(lua, img)?)
        }
        "CreateSurfaceAppearanceAsync" => {
            let maps = arg(args, 1);
            let r = ext::create_any(lua, "SurfaceAppearance")?;
            for key in ["ColorMap", "NormalMap", "RoughnessMap", "MetalnessMap", "EmissiveMask"] {
                if let Some(c) = content_of(&field(&maps, key)).filter(|c| !matches!(c.value(), ContentType::None)) {
                    store_prop(lua, r, &format!("{key}Content"), Variant::Content(c))?;
                }
            }
            one(inst_value(lua, r)?)
        }
        _ => none(),
    }
}

// ------------------------------------------------------------------ images

fn rgba_of(color: &Value, transparency: &Value) -> [f32; 4] {
    let c = opt_ud::<LColor3>(color).map(|c| c.0).unwrap_or([1.0; 3]);
    let a = 1.0 - num(transparency).unwrap_or(0.0).clamp(0.0, 1.0) as f32;
    [c[0], c[1], c[2], a]
}

/// ImageCombineType: BlendSourceOver 1 (default), Overwrite 2, Add 3,
/// Multiply 4, AlphaBlend 5, Subtract 7.
fn blend(dst: &mut [u8], src: [f32; 4], mode: u32) {
    let d = [dst[0] as f32 / 255.0, dst[1] as f32 / 255.0, dst[2] as f32 / 255.0, dst[3] as f32 / 255.0];
    let a = src[3];
    let out = match mode {
        2 => src,
        3 => [d[0] + src[0] * a, d[1] + src[1] * a, d[2] + src[2] * a, d[3]],
        4 => [
            d[0] * (1.0 - a + src[0] * a),
            d[1] * (1.0 - a + src[1] * a),
            d[2] * (1.0 - a + src[2] * a),
            d[3],
        ],
        7 => [d[0] - src[0] * a, d[1] - src[1] * a, d[2] - src[2] * a, d[3]],
        _ => {
            let oa = a + d[3] * (1.0 - a);
            if oa <= 1e-6 {
                [0.0; 4]
            } else {
                let f = |s: f32, t: f32| (s * a + t * d[3] * (1.0 - a)) / oa;
                [f(src[0], d[0]), f(src[1], d[1]), f(src[2], d[2]), oa]
            }
        }
    };
    for k in 0..4 {
        dst[k] = (out[k].clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

fn edit_pixels<R>(r: Ref, f: impl FnOnce(u32, u32, &mut Vec<u8>) -> R) -> mlua::Result<R> {
    with(|s| {
        let img = s.images.get_mut(&r).ok_or_else(|| rt_err("the EditableImage was destroyed"))?;
        let (w, h) = (img.w, img.h);
        Ok(f(w, h, Arc::make_mut(&mut img.px)))
    })
}

fn combine_arg(v: &Value) -> u32 {
    enum_val(v).unwrap_or(1)
}

fn image_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    match name {
        "Destroy" => {
            with(|s| {
                if let Some(i) = s.images.remove(&this) {
                    s.image_by_n.remove(&i.n);
                }
            });
            none()
        }
        "ReadPixelsBuffer" => {
            let pos = v2(&arg(args, 1)).ok_or_else(|| rt_err("ReadPixelsBuffer: Vector2 position expected"))?;
            let size = v2(&arg(args, 2)).ok_or_else(|| rt_err("ReadPixelsBuffer: Vector2 size expected"))?;
            let out = with(|s| {
                let img = s.images.get(&this).ok_or_else(|| rt_err("the EditableImage was destroyed"))?;
                let (x0, y0, w, h) = (pos.x as i64, pos.y as i64, size.x as i64, size.y as i64);
                if w < 1 || h < 1 || x0 < 0 || y0 < 0 || x0 + w > img.w as i64 || y0 + h > img.h as i64 {
                    return Err(rt_err("ReadPixelsBuffer: the region is outside the image"));
                }
                let mut out = Vec::with_capacity((w * h * 4) as usize);
                for y in y0..y0 + h {
                    let o = ((y * img.w as i64 + x0) * 4) as usize;
                    out.extend_from_slice(&img.px[o..o + (w * 4) as usize]);
                }
                Ok(out)
            })?;
            one(Value::Buffer(lua.create_buffer(out)?))
        }
        "WritePixelsBuffer" => {
            let pos = v2(&arg(args, 1)).ok_or_else(|| rt_err("WritePixelsBuffer: Vector2 position expected"))?;
            let size = v2(&arg(args, 2)).ok_or_else(|| rt_err("WritePixelsBuffer: Vector2 size expected"))?;
            let Value::Buffer(b) = arg(args, 3) else {
                return Err(rt_err("WritePixelsBuffer: buffer expected"));
            };
            let data = b.to_vec();
            edit_pixels(this, |iw, ih, px| {
                let (x0, y0, w, h) = (pos.x as i64, pos.y as i64, size.x as i64, size.y as i64);
                if w < 1 || h < 1 || x0 < 0 || y0 < 0 || x0 + w > iw as i64 || y0 + h > ih as i64 {
                    return Err(rt_err("WritePixelsBuffer: the region is outside the image"));
                }
                if data.len() < (w * h * 4) as usize {
                    return Err(rt_err("WritePixelsBuffer: the buffer is smaller than the region"));
                }
                for y in 0..h {
                    let o = (((y0 + y) * iw as i64 + x0) * 4) as usize;
                    let s = (y * w * 4) as usize;
                    px[o..o + (w * 4) as usize].copy_from_slice(&data[s..s + (w * 4) as usize]);
                }
                Ok(())
            })??;
            touched(lua, this)?;
            none()
        }
        "DrawRectangle" => {
            let pos = v2(&arg(args, 1)).unwrap_or(Vec2::ZERO);
            let size = v2(&arg(args, 2)).unwrap_or(Vec2::ZERO);
            let src = rgba_of(&arg(args, 3), &arg(args, 4));
            let mode = combine_arg(&arg(args, 5));
            edit_pixels(this, |w, h, px| {
                let x0 = (pos.x.round() as i64).clamp(0, w as i64);
                let y0 = (pos.y.round() as i64).clamp(0, h as i64);
                let x1 = ((pos.x + size.x).round() as i64).clamp(0, w as i64);
                let y1 = ((pos.y + size.y).round() as i64).clamp(0, h as i64);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let o = ((y * w as i64 + x) * 4) as usize;
                        blend(&mut px[o..o + 4], src, mode);
                    }
                }
            })?;
            touched(lua, this)?;
            none()
        }
        "DrawCircle" => {
            let c = v2(&arg(args, 1)).unwrap_or(Vec2::ZERO);
            let rad = num(&arg(args, 2)).unwrap_or(0.0) as f32;
            let src = rgba_of(&arg(args, 3), &arg(args, 4));
            let mode = combine_arg(&arg(args, 5));
            edit_pixels(this, |w, h, px| {
                let x0 = ((c.x - rad).floor() as i64).max(0);
                let y0 = ((c.y - rad).floor() as i64).max(0);
                let x1 = ((c.x + rad).ceil() as i64).min(w as i64 - 1);
                let y1 = ((c.y + rad).ceil() as i64).min(h as i64 - 1);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let d = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - c;
                        if d.length_squared() <= rad * rad {
                            let o = ((y * w as i64 + x) * 4) as usize;
                            blend(&mut px[o..o + 4], src, mode);
                        }
                    }
                }
            })?;
            touched(lua, this)?;
            none()
        }
        "DrawLine" => {
            let p0 = v2(&arg(args, 1)).unwrap_or(Vec2::ZERO);
            let p1 = v2(&arg(args, 2)).unwrap_or(Vec2::ZERO);
            let src = rgba_of(&arg(args, 3), &arg(args, 4));
            let mode = combine_arg(&arg(args, 5));
            edit_pixels(this, |w, h, px| {
                let steps = (p1 - p0).abs().max_element().ceil().max(1.0) as i64;
                let mut last = (-1i64, -1i64);
                for i in 0..=steps {
                    let p = p0 + (p1 - p0) * (i as f32 / steps as f32);
                    let (x, y) = (p.x.floor() as i64, p.y.floor() as i64);
                    if (x, y) == last || x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
                        continue;
                    }
                    last = (x, y);
                    let o = ((y * w as i64 + x) * 4) as usize;
                    blend(&mut px[o..o + 4], src, mode);
                }
            })?;
            touched(lua, this)?;
            none()
        }
        "DrawTriangle" => {
            let a = v2(&arg(args, 1)).unwrap_or(Vec2::ZERO);
            let b = v2(&arg(args, 2)).unwrap_or(Vec2::ZERO);
            let c = v2(&arg(args, 3)).unwrap_or(Vec2::ZERO);
            let src = rgba_of(&arg(args, 4), &arg(args, 5));
            let mode = combine_arg(&arg(args, 6));
            edit_pixels(this, |w, h, px| {
                let lo = a.min(b).min(c).floor().max(Vec2::ZERO);
                let hi = a.max(b).max(c).ceil().min(Vec2::new(w as f32, h as f32));
                let edge = |p: Vec2, q: Vec2, r: Vec2| (q - p).perp_dot(r - p);
                let area = edge(a, b, c);
                if area.abs() < 1e-6 {
                    return;
                }
                for y in lo.y as i64..hi.y as i64 {
                    for x in lo.x as i64..hi.x as i64 {
                        let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                        let (e0, e1, e2) = (edge(a, b, p), edge(b, c, p), edge(c, a, p));
                        if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                            let o = ((y * w as i64 + x) * 4) as usize;
                            blend(&mut px[o..o + 4], src, mode);
                        }
                    }
                }
            })?;
            touched(lua, this)?;
            none()
        }
        "DrawImageProjected" => project::draw_projected(lua, this, args),
        "SampleImageProjected" => project::sample_projected(lua, this, args),
        "DrawImage" | "DrawImageTransformed" => {
            // DrawImage(position, image, combineType);
            // DrawImageTransformed(position, scale, rotation, image, options).
            let (pos, scale, rot, img_v, mode, pivot) = if name == "DrawImage" {
                (v2(&arg(args, 1)).unwrap_or(Vec2::ZERO), Vec2::ONE, 0.0f32, arg(args, 2), combine_arg(&arg(args, 3)), None)
            } else {
                let opts = arg(args, 5);
                (
                    v2(&arg(args, 1)).unwrap_or(Vec2::ZERO),
                    v2(&arg(args, 2)).unwrap_or(Vec2::ONE),
                    num(&arg(args, 3)).unwrap_or(0.0) as f32,
                    arg(args, 4),
                    combine_arg(&field(&opts, "CombineType")),
                    Some(v2(&field(&opts, "PivotPoint"))),
                )
            };
            let src_r = opt_inst(&img_v).ok_or_else(|| rt_err(format!("{name}: EditableImage expected")))?;
            let (sw, sh, spx) = with(|s| s.images.get(&src_r).map(|i| (i.w, i.h, i.px.clone())))
                .ok_or_else(|| rt_err(format!("{name}: EditableImage expected")))?;
            edit_pixels(this, |w, h, px| {
                let src_px = |x: i64, y: i64| -> [f32; 4] {
                    let o = ((y * sw as i64 + x) * 4) as usize;
                    [spx[o] as f32 / 255.0, spx[o + 1] as f32 / 255.0, spx[o + 2] as f32 / 255.0, spx[o + 3] as f32 / 255.0]
                };
                match pivot {
                    None => {
                        for y in 0..sh as i64 {
                            for x in 0..sw as i64 {
                                let (dx, dy) = (pos.x as i64 + x, pos.y as i64 + y);
                                if dx < 0 || dy < 0 || dx >= w as i64 || dy >= h as i64 {
                                    continue;
                                }
                                let o = ((dy * w as i64 + dx) * 4) as usize;
                                blend(&mut px[o..o + 4], src_px(x, y), mode);
                            }
                        }
                    }
                    Some(pv) => {
                        // `pos` is where the source's pivot lands (centre by default).
                        let pv = pv.unwrap_or(Vec2::new(sw as f32, sh as f32) * 0.5);
                        let (sn, cs) = rot.to_radians().sin_cos();
                        let fwd = |p: Vec2| {
                            let q = (p - pv) * scale;
                            pos + Vec2::new(q.x * cs - q.y * sn, q.x * sn + q.y * cs)
                        };
                        let corners = [fwd(Vec2::ZERO), fwd(Vec2::new(sw as f32, 0.0)), fwd(Vec2::new(0.0, sh as f32)), fwd(Vec2::new(sw as f32, sh as f32))];
                        let lo = corners.iter().fold(Vec2::splat(f32::MAX), |a, c| a.min(*c)).floor().max(Vec2::ZERO);
                        let hi = corners.iter().fold(Vec2::splat(f32::MIN), |a, c| a.max(*c)).ceil().min(Vec2::new(w as f32, h as f32));
                        if scale.x.abs() < 1e-6 || scale.y.abs() < 1e-6 {
                            return;
                        }
                        for y in lo.y as i64..hi.y as i64 {
                            for x in lo.x as i64..hi.x as i64 {
                                let d = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - pos;
                                let q = Vec2::new(d.x * cs + d.y * sn, -d.x * sn + d.y * cs) / scale + pv;
                                let (sx, sy) = (q.x.floor() as i64, q.y.floor() as i64);
                                if sx < 0 || sy < 0 || sx >= sw as i64 || sy >= sh as i64 {
                                    continue;
                                }
                                let o = ((y * w as i64 + x) * 4) as usize;
                                blend(&mut px[o..o + 4], src_px(sx, sy), mode);
                            }
                        }
                    }
                }
            })?;
            touched(lua, this)?;
            none()
        }
        _ => none(),
    }
}

// ------------------------------------------------------------------ meshes

fn mesh_from_uri(uri: &str) -> mlua::Result<Mesh> {
    let bytes = rustudio_core::assets::generated(uri)
        .map(Ok)
        .unwrap_or_else(|| rustudio_core::assets::script_content_blocking(uri))
        .map_err(|e| rt_err(format!("CreateEditableMeshAsync: {e}")))?;
    if let Some(st) = rustudio_core::mesh::stored(&bytes) {
        let skin = if st.major >= 4 { rustudio_core::mesh::parse(&bytes).ok().and_then(|d| d.skin) } else { None };
        return Ok(Mesh::from_stored(&st, skin.as_ref()));
    }
    let d = rustudio_core::mesh::parse(&bytes).map_err(|e| rt_err(format!("CreateEditableMeshAsync: {e}")))?;
    Ok(Mesh::from_mesh_data(&d))
}

/// The mesh registered as generated content: a new id when the mesh
/// changed since its last snapshot, else that snapshot's (parts made from
/// one unchanged mesh share one live copy).
fn snapshot(r: Ref) -> Option<(String, Vec3, Vec3)> {
    let (bytes, lo, hi, id) = with(|s| {
        let mut m = lock(s.meshes.get(&r)?);
        if let Some((id, lo, hi)) = live_reuse(r, &m) {
            return Some((None, lo, hi, id));
        }
        m.gen += 1;
        let (b, lo, hi) = m.to_mesh_bytes();
        Some((Some(b), lo, hi, format!("{MESH_PREFIX}mesh/{}@{}", m.n, m.gen)))
    })?;
    if let Some(bytes) = bytes {
        rustudio_core::assets::register_generated(&id, bytes);
    }
    Some((id, lo, hi))
}

/// AssetService:CreateMeshPartAsync(Content.fromObject(editableMesh)):
/// the generated mesh id and the mesh's bounds size.
pub fn mesh_part_source(c: &Content) -> Option<mlua::Result<(String, Vec3)>> {
    let r = c.as_object()?;
    if !is_editable_mesh(r) {
        return Some(Err(rt_err("CreateMeshPartAsync: the object is not an EditableMesh")));
    }
    let (id, lo, hi) = snapshot(r)?;
    if hi.cmplt(lo).any() || lo == hi && lo == Vec3::ZERO && hi == Vec3::ZERO {
        return Some(Err(rt_err("CreateMeshPartAsync: the EditableMesh has no faces")));
    }
    register_live(r, &id, lo, hi);
    Some(Ok((id, (hi - lo).max(Vec3::splat(0.001)))))
}

/// MeshParts made from an EditableMesh (CreateMeshPartAsync): part →
/// (mesh, generated id). Keyed by part so a MeshContent read is one
/// lookup; a part leaves it when destroyed (`forget_parts`).
static LINKS: Mutex<Option<HashMap<Ref, (Ref, String)>>> = Mutex::new(None);

/// Remembers that `part` shows the EditableMesh behind `c` through the
/// generated mesh `id` (its CreateMeshPartAsync snapshot), which from now
/// on follows the mesh's edits (`sync_live`).
pub fn link(c: &Content, part: Ref, id: &str) {
    let Some(em) = c.as_object() else { return };
    LINKS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(part, (em, id.to_string()));
}

/// ApplyMesh: `to` now shows what `from` shows.
pub fn link_like(from: Ref, to: Ref) {
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = g.as_mut() else { return };
    match map.get(&from).cloned() {
        Some(l) => map.insert(to, l),
        None => map.remove(&to),
    };
}

/// Destroyed parts (and their descendants) leave the link table.
pub fn forget_parts(parts: impl IntoIterator<Item = Ref>) {
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = g.as_mut().filter(|m| !m.is_empty()) else { return };
    for p in parts {
        map.remove(&p);
    }
}

/// Number of linked parts (checks).
pub fn link_count() -> usize {
    LINKS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |m| m.len())
}

/// The EditableMesh a part made by CreateMeshPartAsync(Content.fromObject)
/// was made from: Studio's MeshPart.MeshContent reads Content.fromObject(that
/// mesh), even after the mesh is destroyed, and its MeshId "" (AUDIT.md (f);
/// they read Content.none and the generated id here).
pub fn linked_mesh(part: Ref) -> Option<Ref> {
    LINKS.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&part).map(|l| l.0)
}

/// Parts drawn with another mesh than their own (a WrapDeformer's result):
/// part → generated mesh id. Scripts still read the part's MeshId.
static SHOWN: Mutex<Option<HashMap<Ref, String>>> = Mutex::new(None);

/// The mesh a part is drawn with instead of its own, if any.
pub fn shown_mesh(part: Ref) -> Option<String> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&part).cloned()
}

/// Draws `part` with a snapshot of the EditableMesh `em` (mesh space,
/// padded about `centre`, the part's own mesh's bounds centre, so it keeps
/// the part's frame), or with its own mesh again (`em` None).
pub fn show_mesh(lua: &Lua, part: Ref, em: Option<Ref>, centre: Vec3) -> mlua::Result<()> {
    let id = match em {
        Some(em) => {
            let Some((bytes, id)) = with(|s| {
                let mut m = lock(s.meshes.get(&em)?);
                m.gen += 1;
                let (b, _, _) = m.to_mesh_bytes_about(Some(centre));
                Some((b, format!("{MESH_PREFIX}shown/{}@{}", m.n, m.gen)))
            }) else {
                return Ok(());
            };
            rustudio_core::assets::register_generated(&id, bytes);
            Some(id)
        }
        None => None,
    };
    {
        let mut g = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        match id {
            Some(id) => map.insert(part, id),
            None => map.remove(&part),
        };
    }
    let mut w = world(lua)?;
    w.dm().mark_prop(part, rbx_dom_weak::ustr("MeshId"));
    Ok(())
}

/// Whether the EditableMesh behind `c` has bones (its part is skinned).
pub fn content_skinned(c: &Content) -> bool {
    c.as_object().and_then(|r| with(|s| s.meshes.get(&r).map(|m| lock(m).skinned()))).unwrap_or(false)
}

fn mutates(name: &str) -> bool {
    ["Add", "Set", "Remove", "BatchAdd", "BatchSet", "BatchRemove", "Merge", "Triangulate", "Clear", "Reset"]
        .iter()
        .any(|p| name.starts_with(p))
}

// ------------------------------------------------------------------ live meshes
//
// Studio's MeshPart from CreateMeshPartAsync(Content.fromObject(em)) keeps
// drawing the EditableMesh: edits made after it (server or client, any
// Set* / Add* / Remove*) show from the next frame (AUDIT.md (f)). Here the
// snapshot's generated id stays the part's mesh and follows the mesh:
// - each mutating call records what it changed in the mesh's journal
//   (`em::note_edit`) and marks the mesh in CHANGED;
// - once a frame the renderer (`sync_live`, viewport.rs) takes the changed
//   meshes' journals: value edits (SetPosition, SetNormal, SetUV, SetColor,
//   BatchSetValues) rewrite only the corners that use those ids (and the
//   automatic normals of the triangles around moved vertices) in the GPU
//   buffer in place; layout edits (faces, ids per corner, bones) rebuild the
//   copy. Skinned meshes: colour / UV edits go into the renderer's copies
//   in place (`LivePatch::SkinAttrs`: the skin base, the shared rest mesh,
//   each posed part's slot, which the compute pass skins again); any other
//   edit re-snapshots their bytes under the same id and their parts are
//   redrawn (the skin path deforms from the bytes).
// The copy is normalised by the bounds at CreateMeshPartAsync, so a vertex
// moved past them draws there and the others don't shift (Studio).

/// One generated mesh id that follows an EditableMesh.
struct Live {
    em: Ref,
    /// The snapshot's bounds: the copy's normalisation (fixed).
    lo: Vec3,
    hi: Vec3,
    /// Mesh edit count when the snapshot was made (`live_reuse`).
    edits_at: u64,
    skinned: bool,
    /// The renderer's copy (non-skinned), None until loaded / after a
    /// layout change.
    copy: Option<Copy>,
    /// Parts the renderer draws with this id (redrawn on a rebuild).
    users: HashSet<Ref>,
    /// Skinned: colour / UV id → the corners (of `Mesh::corners`, which is
    /// the snapshot's vertex order) using it; None until the first
    /// value-only edit and after any rebuild.
    skin_ids: Option<SkinIds>,
    /// Skinned: the registered bytes are behind the mesh (colour / UV edits
    /// went straight to the renderer's copies, `LivePatch::SkinAttrs`).
    bytes_stale: bool,
}

struct SkinIds {
    by_uv: HashMap<u64, Vec<u32>>,
    by_color: HashMap<u64, Vec<u32>>,
    /// Each corner's colour id (a frame that rewrites most colours reads
    /// them all in corner order instead of id by id).
    corner_color: Vec<u64>,
    colors: usize,
}

fn build_skin_ids(m: &Mesh) -> SkinIds {
    let mut by_uv: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut by_color: HashMap<u64, Vec<u32>> = HashMap::new();
    let ids = m.corner_ids();
    for (i, id) in ids.iter().enumerate() {
        by_uv.entry(id[2]).or_default().push(i as u32);
        by_color.entry(id[3]).or_default().push(i as u32);
    }
    let colors = by_color.len();
    SkinIds { by_uv, by_color, corner_color: ids.iter().map(|id| id[3]).collect(), colors }
}

/// The renderer's copy of a live mesh: per corner the ids behind it, ids →
/// corners, and the vertices as uploaded.
struct Copy {
    ids: Vec<[u64; 4]>,
    by_vertex: HashMap<u64, Vec<u32>>,
    by_normal: HashMap<u64, Vec<u32>>,
    by_uv: HashMap<u64, Vec<u32>>,
    by_color: HashMap<u64, Vec<u32>>,
    /// Vertex colours drawn (not all white when built).
    tint: bool,
    /// Faces of more than 3 vertices: automatic normals by face.
    polys: bool,
    verts: Vec<rustudio_render::render::Vertex>,
}

#[derive(Default)]
struct LiveSet {
    by_id: HashMap<String, Live>,
    of_em: HashMap<Ref, Vec<String>>,
    /// Meshes with journal entries since the last `sync_live`.
    changed: HashSet<Ref>,
    /// Edit counter per mesh (bumped by every drawn change).
    edits: HashMap<Ref, u64>,
    /// Destroyed meshes with edits the renderer hasn't taken yet: their
    /// parts keep drawing the last state (Studio).
    dying: HashMap<Ref, SharedMesh>,
    /// Ids no drawn part uses any more: their copies are dropped (the
    /// renderer forgets them too), so later edits don't patch them.
    released: Vec<String>,
    /// Part → the live id it is drawn with.
    user_of: HashMap<Ref, String>,
}

static LIVE: Mutex<Option<LiveSet>> = Mutex::new(None);

fn live<R>(f: impl FnOnce(&mut LiveSet) -> R) -> R {
    let mut g = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(LiveSet::default))
}

/// An earlier snapshot id of `em` still equal to the mesh (no drawn edit
/// since): (id, lo, hi).
fn live_reuse(em: Ref, m: &Mesh) -> Option<(String, Vec3, Vec3)> {
    if !m.live || !m.journal.is_empty() {
        return None;
    }
    live(|l| {
        let now = l.edits.get(&em).copied().unwrap_or(0);
        let id = l.of_em.get(&em)?.iter().rev().find(|id| l.by_id.get(*id).is_some_and(|x| x.edits_at == now))?;
        let x = &l.by_id[id];
        Some((id.clone(), x.lo, x.hi))
    })
}

/// Makes generated mesh `id` (just snapshotted from `em`, bounds lo..hi)
/// follow the mesh.
fn register_live(em: Ref, id: &str, lo: Vec3, hi: Vec3) {
    let skinned = with(|s| {
        let m = s.meshes.get(&em)?;
        let mut m = lock(m);
        m.live = true;
        Some(m.skinned())
    });
    let Some(skinned) = skinned else { return };
    live(|l| {
        if l.by_id.contains_key(id) {
            return;
        }
        let edits_at = l.edits.get(&em).copied().unwrap_or(0);
        l.by_id.insert(id.to_string(), Live { em, lo, hi, edits_at, skinned, copy: None, users: HashSet::new(), skin_ids: None, bytes_stale: false });
        l.of_em.entry(em).or_default().push(id.to_string());
    });
}

/// Whether `id` is a live EditableMesh copy the renderer builds itself
/// (`live_build`) rather than from its bytes.
pub fn is_live(id: &str) -> bool {
    id.starts_with(MESH_PREFIX) && live(|l| l.by_id.get(id).is_some_and(|x| !x.skinned))
}

/// The renderer draws `part` with mesh `id`.
pub fn live_user(id: &str, part: Ref) {
    if !id.starts_with(MESH_PREFIX) {
        return;
    }
    live(|l| {
        if let Some(x) = l.by_id.get_mut(id) {
            if x.users.insert(part) {
                l.user_of.insert(part, id.to_string());
            }
        }
    });
}

/// The renderer stopped drawing these parts.
pub fn live_users_gone(parts: &[Ref]) {
    if parts.is_empty() {
        return;
    }
    live(|l| {
        if l.user_of.is_empty() {
            return;
        }
        for p in parts {
            let Some(id) = l.user_of.remove(p) else { continue };
            let Some(x) = l.by_id.get_mut(&id) else { continue };
            x.users.remove(p);
            if x.users.is_empty() {
                x.copy = None;
                l.released.push(id);
            }
        }
    });
}

/// (live ids, ids with users, total users, changed meshes): checks.
pub fn live_stats() -> (usize, usize, usize, usize) {
    live(|l| {
        let used = l.by_id.values().filter(|x| !x.users.is_empty()).count();
        let users = l.by_id.values().map(|x| x.users.len()).sum();
        (l.by_id.len(), used, users, l.changed.len())
    })
}

fn quant(x: f32) -> f32 {
    // (round half up of a value in 0..255: no libm call per vertex)
    ((x.clamp(0.0, 1.0) * 255.0 + 0.5) as u32) as f32 / 255.0
}

/// One corner as the GPU holds a loaded 2.00 mesh (assets.rs
/// `load_mesh_projected` of `to_mesh_bytes`): position in the unit box,
/// unit normal (the triangle's when none) × extent, UV, colour bytes.
fn gpu_vertex(
    p: Vec3, n: Option<Vec3>, flat: Vec3, uv: Vec2, c: ([f32; 3], f32), tint: bool, centre: Vec3, extent: Vec3,
) -> rustudio_render::render::Vertex {
    let face = flat.try_normalize().unwrap_or(Vec3::Y);
    let n = n.filter(|n| n.length_squared() > 1e-12).and_then(Vec3::try_normalize).unwrap_or(face);
    let color = if tint { [quant(c.0[0]), quant(c.0[1]), quant(c.0[2]), -1.0 - quant(c.1)] } else { [0.0; 4] };
    rustudio_render::render::Vertex {
        pos: ((p - centre) / extent).to_array(),
        normal: (n * extent).to_array(),
        uv: uv.to_array(),
        projection: extent.extend(0.0).to_array(),
        color,
    }
}

fn white(c: ([f32; 3], f32)) -> bool {
    c.0.iter().chain(std::iter::once(&c.1)).all(|x| quant(*x) >= 1.0)
}

fn norm_box(lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    ((lo + hi) * 0.5, (hi - lo).max(Vec3::splat(1e-4)))
}

fn build_copy(m: &Mesh, lo: Vec3, hi: Vec3) -> Copy {
    let corners = m.corners();
    let ids = m.corner_ids();
    let (centre, extent) = norm_box(lo, hi);
    let tint = corners.iter().any(|k| k.3.iter().any(|x| quant(*x) < 1.0));
    let mut by_vertex: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut by_normal: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut by_uv: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut by_color: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut verts = Vec::with_capacity(corners.len());
    for (i, (k, id)) in corners.iter().zip(&ids).enumerate() {
        let t = i / 3 * 3;
        let flat = (corners[t + 1].0 - corners[t].0).cross(corners[t + 2].0 - corners[t].0);
        verts.push(gpu_vertex(k.0, Some(k.1), flat, k.2, ([k.3[0], k.3[1], k.3[2]], k.3[3]), tint, centre, extent));
        let c = i as u32;
        by_vertex.entry(id[0]).or_default().push(c);
        by_normal.entry(id[1]).or_default().push(c);
        by_uv.entry(id[2]).or_default().push(c);
        by_color.entry(id[3]).or_default().push(c);
    }
    let polys = m.faces.iter().any(|(_, f)| f.v.len() > 3);
    Copy { ids, by_vertex, by_normal, by_uv, by_color, tint, polys, verts }
}

/// The renderer's first copy of live mesh `id`: (vertices, extent).
pub fn live_build(id: &str) -> Option<(Vec<rustudio_render::render::Vertex>, Vec3)> {
    let (em, lo, hi) = live(|l| l.by_id.get(id).map(|x| (x.em, x.lo, x.hi)))?;
    let m = with(|s| s.meshes.get(&em).cloned());
    let Some(m) = m else {
        // The mesh is gone: its last state is in the bytes (`drop_live`).
        return None;
    };
    let copy = build_copy(&lock(&m), lo, hi);
    let verts = copy.verts.clone();
    live(|l| {
        if let Some(x) = l.by_id.get_mut(id) {
            x.copy = Some(copy);
        }
    });
    Some((verts, norm_box(lo, hi).1))
}

/// What the renderer does with a live id this frame.
pub enum LivePatch {
    /// Write these runs of vertices (first corner, vertices) in place.
    Ranges(Vec<(u32, Vec<rustudio_render::render::Vertex>)>),
    /// Build it again (`live_build`, or its re-registered bytes when
    /// skinned) and redraw these parts.
    Rebuild { users: Vec<Ref>, skinned: bool },
    /// A skinned mesh whose colours / UVs changed and nothing else: these
    /// corners' new values (corner of the snapshot, RGBA / UV), written
    /// into the renderer's copies in place (the shared rest mesh, the skin
    /// base and each posed part's slot); no bytes, no reparse, no redraw of
    /// the parts. The renderer answers with `live_resnap` when it has no
    /// copy to patch.
    SkinAttrs { users: Vec<Ref>, colors: Vec<(u32, [f32; 4])>, uvs: Vec<(u32, [f32; 2])> },
}

/// The fallback of `LivePatch::SkinAttrs`: the skinned mesh's bytes brought
/// up to date under `id` (then rebuilt from them as before); its parts.
pub fn live_resnap(id: &str) -> Vec<Ref> {
    let Some((em, centre, users)) = live(|l| {
        let x = l.by_id.get_mut(id)?;
        x.skin_ids = None;
        x.bytes_stale = false;
        Some((x.em, (x.lo + x.hi) * 0.5, x.users.iter().copied().collect::<Vec<_>>()))
    }) else {
        return Vec::new();
    };
    if let Some(m) = with(|s| s.meshes.get(&em).cloned()) {
        let (b, _, _) = lock(&m).to_mesh_bytes_about(Some(centre));
        rustudio_core::assets::register_generated(id, b);
    }
    users
}

/// The corners of `copy` to rewrite for journal `j`, with the automatic
/// normals they need recomputed. None: rebuild.
fn patch_copy(m: &Mesh, copy: &mut Copy, j: &em::Journal, lo: Vec3, hi: Vec3) -> Option<Vec<(u32, u32)>> {
    let mut corners: Vec<u32> = Vec::new();
    let mut autos: HashSet<u64> = HashSet::new();
    let tri_of = |c: u32| c / 3 * 3;
    for v in &j.verts {
        let Some(cs) = copy.by_vertex.get(v) else { continue };
        for &c in cs {
            let t = tri_of(c);
            for k in t..t + 3 {
                corners.push(k);
                let n = copy.ids[k as usize][1];
                if m.normal_is_auto(n) {
                    autos.insert(n);
                }
            }
        }
    }
    for n in &j.normals {
        if let Some(cs) = copy.by_normal.get(n) {
            corners.extend(cs);
            if m.normal_is_auto(*n) {
                autos.insert(*n);
            }
        }
    }
    for u in &j.uvs {
        corners.extend(copy.by_uv.get(u).into_iter().flatten());
    }
    for c in &j.colors {
        if let Some(cs) = copy.by_color.get(c) {
            if !copy.tint && m.color(*c).is_some_and(|c| !white(c)) {
                return None; // colours appear: every corner's encoding changes
            }
            corners.extend(cs);
        }
    }
    if corners.is_empty() {
        return Some(Vec::new());
    }
    // Automatic normals around what moved, from the triangles that use them.
    let mut normal: HashMap<u64, Vec3> = HashMap::with_capacity(autos.len());
    for n in autos {
        let v = if copy.polys {
            m.normal_value(n).unwrap_or(Vec3::ZERO)
        } else {
            let mut s = Vec3::ZERO;
            for &c in copy.by_normal.get(&n).into_iter().flatten() {
                let t = tri_of(c) as usize;
                let p = [0, 1, 2].map(|k| m.pos(copy.ids[t + k][0]).unwrap_or(Vec3::ZERO));
                let i = c as usize - t;
                let fnm = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
                s += fnm * em::corner_weight(p[i], p[(i + 1) % 3], p[(i + 2) % 3]);
            }
            s.normalize_or_zero()
        };
        normal.insert(n, v);
    }
    let (centre, extent) = norm_box(lo, hi);
    let mut runs: Vec<(u32, u32)> = Vec::new();
    corners.sort_unstable();
    corners.dedup();
    for c in corners {
        let id = copy.ids[c as usize];
        let t = tri_of(c) as usize;
        let p = [0, 1, 2].map(|k| m.pos(copy.ids[t + k][0]).unwrap_or(Vec3::ZERO));
        let flat = (p[1] - p[0]).cross(p[2] - p[0]);
        let n = normal.get(&id[1]).copied().or_else(|| m.normal_set(id[1]));
        let uv = m.uv(id[2]).unwrap_or(Vec2::ZERO);
        let col = m.color(id[3]).unwrap_or(([1.0; 3], 1.0));
        let pos = m.pos(id[0]).unwrap_or(Vec3::ZERO);
        let flat_n = if n.is_some_and(|n| n.length_squared() > 1e-12) { flat } else { flat.normalize_or_zero() };
        copy.verts[c as usize] = gpu_vertex(pos, n, flat_n, uv, col, copy.tint, centre, extent);
        // Runs of corners, joined over short gaps (one write each).
        match runs.last_mut() {
            Some(r) if c <= r.1 + 8 => r.1 = c,
            _ => runs.push((c, c)),
        }
    }
    Some(runs)
}

/// Once a frame, from the renderer: the live ids whose meshes changed and
/// what to do with each (only ids of meshes edited since the last call are
/// looked at).
pub fn sync_live() -> Vec<(String, LivePatch)> {
    let (changed, dying, released): (Vec<Ref>, HashMap<Ref, SharedMesh>, Vec<String>) =
        live(|l| (l.changed.drain().collect(), std::mem::take(&mut l.dying), std::mem::take(&mut l.released)));
    let mut out: Vec<(String, LivePatch)> =
        released.into_iter().map(|id| (id, LivePatch::Rebuild { users: Vec::new(), skinned: false })).collect();
    for em in changed {
        let Some(m) = with(|s| s.meshes.get(&em).cloned()).or_else(|| dying.get(&em).cloned()) else { continue };
        let mut g = lock(&m);
        let j = std::mem::take(&mut g.journal);
        if j.is_empty() {
            continue;
        }
        let m: &Mesh = &g;
        let mut resnap: Vec<(String, Vec3)> = Vec::new();
        live(|l| {
            *l.edits.entry(em).or_default() += 1;
            let ids = l.of_em.get(&em).cloned().unwrap_or_default();
            for id in ids {
                let Some(x) = l.by_id.get_mut(&id) else { continue };
                if x.skinned && m.skinned() && !j.layout && j.verts.is_empty() && j.normals.is_empty() {
                    // Colours / UVs only: straight into the renderer's copies.
                    let ids = x.skin_ids.get_or_insert_with(|| build_skin_ids(m));
                    // (an id set several times since the last frame counts once)
                    let once = |l: &Vec<u64>| {
                        let mut l = l.clone();
                        l.sort_unstable();
                        l.dedup();
                        l
                    };
                    let mut colors = Vec::new();
                    let q = |v: ([f32; 3], f32)| [quant(v.0[0]), quant(v.0[1]), quant(v.0[2]), quant(v.1)];
                    if j.colors.len() * 2 >= ids.colors {
                        // Most of them: every corner, in order.
                        colors.reserve(ids.corner_color.len());
                        for (k, c) in ids.corner_color.iter().enumerate() {
                            if let Some(v) = m.color(*c) {
                                colors.push((k as u32, q(v)));
                            }
                        }
                    } else {
                        for c in &once(&j.colors) {
                            let Some(v) = m.color(*c) else { continue };
                            colors.extend(ids.by_color.get(c).into_iter().flatten().map(|k| (*k, q(v))));
                        }
                    }
                    let mut uvs = Vec::new();
                    for u in &once(&j.uvs) {
                        let Some(v) = m.uv(*u) else { continue };
                        uvs.extend(ids.by_uv.get(u).into_iter().flatten().map(|k| (*k, v.to_array())));
                    }
                    x.bytes_stale = true;
                    out.push((id, LivePatch::SkinAttrs { users: x.users.iter().copied().collect(), colors, uvs }));
                    continue;
                }
                if x.skinned != m.skinned() || x.skinned {
                    // Skinned (or became / stopped being): bytes again.
                    x.skinned = m.skinned();
                    x.copy = None;
                    x.skin_ids = None;
                    x.bytes_stale = false;
                    resnap.push((id.clone(), (x.lo + x.hi) * 0.5));
                    out.push((id, LivePatch::Rebuild { users: x.users.iter().copied().collect(), skinned: true }));
                    continue;
                }
                let Some(copy) = x.copy.as_mut() else { continue }; // not loaded: built when it is
                let res = if j.layout { None } else { patch_copy(m, copy, &j, x.lo, x.hi) };
                match res {
                    Some(runs) if runs.is_empty() => {}
                    Some(runs) => out.push((
                        id,
                        LivePatch::Ranges(
                            runs.into_iter().map(|(a, b)| (a, copy.verts[a as usize..=b as usize].to_vec())).collect(),
                        ),
                    )),
                    None => {
                        x.copy = None;
                        out.push((id, LivePatch::Rebuild { users: x.users.iter().copied().collect(), skinned: false }));
                    }
                }
            }
        });
        for (id, centre) in resnap {
            let (b, _, _) = m.to_mesh_bytes_about(Some(centre));
            rustudio_core::assets::register_generated(&id, b);
        }
    }
    for (em, m) in dying {
        drop_live(em, &lock(&m));
    }
    out
}

/// A destroyed EditableMesh: its parts keep drawing its last state
/// (Studio), so each live id's bytes are brought up to date and the live
/// bookkeeping goes.
fn drop_live(em: Ref, m: &Mesh) {
    let ids = live(|l| {
        let now = l.edits.remove(&em).unwrap_or(0);
        let ids = l.of_em.remove(&em).unwrap_or_default();
        let mut out = Vec::new();
        for id in ids {
            if let Some(x) = l.by_id.remove(&id) {
                for u in &x.users {
                    l.user_of.remove(u);
                }
                if x.edits_at != now || !m.journal.is_empty() || x.bytes_stale {
                    out.push((id, (x.lo + x.hi) * 0.5));
                }
            }
        }
        out
    });
    for (id, centre) in ids {
        let (b, _, _) = m.to_mesh_bytes_about(Some(centre));
        rustudio_core::assets::register_generated(&id, b);
    }
}

/// `em:SetColor(id, Color3)` straight from the namecall fast path
/// (runtime_routes.rs): a per-frame vertex painter calls it thousands of
/// times a frame. False: not an EditableMesh or not its colour id (the
/// normal path answers, with its errors).
pub fn set_color_fast(this: Ref, id: f64, rgb: [f32; 3]) -> bool {
    let Some(m) = with(|s| s.meshes.get(&this).cloned()) else { return false };
    let mut g = lock(&m);
    let was_empty = g.journal.is_empty();
    if !g.set_color_rgb(id, rgb) {
        return false;
    }
    if was_empty && !g.journal.is_empty() {
        drop(g);
        live(|l| l.changed.insert(this));
    }
    true
}

fn mesh_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    if name == "Destroy" {
        if let Some(m) = with(|s| s.meshes.remove(&this)) {
            let g = lock(&m);
            if g.live {
                if g.journal.is_empty() {
                    drop_live(this, &g);
                } else {
                    // The renderer takes the last edits first (`sync_live`).
                    drop(g);
                    live(|l| l.dying.insert(this, m.clone()));
                }
            }
        }
        return none();
    }
    let m = with(|s| s.meshes.get(&this).cloned()).ok_or_else(|| rt_err("the EditableMesh was destroyed"))?;
    let mut g = lock(&m);
    let r = em::call(lua, &mut g, name, args)?.unwrap_or_default();
    if g.live && mutates(name) {
        // (a journal with entries is already in `changed`: every taker of
        // the journal drains `changed` first, `sync_live`)
        let was_empty = g.journal.is_empty();
        em::note_edit(&mut g, name, args);
        if was_empty && !g.journal.is_empty() {
            drop(g);
            live(|l| l.changed.insert(this));
        }
    }
    Ok(r)
}

