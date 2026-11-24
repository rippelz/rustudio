//! Plugin widgets: plugin:CreateDockWidgetPluginGui(Async),
//! plugin:CreateQWidgetPluginGui, PluginGui members, the
//! DockWidgetPluginGuiInfo datatype, and plugin:StartDrag's drag into a
//! widget. The widget objects are real (DockWidgetPluginGui /
//! QWidgetPluginGui under PluginGuiService, Name "PluginGui", Title "",
//! Enabled = InitialEnabled or the restored state: measured in Studio,
//! F54) and recorded for the editor (`vm::UiKind::DockWidget`), which
//! draws each as a dock panel (src/plugin_host/widgets_ui.rs) and feeds
//! its input back here: the × (`close`: BindToClose's callback, else
//! Enabled = false), WindowFocused / WindowFocusReleased, and the
//! PluginDrag* events of a drag (`drag_fire`).

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::{Ref, Variant};

use super::*;

/// DockWidgetPluginGuiInfo.new(initialDockState, initialEnabled,
/// overrideEnabledRestore, floatingXSize, floatingYSize, minWidth,
/// minHeight), as Studio's: the dock state is an
/// Enum.InitialDockState (or its number) and can't be read back; the other
/// fields read but none can be written.
const DOCK_INFO: &str = r#"
local data = setmetatable({}, { __mode = "k" })
local READ = { FloatingXSize = true, FloatingYSize = true, InitialEnabled = true,
    InitialEnabledShouldOverrideRestore = true, MinHeight = true, MinWidth = true }
local function b01(v) return if v then 1 else 0 end
local function new(state, enabled, override, fx, fy, mw, mh)
    if state == nil then
        state = Enum.InitialDockState.Right
    elseif type(state) == "number" then
        local found
        for _, item in ipairs(Enum.InitialDockState:GetEnumItems()) do
            if item.Value == state then found = item end
        end
        if not found then error("DockWidgetPluginGuiInfo.new first argument expects InitialDockState input", 2) end
        state = found
    elseif typeof(state) ~= "EnumItem" or state.EnumType ~= Enum.InitialDockState then
        error("DockWidgetPluginGuiInfo.new first argument expects InitialDockState input", 2)
    end
    local p = newproxy(true)
    local mt = getmetatable(p)
    data[p] = {
        InitialDockState = state,
        InitialEnabled = enabled == true,
        InitialEnabledShouldOverrideRestore = override == true,
        FloatingXSize = tonumber(fx) or 0, FloatingYSize = tonumber(fy) or 0,
        MinWidth = tonumber(mw) or 0, MinHeight = tonumber(mh) or 0,
    }
    mt.__type = "DockWidgetPluginGuiInfo"
    mt.__index = function(_, k)
        if READ[k] then return data[p][k] end
        error(string.format("'%s' is not a valid member of DockWidgetPluginGuiInfo", tostring(k)), 0)
    end
    mt.__newindex = function(_, k)
        error(string.format("%s is not a valid member of DockWidgetPluginGuiInfo", tostring(k)), 0)
    end
    mt.__tostring = function()
        local d = data[p]
        return string.format("InitialDockState:%s InitialEnabled:%d InitialEnabledShouldOverrideRestore:%d FloatingXSize:%d FloatingYSize:%d MinWidth:%d MinHeight:%d",
            d.InitialDockState.Name, b01(d.InitialEnabled), b01(d.InitialEnabledShouldOverrideRestore),
            d.FloatingXSize, d.FloatingYSize, d.MinWidth, d.MinHeight)
    end
    mt.__metatable = "The metatable is locked"
    return p
end
DockWidgetPluginGuiInfo = table.freeze({ new = new })
return function(p) return data[p] end
"#;

/// CreateDockWidgetPluginGui(Async) yields, as Studio's (its timestamps
/// in captures/ui.txt: each call returns a frame or more later): the widget
/// is made at once (PluginGuiService.ChildAdded fires then), the caller
/// resumes on the next frame.
const CREATE_YIELDS: &str = r#"
local make = ...
local function create(self, id, info)
    __rs_member_check(self, "Plugin", "CreateDockWidgetPluginGui")
    local ok, w = make(self, id, info)
    if not ok then error(w, 0) end
    task.wait()
    return w
end
__rs_lua_methods.CreateDockWidgetPluginGui = create
__rs_lua_methods.CreateDockWidgetPluginGuiAsync = create
"#;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let read: mlua::Function = lua.load(DOCK_INFO).set_name("=rustudio.plugin_dockinfo").eval()?;
    lua.set_named_registry_value("rs_plugin_dockinfo", read)?;
    if lua.globals().get::<Value>("__rs_lua_methods")?.is_table() {
        let make = lua.create_function(|lua, args: MultiValue| {
            let this = opt_inst(&argv(&args, 0));
            let made = match this {
                Some(this) => create_dock(lua, this, &args),
                None => Err(rt_err("Expected ':' not '.' calling member function CreateDockWidgetPluginGui")),
            };
            Ok(match made {
                Ok(r) => (true, inst_value(lua, r)?),
                Err(e) => (false, Value::String(lua.create_string(rustudio_engine::runtime::error_message(&e))?)),
            })
        })?;
        lua.load(CREATE_YIELDS).set_name("=rustudio.plugin_widgets").call::<()>(make)?;
    }
    Ok(())
}

fn create_dock(lua: &Lua, this: Ref, args: &MultiValue) -> mlua::Result<Ref> {
    require_args(args, 2)?;
    let id = arg_string(args, 1)?;
    let info = match dock_info(lua, &argv(args, 2))? {
        Some(i) => i,
        None if matches!(argv(args, 2), Value::Nil) => return Err(rt_err("Argument 2 missing or nil")),
        None => return Err(rt_err("Unable to cast value to DockWidgetPluginGuiInfo")),
    };
    make_widget(lua, this, "DockWidgetPluginGui", id, Some(info), None)
}

fn dock_info(lua: &Lua, v: &Value) -> mlua::Result<Option<vm::DockInfo>> {
    let read: mlua::Function = lua.named_registry_value("rs_plugin_dockinfo")?;
    let t: Option<mlua::Table> = read.call(v.clone())?;
    let Some(t) = t else { return Ok(None) };
    let state: Value = t.get("InitialDockState")?;
    let state = opt_ud::<rustudio_engine::ltypes::LEnumItem>(&state).map(|e| e.name.clone()).unwrap_or_else(|| "Right".into());
    Ok(Some(vm::DockInfo {
        initial_dock_state: state,
        initial_enabled: t.get("InitialEnabled")?,
        override_enabled_restore: t.get("InitialEnabledShouldOverrideRestore")?,
        float_x: t.get("FloatingXSize")?,
        float_y: t.get("FloatingYSize")?,
        min_width: t.get("MinWidth")?,
        min_height: t.get("MinHeight")?,
    }))
}

/// Creates a QWidgetPluginGui for Panels:CreateAsync (api_builtin.rs).
pub(crate) fn make_panel_widget(lua: &Lua, this: Ref, id: String) -> mlua::Result<Ref> {
    make_widget(lua, this, "QWidgetPluginGui", id, None, None)
}

struct QWidgetOpts {
    title: Option<String>,
    initial_enabled: bool,
    float_x: f64,
    float_y: f64,
}

fn read_qwidget_opts(v: &Value) -> QWidgetOpts {
    let mut o = QWidgetOpts {
        title: None,
        initial_enabled: false,
        float_x: 0.0,
        float_y: 0.0,
    };
    let Value::Table(t) = v else { return o };
    if let Ok(Value::String(s)) = t.get::<Value>("Title") {
        if let Ok(st) = s.to_str() {
            o.title = Some(st.to_string());
        }
    }
    if let Ok(Value::Boolean(b)) = t.get::<Value>("InitialEnabled") {
        o.initial_enabled = b;
    }
    if let Ok(sz) = t.get::<Value>("Size") {
        if let Some(v2) = opt_ud::<rustudio_engine::ltypes::LVector2>(&sz) {
            o.float_x = v2.0.x as f64;
            o.float_y = v2.0.y as f64;
        }
    }
    o
}

/// A widget as Studio makes it: Name
/// "PluginGui", Title "", Enabled = InitialEnabled, ZIndexBehavior Global,
/// ResetOnSpawn true, under PluginGuiService; one per id per plugin.
fn make_widget(
    lua: &Lua,
    this: Ref,
    class: &str,
    id: String,
    info: Option<vm::DockInfo>,
    qopts: Option<QWidgetOpts>,
) -> mlua::Result<Ref> {
    let root = plugin_root(lua, this)?;
    let dup = vm::vm(lua).is_some_and(|v| {
        let pid = v.by_root(root).map(|p| p.id);
        v.ui.iter().any(|u| {
            Some(u.plugin) == pid
                && matches!(&u.kind, UiKind::DockWidget { id: i, .. } | UiKind::QWidget { id: i, .. } if *i == id)
        })
    });
    if dup {
        return Err(rt_err(format!("This plugin cannot create more than one PluginGui with id: \"{id}\"")));
    }
    // Studio restores a widget docked last session (F54): its Enabled
    // unless InitialEnabledShouldOverrideRestore.
    let pname = vm::vm(lua).and_then(|v| v.by_root(root).map(|p| p.name.clone())).unwrap_or_default();
    let edit = world(lua)?.mode == RunMode::Edit;
    let key = format!("edit_{pname}_{id}");
    let saved = if edit && info.is_some() { crate::plugin_host::widgets_ui::restored(&key) } else { None };
    let initial = info.as_ref().map(|i| i.initial_enabled).or_else(|| qopts.as_ref().map(|q| q.initial_enabled)).unwrap_or(false);
    let enabled = match (saved, info.as_ref()) {
        (Some(_), Some(i)) if i.override_enabled_restore => initial,
        (Some(e), _) => e,
        _ => initial,
    };
    let title = qopts.as_ref().and_then(|q| q.title.clone()).unwrap_or_default();
    let r = vm::make_owned(
        lua,
        root,
        class,
        vec![
            ("Title", Variant::String(title)),
            ("Enabled", Variant::Bool(enabled)),
            ("ResetOnSpawn", Variant::Bool(true)),
            // ZIndexBehavior.Global
            ("ZIndexBehavior", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(0))),
        ],
    )?;
    {
        let mut w = world(lua)?;
        let d = w.dm();
        if let Some(i) = d.dom.get_by_ref_mut(r) {
            i.name = "PluginGui".into();
        }
    }
    let is_builtin = vm::vm(lua).and_then(|v| v.by_root(root).map(|p| p.is_builtin)).unwrap_or(false);
    let svc_name = if is_builtin { "RobloxPluginGuiService" } else { "PluginGuiService" };
    let service = {
        let mut w = world(lua)?;
        let d = w.dm();
        let top = d.dom.root_ref();
        match d.find_child(top, svc_name) {
            Some(s) => s,
            None => {
                let s = d.dom.insert(top, rbx_dom_weak::InstanceBuilder::new(svc_name).with_name(svc_name));
                d.mark_tree(s);
                s
            }
        }
    };
    // Through the runtime, so PluginGuiService's ChildAdded fires.
    set_parent(lua, r, service)?;
    if saved.is_some() {
        set_store(lua, r, "HostWidgetWasRestored", Value::Boolean(true))?;
    }
    let (qfx, qfy) = qopts.as_ref().map(|q| (q.float_x, q.float_y)).unwrap_or((0.0, 0.0));
    // Its content size from the start (F47: a 320×240 Float reads 318×216
    // at once; 0, 0 while disabled), until the panel is laid out.
    if enabled {
        let size = match &info {
            Some(i) => crate::plugin_host::widgets_ui::initial_size(&i.initial_dock_state, i.float_x, i.float_y),
            None if qfx > 0.0 && qfy > 0.0 => crate::plugin_host::widgets_ui::float_client(qfx as f32, qfy as f32),
            None => [0.0, 0.0],
        };
        world(lua)?.input.gui_layout.insert(r, rustudio_engine::gui::GuiBox { size, ..Default::default() });
        if !edit {
            // (a Play copy's: kept when the client's game GUI lays out anew)
            crate::plugin_host::widgets_ui::note_play_laid(r);
        }
    }
    let kind = match info {
        Some(info) => UiKind::DockWidget { id, info },
        None => UiKind::QWidget { id, float_x: qfx, float_y: qfy },
    };
    vm::record_ui(lua, root, r, kind);
    Ok(r)
}

/// The testing domain's per-instance store (BindToClose's callback,
/// HostWidgetWasRestored live there: crates/rustudio-engine/src/domains/testing.rs `store`).
fn store_table(lua: &Lua, r: Ref) -> mlua::Result<mlua::Table> {
    let all: mlua::Table = match lua.named_registry_value::<mlua::Table>("rs_testing_store") {
        Ok(t) => t,
        Err(_) => {
            let t = lua.create_table()?;
            lua.set_named_registry_value("rs_testing_store", t.clone())?;
            t
        }
    };
    let key = format!("{r:?}");
    if let Ok(t) = all.get::<mlua::Table>(key.as_str()) {
        return Ok(t);
    }
    let t = lua.create_table()?;
    all.set(key, t.clone())?;
    Ok(t)
}

fn set_store(lua: &Lua, r: Ref, key: &str, v: Value) -> mlua::Result<()> {
    store_table(lua, r)?.set(key, v)
}

/// The user closed widget `r`'s panel: its BindToClose callback runs if
/// it set one (the widget stays; the plugin decides), else Enabled turns
/// false (the panel closes, the Enabled changed signal fires).
pub fn close(rt: &rustudio_engine::runtime::Runtime, r: Ref) {
    // A widget the plugin destroyed has no panel in Studio. Its panel can
    // still be up here until the dock catches up, and a × click (or one
    // queued before the Destroy) ran the callback again: Argon's settings
    // callback then indexed its cleared state ("attempt to index nil with
    // 'Destroy'", Argon.App:269).
    let gone = rt.world_mut().is_none_or(|mut w| {
        w.destroyed.contains(&r) || w.dm().dom.get_by_ref(r).is_none_or(|i| i.parent().is_none())
    });
    if gone {
        return;
    }
    let lua = &rt.lua;
    let cb = store_table(lua, r).and_then(|t| t.get::<Value>("__close")).unwrap_or(Value::Nil);
    match cb {
        Value::Function(f) => {
            if let Ok(th) = rustudio_engine::runtime::domains::assets_cloud::scene::create_thread(lua, f) {
                rustudio_engine::runtime::resume(lua, &th, Vec::new());
            }
        }
        _ => rt.set(r, "Enabled", Value::Boolean(false)),
    }
}

// ------------------------------------------------------------ StartDrag

/// plugin:StartDrag's drag, while the mouse is held: its dragInfo table.
static DRAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn drag_active() -> bool {
    DRAG.load(std::sync::atomic::Ordering::Relaxed)
}

/// PluginDragEntered / Moved / Left / Dropped on widget `r` with the drag's
/// data: a plain table {Sender, MimeType, Data, MouseIcon, DragIcon,
/// HotSpot} (F58: typeof table; Sender / MimeType / Data as given).
pub fn drag_fire(rt: &rustudio_engine::runtime::Runtime, r: Ref, name: &str) {
    let lua = &rt.lua;
    let Ok(info) = lua.named_registry_value::<mlua::Table>("rs_plugin_drag") else { return };
    let ev = (|| -> mlua::Result<mlua::Table> {
        let t = lua.create_table()?;
        for k in ["Sender", "MimeType", "Data", "MouseIcon", "DragIcon", "HotSpot"] {
            t.set(k, info.get::<Value>(k)?)?;
        }
        Ok(t)
    })();
    if let Ok(t) = ev {
        rt.fire(r, name, vec![Value::Table(t)]);
    }
}

/// PluginDragEntered / Moved / Left / Dropped on widget `r` for Explorer
/// instances dragged onto it. Studio 0.747 gives the same plain table for
/// one or several instances; the instances themselves are the Selection
/// (checks/plugins/ref/captures/xdrag.txt).
pub fn explorer_drag_fire(rt: &rustudio_engine::runtime::Runtime, r: Ref, name: &str) {
    let lua = &rt.lua;
    let ev = (|| -> mlua::Result<mlua::Table> {
        let t = lua.create_table()?;
        for (k, v) in [("Sender", "Explorer"), ("MimeType", "roblox/selection"), ("Data", ""), ("MouseIcon", ""), ("DragIcon", "")] {
            t.set(k, lstr(lua, v)?)?;
        }
        t.set("HotSpot", Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::ZERO))?))?;
        Ok(t)
    })();
    if let Ok(t) = ev {
        rt.fire(r, name, vec![Value::Table(t)]);
    }
}

pub fn drag_end(lua: &Lua) {
    DRAG.store(false, std::sync::atomic::Ordering::Relaxed);
    let _ = lua.set_named_registry_value("rs_plugin_drag", Value::Nil);
}

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match (class, name) {
        // (normally the yielding Luau wrapper above takes these)
        ("Plugin", "CreateDockWidgetPluginGui" | "CreateDockWidgetPluginGuiAsync") => (|| {
            let r = create_dock(lua, this, args)?;
            ret1(inst_value(lua, r)?)
        })(),
        ("Plugin", "CreateQWidgetPluginGui") => (|| {
            let id = arg_string(args, 1)?;
            let opts = read_qwidget_opts(&argv(args, 2));
            let r = make_widget(lua, this, "QWidgetPluginGui", id, None, Some(opts))?;
            ret1(inst_value(lua, r)?)
        })(),
        // Widget pixels from the panel's content top-left (title bar not
        // included); 0, 0 when the mouse isn't over it (F47, F55).
        (_, "GetRelativeMousePosition") if rustudio_core::doc::is_a(class, "PluginGui") => (|| {
            let p = crate::plugin_host::widgets_ui::live(this).rel.unwrap_or([0.0, 0.0]);
            ret1(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::new(p[0], p[1])))?))
        })(),
        // A drag of plugin data: widgets the mouse passes over hear
        // PluginDragEntered / Moved / Left and Dropped where it's let go
        // (F58: returns nothing, no error; called from a press).
        ("Plugin", "StartDrag") => (|| {
            let info = match argv(args, 1) {
                Value::Table(t) => t,
                Value::Nil => return Err(rt_err("Argument 1 missing or nil")),
                v => return Err(rt_err(format!("Unable to cast {} to Dictionary", v.type_name()))),
            };
            let t = lua.create_table()?;
            for k in ["Sender", "MimeType", "Data", "MouseIcon", "DragIcon"] {
                let v: Value = info.get(k)?;
                t.set(k, if v.is_nil() { lstr(lua, "")? } else { v })?;
            }
            let hs: Value = info.get("HotSpot")?;
            t.set("HotSpot", if hs.is_nil() { Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LVector2(glam::Vec2::ZERO))?) } else { hs })?;
            lua.set_named_registry_value("rs_plugin_drag", t)?;
            DRAG.store(true, std::sync::atomic::Ordering::Relaxed);
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}

pub fn prop_get(_lua: &Lua, _r: Ref, _class: &str, _key: &str) -> mlua::Result<Option<Value>> {
    Ok(None)
}

pub fn newindex(_lua: &Lua, _r: Ref, _class: &str, _key: &str, _value: &Value) -> mlua::Result<bool> {
    Ok(false)
}
