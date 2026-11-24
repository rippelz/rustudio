//! Plugin actions and menus: plugin:CreatePluginAction,
//! plugin:CreatePluginMenu and PluginMenu's members, ShowAsync's open-menu
//! stack (drawn by src/plugin_host/ui_ribbon.rs, answered by a pick, a
//! dismissal, or the harness's `--action plugin-menu:<choice>`).
//!
//! Measured in Studio 0.747 (measured in Studio; probes
//! checks/plugins/ui/probes/UiA_main.lua, capture
//! checks/plugins/ui/captures/studio-ui.txt):
//! - ShowAsync opens a popup with its top-left at the mouse, yields, and
//!   returns the picked PluginAction or nil (Escape, click outside); the
//!   action's Triggered fires right **after** ShowAsync returned.
//! - An empty menu returns nil at once (0.02 s, no popup); a menu emptied
//!   with Clear likewise.
//! - Lua keeps running while a popup is open: a second ShowAsync (another
//!   menu, from another thread) opens a second popup on top of the first;
//!   Escape closes the top one first (its ShowAsync returns nil), then the
//!   one under it.
//! - Actions made with AddNewAction are not listed in Customize Shortcuts
//!   (no key-mapping entry); CreatePluginAction(..., allowBinding=false) not
//!   either. A bound action shows its shortcut on the right in a menu.

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::{Ref, Variant};

use super::*;

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match (class, name) {
        ("Plugin", "CreatePluginAction") => (|| {
            require_args(args, 3)?;
            let id = arg_string(args, 1)?;
            let text = arg_string(args, 2)?;
            let tip = arg_string(args, 3)?;
            let icon = arg_string_or(args, 4, "")?;
            let allow = arg_bool_or(args, 5, true);
            let root = plugin_root(lua, this)?;
            let r = new_action(lua, root, id, text, tip, icon, allow)?;
            ret1(inst_value(lua, r)?)
        })(),
        ("Plugin", "CreatePluginMenu") => (|| {
            let id = arg_string(args, 1)?;
            let title = arg_string_or(args, 2, "")?;
            let icon = arg_string_or(args, 3, "")?;
            let root = plugin_root(lua, this)?;
            let r = vm::make_owned(
                lua,
                root,
                "PluginMenu",
                vec![("Title", Variant::String(title.clone())), ("Icon", Variant::String(icon.clone()))],
            )?;
            // (A menu's Name is its id: .)
            if let Some(i) = world(lua)?.dm().dom.get_by_ref_mut(r) {
                i.name = id.clone();
            }
            vm::record_ui(lua, root, r, UiKind::Menu { id, title, icon });
            ret1(inst_value(lua, r)?)
        })(),
        ("PluginMenu", "AddAction" | "AddMenu") => (|| {
            let item = opt_inst(&argv(args, 1)).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
            let (want, err) = if name == "AddAction" {
                ("PluginAction", "Instance is not a PluginAction")
            } else {
                ("PluginMenu", "Instance is not a PluginMenu")
            };
            if world(lua)?.d().class_of(item).as_deref() != Some(want) {
                return Err(rt_err(err));
            }
            menu_items(lua, this, |items| items.push(MenuItem::Item(item)))?;
            ret0()
        })(),
        ("PluginMenu", "AddNewAction") => (|| {
            require_args(args, 2)?;
            let id = arg_string(args, 1)?;
            let text = arg_string(args, 2)?;
            let icon = arg_string_or(args, 3, "")?;
            let root = vm::vm(lua)
                .and_then(|v| v.owner_of(this).map(|p| p.root))
                .ok_or_else(|| rt_err("This PluginMenu's plugin is not loaded"))?;
            let r = new_action(lua, root, id, text, String::new(), icon, true)?;
            // (Not listed in Customize Shortcuts: measured, see the header.)
            if let Some(mut v) = vm::vm(lua) {
                if let Some(u) = v.ui.iter_mut().find(|u| u.r == r) {
                    if let UiKind::Action { allow_binding, .. } = &mut u.kind {
                        *allow_binding = false;
                    }
                }
                v.ui_rev = v.ui_rev.wrapping_add(1);
            }
            menu_items(lua, this, |items| items.push(MenuItem::Item(r)))?;
            ret1(inst_value(lua, r)?)
        })(),
        ("PluginMenu", "AddSeparator") => (|| {
            menu_items(lua, this, |items| items.push(MenuItem::Separator))?;
            ret0()
        })(),
        ("PluginMenu", "Clear") => (|| {
            menu_items(lua, this, |items| items.clear())?;
            ret0()
        })(),
        // (Normally the yielding override from `install` runs instead.)
        ("PluginMenu", "ShowAsync") => (|| {
            let token = open_menu(lua, this)?;
            ret1(token.map(|t| Value::Number(t as f64)).unwrap_or(Value::Nil))
        })(),
        _ => return None,
    };
    Some(r)
}

/// A PluginAction: its ActionId is `<plugin Name>_<id>` (
/// F31), one per id per plugin (F32).
fn new_action(lua: &Lua, root: Ref, id: String, text: String, tip: String, icon: String, allow: bool) -> mlua::Result<Ref> {
    let (dup, pname) = {
        let v = vm::vm(lua).ok_or_else(|| rt_err("This Plugin is not loaded"))?;
        let p = v.by_root(root).ok_or_else(|| rt_err("This Plugin is not loaded"))?;
        let dup = v.ui.iter().any(|u| u.plugin == p.id && matches!(&u.kind, UiKind::Action { id: i, .. } if *i == id));
        (dup, p.name.clone())
    };
    if dup {
        return Err(rt_err(format!("This plugin cannot create more than one PluginAction with id: \"{id}\"")));
    }
    let r = vm::make_owned(
        lua,
        root,
        "PluginAction",
        vec![
            ("ActionId", Variant::String(format!("{pname}_{id}"))),
            ("Text", Variant::String(text.clone())),
            ("StatusTip", Variant::String(tip.clone())),
            ("AllowBinding", Variant::Bool(allow)),
        ],
    )?;
    vm::record_ui(lua, root, r, UiKind::Action { id, text, status_tip: tip, icon, allow_binding: allow });
    Ok(r)
}

/// A menu's entries in order (the UI reads them with `menu_entries`).
#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    Item(Ref),
    Separator,
}

#[derive(Default)]
struct Menus(std::collections::HashMap<Ref, Vec<MenuItem>>);

fn menu_items(lua: &Lua, menu: Ref, f: impl FnOnce(&mut Vec<MenuItem>)) -> mlua::Result<()> {
    if lua.app_data_ref::<Menus>().is_none() {
        lua.set_app_data(Menus::default());
    }
    let mut m = lua.app_data_mut::<Menus>().ok_or_else(|| rt_err("menus missing"))?;
    f(m.0.entry(menu).or_default());
    drop(m);
    vm::bump_ui_rev(lua);
    Ok(())
}

pub fn menu_entries(lua: &Lua, menu: Ref) -> Vec<MenuItem> {
    lua.app_data_ref::<Menus>().and_then(|m| m.0.get(&menu).cloned()).unwrap_or_default()
}

pub fn prop_get(_lua: &Lua, _r: Ref, _class: &str, _key: &str) -> mlua::Result<Option<Value>> {
    Ok(None)
}


// ------------------------------------------------------------- ShowAsync

/// A PluginMenu:ShowAsync waiting for an answer.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenMenu {
    pub token: u64,
    pub menu: Ref,
    pub plugin: crate::plugin_host::PluginId,
}

#[derive(Default)]
struct MenuState {
    next: u64,
    /// Open popups, bottom first (the last is on top).
    open: Vec<OpenMenu>,
    /// Answers not yet picked up by their ShowAsync: token → the action.
    done: std::collections::HashMap<u64, Option<Ref>>,
}

fn state(lua: &Lua) -> Option<mlua::AppDataRefMut<'_, MenuState>> {
    if lua.app_data_ref::<MenuState>().is_none() {
        lua.set_app_data(MenuState::default());
    }
    lua.app_data_mut::<MenuState>()
}

/// Opens `menu` as a popup; None when it has nothing to show (ShowAsync
/// then returns nil at once, as Studio's).
pub fn open_menu(lua: &Lua, menu: Ref) -> mlua::Result<Option<u64>> {
    let plugin = vm::vm(lua)
        .and_then(|v| v.owner_of(menu).map(|p| p.id))
        .ok_or_else(|| rt_err("This PluginMenu's plugin is not loaded"))?;
    if menu_entries(lua, menu).iter().all(|e| *e == MenuItem::Separator) {
        return Ok(None);
    }
    let mut s = state(lua).ok_or_else(|| rt_err("menus missing"))?;
    s.next += 1;
    let token = s.next;
    s.open.push(OpenMenu { token, menu, plugin });
    drop(s);
    vm::bump_ui_rev(lua);
    Ok(Some(token))
}

/// The open popups, bottom first.
pub fn open_menus(lua: &Lua) -> Vec<OpenMenu> {
    lua.app_data_ref::<MenuState>().map(|s| s.open.clone()).unwrap_or_default()
}

/// Answers an open popup: `choice` is the picked action (nil: dismissed).
/// Its ShowAsync returns on the calling thread's next resume.
pub fn answer(lua: &Lua, token: u64, choice: Option<Ref>) {
    let mut changed = false;
    if let Some(mut s) = state(lua) {
        if let Some(i) = s.open.iter().position(|m| m.token == token) {
            s.open.remove(i);
            s.done.insert(token, choice);
            changed = true;
        }
    }
    if changed {
        vm::bump_ui_rev(lua);
    }
}

/// Closes every popup of a plugin (it unloaded): their ShowAsync get nil.
pub fn close_plugin_menus(lua: &Lua, plugin: crate::plugin_host::PluginId) {
    let toks: Vec<u64> = open_menus(lua).iter().filter(|m| m.plugin == plugin).map(|m| m.token).collect();
    for t in toks {
        answer(lua, t, None);
    }
}

fn poll(lua: &Lua, token: u64) -> (bool, Option<Ref>) {
    let Some(mut s) = state(lua) else { return (true, None) };
    if let Some(c) = s.done.remove(&token) {
        return (true, c);
    }
    (!s.open.iter().any(|m| m.token == token), None)
}

/// One entry of a menu as a popup shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuView {
    Action { r: Ref, text: String, action_id: String, icon: String },
    Separator,
    Sub { r: Ref, title: String, items: Vec<MenuView> },
}

/// `menu`'s entries, submenus expanded (a menu inside itself stops at a
/// depth of 8). Text and titles are read live from `doc`.
pub fn menu_view(lua: &Lua, doc: &rustudio_core::doc::Document, menu: Ref) -> Vec<MenuView> {
    fn walk(lua: &Lua, doc: &rustudio_core::doc::Document, menu: Ref, depth: u32) -> Vec<MenuView> {
        let s = |r: Ref, k: &str| match doc.prop(r, k) {
            Some(Variant::String(s)) => s,
            Some(Variant::ContentId(c)) => c.as_str().to_string(),
            _ => String::new(),
        };
        menu_entries(lua, menu)
            .into_iter()
            .filter_map(|e| match e {
                MenuItem::Separator => Some(MenuView::Separator),
                MenuItem::Item(r) => match doc.dom.get_by_ref(r).map(|i| i.class.as_str()) {
                    Some("PluginAction") => {
                        Some(MenuView::Action { r, text: s(r, "Text"), action_id: s(r, "ActionId"), icon: String::new() })
                    }
                    Some("PluginMenu") if depth < 8 => {
                        Some(MenuView::Sub { r, title: s(r, "Title"), items: walk(lua, doc, r, depth + 1) })
                    }
                    _ => None,
                },
            })
            .collect()
    }
    walk(lua, doc, menu, 0)
}

/// Picks an entry of the top popup by the harness's choice: `0`, `none`,
/// `escape` or `nil` dismiss; `N` / `N.M` is the Nth action-or-submenu
/// entry (1-based, separators not counted) and its Mth entry; anything else
/// is an action's id, full ActionId or Text. Returns whether a popup was
/// open.
pub fn answer_top(lua: &Lua, doc: &rustudio_core::doc::Document, choice: &str) -> bool {
    let Some(top) = open_menus(lua).last().cloned() else { return false };
    let c = choice.trim();
    if matches!(c.to_ascii_lowercase().as_str(), "" | "0" | "none" | "nil" | "escape") {
        answer(lua, top.token, None);
        return true;
    }
    let items = menu_view(lua, doc, top.menu);
    let picked = if c.split('.').all(|p| p.parse::<usize>().is_ok()) {
        let mut list = &items;
        let mut found = None;
        let parts: Vec<usize> = c.split('.').filter_map(|p| p.parse().ok()).collect();
        for (k, n) in parts.iter().enumerate() {
            let real: Vec<&MenuView> = list.iter().filter(|e| **e != MenuView::Separator).collect();
            match real.get(n.saturating_sub(1)) {
                Some(MenuView::Action { r, .. }) if k + 1 == parts.len() => found = Some(*r),
                Some(MenuView::Sub { items, .. }) => list = items,
                _ => break,
            }
        }
        found
    } else {
        fn find(items: &[MenuView], c: &str) -> Option<Ref> {
            items.iter().find_map(|e| match e {
                MenuView::Action { r, text, action_id, .. } => {
                    (action_id == c || action_id.ends_with(&format!("_{c}")) || text == c).then_some(*r)
                }
                MenuView::Sub { items, .. } => find(items, c),
                MenuView::Separator => None,
            })
        }
        find(&items, c)
    };
    answer(lua, top.token, picked);
    true
}

/// ShowAsync as Studio's: yields until the popup is answered, returns the
/// action (or nil), then the action's Triggered fires.
const SHOW_ASYNC: &str = r#"
local open, poll, trigger = ...
return function(self)
    local token = open(self)
    if token == nil then
        task.wait()
        return nil
    end
    while true do
        local done, picked = poll(token)
        if done then
            if picked ~= nil then task.defer(trigger, picked) end
            return picked
        end
        task.wait()
    end
end
"#;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let open = lua.create_function(|lua, v: Value| {
        let r = opt_inst(&v).ok_or_else(|| rt_err("Expected ':' not '.' calling member function ShowAsync"))?;
        Ok(open_menu(lua, r)?.map(|t| t as f64))
    })?;
    let poll_f = lua.create_function(|lua, token: f64| {
        let (done, r) = poll(lua, token as u64);
        let v = match r {
            Some(r) => inst_value(lua, r)?,
            None => Value::Nil,
        };
        Ok((done, v))
    })?;
    let trigger = lua.create_function(|lua, v: Value| {
        if let Some(r) = opt_inst(&v) {
            world(lua)?.fire(r, "Triggered", vec![]);
        }
        Ok(())
    })?;
    let maker: mlua::Function = lua.load(SHOW_ASYNC).set_name("=rustudio.plugin_showasync").into_function()?;
    let f: mlua::Function = maker.call((open, poll_f, trigger))?;
    if let Some(mut c) = lua.app_data_mut::<Cache>() {
        c.overrides.insert("ShowAsync", f);
    }
    Ok(())
}
