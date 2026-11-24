//! rustudio's plugin store: the local Plugins folder and the installed
//! (Creator Store) plugins on disk, laid out like Studio's own folders so a
//! Studio install can be copied in as is.
//!
//! Studio (0.747, Windows layout, seen in the owner's Wine prefix and its
//! logs):
//!
//! * local plugins: `%LOCALAPPDATA%\Roblox\Plugins` (settings().Studio.PluginsDir);
//!   every `.lua` (any depth) and top-level `.rbxmx` / `.rbxm` file is one
//!   plugin, id `user_<path from the folder>` (`user_TerrainPainterPlus.rbxm`,
//!   `user_Sub/tool.lua`); `.luau` files are not plugins;
//! * installed plugins: `%LOCALAPPDATA%\Roblox\<userId>\InstalledPlugins\<assetId>\<assetVersionId>\Plugin.rbxm`
//!   (older versions stay beside the current one), id `cloud_<assetId>`;
//!   the account's list, enabled flags and versions come from Roblox
//!   (studio-user-data-api/plugin-metadata) and rustudio keeps its copy in
//!   `<userId>/InstalledPlugins/PluginMetadata.json`;
//! * plugin settings (plugin:SetSetting): `InstalledPlugins\<assetId>\settings.json`
//!   per installed plugin, `InstalledPlugins\0\settings.json` shared by the
//!   local ones; signed out, `<userId>` is `0`;
//! * load order: local plugins first (`.lua` by path, then `.rbxmx`, then
//!   `.rbxm` run 2), then the installed ones (log
//!   "loadPlugin … / Running plugin …"); plugins load when a place opens
//!   (F3), not at the Start Page.
//!
//! rustudio's root is `<data>/rustudio` (crates/rustudio-core/src/paths.rs): `Plugins/` (unless
//! Studio Settings > Plugins Dir points elsewhere) and `<userId>/InstalledPlugins/`.

pub mod cli;
pub mod import;
pub mod manager;
pub mod permissions;
pub mod runtime;
pub mod ui;
pub mod watcher;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File types Studio loads from the Plugins folder (measured in Studio:
/// `.lua` in any case at any depth; `.rbxmx` / `.rbxm` at the top level
/// only; `.luau` is not loaded).
pub const PLUGIN_EXTS: &[&str] = &["lua", "rbxmx", "rbxm"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginKind {
    /// A file in the local Plugins folder.
    Local,
    /// A Creator Store plugin the account installed.
    Installed,
}

/// One plugin as the store sees it, handed to the runtime to load.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSource {
    /// Studio's plugin id: `user_<file>` or `cloud_<assetId>`.
    pub id: String,
    pub name: String,
    pub kind: PluginKind,
    /// The plugin file (`.rbxm` / `.rbxmx` / `.lua`). For an
    /// installed plugin not downloaded yet, where it will be.
    pub path: PathBuf,
    pub enabled: bool,
    pub asset_id: Option<u64>,
    /// Installed: the asset version id on disk.
    pub version: Option<u64>,
    /// Installed: the plugin's newest version id (0: not known yet).
    pub latest_version: Option<u64>,
    pub creator: Option<String>,
    /// Installed: when the asset was last updated (ISO date).
    pub updated: Option<String>,
    pub moderated: bool,
    pub auto_update: bool,
    /// The file is there (installed plugins download on demand).
    pub present: bool,
    /// Where `plugin:GetSetting` / `SetSetting` values live.
    pub settings_path: PathBuf,
}

impl PluginSource {
    pub fn has_update(&self) -> bool {
        matches!((self.version, self.latest_version), (Some(v), Some(l)) if l != 0 && v != l)
    }
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            PluginKind::Local => "local",
            PluginKind::Installed => "installed",
        }
    }
}

/// One installed plugin in PluginMetadata.json: the fields of Roblox's
/// plugin-metadata rows plus what Manage Plugins shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMeta {
    pub plugin_asset_id: u64,
    #[serde(default)]
    pub asset_version_id: u64,
    #[serde(default)]
    pub latest_version: u64,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub moderated: bool,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default)]
    pub auto_update_enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creator_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Installed by rustudio itself (not on the account's Roblox list), so a
    /// sync with Roblox keeps it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_only: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalMeta {
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// PluginMetadata.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    #[serde(default)]
    pub plugin_metadata: Vec<InstalledMeta>,
    /// Local plugins by id (`user_…`); missing = enabled.
    #[serde(default)]
    pub local: BTreeMap<String, LocalMeta>,
    /// Installed plugins uninstalled in rustudio: the account still lists
    /// them (rustudio doesn't change the account's list), so a sync must
    /// not bring them back. Installing again clears the entry.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub uninstalled: std::collections::BTreeSet<u64>,
}

/// The store: folders and the metadata file.
#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    pub plugins_dir: PathBuf,
    pub user_id: u64,
    pub meta: Metadata,
}

/// rustudio's data root (`<data>/rustudio`), or `RUSTUDIO_PLUGIN_ROOT`.
pub fn default_root() -> PathBuf {
    std::env::var_os("RUSTUDIO_PLUGIN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| rustudio_core::paths::data().join("rustudio"))
}

/// The local Plugins folder: Studio Settings > Plugins Dir (default
/// `<root>/Plugins`), or `<RUSTUDIO_PLUGIN_ROOT>/Plugins`.
pub fn default_plugins_dir() -> PathBuf {
    if let Some(r) = std::env::var_os("RUSTUDIO_PLUGIN_ROOT") {
        return PathBuf::from(r).join("Plugins");
    }
    match crate::studio_settings::get("Studio", "PluginsDir") {
        Some(crate::studio_settings::Val::Str(s)) if !s.is_empty() => PathBuf::from(s),
        _ => default_root().join("Plugins"),
    }
}

/// The signed-in account for plugin folders without touching the network:
/// the cached profile of the keyring's session, else 0 (signed out).
/// `RUSTUDIO_PLUGIN_USER` overrides.
pub fn current_user_id() -> u64 {
    if let Some(u) = std::env::var("RUSTUDIO_PLUGIN_USER").ok().and_then(|v| v.parse().ok()) {
        return u;
    }
    rustudio_account::Client::cached_profile().map(|c| c.profile.id).unwrap_or(0)
}

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A file Studio would load as a plugin, `top` = directly in the Plugins
/// folder (model files load only from there).
pub fn is_plugin_file_at(p: &Path, top: bool) -> bool {
    let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
    !hidden && match ext_of(p).as_str() {
        "lua" => true,
        "rbxm" | "rbxmx" => top,
        _ => false,
    }
}

pub fn is_plugin_file(p: &Path) -> bool {
    is_plugin_file_at(p, true)
}

/// Every plugin file under `dir` in Studio's (deterministic) load order
/// (measured in Studio, run 2): every `.lua` at any depth sorted by path
/// case-insensitively, then the top-level `.rbxmx`, then the top-level `.rbxm`.
pub fn scan_local(dir: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            match std::fs::metadata(&p) {
                Ok(m) if m.is_dir() && !hidden && depth < 16 => walk(&p, out, depth + 1),
                Ok(m) if m.is_file() && is_plugin_file_at(&p, depth == 0) => out.push(p),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out, 0);
    let rank = |p: &PathBuf| match ext_of(p).as_str() {
        "lua" => 0,
        "rbxmx" => 1,
        _ => 2,
    };
    out.sort_by_key(|p| (rank(p), p.strip_prefix(dir).unwrap_or(p).to_string_lossy().to_lowercase()));
    out
}

/// `user_<path relative to the Plugins folder>` with `/` separators.
pub fn local_id(dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(dir).unwrap_or(file);
    let s = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/");
    format!("user_{s}")
}

pub fn cloud_id(asset_id: u64) -> String {
    format!("cloud_{asset_id}")
}

/// A plugin's display name from its file: the name without extension.
fn file_stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

impl Store {
    /// The store for this run's account at the default folders.
    pub fn open_default() -> Store {
        Store::open(default_root(), default_plugins_dir(), current_user_id())
    }

    pub fn open(root: PathBuf, plugins_dir: PathBuf, user_id: u64) -> Store {
        let mut s = Store { root, plugins_dir, user_id, meta: Metadata::default() };
        s.meta = std::fs::read(s.metadata_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        s
    }

    pub fn installed_dir(&self) -> PathBuf {
        self.root.join(self.user_id.to_string()).join("InstalledPlugins")
    }

    pub fn metadata_path(&self) -> PathBuf {
        self.installed_dir().join("PluginMetadata.json")
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = self.metadata_path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.meta).map_err(std::io::Error::other)?)?;
        std::fs::rename(tmp, p)
    }

    /// Creates the local Plugins folder (Plugins Folder button).
    pub fn ensure_plugins_dir(&self) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.plugins_dir)?;
        Ok(self.plugins_dir.clone())
    }

    /// Where an installed plugin's version lives.
    pub fn version_file(&self, asset_id: u64, version_id: u64) -> PathBuf {
        self.installed_dir().join(asset_id.to_string()).join(version_id.to_string()).join("Plugin.rbxm")
    }

    /// The installed file to load: the metadata's version if present, else
    /// the highest version folder holding a Plugin.rbxm.
    pub fn installed_file(&self, asset_id: u64, version_id: u64) -> (PathBuf, Option<u64>, bool) {
        let want = self.version_file(asset_id, version_id);
        if version_id != 0 && want.is_file() {
            return (want, Some(version_id), true);
        }
        let dir = self.installed_dir().join(asset_id.to_string());
        let best = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()))
            .filter(|v| self.version_file(asset_id, *v).is_file())
            .max();
        match best {
            Some(v) if version_id == 0 => (self.version_file(asset_id, v), Some(v), true),
            _ => (want, (version_id != 0).then_some(version_id), false),
        }
    }

    /// All plugins in Studio's load order: local, then installed.
    pub fn list(&self) -> Vec<PluginSource> {
        let mut out = Vec::new();
        let local_settings = self.installed_dir().join("0").join("settings.json");
        for f in scan_local(&self.plugins_dir) {
            let id = local_id(&self.plugins_dir, &f);
            let enabled = self.meta.local.get(&id).map(|m| m.enabled).unwrap_or(true);
            out.push(PluginSource {
                name: file_stem(&f),
                id,
                kind: PluginKind::Local,
                path: f,
                enabled,
                asset_id: None,
                version: None,
                latest_version: None,
                creator: None,
                updated: None,
                moderated: false,
                auto_update: false,
                present: true,
                settings_path: local_settings.clone(),
            });
        }
        let mut seen = std::collections::HashSet::new();
        for m in &self.meta.plugin_metadata {
            if !seen.insert(m.plugin_asset_id) {
                continue;
            }
            out.push(self.installed_source(m));
        }
        // Folders with no metadata row (copied in by hand): listed, enabled.
        let mut extra: Vec<u64> = std::fs::read_dir(self.installed_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()))
            .filter(|id| *id != 0 && !seen.contains(id))
            .collect();
        extra.sort();
        for id in extra {
            let m = InstalledMeta { plugin_asset_id: id, enabled: true, plugin_name: format!("Plugin {id}"), ..Default::default() };
            let s = self.installed_source(&m);
            if s.present {
                out.push(s);
            }
        }
        out
    }

    fn installed_source(&self, m: &InstalledMeta) -> PluginSource {
        let (path, version, present) = self.installed_file(m.plugin_asset_id, m.asset_version_id);
        PluginSource {
            id: cloud_id(m.plugin_asset_id),
            name: if m.plugin_name.is_empty() { format!("Plugin {}", m.plugin_asset_id) } else { m.plugin_name.clone() },
            kind: PluginKind::Installed,
            path,
            enabled: m.enabled && !m.moderated,
            asset_id: Some(m.plugin_asset_id),
            version,
            latest_version: (m.latest_version != 0).then_some(m.latest_version),
            creator: (!m.creator_name.is_empty()).then(|| m.creator_name.clone()),
            updated: (!m.updated.is_empty()).then(|| m.updated.clone()),
            moderated: m.moderated,
            auto_update: m.auto_update_enabled,
            present,
            settings_path: self.installed_dir().join(m.plugin_asset_id.to_string()).join("settings.json"),
        }
    }

    /// The enabled plugins whose file is there, in load order (what the
    /// runtime gets at startup).
    pub fn enabled(&self) -> Vec<PluginSource> {
        self.list().into_iter().filter(|p| p.enabled && p.present).collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginSource> {
        self.list().into_iter().find(|p| p.id == id)
    }

    /// Resolves what a person typed: a full id, an asset id, or a local
    /// file name.
    pub fn resolve(&self, what: &str) -> Option<PluginSource> {
        let list = self.list();
        list.iter()
            .find(|p| p.id == what)
            .or_else(|| list.iter().find(|p| p.asset_id.map(|a| a.to_string()).as_deref() == Some(what)))
            .or_else(|| list.iter().find(|p| p.id.strip_prefix("user_") == Some(what)))
            .or_else(|| list.iter().find(|p| p.name.eq_ignore_ascii_case(what)))
            .cloned()
    }

    /// Enables / disables a plugin and saves. Err when there is no such plugin.
    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<PluginSource, String> {
        let p = self.resolve(id).ok_or_else(|| format!("no plugin {id:?}"))?;
        match p.kind {
            PluginKind::Local => {
                if on {
                    self.meta.local.remove(&p.id);
                } else {
                    self.meta.local.insert(p.id.clone(), LocalMeta { enabled: false });
                }
            }
            PluginKind::Installed => {
                let aid = p.asset_id.unwrap();
                match self.meta.plugin_metadata.iter_mut().find(|m| m.plugin_asset_id == aid) {
                    Some(m) => m.enabled = on,
                    None => self.meta.plugin_metadata.push(InstalledMeta {
                        plugin_asset_id: aid,
                        asset_version_id: p.version.unwrap_or(0),
                        enabled: on,
                        plugin_name: p.name.clone(),
                        local_only: true,
                        ..Default::default()
                    }),
                }
            }
        }
        self.save().map_err(|e| e.to_string())?;
        self.get(&p.id).ok_or_else(|| "plugin vanished".into())
    }

    pub fn set_auto_update(&mut self, asset_id: u64, on: bool) -> Result<(), String> {
        let m = self.meta.plugin_metadata.iter_mut().find(|m| m.plugin_asset_id == asset_id).ok_or("not installed")?;
        m.auto_update_enabled = on;
        self.save().map_err(|e| e.to_string())
    }

    /// Stores a downloaded plugin version and records it (installs, or
    /// updates to that version). Keeps the account's flags when the row
    /// exists; a new row is enabled.
    pub fn install_bytes(&mut self, meta: InstalledMeta, bytes: &[u8]) -> Result<PluginSource, String> {
        if meta.asset_version_id == 0 {
            return Err("no asset version id".into());
        }
        check_model_bytes(bytes)?;
        let f = self.version_file(meta.plugin_asset_id, meta.asset_version_id);
        std::fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
        self.meta.uninstalled.remove(&meta.plugin_asset_id);
        let tmp = f.with_extension("rbxm.part");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &f).map_err(|e| e.to_string())?;
        match self.meta.plugin_metadata.iter_mut().find(|m| m.plugin_asset_id == meta.plugin_asset_id) {
            Some(m) => {
                m.asset_version_id = meta.asset_version_id;
                if meta.latest_version != 0 {
                    m.latest_version = meta.latest_version;
                }
                for (dst, src) in [(&mut m.plugin_name, &meta.plugin_name), (&mut m.creator_name, &meta.creator_name), (&mut m.updated, &meta.updated), (&mut m.description, &meta.description)] {
                    if !src.is_empty() {
                        *dst = src.clone();
                    }
                }
            }
            None => self.meta.plugin_metadata.push(meta.clone()),
        }
        self.save().map_err(|e| e.to_string())?;
        self.get(&cloud_id(meta.plugin_asset_id)).ok_or_else(|| "installed plugin not listed".into())
    }

    /// Removes an installed plugin: its row and its folder (Remove in
    /// Manage Plugins). Its settings.json goes with the folder, as in Studio.
    pub fn uninstall(&mut self, asset_id: u64) -> Result<(), String> {
        let before = self.meta.plugin_metadata.len();
        self.meta.plugin_metadata.retain(|m| m.plugin_asset_id != asset_id);
        let dir = self.installed_dir().join(asset_id.to_string());
        let had_dir = dir.is_dir();
        if had_dir {
            std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        if before == self.meta.plugin_metadata.len() && !had_dir {
            return Err(format!("plugin {asset_id} isn't installed"));
        }
        self.meta.uninstalled.insert(asset_id);
        self.save().map_err(|e| e.to_string())
    }

    /// Merges the account's list from Roblox: new rows are added with the
    /// account's flags, rows gone from the account are dropped (unless
    /// rustudio installed them), versions and names refresh; the enabled
    /// flag set in rustudio wins for rows already here.
    pub fn merge_cloud(&mut self, rows: &[rustudio_account::plugins::CloudPlugin]) {
        let removed = self.meta.uninstalled.clone();
        let rows: Vec<&rustudio_account::plugins::CloudPlugin> = rows.iter().filter(|r| !removed.contains(&r.plugin_asset_id)).collect();
        let ids: std::collections::HashSet<u64> = rows.iter().map(|r| r.plugin_asset_id).collect();
        self.meta.plugin_metadata.retain(|m| m.local_only || ids.contains(&m.plugin_asset_id));
        for r in rows.iter().copied() {
            match self.meta.plugin_metadata.iter_mut().find(|m| m.plugin_asset_id == r.plugin_asset_id) {
                Some(m) => {
                    m.moderated = r.moderated;
                    if !r.plugin_name.is_empty() {
                        m.plugin_name = r.plugin_name.clone();
                    }
                    if m.asset_version_id == 0 {
                        m.asset_version_id = r.asset_version_id;
                    }
                    // First time the account lists a row rustudio made (an
                    // import): the account's on/off and auto-update apply.
                    if m.local_only {
                        m.enabled = r.enabled;
                        m.auto_update_enabled = r.auto_update_enabled;
                    }
                    m.local_only = false;
                }
                None => self.meta.plugin_metadata.push(InstalledMeta {
                    plugin_asset_id: r.plugin_asset_id,
                    asset_version_id: r.asset_version_id,
                    latest_version: r.latest_version,
                    enabled: r.enabled,
                    moderated: r.moderated,
                    plugin_name: r.plugin_name.clone(),
                    auto_update_enabled: r.auto_update_enabled,
                    ..Default::default()
                }),
            }
        }
        // The account's order (Manage Plugins lists them so); rustudio's own after.
        let order: std::collections::HashMap<u64, usize> = rows.iter().enumerate().map(|(i, r)| (r.plugin_asset_id, i)).collect();
        self.meta.plugin_metadata.sort_by_key(|m| order.get(&m.plugin_asset_id).copied().unwrap_or(usize::MAX));
    }

    /// Fills newest versions / creators / dates from Roblox.
    pub fn merge_details(
        &mut self,
        info: &std::collections::HashMap<u64, rustudio_account::plugins::PluginInfo>,
        details: &std::collections::HashMap<u64, rustudio_account::plugins::ItemDetails>,
    ) {
        for m in &mut self.meta.plugin_metadata {
            if let Some(i) = info.get(&m.plugin_asset_id) {
                m.latest_version = i.version_id;
                m.updated = i.updated.clone();
                if m.description.is_empty() {
                    m.description = i.description.clone();
                }
            }
            if let Some(d) = details.get(&m.plugin_asset_id) {
                m.creator_name = d.creator_name.clone();
                if m.plugin_name.is_empty() {
                    m.plugin_name = d.name.clone();
                }
            }
        }
    }

    /// The next free `<name>.rbxmx` in the local folder (Save as Local Plugin).
    pub fn local_save_path(&self, name: &str, ext: &str) -> PathBuf {
        let clean: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect();
        let clean = if clean.trim().is_empty() { "Plugin".to_string() } else { clean.trim().to_string() };
        self.plugins_dir.join(format!("{clean}.{ext}"))
    }
}

/// Checks bytes are a Roblox model (binary or XML) that decodes.
pub fn check_model_bytes(bytes: &[u8]) -> Result<usize, String> {
    let dom = if bytes.starts_with(b"<roblox!") {
        rbx_binary::from_reader(bytes).map_err(|e| format!("not a readable binary model: {e}"))?
    } else if bytes.starts_with(b"<roblox") {
        rbx_xml::from_reader_default(bytes).map_err(|e| format!("not a readable XML model: {e}"))?
    } else {
        return Err("not a Roblox model file".into());
    };
    Ok(dom.descendants().count().saturating_sub(1))
}

/// Parses a plugin file into a model (a `.lua` becomes one Script named
/// `Script`, the way Studio wraps a script plugin: ).
pub fn read_plugin_file(path: &Path) -> Result<rbx_dom_weak::WeakDom, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "lua" | "luau" => {
            let src = String::from_utf8_lossy(&bytes).to_string();
            let mut dom = rbx_dom_weak::WeakDom::new(rbx_dom_weak::InstanceBuilder::new("DataModel"));
            let root = dom.root_ref();
            dom.insert(
                root,
                rbx_dom_weak::InstanceBuilder::new("Script")
                    .with_name("Script")
                    .with_property("Source", rbx_dom_weak::types::Variant::String(src)),
            );
            Ok(dom)
        }
        "rbxmx" => rbx_xml::from_reader_default(&bytes[..]).map_err(|e| e.to_string()),
        _ => rbx_binary::from_reader(&bytes[..]).map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rustudio-plugin-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn tiny_rbxm() -> Vec<u8> {
        let mut dom = rbx_dom_weak::WeakDom::new(rbx_dom_weak::InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        let s = dom.insert(
            root,
            rbx_dom_weak::InstanceBuilder::new("Script").with_name("P").with_property("Source", rbx_dom_weak::types::Variant::String("print('hi')".into())),
        );
        let mut out = Vec::new();
        rbx_binary::to_writer(&mut out, &dom, &[s]).unwrap();
        out
    }

    #[test]
    fn layout_and_order() {
        let root = tmp("layout");
        let dir = root.join("Plugins");
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("Sub/Deeper")).unwrap();
        for f in ["b.lua", "A.rbxm", "c.luau", "notes.txt", ".hidden.lua", "Sub/z.rbxmx", "Sub/y.rbxm", ".git/x.lua", "d.RBXM", "E.LUA", "m.rbxmx", "Sub/Deeper/deep.lua", "Sub/a.lua"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut s = Store::open(root.clone(), dir.clone(), 42);
        let ids: Vec<String> = s.list().iter().map(|p| p.id.clone()).collect();
        // .lua anywhere by path, then top-level .rbxmx, then top-level .rbxm;
        // no .luau, no model files in subfolders, nothing hidden.
        assert_eq!(ids, ["user_b.lua", "user_E.LUA", "user_Sub/a.lua", "user_Sub/Deeper/deep.lua", "user_m.rbxmx", "user_A.rbxm", "user_d.RBXM"]);
        assert_eq!(s.list()[0].name, "b");
        assert!(s.list().iter().all(|p| p.enabled && p.kind == PluginKind::Local));
        // Installed after local, keyed by asset id + version.
        let meta = InstalledMeta { plugin_asset_id: 7, asset_version_id: 70, plugin_name: "Seven".into(), enabled: true, ..Default::default() };
        let p = s.install_bytes(meta, &tiny_rbxm()).unwrap();
        assert_eq!(p.id, "cloud_7");
        assert_eq!(p.path, root.join("42/InstalledPlugins/7/70/Plugin.rbxm"));
        assert_eq!(p.settings_path, root.join("42/InstalledPlugins/7/settings.json"));
        assert_eq!(s.list().last().unwrap().id, "cloud_7");
        assert_eq!(s.list()[0].settings_path, root.join("42/InstalledPlugins/0/settings.json"));
        // A newer version beside the old one becomes the current one.
        let meta = InstalledMeta { plugin_asset_id: 7, asset_version_id: 71, ..Default::default() };
        s.install_bytes(meta, &tiny_rbxm()).unwrap();
        assert_eq!(s.get("cloud_7").unwrap().version, Some(71));
        assert!(root.join("42/InstalledPlugins/7/70/Plugin.rbxm").is_file());
        // Not a model: refused.
        let bad = InstalledMeta { plugin_asset_id: 8, asset_version_id: 1, ..Default::default() };
        assert!(s.install_bytes(bad, b"hello").is_err());
        // Reopen: persisted.
        let s2 = Store::open(root.clone(), dir.clone(), 42);
        assert_eq!(s2.get("cloud_7").unwrap().name, "Seven");
        // Another account sees none of it.
        assert!(Store::open(root.clone(), dir, 43).get("cloud_7").is_none());
        s.uninstall(7).unwrap();
        assert!(s.get("cloud_7").is_none());
        assert!(!root.join("42/InstalledPlugins/7").exists());
        assert!(s.uninstall(7).is_err());
    }

    #[test]
    fn enable_state_persists() {
        let root = tmp("enable");
        let dir = root.join("Plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.lua"), b"print(1)").unwrap();
        let mut s = Store::open(root.clone(), dir.clone(), 0);
        s.install_bytes(InstalledMeta { plugin_asset_id: 5, asset_version_id: 50, enabled: true, ..Default::default() }, &tiny_rbxm()).unwrap();
        assert_eq!(s.enabled().len(), 2);
        s.set_enabled("one.lua", false).unwrap();
        s.set_enabled("5", false).unwrap();
        let s = Store::open(root.clone(), dir.clone(), 0);
        assert!(!s.get("user_one.lua").unwrap().enabled);
        assert!(!s.get("cloud_5").unwrap().enabled);
        assert!(s.enabled().is_empty());
        let mut s = s;
        s.set_enabled("user_one.lua", true).unwrap();
        assert!(Store::open(root, dir, 0).get("user_one.lua").unwrap().enabled);
        assert!(s.set_enabled("nope", true).is_err());
    }

    #[test]
    fn cloud_merge_keeps_local_choices() {
        use rustudio_account::plugins::CloudPlugin;
        let root = tmp("merge");
        let mut s = Store::open(root.clone(), root.join("Plugins"), 1);
        let row = |id, en| CloudPlugin { plugin_asset_id: id, asset_version_id: id * 10, latest_version: 0, enabled: en, moderated: false, plugin_name: format!("P{id}"), auto_update_enabled: false };
        s.merge_cloud(&[row(1, true), row(2, false)]);
        assert_eq!(s.list().len(), 2);
        assert!(!s.get("cloud_2").unwrap().present);
        assert!(s.enabled().is_empty(), "nothing downloaded yet");
        s.install_bytes(InstalledMeta { plugin_asset_id: 3, asset_version_id: 30, local_only: true, enabled: true, ..Default::default() }, &tiny_rbxm()).unwrap();
        s.set_enabled("cloud_1", false).unwrap();
        s.merge_cloud(&[row(1, true)]);
        let ids: Vec<_> = s.list().iter().map(|p| p.id.clone()).collect();
        assert_eq!(ids, ["cloud_1", "cloud_3"], "2 left the account; 3 was installed by rustudio");
        assert!(!s.get("cloud_1").unwrap().enabled, "rustudio's choice wins");
        // Uninstalled in rustudio: a sync doesn't bring it back; installing does.
        s.uninstall(1).unwrap();
        s.merge_cloud(&[row(1, true)]);
        assert!(s.get("cloud_1").is_none());
        s.install_bytes(InstalledMeta { plugin_asset_id: 1, asset_version_id: 10, enabled: true, ..Default::default() }, &tiny_rbxm()).unwrap();
        s.merge_cloud(&[row(1, true)]);
        assert!(s.get("cloud_1").is_some());
    }

    #[test]
    fn plugin_files_parse() {
        let root = tmp("parse");
        let f = root.join("x.lua");
        std::fs::write(&f, "print('x')").unwrap();
        let dom = read_plugin_file(&f).unwrap();
        let s = dom.get_by_ref(dom.root().children()[0]).unwrap();
        assert_eq!(s.class.as_str(), "Script");
        assert_eq!(s.name.as_str(), "Script");
        let m = root.join("m.rbxm");
        std::fs::write(&m, tiny_rbxm()).unwrap();
        assert_eq!(read_plugin_file(&m).unwrap().root().children().len(), 1);
        assert_eq!(check_model_bytes(&tiny_rbxm()).unwrap(), 1);
    }
}
