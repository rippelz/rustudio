//! Plugin sources: a file (.lua / .luau / .rbxm / .rbxmx), an installed
//! Creator Store plugin's model, or an in-memory instance tree, read into
//! the model the loader copies under the Plugin instance.
//!
//! Studio names (measured, 0.747): a local plugin file `X.rbxmx` loads as
//! Plugin `user_X.rbxmx`, a bare `X.lua` as `user_X.lua` holding one Script
//! named "Script", a .lua in a subfolder as `user_Sub/Deeper/X.lua`;
//! installed plugins are `cloud_<asset id>` (Studio's log: "Running plugin
//! cloud_701506235"). A Plugins folder: `.lua` files
//! (any case) at any depth, `.rbxmx` / `.rbxm` at the top level only,
//! `.luau` not at all; loaded as all .lua sorted case-insensitively by
//! path, then the .rbxmx, then the .rbxm (Studio reads them in parallel and
//! its order varies; this is its deterministic run). `--plugin X.luau`
//! still loads a .luau file given by name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rbx_dom_weak::types::Ref;
use rbx_dom_weak::{InstanceBuilder, WeakDom};
use rustudio_core::core_scripts::Bytecode;

/// A decoded plugin model: its instance tree plus any ProtectedString Luau
/// bytecode extracted from `Source` properties (built-in plugins ship compiled
/// Luau bytecode v14).
#[derive(Clone)]
pub struct PluginModel {
    pub dom: Arc<WeakDom>,
    pub bytecode: Arc<HashMap<Ref, Bytecode>>,
}

/// Where a plugin comes from.
#[derive(Clone, Debug)]
pub enum PluginSource {
    /// A local plugin file in a Plugins folder (or given with --plugin).
    File(PathBuf),
    /// A .lua in a subfolder of a Plugins folder: `rel` is its path below
    /// the folder with '/' separators (Plugin.Name `user_<rel>`).
    Nested { path: PathBuf, rel: String },
    /// An installed (Creator Store) plugin: its model file on disk, as the
    /// store downloaded it.
    Cloud { asset_id: u64, path: PathBuf },
    /// A Roblox Studio built-in plugin from
    /// `<Studio>/BuiltInPlugins/Optimized_Embedded_Signature/<Name>.rbxm`.
    BuiltIn(PathBuf),
    /// An instance tree already in memory: the root's children are the
    /// plugin's top-level items. `name` is the Plugin's Name as Studio
    /// would give it (`user_Name.rbxmx`, …).
    Tree { name: String, asset_id: Option<u64>, dom: Arc<WeakDom> },
}

impl PluginSource {
    /// Whether this is a Studio built-in plugin (higher security identity 6,
    /// RobloxPluginGuiService widgets, ProtectedString bytecode sources).
    pub fn is_builtin(&self) -> bool {
        matches!(self, PluginSource::BuiltIn(_))
    }

    /// The Plugin instance's Name.
    pub fn plugin_name(&self) -> String {
        match self {
            PluginSource::File(p) => {
                format!("user_{}", p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
            }
            PluginSource::Nested { rel, .. } => format!("user_{rel}"),
            PluginSource::Cloud { asset_id, .. } => format!("cloud_{asset_id}"),
            PluginSource::BuiltIn(p) => {
                format!("builtin_{}", p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default())
            }
            PluginSource::Tree { name, .. } => name.clone(),
        }
    }

    /// printidentity's asset id: the store id, -1 for built-ins, -2 for local plugins.
    pub fn asset_id(&self) -> i64 {
        match self {
            PluginSource::Cloud { asset_id, .. } => *asset_id as i64,
            PluginSource::BuiltIn(_) => -1,
            PluginSource::Tree { asset_id: Some(a), .. } => *a as i64,
            _ => -2,
        }
    }

    /// The settings file the plugin's GetSetting / SetSetting use: local and
    /// built-in plugins share InstalledPlugins/0, installed ones have their own.
    pub fn settings_key(&self) -> String {
        match self {
            PluginSource::Cloud { asset_id, .. } => asset_id.to_string(),
            PluginSource::Tree { asset_id: Some(a), .. } => a.to_string(),
            _ => "0".to_string(),
        }
    }

    /// The file it was read from, if any.
    pub fn path(&self) -> Option<&Path> {
        match self {
            PluginSource::File(p)
            | PluginSource::Nested { path: p, .. }
            | PluginSource::Cloud { path: p, .. }
            | PluginSource::BuiltIn(p) => Some(p),
            PluginSource::Tree { .. } => None,
        }
    }

    /// Reads the plugin's model (again, for a reload).
    pub fn read(&self) -> Result<PluginModel, String> {
        match self {
            PluginSource::BuiltIn(p) => read_builtin_file(p),
            PluginSource::File(p) | PluginSource::Nested { path: p, .. } | PluginSource::Cloud { path: p, .. } => {
                read_file(p).map(|dom| PluginModel { dom: Arc::new(dom), bytecode: Arc::new(HashMap::new()) })
            }
            PluginSource::Tree { dom, .. } => Ok(PluginModel { dom: dom.clone(), bytecode: Arc::new(HashMap::new()) }),
        }
    }
}

/// Decodes a Studio built-in plugin `.rbxm` (ProtectedString Luau bytecode
/// sources, embedded HASH / SIGN signature chunks stripped).
pub fn read_builtin_file(path: &Path) -> Result<PluginModel, String> {
    let patch = rustudio_core::core_scripts::read(path)?;
    Ok(PluginModel {
        dom: Arc::new(patch.dom),
        bytecode: Arc::new(patch.bytecode),
    })
}

/// Built-in `.rbxm` files in `dir`, sorted case-insensitively by filename.
/// When `filter` is `Some`, only entries whose stem or filename matches
/// (case-insensitively) are included.
pub fn builtin_dir_order(dir: &Path, filter: Option<&[String]>) -> Result<Vec<PluginSource>, String> {
    if !dir.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let is_rbxm = p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case("rbxm"));
        if !is_rbxm {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(wanted) = filter {
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let hit = wanted.iter().any(|w| {
                w.eq_ignore_ascii_case(&name)
                    || w.eq_ignore_ascii_case(&stem)
                    || w.strip_prefix("builtin_").is_some_and(|s| s.eq_ignore_ascii_case(&name) || s.eq_ignore_ascii_case(&stem))
            });
            if !hit {
                continue;
            }
        }
        files.push((name, p));
    }
    files.sort_by_key(|(n, _)| n.to_lowercase());
    Ok(files.into_iter().map(|(_, p)| PluginSource::BuiltIn(p)).collect())
}

/// A plugin file as a model: a script file becomes one Script named
/// "Script" (measured), a model file its top-level items.
pub fn read_file(path: &Path) -> Result<WeakDom, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match ext.as_str() {
        "lua" | "luau" => {
            let src = String::from_utf8_lossy(&bytes).into_owned();
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let root = dom.root_ref();
            dom.insert(root, InstanceBuilder::new("Script").with_name("Script").with_property("Source", src));
            Ok(dom)
        }
        "rbxm" | "rbxmx" => rustudio_core::doc::Document::decode(&bytes).map_err(|e| format!("{}: {e}", path.display())),
        _ => Err(format!("{}: not a plugin file (.lua, .luau, .rbxm or .rbxmx)", path.display())),
    }
}

/// Whether Studio would load this file as a local plugin.
pub fn is_plugin_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("lua" | "luau" | "rbxm" | "rbxmx")
    )
}

/// The plugins of a Plugins folder in Studio's load order (see the header).
pub fn dir_order(dir: &Path) -> Result<Vec<PluginSource>, String> {
    let ext = |p: &Path| p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    let mut lua: Vec<(String, PathBuf)> = Vec::new();
    let mut models: Vec<(u8, String, PathBuf)> = Vec::new();
    fn walk(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if p.is_dir() {
                if depth < 32 {
                    walk(&p, &r, depth + 1, out);
                }
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lua")) {
                out.push((r, p));
            }
        }
    }
    if !dir.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    walk(dir, "", 0, &mut lua);
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        match ext(&p).as_str() {
            "rbxmx" => models.push((0, name, p)),
            "rbxm" => models.push((1, name, p)),
            _ => {}
        }
    }
    lua.sort_by_key(|(r, _)| r.to_lowercase());
    models.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let mut out: Vec<PluginSource> = lua
        .into_iter()
        .map(|(rel, path)| if rel.contains('/') { PluginSource::Nested { path, rel } } else { PluginSource::File(path) })
        .collect();
    out.extend(models.into_iter().map(|(_, _, p)| PluginSource::File(p)));
    Ok(out)
}
