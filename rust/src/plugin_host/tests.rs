//! Plugin host tests: plugins run against a live edit document, the way the
//! editor drives them (PluginHost::tick once a frame).

use std::sync::Arc;

use rbx_dom_weak::types::Variant;
use rbx_dom_weak::{ustr, InstanceBuilder, WeakDom};

use super::*;

/// Settings go to a per-process temp folder (one for every test module:
/// RUSTUDIO_PLUGIN_DATA is process-wide, so a second folder set by another
/// module's tests would move the files under a running test).
pub(crate) fn temp_data() {
    // (the engine's `__rs_…` internals are hidden in the shipped binary and
    // in a non-test build of the engine crate; plugin tests drive them)
    rustudio_engine::globals_parity::show_internals(true);
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rustudio-plugin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("RUSTUDIO_PLUGIN_DATA", &dir);
    });
}

/// A plugin model holding one Script with `src` (plus `extra` items).
fn tree(src: &str, extra: Vec<InstanceBuilder>) -> Arc<WeakDom> {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(root, InstanceBuilder::new("Script").with_name("Main").with_property("Source", src.to_string()));
    for e in extra {
        dom.insert(root, e);
    }
    Arc::new(dom)
}

fn source(name: &str, src: &str) -> PluginSource {
    PluginSource::Tree { name: format!("user_{name}.rbxmx"), asset_id: None, dom: tree(src, vec![]) }
}

/// Output lines without their " — source" suffix.
fn lines(h: &mut PluginHost) -> Vec<String> {
    h.take_log()
        .into_iter()
        .map(|(_, t)| t.split("  —").next().unwrap_or("").to_string())
        .collect()
}

fn frames(h: &mut PluginHost, doc: &mut Document, n: usize) {
    for _ in 0..n {
        h.tick(doc, 1.0 / 60.0, true, false);
    }
}

fn has(out: &[String], want: &str) -> bool {
    out.iter().any(|l| l == want)
}

#[test]
fn plugin_runs_in_the_edit_document() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Basic",
        r#"
        print("start", typeof(plugin), plugin.ClassName, plugin.Name, script.Name, script.Parent == plugin, plugin.Parent)
        printidentity("ident")
        local RS = game:GetService("RunService")
        print("rs", RS:IsEdit(), RS:IsRunning(), RS:IsStudio(), RS:IsClient(), RS:IsServer(), RS:IsRunMode(), time())
        print("caps", pcall(settings), (pcall(function() return game:GetService("CoreGui") end)))
        print("children", #plugin:GetChildren(), plugin:GetChildren()[2].ClassName)
        local p = Instance.new("Part")
        p.Name = "FromPlugin"
        p.Parent = workspace
        local hb, rs = 0, 0
        RS.Heartbeat:Connect(function() hb += 1 end)
        RS.RenderStepped:Connect(function() rs += 1 end)
        local st = 0
        pcall(function() RS.Stepped:Connect(function() st += 1 end) end)
        task.wait(0.1)
        print("frames", hb >= 5, rs >= 5, st)
        print("loadstring", loadstring("return 1 + 1")())
        "#,
    ))
    .unwrap();
    assert!(!doc.dirty());
    frames(&mut h, &mut doc, 12);
    let out = lines(&mut h);
    assert!(has(&out, "start Instance Plugin user_Basic.rbxmx Main true nil"), "{out:#?}");
    assert!(has(&out, "ident 5"), "{out:#?}");
    assert!(has(&out, "Current asset id is -2"), "{out:#?}");
    assert!(has(&out, "rs true false true true true false 0"), "{out:#?}");
    assert!(has(&out, "caps true true"), "{out:#?}");
    assert!(has(&out, "children 2 MultipleDocumentInterfaceInstance"), "{out:#?}");
    assert!(has(&out, "frames true true 0"), "{out:#?}");
    assert!(has(&out, "loadstring 2"), "{out:#?}");
    // The plugin's Part is in the editor's own document, not a copy.
    let ws = doc.workspace().unwrap();
    assert!(doc.find_named(ws, "FromPlugin").is_some());
    // The plugin's own tree is not in the DataModel and doesn't dirty it…
    let root = doc.dom.root_ref();
    assert!(doc.dom.descendants_of(root).all(|i| i.class != "Plugin"));
    // …but its edit of the place does.
    assert!(doc.dirty());
    assert_eq!(h.loaded()[0].running, true);
}

#[test]
fn own_bookkeeping_keeps_the_place_saved() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("Quiet", "local f = Instance.new('Folder') f.Parent = plugin print('ok')")).unwrap();
    frames(&mut h, &mut doc, 3);
    assert!(has(&lines(&mut h), "ok"));
    assert!(!doc.dirty(), "a plugin working on its own instances must not dirty the place");
}

#[test]
fn editor_edits_reach_plugins_as_signals() {
    temp_data();
    let mut doc = Document::new_place();
    let ws = doc.workspace().unwrap();
    let existing = doc.insert(ws, InstanceBuilder::new("Part").with_name("Existing"));
    let mut h = PluginHost::default();
    h.load(source(
        "Watch",
        r#"
        workspace.ChildAdded:Connect(function(c) print("added", c.Name) end)
        workspace.ChildRemoved:Connect(function(c) print("removed", c.Name) end)
        workspace.DescendantAdded:Connect(function(c) print("desc", c.Name) end)
        local e = workspace.Existing
        e.Changed:Connect(function(n) print("changed", n) end)
        e:GetPropertyChangedSignal("Name"):Connect(function() print("renamed to", e.Name) end)
        e.AncestryChanged:Connect(function(_, p) print("ancestry", p and p.Name) end)
        print("ready")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    assert!(has(&lines(&mut h), "ready"));
    // The editor (not Lua) edits the document between frames.
    doc.insert(ws, InstanceBuilder::new("Part").with_name("NewOne"));
    doc.rename(existing, "Renamed");
    doc.set_props("Anchor", vec![(existing, ustr("Anchored"), Variant::Bool(true))], None);
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "added NewOne"), "{out:#?}");
    assert!(has(&out, "desc NewOne"), "{out:#?}");
    assert!(has(&out, "changed Name"), "{out:#?}");
    assert!(has(&out, "renamed to Renamed"), "{out:#?}");
    assert!(has(&out, "changed Anchored"), "{out:#?}");
    // Undo reaches them too.
    doc.undo();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "changed Anchored"), "{out:#?}");
    doc.remove(&[existing]);
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "removed Renamed"), "{out:#?}");
    assert!(has(&out, "ancestry nil"), "{out:#?}");
    // A plugin's own writes don't come back as edits.
    assert!(!out.iter().any(|l| l.starts_with("added FromPlugin")));
}

#[test]
fn modules_have_no_plugin_global_and_scripts_run_as_studio_picks() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    let main = dom.insert(
        root,
        InstanceBuilder::new("Script")
            .with_name("Main")
            .with_property("Source", "print('main', typeof(require(script.Mod)))".to_string()),
    );
    dom.insert(main, InstanceBuilder::new("ModuleScript").with_name("Mod").with_property("Source", "print('mod plugin', plugin) printidentity('mod') return 1".to_string()));
    dom.insert(main, InstanceBuilder::new("Script").with_name("Nested").with_property("Source", "print('nested', typeof(plugin))".to_string()));
    dom.insert(main, InstanceBuilder::new("LocalScript").with_name("Loc").with_property("Source", "print('local')".to_string()));
    dom.insert(main, InstanceBuilder::new("Script").with_name("Off").with_property("Source", "print('off')".to_string()).with_property("Disabled", true));
    let rc = |v: u32| Variant::Enum(rbx_dom_weak::types::Enum::from_u32(v));
    dom.insert(main, InstanceBuilder::new("Script").with_name("Srv").with_property("Source", "print('server ctx')".to_string()).with_property("RunContext", rc(1)));
    dom.insert(main, InstanceBuilder::new("Script").with_name("Cli").with_property("Source", "print('client ctx')".to_string()).with_property("RunContext", rc(2)));
    dom.insert(main, InstanceBuilder::new("Script").with_name("Plg").with_property("Source", "print('plugin ctx')".to_string()).with_property("RunContext", rc(3)));
    h.load(PluginSource::Tree { name: "user_Tree.rbxmx".into(), asset_id: None, dom: Arc::new(dom) }).unwrap();
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    let order: Vec<String> = out.iter().filter(|l| !l.starts_with("Current asset")).cloned().collect();
    assert_eq!(order, ["mod plugin nil", "mod 5", "main number", "nested Instance", "local", "plugin ctx"], "{out:#?}");
}

#[test]
fn errors_are_attributed_and_reported() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("Err", "local x = nil\nprint(x.y)")).unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "user_Err.rbxmx.Main:2: attempt to index nil with 'y'"), "{out:#?}");
    assert!(out.iter().any(|l| l.ends_with("Script 'user_Err.rbxmx.Main', Line 2")), "{out:#?}");
    let info = &h.loaded()[0];
    assert_eq!(info.errors, vec!["user_Err.rbxmx.Main:2: attempt to index nil with 'y'".to_string()]);
    assert!(h.result_json()[0]["errors"][0].as_str().unwrap().contains("attempt to index nil"));
}

#[test]
fn unload_fires_unloading_then_stops_everything() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let id = h
        .load(source(
            "Bye",
            r#"
            local n = 0
            game:GetService("RunService").Heartbeat:Connect(function() n += 1 end)
            task.spawn(function() while true do task.wait(0.05) _G.bye_ticks = n end end)
            plugin.Unloading:Connect(function() print("unloading", plugin.Parent, script.Parent == plugin) end)
            plugin.Destroying:Connect(function() print("destroying") end)
            "#,
        ))
        .unwrap();
    frames(&mut h, &mut doc, 10);
    h.unload(id);
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "unloading nil true"), "{out:#?}");
    assert!(!has(&out, "destroying"), "Studio fires no Destroying on unload: {out:#?}");
    assert!(h.loaded().is_empty());
}

/// A thread parked inside a required ModuleScript's function (Time +'s
/// PluginManager `repeat task.wait(1)`, AutoScale's StudioLoaded wait) ends
/// with the plugin: it never resumes into the unloaded Plugin.
#[test]
fn unload_stops_threads_waiting_inside_modules() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    let module = InstanceBuilder::new("ModuleScript").with_name("Waiter").with_property(
        "Source",
        r#"
        local m = {}
        function m.later(p) task.wait(0.2) print("module resumed") p:CreateToolbar("after") end
        function m.signal(p) game:GetService("RunService").Heartbeat:Wait() task.wait(0.2) print("signal resumed") end
        return m
        "#
        .to_string(),
    );
    let dom = tree(
        r#"
        local W = require(script.Parent.Waiter)
        task.spawn(W.signal, plugin)
        W.later(plugin)
        print("main resumed")
        "#,
        vec![module],
    );
    let id = h.load(PluginSource::Tree { name: "user_ModWait.rbxmx".into(), asset_id: None, dom }).unwrap();
    frames(&mut h, &mut doc, 3);
    h.unload(id);
    frames(&mut h, &mut doc, 40);
    let out = lines(&mut h);
    assert!(!out.iter().any(|l| l.contains("resumed") || l.contains("not loaded")), "{out:#?}");
    assert!(h.loaded().is_empty());
}

#[test]
fn settings_round_trip_and_share_between_local_plugins() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "SetA",
        r#"
        plugin:SetSetting("st_n", 5)
        plugin:SetSetting("st_t", {a = 1, b = {true, "x"}})
        plugin:SetSetting("st_v", Vector3.new(1, 2, 3))
        plugin:SetSetting("st_e", Enum.Material.Grass)
        plugin:SetSetting("st_i", workspace)
        plugin:SetSetting("st_m", {1, 2, x = 3})
        plugin:SetSetting("st.dot", 7)
        plugin:SetSetting("st_nan", 0/0)
        print("getA", plugin:GetSetting("st_n"), plugin:GetSetting("st_t").b[2], plugin:GetSetting("st_v"),
            plugin:GetSetting("st_e"), plugin:GetSetting("st_i"), #plugin:GetSetting("st_m"),
            plugin:GetSetting("st_dot"), plugin:GetSetting("st_nan"), plugin:GetSetting("missing"))
        print("argerr", pcall(function() plugin:GetSetting() end))
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 1);
    h.load(source("GetB", "print('getB', plugin:GetSetting('st_n'))")).unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "getA 5 x 1, 2, 3 Grass Instance 2 7 nil nil"), "{out:#?}");
    assert!(has(&out, "argerr false Argument 1 missing or nil"), "{out:#?}");
    assert!(has(&out, "getB 5"), "{out:#?}");
    let file = settings::file("0");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("{ ") && text.contains("\"st_v\":\"1, 2, 3\""), "{text}");
}

#[test]
fn toolbar_button_and_action_fire_from_the_harness() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Ui",
        r#"
        local tb = plugin:CreateToolbar("Tools")
        local b = tb:CreateButton("Go", "tip", "", "Go!")
        print("objs", tb.ClassName, tb.Name, tb.Parent, b.ClassName, b.Enabled)
        b.Click:Connect(function() print("click") end)
        local a = plugin:CreatePluginAction("act", "Act", "tip")
        print("action", a.ActionId, a.Name)
        a.Triggered:Connect(function() print("triggered") end)
        print("argerr", pcall(function() plugin:CreatePluginAction() end))
        print("dup", pcall(function() plugin:CreatePluginAction("act", "Act", "tip") end))
        local m = plugin:GetMouse()
        print("mouse", m.ClassName, m.Name, m.X, m.Y, m == plugin:GetMouse())
        local info = DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Float, true, false, 300, 200, 100, 80)
        print("info", tostring(info), info.FloatingXSize, pcall(function() return info.InitialDockState end))
        local w = plugin:CreateDockWidgetPluginGui("W", info)
        print("widget", w.ClassName, w.Name, w.Title, w.Enabled, w.Parent and w.Parent.Name)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 1);
    h.trigger(Trigger::parse("plugin-button:Tools/Go").unwrap());
    h.trigger(Trigger::parse("plugin-action:act").unwrap());
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "objs PluginToolbar Instance nil PluginToolbarButton true"), "{out:#?}");
    assert!(has(&out, "action user_Ui.rbxmx_act Instance"), "{out:#?}");
    assert!(has(&out, "argerr false Argument 3 missing or nil"), "{out:#?}");
    assert!(has(&out, "dup false This plugin cannot create more than one PluginAction with id: \"act\""), "{out:#?}");
    assert!(has(&out, "mouse PluginMouse Instance -1 -1 true"), "{out:#?}");
    assert!(
        has(&out, "info InitialDockState:Float InitialEnabled:1 InitialEnabledShouldOverrideRestore:0 FloatingXSize:300 FloatingYSize:200 MinWidth:100 MinHeight:80 300 false 'InitialDockState' is not a valid member of DockWidgetPluginGuiInfo"),
        "{out:#?}"
    );
    assert!(has(&out, "widget DockWidgetPluginGui PluginGui  true PluginGuiService"), "{out:#?}");
    assert!(has(&out, "click"), "{out:#?}");
    assert!(has(&out, "triggered"), "{out:#?}");
    let kinds: Vec<_> = h.ui_items().into_iter().map(|u| u.kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, UiKind::Button { id, .. } if id == "Go")));
    assert!(kinds.iter().any(|k| matches!(k, UiKind::DockWidget { id, info } if id == "W" && info.initial_dock_state == "Float")));
    // The widget's service isn't saved with the place.
    let bytes = doc.encode_all(true).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PluginGuiService"));
}

#[test]
fn selection_changes_announce_after_the_caller_yields() {
    temp_data();
    let mut doc = Document::new_place();
    let ws = doc.workspace().unwrap();
    let a = doc.insert(ws, InstanceBuilder::new("Part").with_name("A"));
    let mut h = PluginHost::default();
    h.load(source(
        "Sel",
        r#"
        local S = game:GetService("Selection")
        local n = 0
        S.SelectionChanged:Connect(function() n += 1 print("changed", #S:Get()) end)
        S:Set({workspace.A})
        S:Add({workspace.A, workspace})
        print("right after", n, #S:Get())
        print("err1", pcall(function() S:Set({1}) end))
        print("err2", pcall(function() S:Set(workspace) end))
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "right after 0 2"), "{out:#?}");
    assert_eq!(out.iter().filter(|l| l.starts_with("changed")).count(), 2, "{out:#?}");
    assert!(has(&out, "err1 false Unable to cast value to Object"), "{out:#?}");
    assert!(has(&out, "err2 false Expected table argument, got userdata"), "{out:#?}");
    assert_eq!(doc.selection.len(), 2);
    // The editor selects: one SelectionChanged next frame.
    doc.selection = vec![a];
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    assert_eq!(out, vec!["changed 1".to_string()], "{out:#?}");
}

#[test]
fn activation_is_one_plugin_at_a_time() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("Act1", "plugin.Deactivation:Connect(function() print('act1 deactivated', plugin:IsActivated()) end) plugin:Activate(true) _G.act1 = plugin")).unwrap();
    frames(&mut h, &mut doc, 1);
    assert!(h.active_plugin().is_some());
    assert!(h.take_activation_change());
    h.load(source("Act2", "plugin:Activate(false) print('act2', plugin:IsActivated(), _G.act1:IsActivated()) plugin:Deactivate() plugin:Deactivate() print('act2 off', plugin:IsActivated())")).unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "act1 deactivated false"), "{out:#?}");
    assert!(has(&out, "act2 true false"), "{out:#?}");
    assert!(has(&out, "act2 off false"), "{out:#?}");
    assert!(h.active_plugin().is_none());
}

#[test]
fn new_document_reloads_plugins() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("Again", "print('loaded in', game.Name ~= nil)")).unwrap();
    frames(&mut h, &mut doc, 1);
    assert_eq!(lines(&mut h).iter().filter(|l| l.starts_with("loaded in")).count(), 1);
    let mut other = Document::new_place();
    frames(&mut h, &mut other, 1);
    assert_eq!(lines(&mut h).iter().filter(|l| l.starts_with("loaded in")).count(), 1);
}

#[test]
fn play_copies_run_with_play_values() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("Both", "local RS = game:GetService('RunService') print('dm', RS:IsEdit(), RS:IsRunning(), RS:IsServer(), RS:IsClient()) printidentity('id')")).unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "dm true false true true"), "{out:#?}");
    // A Play server: the same plugin loads again there.
    let rt = crate::studio_hooks::new_runtime().unwrap();
    rt.enter(World::new(doc.clone_for_play("p".into()), RunMode::Play));
    rt.bind_globals().unwrap();
    h.load_into_play(&rt);
    rt.dispatch();
    h.unload_from_play(&rt);
    let w = rt.leave();
    let out: Vec<String> = w.output.iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()).collect();
    assert!(has(&out, "dm false true true false"), "{out:#?}");
    assert!(has(&out, "id 5"), "{out:#?}");
}

#[test]
fn plugins_folder_order() {
    let dir = std::env::temp_dir().join(format!("rustudio-plugin-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Sub/Deeper")).unwrap();
    for f in ["Zz.lua", "Aa.lua", "b.LUA", "Ab.rbxmx", "Zy.rbxmx", "M.rbxm", "skip.luau", "x.txt", "Sub/In.lua", "Sub/Deeper/D.lua", "Sub/no.rbxmx"] {
        std::fs::write(dir.join(f), "").unwrap();
    }
    let names: Vec<String> = source::dir_order(&dir).unwrap().iter().map(|s| s.plugin_name()).collect();
    assert_eq!(
        names,
        ["user_Aa.lua", "user_b.LUA", "user_Sub/Deeper/D.lua", "user_Sub/In.lua", "user_Zz.lua", "user_Ab.rbxmx", "user_Zy.rbxmx", "user_M.rbxm"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn turned_off_loads_nothing() {
    let mut h = PluginHost::default();
    h.set_enabled(false);
    assert!(h.load(source("No", "print('no')")).is_err());
    let mut doc = Document::new_place();
    frames(&mut h, &mut doc, 1);
    assert!(lines(&mut h).iter().all(|l| l != "no"));
}

#[test]
fn studio_theme_colors_as_studio() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Theme",
        r#"
        local th = settings().Studio.Theme
        print("theme", th.ClassName, th.Name)
        print("bg", th:GetColor(Enum.StudioStyleGuideColor.MainBackground):ToHex(), th:GetColor("MainBackground"):ToHex())
        print("sb", th:GetColor(Enum.StudioStyleGuideColor.ScrollBar, Enum.StudioStyleGuideModifier.Pressed):ToHex())
        print("err", pcall(function() return th:GetColor() end))
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    // Studio 0.747's Dark palette (studio_theme.txt).
    assert!(has(&out, "theme StudioTheme Dark"), "{out:#?}");
    assert!(has(&out, "bg 191a1f 191a1f"), "{out:#?}");
    assert!(has(&out, "sb 494d5a"), "{out:#?}");
    assert!(has(&out, "err false Argument 1 missing or nil"), "{out:#?}");
}

#[test]
fn input_object_changed_and_property_signals_fire_once_per_write() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "IoSignals",
        r#"
        local io = __rs_new_input_object({ UserInputType = Enum.UserInputType.MouseButton1 })
        local n, c = 0, {}
        io:GetPropertyChangedSignal("UserInputState"):Connect(function() n += 1 end)
        io.Changed:Connect(function(p) table.insert(c, p) end)
        io.UserInputState = Enum.UserInputState.End
        io.UserInputState = Enum.UserInputState.End -- same value: no signal
        io.Position = Vector3.new(1, 2, 0)
        local ok, err = pcall(function() return io:GetPropertyChangedSignal("Nope") end)
        print("io", n, table.concat(c, ","), ok, err)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    assert!(has(&out, "io 1 UserInputState,Position false user_IoSignals.rbxmx.Main:9: Nope is not a valid property name."), "{out:#?}");
}

#[test]
fn command_identity_gets_plugin_manager_interface() {
    use rustudio_engine::runtime::security::{with_main_identity, COMMAND};

    let rt = crate::studio_hooks::new_runtime().unwrap();
    rt.enter(World::new(Document::new_place(), RunMode::Run));
    rt.bind_globals().unwrap();
    let class = with_main_identity(&rt.lua, COMMAND, || {
        rt.lua.load("return PluginManager().ClassName").eval::<String>()
    }).unwrap();
    assert_eq!(class, "PluginManagerInterface");
}

#[test]
fn builtin_plugin_runs_at_identity_6_with_roblox_script_capabilities() {
    temp_data();
    let dir = std::env::temp_dir().join(format!("rustudio-builtin-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Write a minimal valid binary .rbxm with a Script whose Source is Luau bytecode or plain string.
    // CoreScripts/BuiltInPlugins decode plain strings too when Source isn't ProtectedString.
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(
        root,
        InstanceBuilder::new("Script").with_name("Loader").with_property(
            "Source",
            r#"
            printidentity("bi")
            print("name", plugin.Name)
            print("dm", plugin.HostDataModelType.Name, plugin.HostDataModelTypeIsCurrent)
            local mdi = plugin.MultipleDocumentInterfaceInstance
            print("mdi", mdi and mdi.ClassName, mdi and mdi.FocusedDataModelSession and mdi.FocusedDataModelSession.CurrentDataModelType.Name)
            local okf, fv = pcall(function() return game:DefineFastFlag("BuiltInTestFlag", true) end)
            print("flag", okf, fv)
            local w = plugin:CreateDockWidgetPluginGui("TestDock", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Left, true, true, 200, 200, 100, 100))
            print("widget_parent", w.Parent and w.Parent.ClassName)
            "#
            .to_string(),
        ),
    );
    let tops = dom.root().children().to_vec();
    let mut bin = Vec::new();
    rbx_binary::to_writer(&mut bin, &dom, &tops).unwrap();
    let file = dir.join("SampleTool.rbxm");
    std::fs::write(&file, &bin).unwrap();

    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load_builtin_dir(&dir, None).into_iter().for_each(|r| { r.unwrap(); });
    frames(&mut h, &mut doc, 4);
    let out = lines(&mut h);
    assert!(has(&out, "bi 6"), "{out:#?}");
    assert!(has(&out, "name builtin_SampleTool.rbxm"), "{out:#?}");
    assert!(has(&out, "dm Edit true"), "{out:#?}");
    assert!(has(&out, "mdi MultipleDocumentInterfaceInstance Edit"), "{out:#?}");
    assert!(has(&out, "flag true true"), "{out:#?}");
    assert!(has(&out, "widget_parent RobloxPluginGuiService"), "{out:#?}");
    // Built-in widget under RobloxPluginGuiService must not dirty the place.
    assert!(!doc.dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn builtin_plugin_round2_apis_and_security_gates() {
    temp_data();
    let dir = std::env::temp_dir().join(format!("rustudio-builtin-r2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(
        root,
        InstanceBuilder::new("Script").with_name("Loader").with_property(
            "Source",
            r#"
            -- Actions component: CreateAsync returns { signal }, BindToChangedAsync & GetAsync defaults
            local actions = plugin:GetPluginComponent("Actions")
            local uri = { DataModel = "Standalone", PluginType = "Standalone", PluginId = "Test", Category = "Actions", ItemId = "DoThing" }
            local created = actions:CreateAsync({ Uri = uri, Text = "Do", Enabled = true }, false)
            assert(type(created) == "table" and typeof(created[1]) == "RBXScriptSignal", "CreateAsync array signal")
            local fired = 0
            created[1]:Connect(function() fired += 1 end)
            local ch = actions:BindToChangedAsync(uri)
            assert(typeof(ch) == "RBXScriptSignal", "BindToChangedAsync signal")
            local changed = 0
            ch:Connect(function() changed += 1 end)
            local got = actions:GetAsync({ { DataModel = "Edit", PluginId = "Studio", Category = "Actions", ItemId = "PreExisting" } })
            assert(got[1] and got[1].Checked == false and got[1].Enabled == true, "GetAsync default descriptor")
            actions:UpdateAsync({ { Uri = uri, Checked = true } })
            assert(changed == 1, "UpdateAsync fires BindToChangedAsync")

            -- Policy service (lowercase alias + canonical)
            local pps = game:GetService("PluginPolicyService")
            local tb = pps:getPluginPolicy("Toolbox")
            assert(type(tb) == "table" and tb.ShowRobloxCreatedAssets == false and tb.Enabled == true, "getPluginPolicy Toolbox")

            -- Built-in services & signals
            local gs = game:GetService("GuiService")
            assert(typeof(gs.Open9SliceEditor) == "RBXScriptSignal")
            assert(typeof(gs.OpenStyleEditor) == "RBXScriptSignal")
            local ss = game:GetService("StudioService")
            assert(typeof(ss.OnOpenGameSettings) == "RBXScriptSignal")
            assert(ss:GetStartupAssetId() == "")
            local pkg = game:GetService("PackageUIService")
            assert(typeof(pkg.OnOpenConvertToPackagePlugin) == "RBXScriptSignal")
            local pub = game:GetService("StudioPublishService")
            assert(pub ~= nil and typeof(pub.OnSaveOrPublishPlaceToRoblox) == "RBXScriptSignal")
            local ms = game:GetService("MaterialService")
            assert(typeof(ms.MaterialFillToolEnabledChanged) == "RBXScriptSignal")
            assert(ms:GetBaseMaterialOverride(Enum.Material.Grass) == "Grass")
            local cs = game:GetService("CollaboratorsService")
            assert(cs:GetSelectionHighlightsEnabled() == true)
            cs:ToggleSelectionHighlights()
            assert(cs:GetSelectionHighlightsEnabled() == false)

            -- Toolbar popup button
            local tbar = plugin:CreateToolbar("PopupBar")
            local pbtn = tbar:CreatePopupButton("Pop", "Tip", "rbxlocaltheme://GameSettings", "Pop")
            pbtn:SetDropdownActive(true)
            pbtn:SetDropdownActive(false)

            -- SecurityCapabilities global
            local caps = SecurityCapabilities.new(Enum.SecurityCapability.Basic, Enum.SecurityCapability.RunClientScript)
            assert(caps:Contains(Enum.SecurityCapability.Basic))
            caps:Remove(Enum.SecurityCapability.Basic)
            assert(not caps:Contains(Enum.SecurityCapability.Basic))
            caps:Add(Enum.SecurityCapability.Basic)
            assert(caps:Contains(Enum.SecurityCapability.Basic, Enum.SecurityCapability.RunClientScript))

            -- Plugin callback assignment (Toolbox drag)
            plugin.ProcessAssetInsertionDrag = function() return true end
            plugin.ProcessAssetInsertionDrop = function() return true end

            -- R15 HumanoidDescription creates standard scale NumberValues (UGCValidation)
            local desc = Instance.new("HumanoidDescription")
            local rig = game:GetService("Players"):CreateHumanoidModelFromDescription(desc, Enum.HumanoidRigType.R15)
            local hum = rig:FindFirstChildOfClass("Humanoid")
            assert(hum and hum:FindFirstChild("BodyTypeScale") and hum:FindFirstChild("BodyProportionScale"), "R15 scales")

            -- LocalizationTable FormatByKey cache + invalidation on SetEntryValue
            local lt = Instance.new("LocalizationTable")
            lt:SetEntryValue("k.hello", "Hello", "", "en-us", "Hello")
            local tr = lt:GetTranslator("en-us")
            assert(tr:FormatByKey("k.hello") == "Hello", "loc first read")
            lt:SetEntryValue("k.hello", "Hello", "", "en-us", "Updated")
            assert(tr:FormatByKey("k.hello") == "Updated", "loc cache invalidated")

            -- HeightmapImporterService validation + import (including rbxtemp:// generated image)
            local hmi = game:GetService("HeightmapImporterService")
            local okH, errH = hmi:IsValidHeightmap("")
            assert(okH == false and type(errH) == "string", "empty heightmap invalid")
            local okV, errV, wV, hV = hmi:IsValidHeightmap("rbxtemp://987654321")
            assert(okV == true and errV == "" and wV == 4 and hV == 4, "rbxtemp heightmap valid")
            local prevId, prevDiscard = hmi:GetHeightmapPreviewAsync("rbxtemp://987654321")
            assert(prevId == "rbxtemp://987654321" and prevDiscard == false, "heightmap preview")
            workspace.Terrain:Clear()
            hmi:ImportHeightmap(Region3.new(Vector3.new(-8, 0, -8), Vector3.new(8, 16, 8)), "rbxtemp://987654321", "", Enum.Material.Grass)
            assert(workspace.Terrain:CountCells() > 0, "heightmap import writes voxels")

            -- SetMaterialInTransformSubregion / ReplaceMaterialInTransformSubregion offset subregions
            workspace.Terrain:Clear()
            local cf = CFrame.new(0, 0, 0)
            local sz = Vector3.new(32, 16, 32)
            for ox = 0, 4, 4 do
                for oy = 0, 2, 2 do
                    for oz = 0, 4, 4 do
                        workspace.Terrain:SetMaterialInTransformSubregion(
                            cf, sz, Enum.Material.Grass,
                            Region3int16.new(Vector3int16.new(ox, oy, oz), Vector3int16.new(ox + 4, oy + 2, oz + 4))
                        )
                    end
                end
            end
            assert(workspace.Terrain:CountCells() == 256, "subregion fill count=" .. workspace.Terrain:CountCells())
            workspace.Terrain:ReplaceMaterialInTransformSubregion(
                cf, sz, Enum.Material.Grass, Enum.Material.Water,
                Region3int16.new(Vector3int16.new(0, 0, 0), Vector3int16.new(8, 4, 8))
            )
            local mats, _ = workspace.Terrain:ReadVoxels(Region3.new(Vector3.new(-16, -8, -16), Vector3.new(16, 8, 16)), 4)
            assert(mats[1][1][1] == Enum.Material.Water and mats[8][4][8] == Enum.Material.Water, "subregion replace all octants")

            -- FastInt defaults for Toolbox & TerrainEditor (Studio 0.748 values)
            assert(game:GetFastInt("ToolboxHomeViewInitialPageSize") == 30, "ToolboxHomeViewInitialPageSize")
            assert(game:GetFastInt("TerrainEditorMaxImportVolumeMegaVoxels") == 256, "TerrainEditorMaxImportVolumeMegaVoxels")
            assert(game:GetFastInt("SmoothTerrainMaxCppRegion") == 67108864, "SmoothTerrainMaxCppRegion")
            assert(game:GetFastInt("SmoothTerrainMaxLuaRegion") == 4194304, "SmoothTerrainMaxLuaRegion")

            -- GameSettings FastFlags (Studio 0.748): must override DefineFastFlag(..., false)
            game:DefineFastFlag("GameSettingsExperienceGuidelines", false)
            game:DefineFastFlag("PruneGameSettings", false)
            assert(game:GetFastFlag("GameSettingsExperienceGuidelines") == true, "GameSettingsExperienceGuidelines")
            assert(game:GetFastFlag("PruneGameSettings") == true, "PruneGameSettings")

            -- TerrainRegion:ApplyTransform & ApplyTransformSubregion + SetMaterialInTransform Air clear
            workspace.Terrain:Clear()
            workspace.Terrain:FillBlock(CFrame.new(0, 8, 0), Vector3.new(32, 16, 32), Enum.Material.Grass)
            assert(workspace.Terrain:CountCells() == 256, "fill 256")
            local copied = workspace.Terrain:CopyRegion(Region3int16.new(Vector3int16.new(-4, 0, -4), Vector3int16.new(3, 3, 3)))
            workspace.Terrain:SetMaterialInTransform(CFrame.new(0, 8, 0), Vector3.new(32, 16, 32), Enum.Material.Air)
            assert(workspace.Terrain:CountCells() == 0, "clear source to 0, got " .. workspace.Terrain:CountCells())
            workspace.Terrain:PasteRegion(copied, Vector3int16.new(4, 0, -4), false)
            assert(workspace.Terrain:CountCells() == 256, "moved 256, got " .. workspace.Terrain:CountCells())

            -- ApplyTransform 90 deg Y rotation on asymmetric 8x4x4 (Grass -X, Rock +X)
            workspace.Terrain:Clear()
            workspace.Terrain:FillBlock(CFrame.new(-8, 8, 0), Vector3.new(16, 16, 16), Enum.Material.Grass)
            workspace.Terrain:FillBlock(CFrame.new(8, 8, 0), Vector3.new(16, 16, 16), Enum.Material.Rock)
            local asym = workspace.Terrain:CopyRegion(Region3int16.new(Vector3int16.new(-4, 0, -2), Vector3int16.new(3, 3, 1)))
            local sub = asym:ApplyTransformSubregion(
                CFrame.Angles(0, math.pi / 2, 0),
                Vector3.new(32, 16, 16),
                Region3int16.new(Vector3int16.new(0, 0, 0), Vector3int16.new(4, 4, 4))
            )
            assert(sub.SizeInCells == Vector3.new(4, 4, 4), "sub size=" .. tostring(sub.SizeInCells))
            asym:ApplyTransform(CFrame.Angles(0, math.pi / 2, 0), Vector3.new(32, 16, 16))
            assert(asym.SizeInCells == Vector3.new(4, 4, 8), "rot90 size=" .. tostring(asym.SizeInCells))
            workspace.Terrain:Clear()
            workspace.Terrain:PasteRegion(asym, Vector3int16.new(0, 0, 0), false)
            assert(workspace.Terrain:CountCells() == 128, "rot90 cells=" .. workspace.Terrain:CountCells())

            local tc = game:GetService("TeamCreateService")
            tc:CloseGameIfUserDoesntHavePerms()

            print("r2_ok")
            "#
            .to_string(),
        ),
    );
    let tops = dom.root().children().to_vec();
    let mut bin = Vec::new();
    rbx_binary::to_writer(&mut bin, &dom, &tops).unwrap();
    std::fs::write(dir.join("Round2Check.rbxm"), &bin).unwrap();

    // Register a tiny 4x4 white PNG under a plain `rbxtemp://<id>` (matching File:GetTemporaryId).
    let mut png_bytes = Vec::new();
    {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .unwrap();
    }
    rustudio_core::assets::register_generated("rbxtemp://987654321", png_bytes);

    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load_builtin_dir(&dir, None).into_iter().for_each(|r| { r.unwrap(); });
    // User plugin (identity 5) must NOT see StudioPublishService, ProcessAssetInsertionDrag, or HttpService:RequestInternal.
    h.load(source(
        "UserGate",
        r#"
        local ok_pub, pub = pcall(function() return game:GetService("StudioPublishService") end)
        local ok_cb = pcall(function() plugin.ProcessAssetInsertionDrag = function() end end)
        local ok_http = pcall(function() game:GetService("HttpService"):RequestInternal({ Url = "https://example.com" }) end)
        print("user_gate", ok_pub, pub == nil, ok_cb, ok_http)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 4);
    let out = lines(&mut h);
    assert!(has(&out, "r2_ok"), "{out:#?}");
    assert!(has(&out, "user_gate true true false false"), "{out:#?}");
    rustudio_core::assets::unregister_generated("rbxtemp://987654321");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_builtin_plugins_load_on_first_use() {
    temp_data();
    let dir = std::env::temp_dir().join(format!("rustudio-lazy-builtin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let write_rbxm = |file: &str, code: &str| {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        dom.insert(
            root,
            InstanceBuilder::new("Script")
                .with_name("Main")
                .with_property("Source", code.to_string()),
        );
        let tops = dom.root().children().to_vec();
        let mut bin = Vec::new();
        rbx_binary::to_writer(&mut bin, &dom, &tops).unwrap();
        std::fs::write(dir.join(file), &bin).unwrap();
    };

    // 1) TagEditor-style: CreateDockWidgetPluginGuiAsync (InitialEnabled=false) then Click opens it.
    write_rbxm(
        "TagEditor.rbxm",
        r#"
        print("tag_loaded")
        local tb = plugin:CreateToolbar("tagEditorToolbar")
        local btn = tb:CreateButton("TagEditor", "", "")
        local w = plugin:CreateDockWidgetPluginGuiAsync("TagEditor", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Right, false, false, 300, 400))
        btn.Click:Connect(function()
            w.Enabled = not w.Enabled
            print("tag_clicked", w.Enabled)
        end)
        "#,
    );

    // 2) Toolbox-style: InitialEnabled=true opens immediately on load; lazy click must NOT toggle it off.
    write_rbxm(
        "Toolbox.rbxm",
        r#"
        print("tb_loaded")
        local tb = plugin:CreateToolbar("luaToolboxToolbar")
        local btn = tb:CreateButton("luaToolboxButton", "Insert items from the toolbox", "")
        local w = plugin:CreateDockWidgetPluginGuiAsync("Toolbox", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Left, true, false, 300, 400))
        btn.Click:Connect(function()
            w.Enabled = not w.Enabled
            print("tb_clicked", w.Enabled)
        end)
        "#,
    );

    // 3) PathEditor-style: wakes on PluginAction or selecting a Path2D.
    write_rbxm(
        "PathEditor.rbxm",
        r#"
        print("path_loaded")
        local act = plugin:CreatePluginAction("PathEditor_MoveTool", "Path2D Editor Select Mode", "Activate Select Mode")
        act.Triggered:Connect(function()
            print("path_triggered")
        end)
        "#,
    );

    // 4) LightGuides-style: wakes automatically when a Light is selected.
    write_rbxm(
        "LightGuides.rbxm",
        r#"
        print("light_loaded")
        "#,
    );

    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load_builtin_dir(&dir, None).into_iter().for_each(|r| { r.unwrap(); });

    // Nothing runs yet; ribbon model already exposes deferred buttons & actions.
    frames(&mut h, &mut doc, 3);
    let out = lines(&mut h);
    assert!(out.is_empty(), "deferred plugins must not run at startup: {out:#?}");
    assert!(h.loaded().iter().all(|p| !p.running));
    let model = h.ui_model_for_edit(&doc);
    let tag_btn = model
        .groups
        .iter()
        .find(|g| g.name == "tagEditorToolbar")
        .and_then(|g| g.buttons.first())
        .expect("deferred TagEditor button")
        .r;
    let tb_btn = model
        .groups
        .iter()
        .find(|g| g.name == "luaToolboxToolbar")
        .and_then(|g| g.buttons.first())
        .expect("deferred Toolbox button")
        .r;
    let path_act = model
        .actions
        .iter()
        .find(|a| a.key == "builtin_PathEditor.rbxm_PathEditor_MoveTool")
        .expect("deferred PathEditor action")
        .r;

    // Clicking TagEditor materializes it and delivers Click after its async widget yields.
    h.trigger(Trigger::Fire(tag_btn, "Click"));
    frames(&mut h, &mut doc, 5);
    let out = lines(&mut h);
    assert!(has(&out, "tag_loaded"), "{out:#?}");
    assert!(has(&out, "tag_clicked true"), "{out:#?}");

    // Clicking Toolbox materializes it and leaves InitialEnabled=true open without toggling off.
    h.trigger(Trigger::Fire(tb_btn, "Click"));
    frames(&mut h, &mut doc, 5);
    let out = lines(&mut h);
    assert!(has(&out, "tb_loaded"), "{out:#?}");
    assert!(!out.iter().any(|l| l.starts_with("tb_clicked")), "must not toggle off InitialEnabled widget: {out:#?}");

    // Triggering PathEditor's deferred action materializes it and fires Triggered.
    h.trigger(Trigger::Fire(path_act, "Triggered"));
    frames(&mut h, &mut doc, 4);
    let out = lines(&mut h);
    assert!(has(&out, "path_loaded"), "{out:#?}");
    assert!(has(&out, "path_triggered"), "{out:#?}");

    // Selecting a PointLight automatically wakes LightGuides.
    let ws = doc.workspace().unwrap();
    let light = doc.dom.insert(ws, InstanceBuilder::new("PointLight").with_name("L"));
    doc.selection = vec![light];
    frames(&mut h, &mut doc, 3);
    let out = lines(&mut h);
    assert!(has(&out, "light_loaded"), "{out:#?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn builtin_tool_selector_registers_one_of_each_and_never_materializes_unchosen() {
    use rustudio_core::rustudio_settings::{BuiltinTool, BuiltinToolChoices, ToolImpl};

    temp_data();
    let dir = std::env::temp_dir().join(format!("rustudio-tool-selector-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let write_rbxm = |file: &str, tag: &str| {
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        dom.insert(
            root,
            InstanceBuilder::new("Script")
                .with_name("Main")
                .with_property("Source", format!("print(\"loaded:{tag}\")")),
        );
        let tops = dom.root().children().to_vec();
        let mut bin = Vec::new();
        rbx_binary::to_writer(&mut bin, &dom, &tops).unwrap();
        std::fs::write(dir.join(file), &bin).unwrap();
    };

    for (file, tag) in [
        ("TagEditor.rbxm", "tag"),
        ("Toolbox.rbxm", "toolbox"),
        ("TerrainEditor.rbxm", "terrain"),
        ("TerrainPaletteEditor.rbxm", "terrain_palette"),
        ("SelectDragger.rbxm", "select"),
        ("MoveDragger.rbxm", "move"),
        ("RotateDragger.rbxm", "rotate"),
        ("ScaleDragger.rbxm", "scale"),
        ("ViewSelector.rbxm", "view_selector"),
        ("RigGenerator.rbxm", "rig"),
        ("MaterialPicker.rbxm", "material_picker"),
    ] {
        write_rbxm(file, tag);
    }

    let names = |h: &PluginHost| -> Vec<String> {
        let mut v: Vec<String> = h.loaded().into_iter().map(|p| p.name).collect();
        v.sort();
        v
    };

    // 1) Built-in plugins OFF: nothing loads, native dock keeps Toolbox, nothing promoted.
    {
        let mut doc = Document::new_place();
        let mut h = PluginHost::default();
        let choices = BuiltinToolChoices {
            plugins_enabled: false,
            toolbox: ToolImpl::Roblox,
            terrain_editor: ToolImpl::Roblox,
            draggers: ToolImpl::Roblox,
            view_selector: ToolImpl::Roblox,
            rig_builder: ToolImpl::Roblox,
            material_picker: ToolImpl::Roblox,
        };
        h.load_builtin_dir_with_choices(&dir, None, choices);
        frames(&mut h, &mut doc, 3);
        assert!(h.loaded().is_empty());
        assert!(lines(&mut h).is_empty());
        assert!(crate::app::default_dock_for(choices).find_tab(&crate::app::Tab::Toolbox).is_some());
        assert_eq!(ui_ribbon::promoted_builtin_tool("builtin_Toolbox.rbxm", choices), None);
        assert_eq!(ui_ribbon::promoted_builtin_tool("builtin_MaterialPicker.rbxm", choices), None);
    }

    // 2) Built-in plugins ON, default (all rustudio): only non-duplicated TagEditor registers.
    //    Unchosen duplicated plugins are absent and never materialize even if woken or triggered.
    {
        let mut doc = Document::new_place();
        let mut h = PluginHost::default();
        let choices = BuiltinToolChoices { plugins_enabled: true, ..Default::default() };
        h.load_builtin_dir_with_choices(&dir, None, choices);
        assert_eq!(names(&h), vec!["builtin_TagEditor.rbxm"]);
        let model = h.ui_model_for_edit(&doc);
        assert_eq!(model.groups.len(), 1);
        assert_eq!(model.groups[0].plugin_name, "builtin_TagEditor.rbxm");
        // Attempt to wake / trigger unchosen plugins: must do nothing and never load them.
        for stem in [
            "Toolbox",
            "TerrainEditor",
            "TerrainPaletteEditor",
            "SelectDragger",
            "MoveDragger",
            "RotateDragger",
            "ScaleDragger",
            "ViewSelector",
            "RigGenerator",
            "MaterialPicker",
        ] {
            assert!(h.wake_plugin_by_name(stem).is_none(), "unchosen {stem} must not exist");
        }
        h.trigger(Trigger::Button("luaToolboxToolbar".into(), "luaToolboxButton".into()));
        h.trigger(Trigger::Button("TerrainEditorLuaToolbarName".into(), "Editor".into()));
        h.trigger(Trigger::Button("Home".into(), "Select".into()));
        h.trigger(Trigger::Button("ViewSelector".into(), "ViewSelector".into()));
        h.trigger(Trigger::Button("Rig Generator".into(), "Generate Rig".into()));
        h.trigger(Trigger::Button("Edit".into(), "MaterialPicker".into()));
        frames(&mut h, &mut doc, 4);
        let out = lines(&mut h);
        assert!(!out.iter().any(|l| l.starts_with("loaded:")), "unchosen plugins must never materialize: {out:#?}");
        assert!(h.loaded().iter().all(|p| !p.running));
        assert!(crate::app::default_dock_for(choices).find_tab(&crate::app::Tab::Toolbox).is_some());
    }

    // 3) Each duplicated tool set to Roblox's registers only its plugin(s), promotes them out
    //    of the Plugins tab, and keeps every other duplicated plugin absent.
    let cases: &[(BuiltinTool, &[&str], &[&str])] = &[
        (BuiltinTool::Toolbox, &["builtin_TagEditor.rbxm", "builtin_Toolbox.rbxm"], &["builtin_Toolbox.rbxm"]),
        (
            BuiltinTool::TerrainEditor,
            &["builtin_TagEditor.rbxm", "builtin_TerrainEditor.rbxm", "builtin_TerrainPaletteEditor.rbxm"],
            &["builtin_TerrainEditor.rbxm"],
        ),
        (
            BuiltinTool::Draggers,
            &[
                "builtin_MoveDragger.rbxm",
                "builtin_RotateDragger.rbxm",
                "builtin_ScaleDragger.rbxm",
                "builtin_SelectDragger.rbxm",
                "builtin_TagEditor.rbxm",
            ],
            &[
                "builtin_MoveDragger.rbxm",
                "builtin_RotateDragger.rbxm",
                "builtin_ScaleDragger.rbxm",
                "builtin_SelectDragger.rbxm",
            ],
        ),
        (BuiltinTool::ViewSelector, &["builtin_TagEditor.rbxm", "builtin_ViewSelector.rbxm"], &["builtin_ViewSelector.rbxm"]),
        (BuiltinTool::RigBuilder, &["builtin_RigGenerator.rbxm", "builtin_TagEditor.rbxm"], &["builtin_RigGenerator.rbxm"]),
        (
            BuiltinTool::MaterialPicker,
            &["builtin_MaterialPicker.rbxm", "builtin_TagEditor.rbxm"],
            &["builtin_MaterialPicker.rbxm"],
        ),
    ];

    for &(tool, expected_loaded, expected_promoted) in cases {
        let mut choices = BuiltinToolChoices { plugins_enabled: true, ..Default::default() };
        match tool {
            BuiltinTool::Toolbox => choices.toolbox = ToolImpl::Roblox,
            BuiltinTool::TerrainEditor => choices.terrain_editor = ToolImpl::Roblox,
            BuiltinTool::Draggers => choices.draggers = ToolImpl::Roblox,
            BuiltinTool::ViewSelector => choices.view_selector = ToolImpl::Roblox,
            BuiltinTool::RigBuilder => choices.rig_builder = ToolImpl::Roblox,
            BuiltinTool::MaterialPicker => choices.material_picker = ToolImpl::Roblox,
        }
        let mut doc = Document::new_place();
        let mut h = PluginHost::default();
        h.load_builtin_dir_with_choices(&dir, None, choices);
        assert_eq!(names(&h), expected_loaded, "loaded plugins for {tool:?}");

        let model = h.ui_model_for_edit(&doc);
        let plugins_tab: Vec<&str> = model
            .groups
            .iter()
            .filter(|g| !ui_ribbon::is_builtin_plugin_group(&g.plugin_name))
            .map(|g| g.plugin_name.as_str())
            .collect();
        let mut promoted: Vec<&str> = model
            .groups
            .iter()
            .filter(|g| ui_ribbon::promoted_builtin_tool(&g.plugin_name, choices).is_some())
            .map(|g| g.plugin_name.as_str())
            .collect();
        promoted.sort();
        assert!(plugins_tab.is_empty(), "Plugins tab must hide all built-ins for {tool:?}: {plugins_tab:?}");
        assert_eq!(promoted, expected_promoted, "promoted groups for {tool:?}");

        // Native Toolbox tab is omitted from default dock only when Toolbox is Roblox's.
        let has_native_tb = crate::app::default_dock_for(choices).find_tab(&crate::app::Tab::Toolbox).is_some();
        assert_eq!(has_native_tb, tool != BuiltinTool::Toolbox);

        // Unchosen plugins still stay absent and unmaterialized after ticking.
        frames(&mut h, &mut doc, 2);
        assert!(lines(&mut h).is_empty(), "deferred plugins stay unmaterialized until used");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn builtin_plugins_stay_off_plugins_tab_and_have_studio_icons() {
    use super::builtin_lazy::{all_manifests, builtin_default_icon, DeferredStub};

    // Every built-in plugin manifest button must resolve to a non-empty Studio
    // icon URI so no blank diamond placeholders remain.
    let mut icon_uris = Vec::new();
    for m in all_manifests() {
        let plugin_name = format!("builtin_{}.rbxm", m.stem);
        assert!(ui_ribbon::is_builtin_plugin_group(&plugin_name));
        let stub = DeferredStub::new(PluginId(1), &plugin_name, m);
        for g in &stub.groups {
            for b in &g.buttons {
                assert!(
                    !b.icon.trim().is_empty(),
                    "built-in button {plugin_name} {}/{} has empty icon",
                    g.name,
                    b.id
                );
                assert!(
                    builtin_default_icon(&plugin_name, &g.name, &b.id).is_some(),
                    "built-in button {plugin_name} {}/{} missing from builtin_default_icon",
                    g.name,
                    b.id
                );
                icon_uris.push((plugin_name.clone(), g.name.clone(), b.id.clone(), b.icon.clone()));
            }
        }
    }
    assert!(icon_uris.len() >= 45, "expected all 46 built-in toolbar buttons, got {}", icon_uris.len());

    // User / cloud plugins stay on the Plugins tab; built-ins never do.
    assert!(!ui_ribbon::is_builtin_plugin_group("user_MyPlugin.lua"));
    assert!(!ui_ribbon::is_builtin_plugin_group("cloud_123456"));

    // On a machine with Studio SVG textures installed, every single built-in
    // icon URI must resolve to an existing PNG file on disk.
    let has_svg = rustudio_core::assets::resolve(
        "rbxasset://studio_svg_textures/Shared/Ribbon/Dark/Medium/RibbonToolbox.png",
        None,
    )
    .is_some_and(|p| p.is_file());
    if has_svg {
        let mut missing = Vec::new();
        for (p, tb, id, uri) in &icon_uris {
            let ok = rustudio_core::assets::resolve(uri, None).is_some_and(|path| path.is_file());
            if !ok {
                missing.push(format!("{p} {tb}/{id} -> {uri}"));
            }
        }
        assert!(missing.is_empty(), "unresolved built-in icons: {missing:#?}");
    }
}

#[test]
fn edit_sound_preview_syncs_dom_properties_and_edit_vm() {
    use rbx_dom_weak::types::Variant;
    use rbx_dom_weak::{ustr, InstanceBuilder};
    use std::time::Duration;

    let dir = rustudio_core::assets::content_dir().join("sounds");
    let _ = std::fs::create_dir_all(&dir);
    // 0.30 s 16-bit mono WAV at 16 kHz.
    let n = 4800u32;
    let data_len = n * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&32000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let v: i16 = if (i / 16) % 2 == 0 { 3000 } else { -3000 };
        wav.extend_from_slice(&v.to_le_bytes());
    }
    let _ = std::fs::write(dir.join("app-edit-preview.wav"), &wav);
    let id = "rbxasset://sounds/app-edit-preview.wav".to_string();

    let mut game = rustudio_player::game::Game::new(None);
    let mut host = PluginHost::default();
    let _ = host.with_edit_vm(&mut game.doc, |_| ());

    let tick = |game: &mut rustudio_player::game::Game, host: &mut PluginHost| {
        let (cmds, seeks, listener) = host.take_edit_audio_requests();
        let ev = game.edit_audio_tick(cmds, seeks, listener);
        host.deliver_edit_audio_events(&mut game.doc, &ev);
        host.sync_edit_audio_to_vm(&game.edit_audio.lengths);
    };

    tick(&mut game, &mut host);
    let ws = game.doc.workspace().unwrap();
    let s = game.doc.insert(
        ws,
        InstanceBuilder::new("Sound")
            .with_name("Preview")
            .with_property("SoundId", Variant::ContentId(id.into()))
            .with_property("Looped", Variant::Bool(true)),
    );
    tick(&mut game, &mut host);
    assert!(game.edit_audio.is_loaded(s));
    assert!((game.edit_audio.time_length(s) - 0.30).abs() < 0.02);

    // Studio 0.748 parity: setting Playing=true in Edit updates flags without
    // starting audible audio or advancing TimePosition.
    game.doc
        .set_props("Set Playing", vec![(s, ustr("Playing"), Variant::Bool(true))], None);
    tick(&mut game, &mut host);
    std::thread::sleep(Duration::from_millis(60));
    tick(&mut game, &mut host);
    assert_eq!(game.edit_audio.playing_count(), 0);

    let (loaded, len, playing, paused, lua_tp): (bool, f64, bool, bool, f64) = host
        .with_edit_vm(&mut game.doc, |rt| {
            rt.lua
                .load("local s = workspace.Preview return s.IsLoaded, s.TimeLength, s.IsPlaying, s.IsPaused, s.TimePosition")
                .eval()
                .unwrap()
        })
        .unwrap();
    assert!(loaded && playing && !paused && (len - 0.30).abs() < 0.02 && lua_tp == 0.0);

    // Properties panel Preview plays audibly and advances preview_state while
    // remaining completely invisible to Sound properties and Edit Luau.
    game.doc
        .set_props("Clear Playing", vec![(s, ustr("Playing"), Variant::Bool(false))], None);
    game.doc.selection = vec![s];
    game.edit_audio_preview_toggle(s);
    std::thread::sleep(Duration::from_millis(80));
    tick(&mut game, &mut host);
    let (pv_playing, pv_pos, pv_len) = game.edit_audio.preview_state(s);
    assert!(pv_playing && pv_pos > 0.02 && (pv_len - 0.30).abs() < 0.02);

    let (lua_pl, lua_ip, lua_pa, lua_tp2): (bool, bool, bool, f64) = host
        .with_edit_vm(&mut game.doc, |rt| {
            rt.lua
                .load("local s = workspace.Preview return s.Playing, s.IsPlaying, s.IsPaused, s.TimePosition")
                .eval()
                .unwrap()
        })
        .unwrap();
    assert!(!lua_pl && !lua_ip && lua_pa && lua_tp2 == 0.0);

    // Stopping Edit preview (e.g. entering Play) stops audio and resets preview thumb.
    game.edit_audio.stop_playback();
    host.stop_edit_audio_vm_state();
    assert_eq!(game.edit_audio.playing_count(), 0);
    assert_eq!(game.edit_audio.preview_state(s).0, false);
}

#[test]
fn sound_and_audio_player_props_layout_matches_studio() {
    let sound_asset = crate::props_layout::rows("Sound", false)
        .into_iter()
        .find(|c| c.name == "Asset")
        .map(|c| {
            c.rows
                .into_iter()
                .filter_map(|r| match r {
                    crate::props_layout::Row::Prop(n, _) => Some(n),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
    if let Some(rows) = sound_asset {
        assert!(rows.contains(&"AudioContent".to_string()), "got {rows:?}");
        assert!(!rows.contains(&"SoundId".to_string()), "SoundId should be hidden: {rows:?}");
        assert!(rows.contains(&"IsLoaded".to_string()), "got {rows:?}");
        assert!(rows.contains(&"TimeLength".to_string()), "got {rows:?}");
    }
    let ap_asset = crate::props_layout::rows("AudioPlayer", false)
        .into_iter()
        .find(|c| c.name == "Asset")
        .map(|c| {
            c.rows
                .into_iter()
                .filter_map(|r| match r {
                    crate::props_layout::Row::Prop(n, _) => Some(n),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
    if let Some(rows) = ap_asset {
        assert!(rows.contains(&"AudioContent".to_string()), "got {rows:?}");
        assert!(!rows.contains(&"Asset".to_string()), "Asset should be hidden: {rows:?}");
        assert!(rows.contains(&"IsReady".to_string()), "got {rows:?}");
        assert!(rows.contains(&"TimeLength".to_string()), "got {rows:?}");
    }
}
