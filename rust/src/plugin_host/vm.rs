//! A plugin's life inside one Luau VM (the edit DataModel's, or a Play
//! session's server / client): its Plugin instance, the scripts it started,
//! the objects it made (mouse, toolbars, buttons, actions, menus, widgets),
//! its activation, and the per-VM state the Plugin API reads.
//!
//! The state lives in the VM as Lua app data ([`PluginVm`]), so the Plugin
//! API (src/plugin_host/api*.rs, a runtime domain) finds it from `&Lua`
//! alone, in whichever DataModel the call runs.
//!
//! Measured in Studio 0.747 (2026-11-14, local plugins in a hidden
//! Vinegar):
//! - The Plugin instance is named `user_<file name>` (local) or
//!   `cloud_<asset id>` (installed), has Parent nil and is not in the
//!   DataModel (GetFullName "user_ProbeA.rbxmx"); the model's top-level items
//!   are its children, then a MultipleDocumentInterfaceInstance; a bare
//!   .lua is one Script named "Script".
//! - Scripts and LocalScripts with RunContext Legacy run at load, in
//!   pre-order, each until it yields, before the next plugin loads; Disabled
//!   ones and RunContext Client / Server ones don't; RunContext Plugin ones
//!   start after the whole batch. ModuleScripts run on require.
//! - `plugin` is a global of the plugin's Scripts and LocalScripts only
//!   (nil in its ModuleScripts). Identity 5 everywhere (printidentity adds
//!   "Current asset id is -2" for a local plugin). `_G` and `shared` are
//!   shared by all plugins of a DataModel.
//! - Plugin.Name and Plugin.Parent can be written by the plugin.

use std::collections::{HashMap, HashSet};

use mlua::{Lua, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{InstanceBuilder, WeakDom};

use rustudio_core::core_scripts::Bytecode;
use rustudio_engine::runtime::{world, RunMode, Runtime};

use super::PluginId;

/// What the host hands a VM to load: the plugin's identity and its model.
#[derive(Clone)]
pub struct LoadSpec {
    pub id: PluginId,
    /// The Plugin instance's Name (`user_X.rbxmx`, `cloud_123`, `builtin_X.rbxm`).
    pub name: String,
    /// printidentity's "Current asset id": -2 for a local plugin, -1 for built-in.
    pub asset_id: i64,
    /// The settings file key: "0" for local/built-in plugins (they share one
    /// file), the asset id for installed ones.
    pub settings_key: String,
    /// Thread security identity (5 for user/cloud plugins, 6 for Studio built-ins).
    pub identity: u8,
    /// True for Roblox Studio built-in plugins (`BuiltInPlugins/Optimized_Embedded_Signature`).
    pub is_builtin: bool,
    /// The plugin's model: the root's children are its top-level items.
    pub tree: std::sync::Arc<WeakDom>,
    /// Precompiled Luau bytecode keyed by Ref in `tree` (built-in plugins).
    pub bytecode: std::sync::Arc<HashMap<Ref, Bytecode>>,
}

/// A plugin-made object the UI draws (toolbars, buttons, actions,
/// menus, widgets) or feed input to (the mouse).
#[derive(Clone, Debug)]
pub struct UiItem {
    pub plugin: PluginId,
    pub r: Ref,
    pub kind: UiKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiKind {
    Mouse,
    Toolbar { name: String },
    Button { toolbar: Ref, id: String, tooltip: String, icon: String, text: String },
    Action { id: String, text: String, status_tip: String, icon: String, allow_binding: bool },
    Menu { id: String, title: String, icon: String },
    DockWidget { id: String, info: DockInfo },
    QWidget { id: String, float_x: f64, float_y: f64 },
}

/// DockWidgetPluginGuiInfo.new's fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DockInfo {
    pub initial_dock_state: String,
    pub initial_enabled: bool,
    pub override_enabled_restore: bool,
    pub float_x: f64,
    pub float_y: f64,
    pub min_width: f64,
    pub min_height: f64,
}

/// One loaded plugin in this VM.
#[derive(Clone, Debug)]
pub struct VmPlugin {
    pub id: PluginId,
    pub root: Ref,
    pub name: String,
    pub asset_id: i64,
    pub settings_key: String,
    pub identity: u8,
    pub is_builtin: bool,
    /// Scripts started (each stopped on unload).
    pub scripts: Vec<Ref>,
    /// RunContext Plugin scripts waiting for the end of the load batch.
    pub later: Vec<Ref>,
    /// Every Script / LocalScript / ModuleScript of the plugin's model at
    /// load: on unload no thread runs their code any more (even moved out).
    pub code: Vec<Ref>,
    /// Objects the plugin made (destroyed on unload).
    pub owned: Vec<Ref>,
    pub mouse: Option<Ref>,
    pub activated: bool,
    pub exclusive_mouse: bool,
}

/// Every plugin of one VM, as Lua app data.
#[derive(Default)]
pub struct PluginVm {
    pub plugins: Vec<VmPlugin>,
    /// The plugin that is activated (one at a time, as in Studio).
    pub active: Option<PluginId>,
    /// Bumped when activation changes: the editor drops its own tool when
    /// a plugin activates (`PluginHost::take_activation_change`).
    pub activation_seq: u64,
    pub ui: Vec<UiItem>,
    /// Monotonic counter bumped whenever `ui`, button active state, or open
    /// menus change, so the ribbon can skip rebuilding its model on idle frames.
    pub ui_rev: u64,
    /// Hooks that warned already ("not implemented yet", once per VM).
    pub warned: HashSet<String>,
    /// Requests for the editor (open a script, pick a ribbon tool, …),
    /// drained by the host after each frame.
    pub requests: Vec<EditorRequest>,
    /// Selection:Set / Add / Remove calls that changed the selection this
    /// frame: one SelectionChanged each, fired after the callers yield
    ///.
    pub selection_fires: u32,
}

/// Something a plugin asked of the editor; the host passes these on.
#[derive(Clone, Debug, PartialEq)]
pub enum EditorRequest {
    OpenScript { script: Ref, line: i64 },
    OpenWikiPage(String),
    SelectRibbonTool { tool: String },
    StartPlaytest {
        mode: RunMode,
        args: String,
        completion: super::PlaytestCompletion,
    },
}

impl PluginVm {
    pub fn by_root(&self, root: Ref) -> Option<&VmPlugin> {
        self.plugins.iter().find(|p| p.root == root)
    }
    pub fn by_root_mut(&mut self, root: Ref) -> Option<&mut VmPlugin> {
        self.plugins.iter_mut().find(|p| p.root == root)
    }
    pub fn by_id(&self, id: PluginId) -> Option<&VmPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }
    /// The plugin an owned object (or the root itself) belongs to.
    pub fn owner_of(&self, r: Ref) -> Option<&VmPlugin> {
        self.plugins.iter().find(|p| p.root == r || p.owned.contains(&r) || p.mouse == Some(r))
    }
}

pub fn vm(lua: &Lua) -> Option<mlua::AppDataRefMut<'_, PluginVm>> {
    lua.app_data_mut::<PluginVm>()
}

/// Makes `lua` able to host plugins (once per VM).
pub fn install(lua: &Lua) {
    if lua.app_data_ref::<PluginVm>().is_none() {
        lua.set_app_data(PluginVm::default());
    }
}

/// The top of `r`'s ancestor chain that is a loaded plugin's root.
pub fn plugin_root_of(lua: &Lua, r: Ref) -> Option<Ref> {
    let roots: Vec<Ref> = lua.app_data_ref::<PluginVm>()?.plugins.iter().map(|p| p.root).collect();
    if roots.is_empty() {
        return None;
    }
    let w = world(lua).ok()?;
    let d = w.d();
    let mut cur = r;
    while let Some(i) = d.dom.get_by_ref(cur) {
        if roots.contains(&cur) {
            return Some(cur);
        }
        cur = i.parent();
    }
    None
}

/// The runtime's compile hook: a Script / LocalScript inside a loaded
/// plugin also gets the `plugin` global (ModuleScripts don't: measured).
pub fn script_env(lua: &Lua, r: Ref, env: &Table) -> mlua::Result<()> {
    if lua.app_data_ref::<PluginVm>().is_none_or(|v| v.plugins.is_empty()) {
        return Ok(());
    }
    let is_script = world(lua)?
        .d()
        .class_of(r)
        .is_some_and(|c| c == "Script" || c == "LocalScript");
    if !is_script {
        return Ok(());
    }
    if let Some(root) = plugin_root_of(lua, r) {
        env.raw_set("plugin", rustudio_engine::runtime::inst_value(lua, root)?)?;
    }
    Ok(())
}

/// printidentity's asset id for the running plugin thread: the plugin of
/// the nearest Lua function's `script` (-2, a local plugin's, otherwise).
pub fn running_asset_id(lua: &Lua) -> i64 {
    let Some(script) = rustudio_engine::runtime::running_script(lua) else { return -2 };
    let Some(root) = plugin_root_of(lua, script) else { return -2 };
    lua.app_data_ref::<PluginVm>()
        .and_then(|v| v.by_root(root).map(|p| p.asset_id))
        .unwrap_or(-2)
}

/// Warns once per VM that a plugin API member has no behaviour in
/// rustudio yet (hook points), in Output.
pub fn todo(lua: &Lua, what: &str) {
    let first = vm(lua).is_some_and(|mut v| v.warned.insert(what.to_string()));
    if first {
        if let Ok(mut w) = world(lua) {
            w.output.push((
                crate::app::Level::Warn,
                format!("rustudio: {what} is not implemented for plugins yet"),
            ));
        }
    }
}

/// Makes a plugin-owned object (Parent nil, Name "Instance" as Studio's
/// are: measured for PluginMouse and PluginToolbar) and records it.
pub fn make_owned(lua: &Lua, owner: Ref, class: &str, props: Vec<(&str, Variant)>) -> mlua::Result<Ref> {
    let r = {
        let mut w = world(lua)?;
        let mut b = InstanceBuilder::new(class).with_name("Instance");
        for (k, v) in props {
            b = b.with_property(k, v);
        }
        w.dm().dom.insert(Ref::none(), b)
    };
    if let Some(mut v) = vm(lua) {
        if let Some(p) = v.by_root_mut(owner) {
            p.owned.push(r);
        }
    }
    Ok(r)
}

/// Records a UI object for the UI.
pub fn record_ui(lua: &Lua, owner: Ref, r: Ref, kind: UiKind) {
    if let Some(mut v) = vm(lua) {
        let Some(id) = v.by_root(owner).map(|p| p.id) else { return };
        v.ui.push(UiItem { plugin: id, r, kind });
        v.ui_rev = v.ui_rev.wrapping_add(1);
    }
}

/// Marks the VM's ribbon / menu UI state dirty.
pub fn bump_ui_rev(lua: &Lua) {
    if let Some(mut v) = vm(lua) {
        v.ui_rev = v.ui_rev.wrapping_add(1);
    }
}

// ------------------------------------------------------------- loading

fn clone_subtree_with_bytecode(
    src_dom: &WeakDom,
    src_bc: &HashMap<Ref, Bytecode>,
    src: Ref,
    dst_dom: &mut WeakDom,
    dst_parent: Ref,
    dst_bc: &mut HashMap<Ref, Bytecode>,
) {
    let Some(i) = src_dom.get_by_ref(src) else { return };
    let mut b = InstanceBuilder::new(i.class).with_name(i.name.as_str());
    for (k, v) in &i.properties {
        b = b.with_property(*k, v.clone());
    }
    let new_r = dst_dom.insert(dst_parent, b);
    if let Some(bc) = src_bc.get(&src) {
        dst_bc.insert(new_r, bc.clone());
    }
    for &c in i.children() {
        clone_subtree_with_bytecode(src_dom, src_bc, c, dst_dom, new_r, dst_bc);
    }
}

enum ModelTransfer<'a> {
    Clone {
        tree: &'a WeakDom,
        bytecode: &'a HashMap<Ref, Bytecode>,
    },
    Move {
        dom: WeakDom,
        bytecode: HashMap<Ref, Bytecode>,
    },
}

/// Puts the plugin's instance tree into the entered World's document (an
/// orphan: not in the DataModel, never saved, not in Explorer) and starts
/// its Scripts at `spec.identity` (5 for user/cloud, 6 for Studio built-ins).
/// Returns the Plugin instance. RunContext Plugin scripts wait for [`start_later`].
pub fn load(rt: &Runtime, spec: &LoadSpec) -> Result<Ref, String> {
    load_inner(
        rt,
        spec.id,
        &spec.name,
        spec.asset_id,
        &spec.settings_key,
        spec.identity,
        spec.is_builtin,
        ModelTransfer::Clone { tree: &spec.tree, bytecode: &spec.bytecode },
    )
}

/// Same as [`load`], but moves `dom` and `bytecode` directly into the edit
/// document without cloning instances or properties (used for built-in plugins,
/// which never load into Play DataModels).
pub fn load_owned(
    rt: &Runtime,
    id: PluginId,
    name: &str,
    asset_id: i64,
    settings_key: &str,
    identity: u8,
    is_builtin: bool,
    dom: WeakDom,
    bytecode: HashMap<Ref, Bytecode>,
) -> Result<Ref, String> {
    load_inner(rt, id, name, asset_id, settings_key, identity, is_builtin, ModelTransfer::Move { dom, bytecode })
}

#[allow(clippy::too_many_arguments)]
fn load_inner(
    rt: &Runtime,
    id: PluginId,
    name: &str,
    asset_id: i64,
    settings_key: &str,
    identity: u8,
    is_builtin: bool,
    transfer: ModelTransfer<'_>,
) -> Result<Ref, String> {
    let lua = &rt.lua;
    install(lua);
    let (root, scripts, later, code) = {
        let mut w = world(lua).map_err(|e| e.to_string())?;
        let mut bc_out = HashMap::new();
        let root = {
            let d = w.dm();
            let root = d.dom.insert(Ref::none(), InstanceBuilder::new("Plugin").with_name(name));
            match transfer {
                ModelTransfer::Clone { tree, bytecode } => {
                    let tops = tree.root().children().to_vec();
                    if bytecode.is_empty() {
                        let copies = tree.clone_multiple_into_external(&tops, &mut d.dom);
                        for c in copies {
                            d.dom.transfer_within(c, root);
                        }
                    } else {
                        bc_out.reserve(bytecode.len());
                        for top in tops {
                            clone_subtree_with_bytecode(tree, bytecode, top, &mut d.dom, root, &mut bc_out);
                        }
                    }
                }
                ModelTransfer::Move { mut dom, bytecode } => {
                    let tops = dom.root().children().to_vec();
                    for top in tops {
                        dom.transfer(top, &mut d.dom, root);
                    }
                    bc_out = bytecode;
                }
            }
            // (Measured: the MultipleDocumentInterfaceInstance holds one
            // DataModelSession named "Instance".)
            d.dom.insert(
                root,
                InstanceBuilder::new("MultipleDocumentInterfaceInstance")
                    .with_name("MultipleDocumentInterfaceInstance")
                    .with_child(InstanceBuilder::new("DataModelSession").with_name("Instance")),
            );
            d.mark_tree(root);
            root
        };
        if !bc_out.is_empty() {
            w.core_scripts.modules.extend(bc_out);
        }
        let d = w.d();
        let (mut now, mut later) = (Vec::new(), Vec::new());
        let code: Vec<Ref> = d
            .dom
            .descendants_of(root)
            .filter(|i| matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript"))
            .map(|i| i.referent())
            .collect();
        for i in d.dom.descendants_of(root) {
            let r = i.referent();
            let class = i.class.as_str();
            if class != "Script" && class != "LocalScript" {
                continue;
            }
            let disabled = matches!(d.prop(r, "Disabled"), Some(Variant::Bool(true)))
                || matches!(d.prop(r, "Enabled"), Some(Variant::Bool(false)));
            if disabled {
                continue;
            }
            // RunContext: Legacy 0, Server 1, Client 2, Plugin 3.
            match rustudio_render::scene::enum_u32(d.prop(r, "RunContext")).unwrap_or(0) {
                0 => now.push(r),
                3 => later.push(r),
                _ => {}
            }
        }
        (root, now, later, code)
    };
    if let Some(mut v) = vm(lua) {
        v.plugins.push(VmPlugin {
            id,
            root,
            name: name.to_string(),
            asset_id,
            settings_key: settings_key.to_string(),
            identity,
            is_builtin,
            scripts: Vec::new(),
            later,
            code,
            owned: Vec::new(),
            mouse: None,
            activated: false,
            exclusive_mouse: false,
        });
        v.ui_rev = v.ui_rev.wrapping_add(1);
    }
    for s in scripts {
        // (A script an earlier one moved out of the plugin or disabled
        // still starts: Studio collected them first.)
        if let Some(mut v) = vm(lua) {
            if let Some(p) = v.by_root_mut(root) {
                p.scripts.push(s);
            }
        }
        rt.run_script_as(s, identity);
    }
    Ok(root)
}

/// Starts the RunContext Plugin scripts of every plugin loaded so far (the
/// end of a load batch, as Studio starts them after the batch).
pub fn start_later(rt: &Runtime) {
    let lua = &rt.lua;
    let todo: Vec<(Ref, u8, Vec<Ref>)> = match vm(lua) {
        Some(mut v) => v
            .plugins
            .iter_mut()
            .map(|p| (p.root, p.identity, std::mem::take(&mut p.later)))
            .filter(|(_, _, l)| !l.is_empty())
            .collect(),
        None => return,
    };
    for (root, ident, scripts) in todo {
        for s in scripts {
            if let Some(mut v) = vm(lua) {
                if let Some(p) = v.by_root_mut(root) {
                    p.scripts.push(s);
                }
            }
            rt.run_script_as(s, ident);
        }
    }
}

/// Unloads a plugin from this VM in Studio's order (measured in Studio,
/// F15): Unloading fires (its handlers run now, with the Plugin already
/// unparented and the scripts still in place), the plugin is deactivated,
/// its scripts stop (their connections and threads go), and the objects it
/// made leave the DataModel. Neither the Plugin's nor a script's Destroying
/// fires (measured). What is left is freed by the collector once no Lua
/// value of it remains.
pub fn unload(rt: &Runtime, id: PluginId) {
    let lua = &rt.lua;
    let Some(p) = vm(lua).and_then(|v| v.by_id(id).cloned()) else { return };
    // A plugin that parented itself somewhere is taken out first.
    if world(lua).is_ok_and(|w| w.d().dom.get_by_ref(p.root).is_some_and(|i| i.parent().is_some())) {
        rt.apply_parent(p.root, Ref::none());
    }
    rt.fire_bare(p.root, "Unloading", vec![]);
    rt.dispatch();
    // Its open PluginMenu popups close (their ShowAsync return nil).
    crate::plugin_host::api::menus::close_plugin_menus(lua, id);
    if let Some(mut v) = vm(lua) {
        if v.active == Some(id) {
            v.active = None;
            v.activation_seq += 1;
        }
    }
    for s in &p.scripts {
        rt.stop_script(*s);
    }
    // Threads parked inside its ModuleScripts (a task.wait in a required
    // module's function) or in scripts it moved elsewhere end too: none of
    // its code runs after Unloading (a resumed one would call into a Plugin
    // that is gone).
    let mut code: HashSet<Ref> = p.code.iter().chain(p.scripts.iter()).copied().collect();
    if let Ok(w) = world(lua) {
        let d = w.d();
        for top in std::iter::once(&p.root).chain(p.owned.iter()) {
            if d.dom.get_by_ref(*top).is_none() {
                continue;
            }
            for i in d.dom.descendants_of(*top) {
                if matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
                    code.insert(i.referent());
                }
            }
        }
    }
    rt.stop_code_of(&code);
    // Its WebStreamClients' connections end with it.
    rustudio_engine::runtime::domains::services::close_streams_of(lua, &code);
    for r in p.owned.iter().chain(p.mouse.iter()) {
        let parented = world(lua).is_ok_and(|w| w.d().dom.get_by_ref(*r).is_some_and(|i| i.parent().is_some()));
        if parented {
            rt.apply_parent(*r, Ref::none());
        }
    }
    if let Ok(mut w) = world(lua) {
        for r in &code {
            w.core_scripts.modules.remove(r);
        }
        for r in p.owned.iter().chain(p.mouse.iter()).chain(std::iter::once(&p.root)) {
            if w.d().dom.get_by_ref(*r).is_some() {
                w.orphans.insert(*r);
            }
        }
        w.gc_busy = true;
    }
    rt.dispatch();
    if let Some(mut v) = vm(lua) {
        v.plugins.retain(|x| x.id != id);
        v.ui.retain(|u| u.plugin != id);
        v.ui_rev = v.ui_rev.wrapping_add(1);
    }
}

/// Every Ref the collector must keep: plugin roots and their objects.
pub fn pinned(lua: &Lua) -> HashSet<Ref> {
    lua.app_data_ref::<PluginVm>()
        .map(|v| {
            v.plugins
                .iter()
                .flat_map(|p| std::iter::once(p.root).chain(p.owned.iter().copied()).chain(p.mouse))
                .collect()
        })
        .unwrap_or_default()
}

/// The Plugin instance's Lua value (tests, the harness).
pub fn plugin_value(rt: &Runtime, id: PluginId) -> Value {
    match vm(&rt.lua).and_then(|v| v.by_id(id).map(|p| p.root)) {
        Some(r) => rt.inst(r),
        None => Value::Nil,
    }
}
