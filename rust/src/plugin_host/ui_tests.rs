//! Toolbars, menus and actions : the Lua-visible behaviour
//! measured in Studio (checks/plugins/ui/README.md) and the ribbon model.

use std::sync::Arc;

use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::*;
use crate::plugin_host::api::{menus, toolbar};

fn src(name: &str, code: &str) -> PluginSource {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(root, InstanceBuilder::new("Script").with_name("Main").with_property("Source", code.to_string()));
    PluginSource::Tree { name: format!("user_{name}.lua"), asset_id: None, dom: Arc::new(dom) }
}

fn frames(h: &mut PluginHost, doc: &mut Document, n: usize) {
    for _ in 0..n {
        h.tick(doc, 1.0 / 60.0, true, false);
    }
}

fn out(h: &mut PluginHost) -> Vec<String> {
    h.take_log().into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()).collect()
}

fn ribbon(h: &PluginHost, doc: &Document) -> Vec<toolbar::RibbonGroup> {
    toolbar::ribbon(h.edit_lua().unwrap(), doc)
}

#[test]
fn toolbars_group_sort_and_dedupe_as_studio() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(src(
        "A",
        r#"
        local tb = plugin:CreateToolbar("UiBar")
        local tb2 = plugin:CreateToolbar("UiBar")
        print("same", tb == tb2, tb.Name, tb.Parent)
        tb:CreateButton("B1", "tip one", "", "One")
        tb2:CreateButton("B2", "tip two", "", "Line1\nLine2")
        tb:CreateButton("B3", "tip three", "", "")
        print("dup", pcall(function() return tb2:CreateButton("B1", "dup", "", "Dup") end))
        for _, n in ipairs({"Zeta", "Alpha", "Mid"}) do plugin:CreateToolbar(n):CreateButton(n, n, "", n) end
        "#,
    ))
    .unwrap();
    h.load(src("B", r#"plugin:CreateToolbar("UiBar"):CreateButton("B1", "other", "", "Other One")"#)).unwrap();
    frames(&mut h, &mut doc, 2);
    let o = out(&mut h);
    assert!(o.iter().any(|l| l == "same true Instance nil"), "{o:?}");
    assert!(
        o.iter().any(|l| l == "dup false Cannot create more than one button with id \"B1\" in toolbar with id \"UiBar\" in plugin with id \"user_A.lua\""),
        "{o:?}"
    );
    let g = ribbon(&h, &doc);
    let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Alpha", "Mid", "UiBar", "Zeta", "UiBar"]);
    let labels: Vec<&str> = g[2].buttons.iter().map(|b| b.label.as_str()).collect();
    assert_eq!(labels, ["One", "Line1 Line2", "B3"], "no text: the id");
    assert_eq!(g[4].buttons[0].label, "Other One");
    assert_eq!(g[2].buttons[0].key, "user_A.lua_UiBar_B1");
}

#[test]
fn button_state_is_live_and_destroyed_buttons_stay() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(src(
        "A",
        r#"
        local tb = plugin:CreateToolbar("T")
        local a = tb:CreateButton("a", "", "", "A")
        local b = tb:CreateButton("b", "", "", "B")
        a.Click:Connect(function() print("first") end)
        a.Click:Connect(function() print("second") end)
        task.wait()
        a.Icon = "rbxassetid://5"
        a.Enabled = false
        a.ClickableWhenViewportHidden = true
        b:SetActive(true)
        b:Destroy()
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 3);
    let g = ribbon(&h, &doc);
    assert_eq!(g.len(), 1);
    let (a, b) = (&g[0].buttons[0], &g[0].buttons[1]);
    assert_eq!(a.icon, "rbxassetid://5");
    assert!(!a.enabled && a.clickable_when_viewport_hidden);
    assert!(b.active, "SetActive look");
    assert_eq!(g[0].buttons.len(), 2, "a destroyed button stays in the ribbon (measured)");
    // Click: handlers most recently connected first.
    h.trigger(Trigger::Fire(a.r, "Click"));
    frames(&mut h, &mut doc, 1);
    let o = out(&mut h);
    let i1 = o.iter().position(|l| l == "first");
    let i2 = o.iter().position(|l| l == "second");
    assert!(i2 < i1 && i1.is_some(), "most recently connected first: {o:?}");
}

#[test]
fn unloading_removes_toolbars_and_closes_menus() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let id = h
        .load(src(
            "A",
            r#"
            plugin:CreateToolbar("T"):CreateButton("a", "", "", "A")
            local m = plugin:CreatePluginMenu("m", "M", "")
            m:AddNewAction("x", "X", "")
            print("got", m:ShowAsync())
            "#,
        ))
        .unwrap();
    frames(&mut h, &mut doc, 2);
    assert_eq!(menus::open_menus(h.edit_lua().unwrap()).len(), 1);
    h.unload(id);
    frames(&mut h, &mut doc, 2);
    assert!(ribbon(&h, &doc).is_empty());
    assert!(menus::open_menus(h.edit_lua().unwrap()).is_empty());
}

#[test]
fn show_async_yields_returns_then_triggers() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(src(
        "A",
        r#"
        local m = plugin:CreatePluginMenu("UiMenu", "Ui Menu", "")
        local a = plugin:CreatePluginAction("UiAct", "Ui Action", "tip", "", true)
        a.Triggered:Connect(function() print("triggered", a.ActionId) end)
        m:AddAction(a)
        m:AddSeparator()
        local sub = plugin:CreatePluginMenu("UiSub", "Sub", "")
        sub:AddNewAction("SubA", "Sub A", "")
        m:AddMenu(sub)
        local empty = plugin:CreatePluginMenu("E", "E", "")
        print("empty", empty:ShowAsync())
        local r = m:ShowAsync()
        print("returned", r and r.ActionId)
        local r2 = m:ShowAsync()
        print("returned2", r2 and r2.ActionId)
        local r3 = m:ShowAsync()
        print("returned3", r3)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 3);
    let o = out(&mut h);
    assert!(o.iter().any(|l| l == "empty nil"), "{o:?}");
    assert!(!o.iter().any(|l| l.starts_with("returned")), "ShowAsync yields: {o:?}");
    let lua = h.edit_lua().unwrap();
    let open = menus::open_menus(lua);
    assert_eq!(open.len(), 1);
    let view = menus::menu_view(lua, &doc, open[0].menu);
    assert_eq!(view.len(), 3);
    assert!(matches!(&view[2], menus::MenuView::Sub { title, items, .. } if title == "Sub" && items.len() == 1));
    // The harness picks by id; Triggered comes after the return.
    h.trigger(Trigger::parse("plugin-menu:UiAct").unwrap());
    frames(&mut h, &mut doc, 2);
    let o = out(&mut h);
    let ret = o.iter().position(|l| l == "returned user_A.lua_UiAct").expect("returned");
    let trig = o.iter().position(|l| l == "triggered user_A.lua_UiAct").expect("triggered");
    assert!(ret < trig, "{o:?}");
    // By position into the submenu, then dismissed.
    h.trigger(Trigger::parse("plugin-menu:2.1").unwrap());
    frames(&mut h, &mut doc, 2);
    h.trigger(Trigger::parse("plugin-menu:escape").unwrap());
    frames(&mut h, &mut doc, 2);
    let o = out(&mut h);
    assert!(o.iter().any(|l| l == "returned2 user_A.lua_SubA"), "{o:?}");
    assert!(o.iter().any(|l| l == "returned3 nil"), "{o:?}");
}

#[test]
fn stacked_menus_answer_top_first() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(src(
        "A",
        r#"
        local m1 = plugin:CreatePluginMenu("m1", "M1", "") m1:AddNewAction("a1", "A1", "")
        local m2 = plugin:CreatePluginMenu("m2", "M2", "") m2:AddNewAction("a2", "A2", "")
        task.spawn(function() print("first", m1:ShowAsync()) end)
        task.wait()
        print("second", m2:ShowAsync())
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 4);
    assert_eq!(menus::open_menus(h.edit_lua().unwrap()).len(), 2);
    h.trigger(Trigger::parse("plugin-menu:escape").unwrap());
    frames(&mut h, &mut doc, 2);
    let o = out(&mut h);
    assert!(o.iter().any(|l| l == "second nil") && !o.iter().any(|l| l.starts_with("first")), "{o:?}");
    h.trigger(Trigger::parse("plugin-menu:escape").unwrap());
    frames(&mut h, &mut doc, 2);
    assert!(out(&mut h).iter().any(|l| l == "first nil"));
}

#[test]
fn actions_by_id_or_action_id_and_binding_flags() {
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(src(
        "A",
        r#"
        local a = plugin:CreatePluginAction("Act", "Text", "Tip", "", true)
        local b = plugin:CreatePluginAction("Hidden", "Hidden", "x", "", false)
        local m = plugin:CreatePluginMenu("m", "M", "")
        m:AddNewAction("New", "New", "")
        a.Triggered:Connect(function() print("act") end)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    h.trigger(Trigger::parse("plugin-action:Act").unwrap());
    h.trigger(Trigger::parse("plugin-action:user_A.lua_Act").unwrap());
    frames(&mut h, &mut doc, 1);
    assert_eq!(out(&mut h).iter().filter(|l| *l == "act").count(), 2);
    let bindable: Vec<String> = h
        .ui_items()
        .into_iter()
        .filter_map(|u| match u.kind {
            UiKind::Action { id, allow_binding: true, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(bindable, ["Act"], "allowBinding false and AddNewAction actions aren't listed");
}
