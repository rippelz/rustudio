//! The store's side of running plugins. The edit-mode Luau runtime is the
//! host's (src/plugin_host); the store only says which
//! plugin sources to load, reload or unload, through [`PluginRuntime`].
//! Until a runtime is attached, [`NoRuntime`] records the requests and
//! says so in Output.

use super::PluginSource;

/// What the store asks of a plugin runtime.
pub trait PluginRuntime {
    /// Loads and runs a plugin (its file is there and it is enabled).
    fn load(&mut self, src: &PluginSource) -> Result<(), String>;
    /// Stops a plugin and removes everything it made (toolbars, widgets…).
    fn unload(&mut self, id: &str);
    /// Unloads and loads again from the (changed) file.
    fn reload(&mut self, src: &PluginSource) -> Result<(), String> {
        self.unload(&src.id);
        self.load(src)
    }
    /// Ids of the plugins running now.
    fn loaded(&self) -> Vec<String>;
    /// Whether plugins run at all in this process (the host's switch).
    fn enabled(&self) -> bool {
        true
    }
    /// The runtime loads every plugin again itself when another place
    /// opens (the host does); else the store unloads and loads them.
    fn follows_documents(&self) -> bool {
        false
    }
    /// The editor's bridge to the host, when this is it.
    fn bridge(&mut self) -> Option<&mut HostBridge> {
        None
    }
}

/// No runtime attached: keeps the loaded set so the store and Manage
/// Plugins behave, and logs each request.
#[derive(Default)]
pub struct NoRuntime {
    pub loaded: Vec<String>,
    pub log: Vec<String>,
}

impl PluginRuntime for NoRuntime {
    fn load(&mut self, src: &PluginSource) -> Result<(), String> {
        if !self.loaded.contains(&src.id) {
            self.loaded.push(src.id.clone());
        }
        self.log.push(format!("load {}", src.id));
        Ok(())
    }
    fn unload(&mut self, id: &str) {
        self.loaded.retain(|l| l != id);
        self.log.push(format!("unload {id}"));
    }
    fn reload(&mut self, src: &PluginSource) -> Result<(), String> {
        self.log.push(format!("reload {}", src.id));
        if !self.loaded.contains(&src.id) {
            self.loaded.push(src.id.clone());
        }
        Ok(())
    }
    fn loaded(&self) -> Vec<String> {
        self.loaded.clone()
    }
}

/// A load request for the host, in its own terms.
#[derive(Debug, Clone)]
pub enum RtCmd {
    Load(PluginSource),
    Unload(String),
    Reload(PluginSource),
}

/// The runtime the editor uses: the store's requests queue here and the
/// editor applies them to the plugin host (src/plugin_host) each frame
/// (`Editor::plugins_tick`), keeping store id ↔ host id. The host loads
/// plugins again by itself when another place opens.
#[derive(Default)]
pub struct HostBridge {
    pub loaded: Vec<String>,
    pub cmds: Vec<RtCmd>,
    /// The host's switch (--no-plugins / RUSTUDIO_NO_PLUGINS), read each frame.
    pub host_enabled: bool,
    pub ids: std::collections::HashMap<String, crate::plugin_host::PluginId>,
}

impl PluginRuntime for HostBridge {
    fn load(&mut self, src: &PluginSource) -> Result<(), String> {
        if !self.host_enabled {
            return Err("plugins are turned off (--no-plugins / RUSTUDIO_NO_PLUGINS)".into());
        }
        if !self.loaded.contains(&src.id) {
            self.loaded.push(src.id.clone());
        }
        self.cmds.push(RtCmd::Load(src.clone()));
        Ok(())
    }
    fn unload(&mut self, id: &str) {
        self.loaded.retain(|l| l != id);
        self.cmds.push(RtCmd::Unload(id.to_string()));
    }
    fn reload(&mut self, src: &PluginSource) -> Result<(), String> {
        if !self.loaded.contains(&src.id) {
            self.loaded.push(src.id.clone());
        }
        self.cmds.push(RtCmd::Reload(src.clone()));
        Ok(())
    }
    fn loaded(&self) -> Vec<String> {
        self.loaded.clone()
    }
    fn enabled(&self) -> bool {
        self.host_enabled
    }
    fn follows_documents(&self) -> bool {
        true
    }
    fn bridge(&mut self) -> Option<&mut HostBridge> {
        Some(self)
    }
}

/// The host's source for a store plugin: a top-level local file as a File
/// (the host names it `user_<file>`), a `.lua` in a subfolder as a tree
/// named with its path (`user_Sub/x.lua`), an installed
/// plugin as Cloud.
pub fn host_source(src: &PluginSource) -> Result<crate::plugin_host::PluginSource, String> {
    use crate::plugin_host::PluginSource as H;
    Ok(match (src.kind, src.asset_id) {
        (super::PluginKind::Installed, Some(a)) => H::Cloud { asset_id: a, path: src.path.clone() },
        _ if src.id == format!("user_{}", src.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()) => {
            H::File(src.path.clone())
        }
        _ => H::Tree { name: src.id.clone(), asset_id: None, dom: std::sync::Arc::new(super::read_plugin_file(&src.path)?) },
    })
}
