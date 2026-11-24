//! One-step import from a real Studio install: its local plugins, its
//! installed plugins and their settings files, copied into rustudio's store.
//! The source is only read.
//!
//! Where Studio keeps them:
//! * Windows: `%LOCALAPPDATA%\Roblox\Plugins` and
//!   `%LOCALAPPDATA%\Roblox\<userId>\InstalledPlugins\<assetId>\<versionId>\Plugin.rbxm`;
//! * Vinegar (Linux): the same tree inside the Wine prefix
//!   (`~/.var/app/org.vinegarhq.Vinegar[/<slot>]/data/vinegar/prefixes/*/drive_c/users/*/AppData/Local/Roblox`):
//!   that is the PluginsDir Studio loads from (GlobalSettings_13.xml
//!   `C:/users/<u>/AppData/Local/Roblox/Plugins`). Vinegar's
//!   `data/vinegar/appdata/Roblox/Plugins` is not read by Studio, so it is
//!   not imported either.
//!
//! The settings files are Studio's own JSON (`settings.json`, one object of
//! key → value, Studio's serialized types inline) and are copied as is.

use std::path::{Path, PathBuf};

use super::{scan_local, InstalledMeta, Store};

#[derive(Debug, Clone, PartialEq)]
pub struct StudioInstall {
    pub label: String,
    /// Folders holding local plugin files.
    pub plugin_dirs: Vec<PathBuf>,
    /// `…/Roblox` folders holding `<userId>/InstalledPlugins`.
    pub roblox_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Local { from: PathBuf, rel: PathBuf },
    Installed { asset_id: u64, version_id: u64, from: PathBuf, settings: Option<PathBuf> },
    LocalSettings { from: PathBuf },
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    pub copied: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

fn dirs_in(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

/// The Studio installs on this machine.
pub fn detect() -> Vec<StudioInstall> {
    let mut out = Vec::new();
    if cfg!(windows) {
        if let Some(l) = dirs::data_local_dir() {
            let r = l.join("Roblox");
            if r.is_dir() {
                out.push(StudioInstall { label: "Roblox Studio".into(), plugin_dirs: vec![r.join("Plugins")], roblox_dirs: vec![r] });
            }
        }
    }
    let base = rustudio_core::paths::home().join(".var/app/org.vinegarhq.Vinegar");
    let mut roots = vec![("Vinegar".to_string(), base.join("data/vinegar"))];
    for d in dirs_in(&base) {
        let name = d.file_name().unwrap().to_string_lossy().to_string();
        if d.join("data/vinegar").is_dir() {
            roots.push((format!("Vinegar ({name})"), d.join("data/vinegar")));
        }
    }
    for (label, v) in roots {
        if !v.is_dir() {
            continue;
        }
        let mut s = StudioInstall { label, plugin_dirs: Vec::new(), roblox_dirs: Vec::new() };
        for prefix in dirs_in(&v.join("prefixes")) {
            for user in dirs_in(&prefix.join("drive_c/users")) {
                let r = user.join("AppData/Local/Roblox");
                if r.is_dir() {
                    s.plugin_dirs.push(r.join("Plugins"));
                    s.roblox_dirs.push(r);
                }
            }
        }
        s.plugin_dirs.retain(|p| p.is_dir());
        if !s.plugin_dirs.is_empty() || !s.roblox_dirs.is_empty() {
            out.push(s);
        }
    }
    out
}

/// What importing `src` would copy for `user_id` (its InstalledPlugins
/// folder; when that account has none there, the folder of the account
/// with the most plugins).
pub fn plan(src: &StudioInstall, user_id: u64) -> Vec<Item> {
    let mut out = Vec::new();
    let mut seen_rel = std::collections::HashSet::new();
    for d in &src.plugin_dirs {
        for f in scan_local(d) {
            let rel = f.strip_prefix(d).unwrap_or(&f).to_path_buf();
            if seen_rel.insert(rel.clone()) {
                out.push(Item::Local { from: f, rel });
            }
        }
    }
    let count = |ip: &Path| dirs_in(ip).iter().filter(|d| d.file_name().and_then(|n| n.to_str()).is_some_and(|n| n != "0" && n.parse::<u64>().is_ok())).count();
    let mut best: Option<PathBuf> = None;
    for r in &src.roblox_dirs {
        let mine = r.join(user_id.to_string()).join("InstalledPlugins");
        if user_id != 0 && mine.is_dir() {
            best = Some(mine);
            break;
        }
        for u in dirs_in(r) {
            let ip = u.join("InstalledPlugins");
            if ip.is_dir() && best.as_ref().is_none_or(|b| count(&ip) > count(b)) {
                best = Some(ip);
            }
        }
    }
    let Some(ip) = best else { return out };
    if ip.join("0/settings.json").is_file() {
        out.push(Item::LocalSettings { from: ip.join("0/settings.json") });
    }
    for d in dirs_in(&ip) {
        let Some(asset_id) = d.file_name().and_then(|n| n.to_str()).and_then(|n| n.parse::<u64>().ok()) else { continue };
        if asset_id == 0 {
            continue;
        }
        // The newest version folder with a Plugin.rbxm (by modification time,
        // then id).
        let ver = dirs_in(&d)
            .into_iter()
            .filter_map(|v| {
                let id = v.file_name()?.to_str()?.parse::<u64>().ok()?;
                let f = v.join("Plugin.rbxm");
                let t = std::fs::metadata(&f).ok()?.modified().ok()?;
                Some((t, id, f))
            })
            .max();
        if let Some((_, version_id, from)) = ver {
            let settings = Some(d.join("settings.json")).filter(|s| s.is_file());
            out.push(Item::Installed { asset_id, version_id, from, settings });
        }
    }
    out
}

/// Copies the plan into the store. Files already there are left alone.
pub fn run(store: &mut Store, items: &[Item]) -> Report {
    let mut r = Report::default();
    let copy = |from: &Path, to: &Path| -> Result<bool, String> {
        if to.exists() {
            return Ok(false);
        }
        std::fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(from, to).map_err(|e| format!("{}: {e}", from.display()))?;
        Ok(true)
    };
    for it in items {
        match it {
            Item::Local { from, rel } => {
                let to = store.plugins_dir.join(rel);
                match copy(from, &to) {
                    Ok(true) => r.copied.push(format!("local {}", rel.display())),
                    Ok(false) => r.skipped.push(format!("local {} (already there)", rel.display())),
                    Err(e) => r.failed.push(e),
                }
            }
            Item::LocalSettings { from } => match copy(from, &store.installed_dir().join("0/settings.json")) {
                Ok(true) => r.copied.push("local plugin settings".into()),
                Ok(false) => r.skipped.push("local plugin settings (already there)".into()),
                Err(e) => r.failed.push(e),
            },
            Item::Installed { asset_id, version_id, from, settings } => {
                if store.meta.plugin_metadata.iter().any(|m| m.plugin_asset_id == *asset_id && m.asset_version_id == *version_id)
                    && store.version_file(*asset_id, *version_id).is_file()
                {
                    r.skipped.push(format!("cloud_{asset_id} (already installed)"));
                    continue;
                }
                let bytes = match std::fs::read(from) {
                    Ok(b) => b,
                    Err(e) => {
                        r.failed.push(format!("{}: {e}", from.display()));
                        continue;
                    }
                };
                let known = store.meta.plugin_metadata.iter().any(|m| m.plugin_asset_id == *asset_id);
                let meta = InstalledMeta {
                    plugin_asset_id: *asset_id,
                    asset_version_id: *version_id,
                    enabled: true,
                    plugin_name: format!("Plugin {asset_id}"),
                    local_only: !known,
                    ..Default::default()
                };
                match store.install_bytes(meta, &bytes) {
                    Ok(_) => r.copied.push(format!("cloud_{asset_id} v{version_id}")),
                    Err(e) => r.failed.push(format!("cloud_{asset_id}: {e}")),
                }
                if let Some(s) = settings {
                    let _ = copy(s, &store.installed_dir().join(asset_id.to_string()).join("settings.json"));
                }
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::super::tests::{tiny_rbxm, tmp};
    use super::*;

    #[test]
    fn imports_a_studio_tree_read_only() {
        let d = tmp("import");
        let r = d.join("studio/Roblox");
        std::fs::create_dir_all(r.join("Plugins")).unwrap();
        std::fs::write(r.join("Plugins/Argon.rbxm"), tiny_rbxm()).unwrap();
        std::fs::write(r.join("Plugins/tool.lua"), "print(1)").unwrap();
        for (v, t) in [("100", 1), ("200", 2)] {
            let f = r.join(format!("9/InstalledPlugins/55/{v}/Plugin.rbxm"));
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, tiny_rbxm()).unwrap();
            let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 * t);
            std::fs::File::options().write(true).open(&f).unwrap().set_modified(when).unwrap();
        }
        std::fs::write(r.join("9/InstalledPlugins/55/settings.json"), r#"{ "k":1 }"#).unwrap();
        std::fs::create_dir_all(r.join("9/InstalledPlugins/0")).unwrap();
        std::fs::write(r.join("9/InstalledPlugins/0/settings.json"), "{}").unwrap();
        let src = StudioInstall { label: "t".into(), plugin_dirs: vec![r.join("Plugins")], roblox_dirs: vec![r.clone()] };
        let items = plan(&src, 9);
        assert_eq!(items.len(), 4, "{items:?}");
        let mut store = Store::open(d.join("rs"), d.join("rs/Plugins"), 9);
        let rep = run(&mut store, &items);
        assert!(rep.failed.is_empty(), "{rep:?}");
        let ids: Vec<_> = store.list().iter().map(|p| (p.id.clone(), p.version)).collect();
        assert_eq!(ids, [("user_tool.lua".into(), None), ("user_Argon.rbxm".into(), None), ("cloud_55".into(), Some(200))]);
        assert!(d.join("rs/9/InstalledPlugins/55/settings.json").is_file());
        assert!(d.join("rs/9/InstalledPlugins/0/settings.json").is_file());
        // Again: nothing new.
        let rep = run(&mut store, &plan(&src, 9));
        assert!(rep.copied.is_empty(), "{rep:?}");
        // Source untouched.
        assert!(r.join("9/InstalledPlugins/55/100/Plugin.rbxm").is_file());
        // Another account id falls back to the fullest folder.
        assert_eq!(plan(&src, 1).len(), 4);
    }
}
