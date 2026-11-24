//! StudioService members plugins call (GetClassIcon, PromptImportFile(s),
//! GizmoRaycast), the File objects PromptImportFile returns, and the
//! dialog-backed Plugin / PluginManager calls (PromptForExistingAssetId,
//! PromptSaveSelection, ImportFbxRig / ImportFbxAnimation, ExportPlace /
//! ExportSelection). StudioService's properties are testing_studio.rs's.
//!
//! Measured in Studio 0.747 (checks/plugins/services/captures/
//! history2.txt `icon.*`, misc.txt `icon.*`, dialogs.txt):
//! - GetClassIcon(name) → {Image = "rbxasset://studio_svg_textures/Shared/
//!   InsertableObjects/<Theme>/Standard/<Class>.png", ImageRectOffset =
//!   0,0, ImageRectSize = 16,16} for the 330 classes Studio ships an icon
//!   for (studio_class_icons.txt, the file names in Studio's content); any
//!   other class (BasePart, Plugin, FlagStand…: no fallback to a
//!   superclass) → the Placeholder icon; not a class → "GetClassIcon: X is
//!   not a Roblox class."
//! - PromptImportFile(s): a native Open dialog; cancel → nil / {}.
//! - ImportFbxRig / ImportFbxAnimation: Studio's "Open 3D File (FBX, OBJ,
//!   GLB, GLTF)" dialog; cancel → error "No file selected". A picked file's
//!   animation becomes a KeyframeSequence in Workspace (api_fbx_anim.rs).
//! - PromptForExistingAssetId(type): the asset picker; cancel → 0; a type
//!   that isn't an AssetType → "Requested asset type is not supported."
//! - PromptSaveSelection(name): a Save As dialog, even with nothing
//!   selected; true when cancelled too.
//! - PluginManager():ExportPlace / ExportSelection: open their "Export
//!   Place" / "Export Selection" save dialog and return at once.
//! Every dialog is a `prompts.rs` prompt: the editor shows it, the harness
//! answers it (`--action plugin-prompt:<answer>@S`).

use std::collections::HashMap;
use std::path::PathBuf;

use mlua::{Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::InstanceBuilder;

use super::*;
use crate::plugin_host::prompts::{self, Answer, PromptKind};

const ICONS: &str = include_str!("studio_class_icons.txt");

fn has_icon(class: &str) -> bool {
    use std::sync::OnceLock;
    static SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| ICONS.lines().map(str::trim).filter(|l| !l.is_empty()).collect()).contains(class)
}

fn is_class(name: &str) -> bool {
    rustudio_core::doc::db().classes.contains_key(name) || rustudio_core::api::get().is_some_and(|a| a.classes.contains_key(name))
}

/// Files PromptImportFile handed out: File instance → its path.
#[derive(Default)]
struct Files(HashMap<Ref, PathBuf>);

fn files(lua: &Lua) -> mlua::AppDataRefMut<'_, Files> {
    if lua.app_data_ref::<Files>().is_none() {
        lua.set_app_data(Files::default());
    }
    lua.app_data_mut::<Files>().expect("files")
}

fn vector2(lua: &Lua, x: f32, y: f32) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::new(x, y)))?))
}

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match (class, name) {
        ("StudioService", "GetClassIcon") => (|| {
            let c = arg_string(args, 1)?;
            if !is_class(&c) {
                return Err(rt_err(format!("GetClassIcon: {c} is not a Roblox class.")));
            }
            let theme = super::theme::theme_name();
            let image = if has_icon(&c) {
                format!("rbxasset://studio_svg_textures/Shared/InsertableObjects/{theme}/Standard/{c}.png")
            } else {
                format!("rbxasset://studio_svg_textures/Shared/Placeholder/{theme}/Standard/Placeholder.png")
            };
            let t = lua.create_table()?;
            t.set("Image", image)?;
            t.set("ImageRectOffset", vector2(lua, 0.0, 0.0)?)?;
            t.set("ImageRectSize", vector2(lua, 16.0, 16.0)?)?;
            ret1(Value::Table(t))
        })(),
        // Studio's gizmo raycast hits its own handles / adornments; rustudio
        // draws plugin adornments in the viewport without pickable gizmo
        // geometry, so nothing is hit (Studio with no gizmos up: nil).
        ("StudioService", "GizmoRaycast") => (|| {
            require_args(args, 2)?;
            ret1(Value::Nil)
        })(),
        ("File", "GetBinaryContents") => (|| {
            let path = files(lua).0.get(&this).cloned().ok_or_else(|| rt_err("File is no longer available"))?;
            let bytes = std::fs::read(&path).map_err(|e| rt_err(format!("Could not read {}: {e}", path.display())))?;
            ret1(Value::String(lua.create_string(&bytes)?))
        })(),
        ("File", "GetTemporaryId") => (|| {
            let path = files(lua).0.get(&this).cloned().ok_or_else(|| rt_err("File is no longer available"))?;
            // The file's bytes are served under the temporary id (images
            // and meshes a plugin previews with it).
            let id = format!("rbxtemp://{}", this_id(this));
            if let Ok(b) = std::fs::read(&path) {
                rustudio_core::assets::register_generated(&id, b);
            }
            ret1(lstr(lua, &id)?)
        })(),
        // A new, parentless Plugin object (Studio: ClassName Plugin, Name
        // "Instance", not the calling plugin; captures/misc2.txt).
        ("PluginManagerInterface" | "PluginManager", "CreatePlugin") => (|| {
            let r = world(lua)?.dm().dom.insert(Ref::none(), InstanceBuilder::new("Plugin").with_name("Instance"));
            ret1(inst_value(lua, r)?)
        })(),
        // Studio returns at once and opens its Asset Configuration window
        // for the selection (nothing with an empty selection).
        ("Plugin", "SaveSelectedToRoblox") => (|| {
            let first = {
                let w = world(lua)?;
                let d = w.d();
                let root = d.dom.root_ref();
                d.selection.iter().copied().find(|r| d.is_ancestor(root, *r)).and_then(|r| d.dom.get_by_ref(r).map(|i| i.name.clone()))
            };
            if let Some(name) = first {
                prompts::open(PromptKind::SaveToRoblox { name });
            }
            ret0()
        })(),
        ("PluginManagerInterface" | "PluginManager", "ExportPlace" | "ExportSelection") => (|| {
            prompts::open(PromptKind::Export { selection: name == "ExportSelection" });
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}

/// PluginManager(): the DataModel's PluginManagerInterface (made on first
/// use, parentless, as Studio's: tostring and ClassName
/// "PluginManagerInterface").
pub fn plugin_manager(lua: &Lua) -> mlua::Result<Value> {
    #[derive(Default)]
    struct Pm(Option<Ref>);
    let have = lua.app_data_ref::<Pm>().and_then(|p| p.0);
    if let Some(r) = have.filter(|r| world(lua).is_ok_and(|w| w.d().exists(*r))) {
        return inst_value(lua, r);
    }
    let r = world(lua)?
        .dm()
        .dom
        .insert(Ref::none(), InstanceBuilder::new("PluginManagerInterface").with_name("PluginManagerInterface"));
    lua.set_app_data(Pm(Some(r)));
    inst_value(lua, r)
}

fn this_id(r: Ref) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.hash(&mut h);
    h.finish() & 0xffff_ffff
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    if class != "File" {
        return Ok(None);
    }
    Ok(match key {
        "Size" => {
            let path = files(lua).0.get(&r).cloned();
            let n = path.and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
            Some(Value::Number(n as f64))
        }
        _ => None,
    })
}

/// A File instance for a picked path (Name = the file name, as Studio's).
fn make_file(lua: &Lua, path: PathBuf) -> mlua::Result<Value> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let r = world(lua)?.dm().dom.insert(Ref::none(), InstanceBuilder::new("File").with_name(name));
    files(lua).0.insert(r, path);
    inst_value(lua, r)
}

fn filters(v: &Value) -> mlua::Result<Vec<String>> {
    match v {
        Value::Nil => Ok(Vec::new()),
        Value::Table(t) => {
            let mut out = Vec::new();
            for x in t.sequence_values::<Value>() {
                match x? {
                    Value::String(s) => out.push(s.to_string_lossy().trim_start_matches("*.").trim_start_matches('.').to_string()),
                    v => return Err(rt_err(format!("Unable to cast {} to string", v.type_name()))),
                }
            }
            Ok(out)
        }
        v => Err(rt_err(format!("Unable to cast {} to Array", v.type_name()))),
    }
}

/// `__svc_prompt_open(kind, ...)` → prompt id, or nil and the call's
/// immediate error (raised by the Luau side without a position, as Studio's).
fn prompt_open(lua: &Lua, (kind, a, b): (String, Value, Value)) -> mlua::Result<(Option<u64>, Option<String>)> {
    match prompt_open_inner(lua, (kind, a, b)) {
        Ok(id) => Ok((Some(id), None)),
        Err(e) => Ok((None, Some(rustudio_engine::runtime::describe_error(&e)))),
    }
}

fn prompt_open_inner(_lua: &Lua, (kind, a, b): (String, Value, Value)) -> mlua::Result<u64> {
    let k = match kind.as_str() {
        "ImportFile" | "ImportFiles" => PromptKind::ImportFile { filters: filters(&a)?, multi: kind == "ImportFiles" },
        "FbxRig" => PromptKind::Import3d { rig: true },
        "FbxAnimation" => {
            if opt_inst(&a).is_none() {
                return Err(rt_err("Argument 1 missing or nil"));
            }
            PromptKind::Import3d { rig: false }
        }
        "AssetId" => {
            let t = match &a {
                Value::Nil => return Err(rt_err("Argument 1 missing or nil")),
                Value::String(s) => s.to_string_lossy(),
                v => return Err(rt_err(format!("Unable to cast {} to string", v.type_name()))),
            };
            if rustudio_engine::ltypes::LEnumItem::from_name("AssetType", &t).is_none() {
                return Err(rt_err("Requested asset type is not supported."));
            }
            PromptKind::ExistingAssetId { asset_type: t }
        }
        "SaveSelection" => PromptKind::SaveSelection {
            name: match &a {
                Value::String(s) => s.to_string_lossy(),
                _ => String::new(),
            },
        },
        _ => return Err(rt_err("unknown prompt")),
    };
    let _ = b;
    Ok(prompts::open(k))
}

/// `__svc_prompt_poll(id, kind, a, b)` → (done, results…): the call's
/// return values once the dialog closed (`a`, `b`: the call's arguments).
fn prompt_poll(lua: &Lua, (id, kind, a_arg, extra): (u64, String, Value, Value)) -> mlua::Result<MultiValue> {
    let Some(a) = prompts::poll(id) else { return Ok(MultiValue::from_iter([Value::Boolean(false)])) };
    let done = Value::Boolean(true);
    let paths = match &a {
        Answer::Paths(p) => p.clone(),
        Answer::Text(t) => vec![PathBuf::from(t)],
        Answer::Cancel => Vec::new(),
    };
    let out: Vec<Value> = match kind.as_str() {
        "ImportFile" => match paths.into_iter().next() {
            Some(p) => vec![make_file(lua, p)?],
            None => vec![Value::Nil],
        },
        "ImportFiles" => {
            let t = lua.create_table()?;
            for (i, p) in paths.into_iter().enumerate() {
                t.raw_set(i + 1, make_file(lua, p)?)?;
            }
            vec![Value::Table(t)]
        }
        "FbxRig" | "FbxAnimation" => {
            let Some(p) = paths.into_iter().next() else {
                return Ok(MultiValue::from_iter([done, Value::Nil, lstr(lua, "No file selected")?]));
            };
            let res = if kind == "FbxRig" { import_3d(lua, &p, true, &extra) } else { import_anim(lua, &p, &a_arg, &extra) };
            match res {
                Ok(v) => vec![v],
                Err(e) => return Ok(MultiValue::from_iter([done, Value::Nil, lstr(lua, &e)?])),
            }
        }
        "AssetId" => {
            let n = match &a {
                Answer::Text(t) => t.trim().parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };
            vec![Value::Number(n)]
        }
        "SaveSelection" => {
            if let Some(p) = paths.into_iter().next() {
                let sel: Vec<Ref> = world(lua)?.d().selection.clone();
                if let Err(e) = world(lua)?.d().save_selection_to(&sel, &p) {
                    world(lua)?.output.push((rustudio_core::log::Level::Error, format!("Save failed: {e}")));
                }
            }
            vec![Value::Boolean(true)]
        }
        _ => vec![Value::Nil],
    };
    let mut mv = MultiValue::from_iter([done, Value::Boolean(false)]);
    for v in out {
        mv.push_back(v);
    }
    Ok(mv)
}

/// ImportFbxAnimation: the file's animation as a KeyframeSequence in
/// Workspace (crates/rustudio-core/src/fbx_anim.rs, api_fbx_anim.rs). After the pick, a rig
/// that isn't a Model is refused (Studio, measured with a Part); any file
/// Studio can't read gives "Import failed".
fn import_anim(lua: &Lua, path: &std::path::Path, rig: &Value, is_r15: &Value) -> Result<Value, String> {
    let rig = opt_inst(rig).ok_or("Argument 1 missing or nil")?;
    let is_model = world(lua).map_err(|e| e.to_string())?.dm().dom.get_by_ref(rig).is_some_and(|i| i.class == "Model");
    if !is_model {
        return Err("A Model rig must be passed as the first argument".into());
    }
    let r15 = !matches!(is_r15, Value::Boolean(false));
    let bytes = std::fs::read(path).map_err(|_| rustudio_core::fbx_anim::IMPORT_FAILED.to_string())?;
    let clip = rustudio_core::fbx_anim::read(&bytes, r15)?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let r = super::fbx_anim::build(lua, &clip, &stem, r15)?;
    let v = inst_value(lua, r).map_err(|e| e.to_string())?;
    // Into Workspace the way a plugin script's own write goes (no waypoint).
    let parent: mlua::Function = lua
        .load("local s = ... s.Parent = workspace")
        .set_name("=ImportFbxAnimation")
        .into_function()
        .map_err(|e| e.to_string())?;
    parent.call::<()>(v.clone()).map_err(|e| e.to_string())?;
    Ok(v)
}

/// ImportFbxRig: the file's meshes as a parentless Model (rustudio's FBX
/// importer, crates/rustudio-core/src/fbx.rs).
fn import_3d(lua: &Lua, path: &std::path::Path, rig: bool, _extra: &Value) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    if !rustudio_core::fbx::is_fbx(&bytes) {
        return Err("Unsupported file format".into());
    }
    if !rig {
        return Err("rustudio: ImportFbxRig reads rigs only".into());
    }
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish() >> 8
    };
    let r = rustudio_engine::runtime::ext::load_fbx(lua, id, &bytes).map_err(|e| e.to_string())?;
    if let Ok(mut w) = world(lua) {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
            i.name = name;
        }
    }
    inst_value(lua, r).map_err(|e| e.to_string())
}

pub const LUA_CODE: &str = r#"
local open, poll = ...
local M = __rs_lua_methods
local function ask(kind, a, b)
	local id, err = open(kind, a, b)
	if not id then error(err, 0) end
	while true do
		local r = table.pack(poll(id, kind, a, b))
		if r[1] then
			if r[2] == nil and r[3] ~= nil and r.n == 3 then error(r[3], 0) end
			return table.unpack(r, 3, r.n)
		end
		task.wait()
	end
end
local function pick(kind)
	return function(self, types) return ask(kind, types) end
end
M.PromptImportFile = pick("ImportFile")
M.PromptImportFileAsync = M.PromptImportFile
M.PromptImportFiles = pick("ImportFiles")
M.PromptImportFilesAsync = M.PromptImportFiles
M.ImportFbxRig = function(self, isR15) return ask("FbxRig", nil, isR15) end
M.ImportFbxRigAsync = M.ImportFbxRig
M.ImportFbxAnimation = function(self, rig, isR15) return ask("FbxAnimation", rig, isR15) end
M.ImportFbxAnimationAsync = M.ImportFbxAnimation
M.PromptForExistingAssetId = function(self, assetType) return ask("AssetId", assetType) end
M.PromptForExistingAssetIdAsync = M.PromptForExistingAssetId
M.PromptSaveSelection = function(self, name) return ask("SaveSelection", name) end
M.PromptSaveSelectionAsync = M.PromptSaveSelection
"#;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let open = lua.create_function(prompt_open)?;
    let poll = lua.create_function(prompt_poll)?;
    lua.load(LUA_CODE).set_name("=rustudio.plugin.prompts").call::<()>((open, poll))
}
