//! The plugin host: runs Roblox Studio plugins against the live edit
//! DataModel (and gives each Play DataModel its own copies, as Studio
//! does). This module documents the
//! API other parts of rustudio use; this header is the design.
//!
//! **One edit VM.** All plugins of the edit DataModel share one Luau VM
//! (as Studio's one ScriptContext per DataModel: `_G` and `shared` are
//! shared by plugins there, measured), made with `Runtime::new` and a
//! `World` in `RunMode::Edit`.
//!
//! **The document is lent, not copied.** Each frame the editor's own
//! `Document` moves into the edit VM's World (`std::mem::replace` with a
//! placeholder, as Play lends its DataModels to their runtimes), plugin code
//! runs, and the document moves back. No copy is made, so a plugin's write
//! is in the document the Explorer, Properties, viewport and undo stack
//! read on the same frame. Writes made outside the VM (the editor, undo,
//! file sync, Team Create) reach plugins as signals through the journal
//! (edit_sync.rs). With no plugin loaded nothing runs and nothing moves.
//!
//! **Plugin instances** are orphans in the document's DOM (Parent nil, not
//! in the DataModel): never saved, not in Explorer, not copied into Play.
//! A new document (another place opened) reloads every plugin into it, as
//! Studio loads plugins per DataModel.
//!
//! **Play.** Studio loads every plugin again in the Play server and client
//! DataModels (each copy with IsEdit false), and the edit DataModel's
//! plugins keep running meanwhile with Heartbeat but no RenderStepped
//! (measured). The host does the same: `load_into_play` from start_play,
//! the edit VM ticks on the saved edit document while Play runs.

pub mod api;
pub mod builtin_lazy;
pub mod edit_sync;
mod glue;
pub mod widgets_ui;
pub mod permissions;
pub mod prompts;
pub mod script_docs;
pub mod settings;
pub mod source;
pub mod ui_ribbon;
pub mod ui_shortcuts;
pub mod vm;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::Value;
use rbx_dom_weak::types::Ref;

use crate::app::Level;
use rustudio_core::doc::Document;
use rustudio_engine::runtime::{RunMode, Runtime, Side, World, STACK_MARK};

pub use source::PluginSource;
pub use vm::{EditorRequest, UiItem, UiKind};

pub(crate) type PlaytestCompletion = std::sync::Arc<std::sync::OnceLock<Result<String, String>>>;

/// A plugin's id in this rustudio session (stable across reloads).
/// (The type lives in the engine: engine code names it.)
pub use rustudio_engine::runtime::domains::plugin_api::PluginId;

/// What `PluginHost::loaded` reports per plugin.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: PluginId,
    /// The Plugin instance's Name (`user_X.rbxm`, `cloud_123`).
    pub name: String,
    pub source: PluginSource,
    /// Running in the edit DataModel now.
    pub running: bool,
    /// Load errors (the file, a decode) and script errors from its code.
    pub errors: Vec<String>,
}

struct Spec {
    id: PluginId,
    source: PluginSource,
    name: String,
    model: Option<source::PluginModel>,
    errors: Vec<String>,
    running: bool,
    deferred: Option<builtin_lazy::DeferredStub>,
}

impl Spec {
    fn load_spec(&self) -> Option<vm::LoadSpec> {
        let model = self.model.as_ref()?;
        let is_builtin = self.source.is_builtin();
        Some(vm::LoadSpec {
            id: self.id,
            name: self.name.clone(),
            asset_id: self.source.asset_id(),
            settings_key: self.source.settings_key(),
            identity: if is_builtin {
                rustudio_engine::runtime::security::BUILTIN_PLUGIN
            } else {
                rustudio_engine::runtime::security::PLUGIN_IDENT
            },
            is_builtin,
            tree: model.dom.clone(),
            bytecode: model.bytecode.clone(),
        })
    }
}

enum Cmd {
    Load(PluginId),
    Wake(PluginId),
    Unload(PluginId),
    Reload(PluginId),
    Trigger(Trigger),
}

enum PendingTrigger {
    Button {
        plugin: PluginId,
        toolbar: String,
        id: String,
        event: &'static str,
        frames_left: u8,
    },
    Action {
        plugin: PluginId,
        id: String,
        frames_left: u8,
    },
}

/// A UI event the harness fires by name.
#[derive(Clone, Debug)]
pub enum Trigger {
    /// PluginToolbarButton.Click: (toolbar name, button id or text).
    Button(String, String),
    /// PluginAction.Triggered by its id (as given to CreatePluginAction)
    /// or its full ActionId.
    Action(String),
    /// Answers the open PluginMenu:ShowAsync popup on top (see
    /// `plugin_host::api::menus::answer_top` for the choice syntax).
    Menu(String),
    /// Fires `event` on a plugin object of the edit VM (the ribbon's clicks).
    Fire(Ref, &'static str),
    /// Deactivates a plugin (the person picked a ribbon tool).
    Deactivate(PluginId),
}

impl Trigger {
    /// `plugin-button:TOOLBAR/BUTTON` or `plugin-action:ACTIONID` (the
    /// harness's --action names).
    pub fn parse(name: &str) -> Option<Trigger> {
        if let Some(rest) = name.strip_prefix("plugin-button:") {
            let (tb, b) = rest.rsplit_once('/')?;
            return Some(Trigger::Button(tb.to_string(), b.to_string()));
        }
        if let Some(m) = name.strip_prefix("plugin-menu:") {
            return Some(Trigger::Menu(m.to_string()));
        }
        name.strip_prefix("plugin-action:").map(|a| Trigger::Action(a.to_string()))
    }
}

struct EditVm {
    rt: Runtime,
    world: Option<World>,
    doc_id: u64,
    bound: bool,
    gc_t: f64,
}

pub struct PluginHost {
    enabled: bool,
    specs: Vec<Spec>,
    next: u32,
    edit: Option<EditVm>,
    repaint_context: Option<egui::Context>,
    sync: edit_sync::EditSync,
    cmds: Vec<Cmd>,
    log: Vec<(Level, String)>,
    requests: Vec<EditorRequest>,
    pub(super) playtest: Option<(String, PlaytestCompletion)>,
    activation_seen: u64,
    activation_changed: bool,
    play_ui: SharedPlayUi,
    deferred_rev: u64,
    pending_triggers: Vec<PendingTrigger>,
}

#[derive(Default)]
struct PlayUi {
    widgets: Vec<UiItem>,
    ribbon: ui_ribbon::UiModel,
}

type SharedPlayUi = Arc<Mutex<[PlayUi; 2]>>;

pub(crate) struct PlayPlugins {
    specs: Vec<vm::LoadSpec>,
    ui: SharedPlayUi,
}

fn play_side_index(side: Side) -> usize {
    match side {
        Side::Server => 0,
        Side::Client => 1,
    }
}

impl Default for PluginHost {
    fn default() -> Self {
        // The engine's registered domains (the plugin API is one of them):
        // before any runtime is made, so its members are in the tables and
        // the atom list. (Tests that make a PluginHost get it too.)
        crate::studio_hooks::install();
        // HTTP / script-injection permissions come from the plugin store
        // (once: a check registered later, e.g. by a test, stays).
        static STORE_CHECKS: std::sync::Once = std::sync::Once::new();
        STORE_CHECKS.call_once(permissions::use_store_checks);
        PluginHost {
            enabled: !disabled_by_env(),
            specs: Vec::new(),
            next: 1,
            edit: None,
            repaint_context: None,
            sync: Default::default(),
            cmds: Vec::new(),
            log: Vec::new(),
            requests: Vec::new(),
            playtest: None,
            activation_seen: 0,
            activation_changed: false,
            play_ui: Default::default(),
            deferred_rev: 0,
            pending_triggers: Vec::new(),
        }
    }
}

/// RUSTUDIO_NO_PLUGINS=1 (or --no-plugins): no plugin loads at all.
pub fn disabled_by_env() -> bool {
    std::env::var_os("RUSTUDIO_NO_PLUGINS").is_some_and(|v| !v.is_empty() && v != "0")
}

impl PluginHost {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_repaint_context(&mut self, ctx: egui::Context) {
        self.repaint_context = Some(ctx);
    }

    pub fn needs_idle_tick(&self) -> bool {
        !self.cmds.is_empty() || !self.pending_triggers.is_empty() || self.specs.iter().any(|s| s.running)
    }

    pub fn deferred_rev(&self) -> u64 {
        self.deferred_rev
    }

    pub fn has_deferred(&self) -> bool {
        self.specs.iter().any(|s| s.deferred.is_some())
    }

    /// Wakes a deferred plugin matching `name` (either its full `Plugin.Name`
    /// such as `builtin_Toolbox.rbxm` or its stem `Toolbox`).
    pub fn wake_plugin_by_name(&mut self, name: &str) -> Option<PluginId> {
        let id = self.specs.iter().find_map(|s| {
            let stub = s.deferred.as_ref()?;
            let hit = s.name.eq_ignore_ascii_case(name)
                || stub.manifest.stem.eq_ignore_ascii_case(name)
                || name
                    .strip_prefix("builtin_")
                    .is_some_and(|rest| rest.trim_end_matches(".rbxm").eq_ignore_ascii_case(stub.manifest.stem));
            hit.then_some(s.id)
        })?;
        self.cmds.push(Cmd::Wake(id));
        Some(id)
    }

    /// Wakes any deferred plugin listening for `(service, event)` (e.g.
    /// `StudioService.OnOpenGameSettings`, `GuiService.Open9SliceEditor`).
    pub fn wake_for_signal(&mut self, service: &str, event: &str) -> bool {
        let ids: Vec<PluginId> = self
            .specs
            .iter()
            .filter_map(|s| {
                let stub = s.deferred.as_ref()?;
                stub.manifest
                    .wake
                    .iter()
                    .any(|w| matches!(w, builtin_lazy::WakeOn::Signal(sv, ev) if *sv == service && *ev == event))
                    .then_some(s.id)
            })
            .collect();
        let any = !ids.is_empty();
        for id in ids {
            self.cmds.push(Cmd::Wake(id));
        }
        any
    }

    /// Registers a deferred built-in plugin stub with zero disk I/O until its
    /// button, action, widget, or wake condition is first triggered.
    pub fn register_deferred(
        &mut self,
        source: PluginSource,
        manifest: &'static builtin_lazy::LazyManifest,
    ) -> Result<PluginId, String> {
        if !self.enabled {
            return Err("plugins are turned off (--no-plugins / RUSTUDIO_NO_PLUGINS)".into());
        }
        let id = PluginId(self.next);
        self.next += 1;
        let name = source.plugin_name();
        let stub = builtin_lazy::DeferredStub::new(id, &name, manifest);
        self.specs.push(Spec {
            id,
            source,
            name,
            model: None,
            errors: Vec::new(),
            running: false,
            deferred: Some(stub),
        });
        self.deferred_rev = self.deferred_rev.wrapping_add(1);
        Ok(id)
    }

    /// Nothing loaded and nothing queued: the editor skips the tick.
    pub fn is_idle(&self) -> bool {
        self.specs.is_empty() && self.cmds.is_empty() && self.edit.is_none() && self.log.is_empty()
    }

    /// Turns all plugin loading off (--no-plugins): loaded ones unload at
    /// the next frame, later loads are refused.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            let ids: Vec<PluginId> = self.specs.iter().map(|s| s.id).collect();
            for id in ids {
                self.unload(id);
            }
        }
    }

    /// Adds a plugin; it starts in the edit DataModel at the next frame.
    /// The file is read now: a read or decode error comes back here (and
    /// is listed in `loaded()`).
    pub fn load(&mut self, source: PluginSource) -> Result<PluginId, String> {
        if !self.enabled {
            return Err("plugins are turned off (--no-plugins / RUSTUDIO_NO_PLUGINS)".into());
        }
        let id = PluginId(self.next);
        self.next += 1;
        let name = source.plugin_name();
        let (model, errors) = match source.read() {
            Ok(m) => (Some(m), Vec::new()),
            Err(e) => (None, vec![e]),
        };
        let err = errors.first().cloned();
        self.specs.push(Spec { id, source, name: name.clone(), model, errors, running: false, deferred: None });
        match err {
            Some(e) => {
                self.log.push((Level::Error, format!("Plugin {name} failed to load: {e}")));
                Err(e)
            }
            None => {
                self.cmds.push(Cmd::Load(id));
                Ok(id)
            }
        }
    }

    /// Loads every plugin file in `dir`, in Studio's order.
    pub fn load_dir(&mut self, dir: &std::path::Path) -> Vec<Result<PluginId, String>> {
        match source::dir_order(dir) {
            Ok(list) => list.into_iter().map(|src| self.load(src)).collect(),
            Err(e) => {
                self.log.push((Level::Error, format!("Plugins folder: {e}")));
                vec![Err(e)]
            }
        }
    }

    /// Loads Roblox Studio's built-in plugins from `dir` (`Optimized_Embedded_Signature`).
    /// Unless an explicit `filter` or `RUSTUDIO_BUILTIN_PLUGINS_EAGER=1` is set,
    /// plugins with a deferred manifest register lightweight ribbon/action stubs
    /// and materialize from disk only on first use.
    pub fn load_builtin_dir(&mut self, dir: &std::path::Path, filter: Option<&[String]>) -> Vec<Result<PluginId, String>> {
        self.load_builtin_dir_with_choices(
            dir,
            filter,
            rustudio_core::rustudio_settings::BuiltinToolChoices::all_roblox(),
        )
    }

    /// Loads Roblox Studio's built-in plugins from `dir`, skipping any
    /// duplicated tool plugin whose choice is `ToolImpl::Rustudio` (when not
    /// overridden by an explicit `filter` or `RUSTUDIO_BUILTIN_PLUGINS_EAGER=1`).
    pub fn load_builtin_dir_with_choices(
        &mut self,
        dir: &std::path::Path,
        filter: Option<&[String]>,
        choices: rustudio_core::rustudio_settings::BuiltinToolChoices,
    ) -> Vec<Result<PluginId, String>> {
        let eager = filter.is_some() || builtin_lazy::eager_env();
        if !choices.plugins_enabled && filter.is_none() {
            return Vec::new();
        }
        match source::builtin_dir_order(dir, filter) {
            Ok(list) => list
                .into_iter()
                .filter(|src| {
                    if eager {
                        return true;
                    }
                    let Some(stem) = src.path().and_then(|p| p.file_stem()).and_then(|s| s.to_str()) else {
                        return true;
                    };
                    choices.should_load_builtin_stem(stem)
                })
                .map(|src| {
                    if !eager {
                        if let Some(manifest) = src.path().and_then(builtin_lazy::manifest_for_path) {
                            return self.register_deferred(src, manifest);
                        }
                    }
                    self.load(src)
                })
                .collect(),
            Err(e) => {
                self.log.push((Level::Error, format!("Built-in plugins folder: {e}")));
                vec![Err(e)]
            }
        }
    }

    /// Loads Studio's built-in plugins from the Studio install (honoring
    /// `RUSTUDIO_BUILTIN_PLUGINS_DIR`, `RUSTUDIO_BUILTIN_PLUGINS` filter, and
    /// per-tool native/Roblox choices).
    pub fn load_builtins(&mut self) -> Vec<Result<PluginId, String>> {
        self.load_builtins_with_choices(rustudio_core::rustudio_settings::BuiltinToolChoices::from_settings())
    }

    /// Loads Studio's built-in plugins using an explicit `BuiltinToolChoices`
    /// snapshot.
    pub fn load_builtins_with_choices(
        &mut self,
        choices: rustudio_core::rustudio_settings::BuiltinToolChoices,
    ) -> Vec<Result<PluginId, String>> {
        if !choices.plugins_enabled {
            return Vec::new();
        }
        let Some(dir) = rustudio_core::core_scripts::builtin_plugins_dir() else {
            self.log.push((
                Level::Warn,
                format!(
                    "Built-in plugins enabled, but no {} folder was found in the Studio install",
                    rustudio_core::core_scripts::BUILTIN_PLUGINS_DIR
                ),
            ));
            return Vec::new();
        };
        let filter = rustudio_core::rustudio_settings::builtin_plugins_filter();
        self.load_builtin_dir_with_choices(&dir, filter.as_deref(), choices)
    }

    /// Replaces the currently registered built-in plugins when a new place
    /// opens (so changes to `builtin_plugins` or per-tool choices apply on
    /// place reopen without restarting).
    pub fn replace_builtins(&mut self, choices: rustudio_core::rustudio_settings::BuiltinToolChoices) {
        let builtin_ids: std::collections::HashSet<PluginId> = self
            .specs
            .iter()
            .filter(|s| s.source.is_builtin())
            .map(|s| s.id)
            .collect();
        if !builtin_ids.is_empty() {
            self.specs.retain(|s| !builtin_ids.contains(&s.id));
            self.pending_triggers.retain(|p| !builtin_ids.contains(&p.plugin()));
            self.cmds.retain(|c| match c {
                Cmd::Load(id) | Cmd::Wake(id) | Cmd::Unload(id) | Cmd::Reload(id) => !builtin_ids.contains(id),
                Cmd::Trigger(Trigger::Deactivate(id)) => !builtin_ids.contains(id),
                Cmd::Trigger(_) => true,
            });
            self.deferred_rev = self.deferred_rev.wrapping_add(1);
        }
        if self.enabled && choices.plugins_enabled {
            let _ = self.load_builtins_with_choices(choices);
        }
    }

    /// Stops and removes a plugin (Unloading fires first, at the next frame).
    pub fn unload(&mut self, id: PluginId) {
        self.cmds.push(Cmd::Unload(id));
    }

    /// Reads the plugin's source again and restarts it (unload, then load).
    pub fn reload(&mut self, id: PluginId) {
        self.cmds.push(Cmd::Reload(id));
    }

    pub fn loaded(&self) -> Vec<PluginInfo> {
        self.specs
            .iter()
            .map(|s| PluginInfo {
                id: s.id,
                name: s.name.clone(),
                source: s.source.clone(),
                running: s.running,
                errors: s.errors.clone(),
            })
            .collect()
    }

    /// `--result`'s "plugins": each plugin's name, source, whether it runs,
    /// and its load / script errors.
    pub fn result_json(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.specs
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id.0,
                        "name": s.name,
                        "source": s.source.path().map(|p| p.display().to_string()),
                        "running": s.running,
                        "deferred": s.deferred.is_some(),
                        "errors": s.errors,
                    })
                })
                .collect(),
        )
    }

    /// Edit-DataModel ribbon / shortcuts model combining running plugins with
    /// deferred built-in stubs in load order.
    pub fn ui_model_for_edit(&self, doc: &Document) -> ui_ribbon::UiModel {
        let mut live = match self.edit_lua() {
            Some(lua) => ui_ribbon::snapshot(lua, doc),
            None => ui_ribbon::UiModel::default(),
        };
        if !self.has_deferred() {
            return live;
        }
        let mut live_groups: std::collections::HashMap<PluginId, Vec<api::toolbar::RibbonGroup>> =
            std::collections::HashMap::new();
        for g in live.groups.drain(..) {
            live_groups.entry(g.plugin).or_default().push(g);
        }
        let mut groups = Vec::new();
        let mut actions = std::mem::take(&mut live.actions);
        for s in &self.specs {
            if let Some(stub) = &s.deferred {
                groups.extend(stub.groups.iter().cloned());
                actions.extend(stub.actions.iter().cloned());
            } else if let Some(gs) = live_groups.remove(&s.id) {
                groups.extend(gs);
            }
        }
        for (_, gs) in live_groups {
            groups.extend(gs);
        }
        ui_ribbon::UiModel { at: live.at, groups, actions, menus: live.menus }
    }

    /// Fires a toolbar button's Click or a PluginAction's Triggered by name
    /// at the next frame (the harness's `--action plugin-button:…`).
    pub fn trigger(&mut self, t: Trigger) {
        self.cmds.push(Cmd::Trigger(t));
    }

    /// Output lines from the host and the edit VM since the last call.
    pub fn take_log(&mut self) -> Vec<(Level, String)> {
        std::mem::take(&mut self.log)
    }

    /// What plugins asked of the editor since the last call.
    pub fn take_requests(&mut self) -> Vec<EditorRequest> {
        std::mem::take(&mut self.requests)
    }

    /// True once after a plugin activated or deactivated (the editor drops
    /// its own tool when one activates: `active_plugin`).
    pub fn take_activation_change(&mut self) -> bool {
        std::mem::take(&mut self.activation_changed)
    }

    /// The edit VM's Lua state, for reading plugin UI state between frames
    /// (no document entered: don't call into the runtime with it).
    pub fn edit_lua(&self) -> Option<&mlua::Lua> {
        self.edit.as_ref().map(|e| &e.rt.lua)
    }

    /// Mirrors `EditAudio`'s loaded asset lengths into the edit VM's
    /// `World.input` so Edit Lua reads the same `IsLoaded` / `TimeLength`
    /// values as Properties and MCP. (In Studio 0.748 Edit mode, `Playing`,
    /// `IsPlaying`, `IsPaused`, and `TimePosition` come directly from the DOM
    /// and Edit preview is invisible to `Sound` scripts.)
    pub fn sync_edit_audio_to_vm(&mut self, lengths: &HashMap<Ref, f64>) {
        let Some(w) = self.edit.as_mut().and_then(|e| e.world.as_mut()) else {
            return;
        };
        for (&r, &len) in lengths {
            w.input.sound_len.insert(r, len);
        }
    }

    /// Clears transient playback flags in the edit VM when entering/leaving Play.
    pub fn stop_edit_audio_vm_state(&mut self) {
        if let Some(w) = self.edit.as_mut().and_then(|e| e.world.as_mut()) {
            w.input.sound_playing.clear();
            w.input.sound_paused.clear();
        }
    }

    /// Drains `PlayLocalSound` / listener requests queued by Edit Lua (Command
    /// Bar, plugins, MCP `run_luau`).
    pub fn take_edit_audio_requests(
        &mut self,
    ) -> (
        Vec<(Ref, rustudio_engine::runtime::ext::SoundCmd)>,
        Vec<(Ref, f64)>,
        Option<(u32, Option<rustudio_core::math::Xform>, Option<Ref>)>,
    ) {
        let Some(w) = self.edit.as_mut().and_then(|e| e.world.as_mut()) else {
            return (Vec::new(), Vec::new(), None);
        };
        (
            std::mem::take(&mut w.requests.sounds),
            std::mem::take(&mut w.requests.sound_seeks),
            w.input.sound_listener,
        )
    }

    /// Delivers `EditAudio` asset load/unload events (`Loaded`, `IsLoaded` /
    /// `IsReady`, `TimeLength`) into the Edit VM when one is running.
    pub fn deliver_edit_audio_events(
        &mut self,
        doc: &mut Document,
        events: &rustudio_player::edit_audio::EditAudioEvents,
    ) {
        if self.edit.is_none() || events.is_empty() {
            return;
        }
        let _ = self.with_edit(doc, |rt| {
            let lua = &rt.lua;
            for r in &events.unloaded {
                let _ = rustudio_engine::runtime::domains::media::clear_sound_asset(lua, *r);
            }
            for (r, id, len) in &events.loaded {
                if let Ok(mut w) = rustudio_engine::runtime::world(lua) {
                    w.input.sound_len.insert(*r, *len);
                    let class = w.d().class_of(*r);
                    match class.as_deref() {
                        Some("Sound") => {
                            if let Ok(v) = lua.create_string(id) {
                                w.fire(*r, "Loaded", vec![Value::String(v)]);
                            }
                            if let Ok(v) = lua.create_string("IsLoaded") {
                                w.fire(*r, "Changed", vec![Value::String(v)]);
                                w.fire(*r, "Changed:IsLoaded", vec![]);
                            }
                            if let Ok(v) = lua.create_string("TimeLength") {
                                w.fire(*r, "Changed", vec![Value::String(v)]);
                                w.fire(*r, "Changed:TimeLength", vec![]);
                            }
                        }
                        Some("AudioPlayer") => {
                            if let Ok(v) = lua.create_string("IsReady") {
                                w.fire(*r, "Changed", vec![Value::String(v)]);
                                w.fire(*r, "Changed:IsReady", vec![]);
                            }
                            if let Ok(v) = lua.create_string("TimeLength") {
                                w.fire(*r, "Changed", vec![Value::String(v)]);
                                w.fire(*r, "Changed:TimeLength", vec![]);
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    /// The activated plugin in the edit DataModel, if any.
    pub fn active_plugin(&self) -> Option<PluginId> {
        let e = self.edit.as_ref()?;
        e.rt.lua.app_data_ref::<vm::PluginVm>()?.active
    }

    /// The edit VM's plugin UI objects (toolbars, buttons, actions, menus,
    /// widgets, mice) for the UI.
    pub fn ui_items(&self) -> Vec<UiItem> {
        self.edit
            .as_ref()
            .and_then(|e| e.rt.lua.app_data_ref::<vm::PluginVm>().map(|v| v.ui.clone()))
            .unwrap_or_default()
    }

    /// Monotonic counter bumped whenever the edit VM's plugin UI items change.
    pub fn ui_rev(&self) -> u64 {
        self.edit
            .as_ref()
            .and_then(|e| e.rt.lua.app_data_ref::<vm::PluginVm>().map(|v| v.ui_rev))
            .unwrap_or(0)
    }

    /// The active plugin's PluginMouse Ref, without cloning the full `ui` list.
    pub fn active_mouse(&self) -> Option<Ref> {
        let e = self.edit.as_ref()?;
        let v = e.rt.lua.app_data_ref::<vm::PluginVm>()?;
        let active = v.active?;
        v.ui.iter()
            .find(|u| u.plugin == active && matches!(u.kind, UiKind::Mouse))
            .map(|u| u.r)
    }

    /// Runs `f` with the edit VM's runtime and the edit document entered
    /// (the UI's entry for input: PluginMouse events, widget input,
    /// button clicks). Output goes to the host's log.
    pub fn with_edit<R>(&mut self, doc: &mut Document, f: impl FnOnce(&Runtime) -> R) -> Option<R> {
        let e = self.edit.as_mut()?;
        if e.doc_id != doc.id {
            return None;
        }
        let mut w = e.world.take()?;
        // Edits the editor made since the last frame reach the plugins
        // first (signals, deleted instances lent back): otherwise the
        // after_frame below would drop their marks, and threads this resumes
        // would find a deleted instance gone.
        let pending = self.sync.take(doc);
        w.doc = Some(std::mem::replace(doc, Document::placeholder()));
        // (the plugins' GUI channel: widgets_ui.rs)
        let _ch = rustudio_engine::gui::share_channel(1);
        e.rt.enter(w);
        let lent = pending.fire(&e.rt);
        if let Ok(mut w) = rustudio_engine::runtime::world(&e.rt.lua) {
            for r in lent {
                w.orphans.remove(&r);
            }
        }
        let r = f(&e.rt);
        e.rt.dispatch();
        let mut w = e.rt.leave();
        *doc = w.doc.take().expect("edit document returned");
        self.log.extend(std::mem::take(&mut w.output));
        e.world = Some(w);
        self.sync.after_frame(doc);
        Some(r)
    }

    /// `with_edit`, starting the edit VM first when no plugin has (the MCP
    /// server's Luau in the edit DataModel). Once started it ticks with the
    /// editor like a plugin's, so a chunk that yields resumes.
    pub fn with_edit_vm<R>(&mut self, doc: &mut Document, f: impl FnOnce(&Runtime) -> R) -> Result<R, String> {
        if self.edit.as_ref().is_some_and(|e| e.doc_id != doc.id) {
            return Err("The open place just changed; call again".into());
        }
        if self.edit.is_none() {
            let (rt, world) = new_vm(RunMode::Edit)?;
            if let Some(ctx) = &self.repaint_context {
                rt.lua.set_app_data(ctx.clone());
            }
            self.edit = Some(EditVm { rt, world: Some(world), doc_id: doc.id, bound: false, gc_t: 0.0 });
        }
        if self.edit.as_ref().is_some_and(|e| !e.bound) {
            studio_place_name(doc);
            let bound = self
                .with_edit(doc, |rt| rt.bind_globals().and_then(|_| edit_loadstring(rt)).map_err(|e| rustudio_engine::runtime::describe_error(&e)))
                .ok_or("The edit DataModel's runtime is busy")?;
            bound?;
            if let Some(e) = self.edit.as_mut() {
                e.bound = true;
            }
        }
        self.with_edit(doc, f).ok_or_else(|| "The edit DataModel's runtime is busy".into())
    }

    /// One edit frame: pending loads / unloads, edits made outside the VM
    /// as signals, RunService's edit-mode events (Heartbeat; RenderStepped
    /// and PreRender too unless Play is running, as Studio's edit
    /// DataModel), then due waits. `doc` is the edit document (the saved
    /// one during Play).
    pub fn tick(&mut self, doc: &mut Document, dt: f64, rendering: bool, realtime: bool) {
        self.tick_with_pre(doc, dt, rendering, realtime, |_| {});
    }

    /// Same as [`tick`], running `pre` inside the same VM enter/leave pass
    /// right after editor-side edits are synced and before plugin commands and
    /// RunService events run (avoids entering/leaving the VM twice per frame).
    pub fn tick_with_pre(
        &mut self,
        doc: &mut Document,
        dt: f64,
        rendering: bool,
        realtime: bool,
        pre: impl FnOnce(&Runtime),
    ) {
        if self.specs.is_empty() && self.cmds.is_empty() && self.edit.is_none() {
            return;
        }
        // Wake deferred plugins whose selection class condition matches now
        // (e.g. LightGuides when a Light is selected, PathEditor for Path2D).
        if !doc.selection.is_empty() && self.specs.iter().any(|s| s.deferred.is_some()) {
            let mut wake_ids = Vec::new();
            for s in &self.specs {
                let Some(stub) = &s.deferred else { continue };
                let hit = stub.manifest.wake.iter().any(|w| match w {
                    builtin_lazy::WakeOn::SelectionClass(cls) => doc
                        .selection
                        .iter()
                        .any(|r| doc.class_of(*r).is_some_and(|c| rustudio_core::doc::is_a(&c, cls))),
                    _ => false,
                });
                if hit {
                    wake_ids.push(s.id);
                }
            }
            for id in wake_ids {
                self.cmds.push(Cmd::Wake(id));
            }
        }
        if self.edit.is_none() && self.cmds.is_empty() && self.specs.iter().all(|s| s.deferred.is_some()) {
            return;
        }
        let was_clean = !doc.dirty();
        let rev = doc.revision;
        // A different document: running plugins load again into it; deferred
        // stubs stay deferred until used.
        if self.edit.as_ref().is_some_and(|e| e.doc_id != doc.id) {
            self.edit = None;
            self.sync.reset();
            self.pending_triggers.clear();
            script_docs::reset();
            for s in &mut self.specs {
                s.running = false;
            }
            let ids: Vec<PluginId> = self.specs.iter().filter(|s| s.deferred.is_none()).map(|s| s.id).collect();
            let mut cmds: Vec<Cmd> = ids.into_iter().map(Cmd::Load).collect();
            cmds.append(&mut self.cmds);
            self.cmds = cmds;
            if self.cmds.is_empty() && self.specs.iter().all(|s| s.deferred.is_some()) {
                return;
            }
        }
        if self.edit.is_none() {
            if self.specs.is_empty() {
                self.cmds.clear();
                return;
            }
            match new_vm(RunMode::Edit) {
                Ok((rt, world)) => {
                    if let Some(ctx) = &self.repaint_context {
                        rt.lua.set_app_data(ctx.clone());
                    }
                    self.edit = Some(EditVm { rt, world: Some(world), doc_id: doc.id, bound: false, gc_t: 0.0 })
                }
                Err(e) => {
                    self.log.push((Level::Error, format!("Could not start the plugin runtime: {e}")));
                    self.cmds.clear();
                    return;
                }
            }
        }
        studio_place_name(doc);
        let pending = self.sync.take(doc);
        let cmds = std::mem::take(&mut self.cmds);
        let specs = &mut self.specs;
        let log = &mut self.log;
        let deferred_rev = &mut self.deferred_rev;
        let pending_triggers = &mut self.pending_triggers;
        let e = self.edit.as_mut().expect("edit vm");
        let Some(mut w) = e.world.take() else { return };
        let _ch = rustudio_engine::gui::share_channel(1);
        w.realtime = realtime;
        w.signal_mode = rustudio_engine::runtime::signal_timing::SignalMode::of(doc);
        w.doc = Some(std::mem::replace(doc, Document::placeholder()));
        e.rt.enter(w);
        if !e.bound {
            e.bound = true;
            if let Err(err) = e.rt.bind_globals().and_then(|_| edit_loadstring(&e.rt)) {
                log.push((Level::Error, rustudio_engine::runtime::describe_error(&err)));
            }
        }
        let lent = pending.fire(&e.rt);
        pre(&e.rt);
        let mut loaded_any = false;
        for c in cmds {
            match c {
                Cmd::Load(id) => loaded_any |= run_load(&e.rt, specs, id, log),
                Cmd::Wake(id) => {
                    if let Some(s) = specs.iter_mut().find(|s| s.id == id) {
                        if s.deferred.take().is_some() {
                            *deferred_rev = deferred_rev.wrapping_add(1);
                            loaded_any |= run_load(&e.rt, specs, id, log);
                        }
                    }
                }
                Cmd::Unload(id) => {
                    if specs.iter().any(|s| s.id == id && s.deferred.is_some()) {
                        *deferred_rev = deferred_rev.wrapping_add(1);
                    }
                    pending_triggers.retain(|p| p.plugin() != id);
                    vm::unload(&e.rt, id);
                    specs.retain(|s| s.id != id);
                }
                Cmd::Reload(id) => {
                    vm::unload(&e.rt, id);
                    if let Some(s) = specs.iter_mut().find(|s| s.id == id) {
                        if s.deferred.take().is_some() {
                            *deferred_rev = deferred_rev.wrapping_add(1);
                        }
                        s.running = false;
                        s.errors.clear();
                        match s.source.read() {
                            Ok(m) => s.model = Some(m),
                            Err(err) => {
                                log.push((Level::Error, format!("Plugin {} failed to load: {err}", s.name)));
                                s.errors.push(err);
                                s.model = None;
                            }
                        }
                    }
                    loaded_any |= run_load(&e.rt, specs, id, log);
                }
                Cmd::Trigger(t) => {
                    if let Some(target) = resolve_deferred_trigger(&e.rt, specs, &t) {
                        let pid = match &target {
                            builtin_lazy::DeferredTarget::Button { plugin, .. }
                            | builtin_lazy::DeferredTarget::Action { plugin, .. } => *plugin,
                        };
                        if let Some(s) = specs.iter_mut().find(|s| s.id == pid) {
                            if s.deferred.take().is_some() {
                                *deferred_rev = deferred_rev.wrapping_add(1);
                            }
                        }
                        if run_load(&e.rt, specs, pid, log) {
                            loaded_any = true;
                            match target {
                                builtin_lazy::DeferredTarget::Button { plugin, toolbar, id, event } => {
                                    pending_triggers.push(PendingTrigger::Button {
                                        plugin,
                                        toolbar,
                                        id,
                                        event,
                                        frames_left: 20,
                                    });
                                }
                                builtin_lazy::DeferredTarget::Action { plugin, id } => {
                                    pending_triggers.push(PendingTrigger::Action {
                                        plugin,
                                        id,
                                        frames_left: 20,
                                    });
                                }
                            }
                        }
                    } else {
                        fire_trigger(&e.rt, &t, log);
                    }
                }
            }
        }
        if loaded_any {
            vm::start_later(&e.rt);
        }
        if !pending_triggers.is_empty() {
            flush_pending_triggers(&e.rt, pending_triggers, log);
        }
        let rs_listening = rustudio_engine::runtime::world(&e.rt.lua)
            .ok()
            .and_then(|w| {
                let d = w.d();
                let rs = d.find_child(d.dom.root_ref(), "RunService")?;
                w.listening.contains(&rs).then_some(rs)
            });
        if let Some(rs) = rs_listening {
            e.rt.fire(rs, "Heartbeat", vec![Value::Number(dt)]);
            if rendering {
                e.rt.fire(rs, "RenderStepped", vec![Value::Number(dt)]);
                e.rt.fire(rs, "PreRender", vec![Value::Number(dt)]);
            }
        }
        e.rt.step(dt);
        if !pending_triggers.is_empty() {
            flush_pending_triggers(&e.rt, pending_triggers, log);
        }
        // A selected instance a plugin destroyed or unparented leaves the
        // selection, and SelectionChanged fires (Studio: Destroy of a
        // selected part fires once).
        if let Ok(mut w) = rustudio_engine::runtime::world(&e.rt.lua) {
            let d = w.d();
            let root = d.dom.root_ref();
            let keep: Vec<_> = d.selection.iter().copied().filter(|r| d.is_ancestor(root, *r)).collect();
            if keep.len() != d.selection.len() {
                w.dm().selection = keep;
                drop(w);
                if let Some(mut v) = vm::vm(&e.rt.lua) {
                    v.selection_fires += 1;
                }
            }
        }
        // Selection:Set / Add / Remove of this frame, after the callers yielded.
        let fires = vm::vm(&e.rt.lua).map(|mut v| std::mem::take(&mut v.selection_fires)).unwrap_or(0);
        if fires > 0 {
            let sel = rustudio_engine::runtime::world(&e.rt.lua).ok().and_then(|w| {
                let d = w.d();
                d.find_child(d.dom.root_ref(), "Selection")
            });
            if let Some(sel) = sel {
                for _ in 0..fires {
                    e.rt.fire(sel, "SelectionChanged", vec![]);
                    e.rt.fire(sel, "SelectionChangedThisFrame", vec![]);
                }
                e.rt.dispatch();
            }
        }
        // The person switched rustudio's theme: settings().Studio.ThemeChanged.
        match crate::plugin_host::api::theme::watch_theme(&e.rt.lua) {
            Ok(true) => e.rt.dispatch(),
            Ok(false) => {}
            Err(err) => log.push((Level::Error, rustudio_engine::runtime::describe_error(&err))),
        }
        // Source writes that waited for a script-injection prompt.
        if permissions::retry_source_writes(&e.rt.lua) {
            e.rt.dispatch();
        }
        match crate::plugin_host::api::script_editor::fire_events(&e.rt.lua) {
            Ok(true) => e.rt.dispatch(),
            Ok(false) => {}
            Err(err) => log.push((Level::Error, rustudio_engine::runtime::describe_error(&err))),
        }
        match crate::plugin_host::api::studio::apply_pending_tool(&e.rt.lua) {
            Ok(true) => e.rt.dispatch(),
            Ok(false) => {}
            Err(err) => log.push((Level::Error, rustudio_engine::runtime::describe_error(&err))),
        }
        // ChangeHistoryService's OnUndo / OnRedo / recording events of this
        // frame (plugins' and the editor's Ctrl+Z), after the callers yielded.
        match crate::plugin_host::api::history::fire_events(&e.rt.lua) {
            Ok(true) => e.rt.dispatch(),
            Ok(false) => {}
            Err(err) => log.push((Level::Error, rustudio_engine::runtime::describe_error(&err))),
        }
        // Instances the editor removed stay lent (in the document,
        // parentless): Studio's deleted instances are still there for a
        // plugin holding them (Parent nil, properties readable); undo / redo
        // take them from there (doc.rs), and the change history pins them.
        let lent: Vec<_> = lent.into_iter().chain(crate::plugin_host::api::history::take_lent(&e.rt.lua)).collect();
        if let Ok(mut w) = rustudio_engine::runtime::world(&e.rt.lua) {
            for r in lent {
                w.orphans.remove(&r);
            }
        }
        e.gc_t += dt;
        let gc_due = e.gc_t > 2.0;
        if gc_due {
            e.gc_t = 0.0;
            let mut pins = vm::pinned(&e.rt.lua);
            // (What the change history can still bring back stays.)
            if let Ok(w) = rustudio_engine::runtime::world(&e.rt.lua) {
                pins.extend(w.d().history_refs());
            }
            e.rt.collect(&|r| pins.contains(&r), rustudio_core::mech::GC_BUDGET);
        }
        let gc_busy = rustudio_engine::runtime::world(&e.rt.lua).is_ok_and(|w| w.gc_busy);
        if gc_busy || gc_due {
            e.rt.gc_step(if gc_due && !gc_busy { 2.0 } else { dt });
        }
        let (reqs, seq) = match vm::vm(&e.rt.lua) {
            Some(mut v) => (std::mem::take(&mut v.requests), v.activation_seq),
            None => (Vec::new(), 0),
        };
        let mut w = e.rt.leave();
        *doc = w.doc.take().expect("edit document returned");
        let out = std::mem::take(&mut w.output);
        e.world = Some(w);
        attribute_errors(specs, &out);
        log.extend(out);
        self.requests.extend(reqs);
        if seq != self.activation_seen {
            self.activation_seen = seq;
            self.activation_changed = true;
        }
        let touched = self.sync.after_frame(doc);
        self.sync.note_selection(doc);
        // Plugin bookkeeping (its own instances, out of the DataModel)
        // doesn't make the place unsaved.
        if was_clean && doc.revision != rev && !touched {
            doc.mark_saved_at(doc.revision);
        }
    }

    /// Loads every running plugin into a Play DataModel's runtime (call
    /// with that side entered, after bind_globals), as Studio loads them in
    /// the Play server and client. Their Output is the side's.
    pub fn load_into_play(&self, rt: &Runtime) {
        if !self.enabled {
            return;
        }
        load_play_plugins(
            rt,
            self.specs
                .iter()
                .filter(|s| s.running && !s.source.is_builtin())
                .filter_map(Spec::load_spec),
        );
    }

    /// The Play DataModel is closing: its plugins get Unloading (Studio
    /// unloads plugins with their DataModel).
    pub fn unload_from_play(&self, rt: &Runtime) {
        unload_play_plugins(rt);
    }

    pub(crate) fn play_worker_plugins(&self) -> PlayPlugins {
        *self.play_ui.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
        let specs = self
            .specs
            .iter()
            .filter(|s| self.enabled && s.running && !s.source.is_builtin())
            .filter_map(Spec::load_spec)
            .collect();
        PlayPlugins { specs, ui: self.play_ui.clone() }
    }

    pub(crate) fn play_widgets(&self, side: Side) -> Vec<UiItem> {
        self.play_ui.lock().unwrap_or_else(|e| e.into_inner())[play_side_index(side)].widgets.clone()
    }

    pub(crate) fn play_ribbon(&self, side: Side) -> ui_ribbon::UiModel {
        self.play_ui.lock().unwrap_or_else(|e| e.into_inner())[play_side_index(side)].ribbon.clone()
    }
}

fn load_play_plugins(rt: &Runtime, specs: impl IntoIterator<Item = vm::LoadSpec>) {
    let mut any = false;
    for spec in specs {
        if let Err(e) = vm::load(rt, &spec) {
            if let Ok(mut w) = rustudio_engine::runtime::world(&rt.lua) {
                w.output.push((Level::Error, format!("Plugin {} failed to load: {e}", spec.name)));
            }
        }
        any = true;
    }
    if any {
        vm::start_later(rt);
        rt.dispatch();
    }
}

fn unload_play_plugins(rt: &Runtime) {
        let ids: Vec<PluginId> = rt
            .lua
            .app_data_ref::<vm::PluginVm>()
            .map(|v| v.plugins.iter().map(|p| p.id).collect())
            .unwrap_or_default();
        for id in ids {
            vm::unload(rt, id);
        }
}

impl crate::game::GameHost for PlayPlugins {
    fn load_plugins(&mut self, rt: &Runtime) {
        load_play_plugins(rt, self.specs.iter().cloned());
    }

    fn unload_plugins(&mut self, rt: &Runtime) {
        unload_play_plugins(rt);
    }

    fn plugin_pins(&self, lua: &mlua::Lua) -> std::collections::HashSet<Ref> {
        vm::pinned(lua)
    }

    fn keep_play_layout(
        &self,
        old: &std::collections::HashMap<Ref, rustudio_engine::gui::GuiBox>,
        new: &mut std::collections::HashMap<Ref, rustudio_engine::gui::GuiBox>,
    ) {
        widgets_ui::keep_play_layout(old, new);
    }

    fn capture_play_ui(&mut self, game: &mut crate::game::Game) -> Option<Box<dyn FnOnce() + Send>> {
        if self.specs.is_empty() {
            return None;
        }
        let mut all = <[PlayUi; 2]>::default();
        for side in game.play_sides() {
            all[play_side_index(side)] = game.with_play_side(side, |rt, _, _| {
                let widgets = rt.lua.app_data_ref::<vm::PluginVm>()
                    .map(|v| v.ui.clone()).unwrap_or_default();
                let ribbon = rustudio_engine::runtime::world(&rt.lua)
                    .map(|w| ui_ribbon::snapshot(&rt.lua, w.d())).unwrap_or_default();
                PlayUi { widgets, ribbon }
            }).unwrap_or_default();
        }
        let ui = self.ui.clone();
        Some(Box::new(move || *ui.lock().unwrap_or_else(|e| e.into_inner()) = all))
    }
}

/// The edit DataModel's Name as Studio's (measured in Studio: "Place1" for a
/// new place; a file's name for a file, a Roblox place's title for one);
/// Play's copies are "Game" (doc.rs clone_for_play). Not journaled: it is
/// not part of the place.
fn studio_place_name(doc: &mut Document) {
    let want: std::borrow::Cow<'_, str> = match (&doc.path, &doc.cloud) {
        // (a local file: its name without the extension; unmeasured)
        (Some(p), _) => p
            .file_stem()
            .map(|n| n.to_string_lossy())
            .unwrap_or(std::borrow::Cow::Borrowed("Place1")),
        (None, Some(_)) => std::borrow::Cow::Borrowed(doc.title.as_str()),
        (None, None) => std::borrow::Cow::Borrowed("Place1"),
    };
    let root = doc.dom.root_ref();
    if doc.dom.get_by_ref(root).is_some_and(|i| i.name == want.as_ref()) {
        return;
    }
    let name = want.into_owned();
    if let Some(i) = doc.dom.get_by_ref_mut(root) {
        i.name = name;
    }
}

/// loadstring works for plugins in the edit DataModel (measured in Studio:
/// `return 1+1` → 2 there; Play DataModels keep refusing it).
fn edit_loadstring(rt: &Runtime) -> mlua::Result<()> {
    let lua = &rt.lua;
    let f = lua.create_function(|lua, (src, name): (mlua::String, Option<String>)| {
        if !matches!(
            rustudio_engine::runtime::security::current(lua),
            rustudio_engine::runtime::security::PLUGIN_IDENT | rustudio_engine::runtime::security::BUILTIN_PLUGIN
        ) {
            return Err(mlua::Error::runtime("loadstring() is not available"));
        }
        let src = src.to_str()?.to_string();
        let env = rustudio_engine::globals_parity::new_env(lua, None)?;
        let chunk = lua.load(&src).set_name(name.unwrap_or_else(|| src.clone())).set_environment(env);
        match chunk.into_function() {
            Ok(f) => Ok((Value::Function(f), Value::Nil)),
            Err(e) => Ok((Value::Nil, Value::String(lua.create_string(rustudio_engine::runtime::describe_error(&e))?))),
        }
    })?;
    let shared: mlua::Table = lua.named_registry_value("rustudio_shared")?;
    shared.raw_set("loadstring", f.clone())?;
    lua.globals().raw_set("loadstring", f)?;
    Ok(())
}

fn new_vm(mode: RunMode) -> Result<(Runtime, World), String> {
    let rt = crate::studio_hooks::new_runtime()?;
    rt.install_side().map_err(|e| rustudio_engine::runtime::describe_error(&e))?;
    vm::install(&rt.lua);
    let mut w = World::new(Document::empty(), mode);
    w.doc = None;
    Ok((rt, w))
}

/// Loads one spec into the entered edit VM; true when it started.
fn run_load(rt: &Runtime, specs: &mut [Spec], id: PluginId, log: &mut Vec<(Level, String)>) -> bool {
    let Some(s) = specs.iter_mut().find(|s| s.id == id) else { return false };
    let res = if s.source.is_builtin() {
        // Move built-in plugin DOMs straight into the edit document rather than
        // cloning 170k+ instances and retaining a second copy in `Spec.model`.
        // Built-ins never load into Play, and a place switch re-reads from disk.
        let model = match s.model.take() {
            Some(m) => m,
            None => match s.source.read() {
                Ok(m) => m,
                Err(e) => {
                    log.push((Level::Error, format!("Plugin {} failed to load: {e}", s.name)));
                    s.errors.push(e);
                    return false;
                }
            },
        };
        let asset_id = s.source.asset_id();
        let settings_key = s.source.settings_key();
        let identity = rustudio_engine::runtime::security::BUILTIN_PLUGIN;
        match (Arc::try_unwrap(model.dom), Arc::try_unwrap(model.bytecode)) {
            (Ok(dom), Ok(bytecode)) => {
                vm::load_owned(rt, s.id, &s.name, asset_id, &settings_key, identity, true, dom, bytecode)
            }
            (dom_res, bc_res) => {
                let dom = match dom_res {
                    Ok(d) => Arc::new(d),
                    Err(a) => a,
                };
                let bytecode = match bc_res {
                    Ok(b) => Arc::new(b),
                    Err(a) => a,
                };
                let spec = vm::LoadSpec {
                    id: s.id,
                    name: s.name.clone(),
                    asset_id,
                    settings_key,
                    identity,
                    is_builtin: true,
                    tree: dom,
                    bytecode,
                };
                vm::load(rt, &spec)
            }
        }
    } else {
        let Some(spec) = s.load_spec() else { return false };
        vm::load(rt, &spec)
    };
    match res {
        Ok(_) => {
            s.running = true;
            rt.dispatch();
            true
        }
        Err(e) => {
            log.push((Level::Error, format!("Plugin {} failed to load: {e}", s.name)));
            s.errors.push(e);
            false
        }
    }
}

/// Error lines in Output that come from a plugin's code (its name starts
/// the message's script path, or a stack line's).
fn attribute_errors(specs: &mut [Spec], out: &[(Level, String)]) {
    // An error without a script path (an engine error) waits for its first
    // stack line, which names the script.
    let mut pending: Option<String> = None;
    for (level, text) in out {
        let t = text.trim_start_matches(STACK_MARK);
        let msg = t.split("  —").next().unwrap_or(t).to_string();
        if *level == Level::Error {
            pending = None;
            match specs.iter().position(|s| t.starts_with(&format!("{}.", s.name))) {
                Some(i) => specs[i].errors.push(msg),
                None => pending = Some(msg),
            }
        } else if let Some(rest) = t.strip_prefix("Script '") {
            if let Some(m) = pending.take() {
                if let Some(i) = specs.iter().position(|s| rest.starts_with(&format!("{}.", s.name))) {
                    specs[i].errors.push(m);
                }
            }
        } else if !t.starts_with("Stack Begin") {
            pending = None;
        }
    }
}

impl PendingTrigger {
    fn plugin(&self) -> PluginId {
        match self {
            PendingTrigger::Button { plugin, .. } | PendingTrigger::Action { plugin, .. } => *plugin,
        }
    }
}

/// Checks whether `t` targets a deferred plugin stub (returning the plugin and
/// button/action to invoke once materialized). Live buttons/actions always take
/// precedence over deferred stubs.
fn resolve_deferred_trigger(
    rt: &Runtime,
    specs: &[Spec],
    t: &Trigger,
) -> Option<builtin_lazy::DeferredTarget> {
    match t {
        Trigger::Fire(r, event) => specs
            .iter()
            .find_map(|s| s.deferred.as_ref().and_then(|d| d.find_ref(*r, event))),
        Trigger::Button(tb, b) => {
            let items = vm::vm(&rt.lua).map(|v| v.ui.clone()).unwrap_or_default();
            let live_hit = items.iter().any(|u| match &u.kind {
                UiKind::Button { toolbar, id, text, .. } => {
                    let tb_name = items.iter().find_map(|x| match &x.kind {
                        UiKind::Toolbar { name } if x.r == *toolbar => Some(name.as_str()),
                        _ => None,
                    });
                    (tb == "*" || tb_name == Some(tb.as_str())) && (id == b || text == b)
                }
                _ => false,
            });
            if live_hit {
                return None;
            }
            specs.iter().find_map(|s| {
                let (toolbar, id) = s.deferred.as_ref()?.matches_button(tb, b)?;
                Some(builtin_lazy::DeferredTarget::Button { plugin: s.id, toolbar, id, event: "Click" })
            })
        }
        Trigger::Action(a) => {
            let items = vm::vm(&rt.lua).map(|v| v.ui.clone()).unwrap_or_default();
            let live_hit = items.iter().any(|u| {
                matches!(&u.kind, UiKind::Action { id, .. } if id == a)
                    || rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| {
                        matches!(
                            w.d().prop(u.r, "ActionId"),
                            Some(rbx_dom_weak::types::Variant::String(s)) if s == *a
                        )
                    })
            });
            if live_hit {
                return None;
            }
            specs.iter().find_map(|s| {
                let id = s.deferred.as_ref()?.matches_action(a)?;
                Some(builtin_lazy::DeferredTarget::Action { plugin: s.id, id })
            })
        }
        Trigger::Menu(_) | Trigger::Deactivate(_) => None,
    }
}

/// Fires queued clicks / actions for freshly materialized deferred plugins once
/// their startup `CreateDockWidgetPluginGuiAsync` yield finishes and their
/// button/action listeners are attached. If loading the plugin already opened a
/// widget (`InitialEnabled = true`, e.g. Toolbox / TerrainEditor), a pending
/// button click is retired without toggling the panel back closed.
fn flush_pending_triggers(
    rt: &Runtime,
    pending: &mut Vec<PendingTrigger>,
    log: &mut Vec<(Level, String)>,
) {
    let items = vm::vm(&rt.lua).map(|v| v.ui.clone()).unwrap_or_default();
    let toolbar_name = |r: Ref| {
        items.iter().find_map(|u| match &u.kind {
            UiKind::Toolbar { name } if u.r == r => Some(name.as_str()),
            _ => None,
        })
    };
    let mut keep = Vec::new();
    for mut p in pending.drain(..) {
        let done = match &mut p {
            PendingTrigger::Button { plugin, toolbar, id, event, frames_left } => {
                // If loading already opened a widget for this plugin, don't
                // toggle it back off with a second Click.
                let already_open = rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| {
                    let d = w.d();
                    items.iter().any(|u| {
                        u.plugin == *plugin
                            && matches!(u.kind, UiKind::DockWidget { .. } | UiKind::QWidget { .. })
                            && matches!(d.prop(u.r, "Enabled"), Some(rbx_dom_weak::types::Variant::Bool(true)))
                    })
                });
                if already_open {
                    true
                } else {
                    let btn_ref = items
                        .iter()
                        .find(|u| {
                            u.plugin == *plugin
                                && matches!(
                                    &u.kind,
                                    UiKind::Button { toolbar: tb, id: bid, text, .. }
                                        if (toolbar == "*" || toolbar_name(*tb) == Some(toolbar.as_str()))
                                            && (bid == id || text == id)
                                )
                        })
                        .map(|u| u.r);
                    match btn_ref {
                        Some(r) => {
                            let listening =
                                rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| w.listening.contains(&r));
                            if listening || *frames_left == 0 {
                                rt.fire(r, event, vec![]);
                                rt.dispatch();
                                true
                            } else {
                                *frames_left -= 1;
                                false
                            }
                        }
                        None if *frames_left == 0 => {
                            log.push((
                                Level::Warn,
                                format!("--action: deferred plugin button {toolbar}/{id} did not appear"),
                            ));
                            true
                        }
                        None => {
                            *frames_left -= 1;
                            false
                        }
                    }
                }
            }
            PendingTrigger::Action { plugin, id, frames_left } => {
                let act_ref = items
                    .iter()
                    .find(|u| {
                        u.plugin == *plugin
                            && (matches!(&u.kind, UiKind::Action { id: aid, .. } if aid == id)
                                || rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| {
                                    matches!(
                                        w.d().prop(u.r, "ActionId"),
                                        Some(rbx_dom_weak::types::Variant::String(s)) if s == *id
                                    )
                                }))
                    })
                    .map(|u| u.r);
                match act_ref {
                    Some(r) => {
                        let listening =
                            rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| w.listening.contains(&r));
                        if listening || *frames_left == 0 {
                            rt.fire(r, "Triggered", vec![]);
                            rt.dispatch();
                            true
                        } else {
                            *frames_left -= 1;
                            false
                        }
                    }
                    None if *frames_left == 0 => {
                        log.push((
                            Level::Warn,
                            format!("--action: deferred plugin action {id} did not appear"),
                        ));
                        true
                    }
                    None => {
                        *frames_left -= 1;
                        false
                    }
                }
            }
        };
        if !done {
            keep.push(p);
        }
    }
    *pending = keep;
}

/// Fires a harness trigger in the entered VM.
fn fire_trigger(rt: &Runtime, t: &Trigger, log: &mut Vec<(Level, String)>) {
    let items = vm::vm(&rt.lua).map(|v| v.ui.clone()).unwrap_or_default();
    let toolbar_name = |r: Ref| {
        items.iter().find_map(|u| match &u.kind {
            UiKind::Toolbar { name } if u.r == r => Some(name.clone()),
            _ => None,
        })
    };
    if let Trigger::Menu(choice) = t {
        let answered = match rustudio_engine::runtime::world(&rt.lua) {
            Ok(w) => crate::plugin_host::api::menus::answer_top(&rt.lua, w.d(), choice),
            Err(_) => false,
        };
        if !answered {
            log.push((Level::Warn, format!("--action: no plugin menu is open to answer {choice:?}")));
        }
        return;
    }
    if let Trigger::Deactivate(id) = t {
        let root = vm::vm(&rt.lua).and_then(|v| v.by_id(*id).map(|p| p.root));
        if let Some(root) = root {
            if let Err(e) = crate::plugin_host::api::deactivate(&rt.lua, root) {
                log.push((Level::Error, rustudio_engine::runtime::describe_error(&e)));
            }
            rt.dispatch();
        }
        return;
    }
    if let Trigger::Fire(r, ev) = t {
        // RUSTUDIO_PLUGIN_TRACE: each UI fire and whether anything listens
        // on the object (checks/plugins/ui/check.py's corpus clicks).
        if std::env::var_os("RUSTUDIO_PLUGIN_TRACE").is_some() {
            let listening = rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| w.listening.contains(r));
            log.push((Level::Info, format!("plugin-ui: {ev} {r:?} listening {listening}")));
        }
        rt.fire(*r, ev, vec![]);
        rt.dispatch();
        return;
    }
    let target = match t {
        Trigger::Button(tb, b) => items
            .iter()
            .find(|u| match &u.kind {
                UiKind::Button { toolbar, id, text, .. } => {
                    // ("*": any toolbar, for the harness)
                    (tb == "*" || toolbar_name(*toolbar).as_deref() == Some(tb.as_str())) && (id == b || text == b)
                }
                _ => false,
            })
            .map(|u| (u.r, "Click")),
        Trigger::Action(a) => items
            .iter()
            .find(|u| {
                matches!(&u.kind, UiKind::Action { id, .. } if id == a)
                    || rustudio_engine::runtime::world(&rt.lua).is_ok_and(|w| {
                        matches!(w.d().prop(u.r, "ActionId"), Some(rbx_dom_weak::types::Variant::String(s)) if s == *a)
                    })
            })
            .map(|u| (u.r, "Triggered")),
        Trigger::Menu(_) | Trigger::Fire(..) | Trigger::Deactivate(_) => None,
    };
    match target {
        Some((r, ev)) => {
            rt.fire(r, ev, vec![]);
            rt.dispatch();
        }
        None => log.push((Level::Warn, format!("--action: no plugin {t:?} to fire"))),
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_tests;
#[cfg(test)]
#[path = "tests_services.rs"]
mod tests_services;
#[cfg(test)]
#[path = "tests_webstream.rs"]
mod tests_webstream;
