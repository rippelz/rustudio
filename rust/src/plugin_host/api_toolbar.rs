//! Plugin toolbars: plugin:CreateToolbar, PluginToolbar:CreateButton and
//! PluginToolbarButton, and the model the ribbon's Plugins tab and the
//! Plugins menu draw (`ribbon`, src/plugin_host/ui_ribbon.rs).
//!
//! Measured in Studio 0.747 (a hidden Studio, probes
//! checks/plugins/ui/probes/UiA_main.lua, UiB_other.lua, UiC_order.lua;
//! capture checks/plugins/ui/captures/studio-ui.txt):
//! - CreateToolbar returns at once in the edit, Play server and Play client
//!   DataModels (`CreateToolbar.after … "0.00"` in all three), a
//!   PluginToolbar with Name "Instance" and Parent nil. A second
//!   CreateToolbar with a name this plugin already used returns **the same
//!   object** (`tb == tb2` true), so its buttons share the group.
//! - Another plugin's toolbar with the same name is its own group (and may
//!   reuse button ids).
//! - CreateButton with an id the toolbar already has → `Cannot create more
//!   than one button with id "B1" in toolbar with id "UiBar" in plugin with
//!   id "user_UiA_main.lua"`.
//! - Ribbon order: plugins in load order; a plugin's toolbars **sorted by
//!   name** (Zeta, Alpha, Mid, then Late, Aardvark created later all show as
//!   Aardvark, Alpha, Late, Mid, Zeta, in the tab and the Plugins menu);
//!   buttons in creation order.
//! - Destroying a button or a toolbar does **not** remove it from the
//!   ribbon or the Plugins menu (still drawn after switching tabs); a
//!   destroyed button no longer fires (its connections are gone), a button
//!   of a destroyed toolbar still does. Only unloading the plugin removes
//!   its toolbars.

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::{Ref, Variant};

use super::*;
use crate::plugin_host::PluginId;

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match (class, name) {
        ("Plugin", "CreateToolbar") => (|| {
            let title = arg_string(args, 1)?;
            let root = plugin_root(lua, this)?;
            // The same name again: the same toolbar (measured).
            let existing = vm::vm(lua).and_then(|v| {
                let id = v.by_root(root)?.id;
                v.ui.iter().find_map(|u| match &u.kind {
                    UiKind::Toolbar { name } if u.plugin == id && *name == title => Some(u.r),
                    _ => None,
                })
            });
            if let Some(r) = existing {
                return ret1(inst_value(lua, r)?);
            }
            let r = vm::make_owned(lua, root, "PluginToolbar", vec![])?;
            vm::record_ui(lua, root, r, UiKind::Toolbar { name: title });
            ret1(inst_value(lua, r)?)
        })(),
        ("PluginToolbar", "CreateButton" | "CreatePopupButton") => (|| {
            require_args(args, 3)?;
            let id = arg_string(args, 1)?;
            let tooltip = arg_string(args, 2)?;
            let icon = arg_string(args, 3)?;
            let text = arg_string_or(args, 4, "")?;
            let (root, dup, tb_name, pname) = {
                let v = vm::vm(lua).ok_or_else(|| rt_err("This PluginToolbar's plugin is not loaded"))?;
                let p = v.owner_of(this).ok_or_else(|| rt_err("This PluginToolbar's plugin is not loaded"))?;
                let dup = v.ui.iter().any(|u| matches!(&u.kind, UiKind::Button { toolbar, id: i, .. } if *toolbar == this && *i == id));
                let tb_name = v
                    .ui
                    .iter()
                    .find_map(|u| match &u.kind {
                        UiKind::Toolbar { name } if u.r == this => Some(name.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                (p.root, dup, tb_name, p.name.clone())
            };
            // (An empty id is never a duplicate: Moon Animator 2 makes its
            // three buttons with "" and Studio shows all three.)
            if dup && !id.is_empty() {
                return Err(rt_err(format!(
                    "Cannot create more than one button with id \"{id}\" in toolbar with id \"{tb_name}\" in plugin with id \"{pname}\""
                )));
            }
            let r = vm::make_owned(
                lua,
                root,
                "PluginToolbarButton",
                vec![
                    ("Enabled", Variant::Bool(true)),
                    ("ClickableWhenViewportHidden", Variant::Bool(false)),
                    ("Icon", Variant::ContentId(icon.clone().into())),
                ],
            )?;
            vm::record_ui(lua, root, r, UiKind::Button { toolbar: this, id, tooltip, icon, text });
            ret1(inst_value(lua, r)?)
        })(),
        // The pressed look of the button (a grey box behind its icon).
        ("PluginToolbarButton", "SetActive" | "SetDropdownActive") => (|| {
            let on = arg_bool(args, 1)?;
            if name == "SetActive" {
                set_button_active(lua, this, on);
            } else {
                set_button_dropdown_active(lua, this, on);
            }
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}

/// Pressed state per button (`button_active`).
fn set_button_active(lua: &Lua, r: Ref, on: bool) {
    if let Ok(t) = active_table(lua) {
        let _ = t.raw_set(format!("{r:?}"), on);
    }
    vm::bump_ui_rev(lua);
}

pub fn button_active(lua: &Lua, r: Ref) -> bool {
    active_table(lua).and_then(|t| t.raw_get::<bool>(format!("{r:?}"))).unwrap_or(false)
}

fn set_button_dropdown_active(lua: &Lua, r: Ref, on: bool) {
    if let Ok(t) = dropdown_active_table(lua) {
        let _ = t.raw_set(format!("{r:?}"), on);
    }
    vm::bump_ui_rev(lua);
}

pub fn button_dropdown_active(lua: &Lua, r: Ref) -> bool {
    dropdown_active_table(lua).and_then(|t| t.raw_get::<bool>(format!("{r:?}"))).unwrap_or(false)
}

fn active_table(lua: &Lua) -> mlua::Result<mlua::Table> {
    match lua.named_registry_value::<mlua::Table>("rs_plugin_button_active") {
        Ok(t) => Ok(t),
        Err(_) => {
            let t = lua.create_table()?;
            lua.set_named_registry_value("rs_plugin_button_active", t.clone())?;
            Ok(t)
        }
    }
}

fn dropdown_active_table(lua: &Lua) -> mlua::Result<mlua::Table> {
    match lua.named_registry_value::<mlua::Table>("rs_plugin_button_dropdown_active") {
        Ok(t) => Ok(t),
        Err(_) => {
            let t = lua.create_table()?;
            lua.set_named_registry_value("rs_plugin_button_dropdown_active", t.clone())?;
            Ok(t)
        }
    }
}

pub fn prop_get(_lua: &Lua, _r: Ref, _class: &str, _key: &str) -> mlua::Result<Option<Value>> {
    Ok(None)
}

pub fn newindex(lua: &Lua, _r: Ref, class: &str, _key: &str, _value: &Value) -> mlua::Result<bool> {
    if matches!(class, "PluginToolbar" | "PluginToolbarButton" | "PluginAction" | "PluginMenu") {
        vm::bump_ui_rev(lua);
    }
    Ok(false)
}

// ------------------------------------------------------- the ribbon model

/// One toolbar button as the ribbon draws it (live properties read from
/// the document the VM's objects live in).
#[derive(Clone, Debug, PartialEq)]
pub struct RibbonButton {
    pub r: Ref,
    pub id: String,
    /// The label: the button's text with line breaks as spaces; with no
    /// text its id (Studio: CreateButton("Launch Archimedes", tip, icon)
    /// shows "Launch Archimedes"), with neither its tooltip (F50: `empty id`).
    pub label: String,
    pub tooltip: String,
    pub icon: String,
    pub enabled: bool,
    pub clickable_when_viewport_hidden: bool,
    pub active: bool,
    /// Studio's shortcut key for it: `<plugin>_<toolbar>_<button id>`.
    pub key: String,
}

/// One toolbar (a group in the Plugins tab, a submenu of the Plugins menu).
#[derive(Clone, Debug, PartialEq)]
pub struct RibbonGroup {
    pub plugin: PluginId,
    pub plugin_name: String,
    pub toolbar: Ref,
    pub name: String,
    pub buttons: Vec<RibbonButton>,
}

/// The label Studio shows for a button text.
pub fn button_label(text: &str, id: &str, tooltip: &str) -> String {
    let t = if !text.is_empty() {
        text
    } else if !id.is_empty() {
        id
    } else {
        tooltip
    };
    t.replace("\r\n", " ").replace(['\n', '\r'], " ")
}

/// Every toolbar of the VM's plugins in Studio's ribbon order: plugins in
/// load order, each plugin's toolbars by name, buttons in creation order.
/// `doc` is the document the VM's objects are in (the edit document, or a
/// Play side's); a destroyed button keeps its last known look.
pub fn ribbon(lua: &Lua, doc: &rustudio_core::doc::Document) -> Vec<RibbonGroup> {
    let Some(v) = lua.app_data_ref::<vm::PluginVm>() else { return Vec::new() };
    let mut out = Vec::new();
    for p in &v.plugins {
        let mut groups: Vec<RibbonGroup> = v
            .ui
            .iter()
            .filter(|u| u.plugin == p.id)
            .filter_map(|u| match &u.kind {
                UiKind::Toolbar { name } => Some(RibbonGroup {
                    plugin: p.id,
                    plugin_name: p.name.clone(),
                    toolbar: u.r,
                    name: name.clone(),
                    buttons: Vec::new(),
                }),
                _ => None,
            })
            .collect();
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        for u in v.ui.iter().filter(|u| u.plugin == p.id) {
            let UiKind::Button { toolbar, id, tooltip, icon, text } = &u.kind else { continue };
            let Some(g) = groups.iter_mut().find(|g| g.toolbar == *toolbar) else { continue };
            let alive = doc.dom.get_by_ref(u.r).is_some();
            let b = |k: &str, d: bool| match doc.prop(u.r, k) {
                Some(Variant::Bool(x)) => x,
                _ => d,
            };
            let mut icon = match doc.prop(u.r, "Icon") {
                Some(Variant::ContentId(c)) => c.as_str().to_string(),
                Some(Variant::String(s)) => s,
                Some(Variant::Content(c)) => c.as_uri().map(str::to_string).unwrap_or_default(),
                _ if alive => String::new(),
                _ => icon.clone(),
            };
            if icon.trim().is_empty() {
                if let Some(fallback) = crate::plugin_host::builtin_lazy::builtin_default_icon(&p.name, &g.name, id) {
                    icon = fallback.to_string();
                }
            }
            g.buttons.push(RibbonButton {
                r: u.r,
                id: id.clone(),
                label: button_label(text, id, tooltip),
                tooltip: tooltip.clone(),
                icon,
                enabled: b("Enabled", true),
                clickable_when_viewport_hidden: b("ClickableWhenViewportHidden", false),
                active: button_active(lua, u.r) || button_dropdown_active(lua, u.r),
                key: format!("{}_{}_{}", p.name, g.name, id),
            });
        }
        out.extend(groups);
    }
    out
}
