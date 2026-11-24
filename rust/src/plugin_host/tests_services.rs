//! Tests for the services plugins call (ChangeHistoryService on the
//! editor's undo stack, Selection, ScriptEditorService, prompts, held
//! Source writes). Studio's measured behaviour is in the comments of the
//! modules under test; these pin it.

use std::sync::Arc;

use rbx_dom_weak::{InstanceBuilder, WeakDom};

use super::*;

fn temp_data() {
    super::tests::temp_data();
}

fn source(name: &str, src: &str) -> PluginSource {
    let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
    let root = dom.root_ref();
    dom.insert(root, InstanceBuilder::new("Script").with_name("Main").with_property("Source", src.to_string()));
    PluginSource::Tree { name: format!("user_{name}.rbxmx"), asset_id: None, dom: Arc::new(dom) }
}

fn lines(h: &mut PluginHost) -> Vec<String> {
    h.take_log().into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()).collect()
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
fn history_model_matches_studio() {
    use rustudio_core::doc::{Change, FinishOp};
    let mut d = Document::new_place();
    let ws = d.workspace().unwrap();
    // A script's insert + waypoint, then a rename with no waypoint: Undo
    // folds the rename into the step, and the part goes with its new name.
    let p = d.dom.insert(rbx_dom_weak::types::Ref::none(), InstanceBuilder::new("Part").with_name("P"));
    d.dom.transfer_within(p, ws);
    d.note_script_change(Change::Enter { r: p, old: rbx_dom_weak::types::Ref::none(), new: ws });
    d.chs_set_waypoint("Insert P");
    assert_eq!(d.chs_can_undo().as_deref(), Some("Insert P"));
    d.dom.get_by_ref_mut(p).unwrap().name = "P2".into();
    d.note_script_change(Change::Name { r: p, old: "P".into(), new: "P2".into() });
    assert!(d.undo().is_some());
    assert!(d.dom.get_by_ref(p).unwrap().parent().is_none());
    assert_eq!(d.dom.get_by_ref(p).unwrap().name, "P2");
    assert_eq!(d.chs_can_redo().as_deref(), Some("Insert P"));
    // SetWaypoint clears redo even with nothing new.
    d.chs_set_waypoint("noop");
    assert_eq!(d.chs_can_redo(), None);
    // The person's edit takes the pending script change into its step.
    let q = d.insert(ws, InstanceBuilder::new("Part").with_name("Q"));
    let r = d.insert(ws, InstanceBuilder::new("Part").with_name("R"));
    d.chs_set_waypoint("base");
    d.dom.get_by_ref_mut(q).unwrap().name = "Q2".into();
    d.note_script_change(Change::Name { r: q, old: "Q".into(), new: "Q2".into() });
    d.remove(&[r]);
    assert_eq!(d.chs_can_undo().as_deref(), Some("DeleteSelectedAction"));
    d.undo();
    assert_eq!(d.dom.get_by_ref(q).unwrap().name, "Q", "the pending rename went with the Delete step");
    // Recordings: one at a time; Cancel reverts; ids look like Studio's.
    let id = d.chs_begin_recording("Rec", Some("Rec Display")).unwrap();
    assert!(id.starts_with("recording_") && id[10..].chars().all(|c| c.is_ascii_digit()));
    assert!(d.chs_begin_recording("Other", None).is_none());
    d.dom.get_by_ref_mut(q).unwrap().name = "Q3".into();
    d.note_script_change(Change::Name { r: q, old: "Q".into(), new: "Q3".into() });
    assert_eq!(d.chs_finish_recording("nope", FinishOp::Commit).unwrap_err(), "Invalid recording identifier.");
    d.chs_finish_recording(&id, FinishOp::Cancel).unwrap();
    assert_eq!(d.dom.get_by_ref(q).unwrap().name, "Q");
    assert_eq!(d.chs_finish_recording(&id, FinishOp::Commit).unwrap_err(), "No recording in progress.");
    // SetEnabled(false) clears the history.
    d.chs_set_enabled(false);
    assert_eq!(d.chs_can_undo(), None);
    d.chs_set_enabled(true);
}

#[test]
fn change_history_service_in_a_plugin() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Chs",
        r#"
        local CHS = game:GetService("ChangeHistoryService")
        CHS.OnUndo:Connect(function(n) print("OnUndo", n) end)
        CHS.OnRedo:Connect(function(n) print("OnRedo", n) end)
        CHS:ResetWaypoints()
        local p = Instance.new("Part") p.Name = "ChsPart" p.Parent = workspace
        CHS:SetWaypoint("Insert ChsPart")
        print("can", CHS:GetCanUndo())
        CHS:Undo()
        print("after undo", p.Parent)
        CHS:Redo()
        print("after redo", p.Parent)
        local s = Instance.new("Script") s.Source = "-- a" s.Parent = workspace
        CHS:SetWaypoint("script")
        s.Source = "-- b"
        CHS:SetWaypoint("source")
        print("source recorded", select(2, CHS:GetCanUndo()))
        print("bad op", pcall(function() CHS:FinishRecording("x", 7) end))
        print("no args", pcall(function() CHS:FinishRecording() end))
        task.wait()
        print("waited")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 6);
    let out = lines(&mut h);
    for want in [
        "can true Insert ChsPart",
        "after undo nil",
        "after redo Workspace",
        "source recorded script",
        "bad op false Unable to cast double to FinishRecordingOperation",
        "no args false Argument 2 missing or nil",
        "OnUndo Insert ChsPart",
        "OnRedo Insert ChsPart",
        "waited",
    ] {
        assert!(has(&out, want), "missing {want:?} in {out:#?}");
    }
    // The events fire after the caller yields (after "after redo").
    let i = out.iter().position(|l| l == "OnUndo Insert ChsPart").unwrap();
    let j = out.iter().position(|l| l == "after redo Workspace").unwrap();
    assert!(i > j, "{out:#?}");
    // The editor's own Ctrl+Z is the same stack and fires OnUndo too.
    doc.undo();
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    assert!(has(&out, "OnUndo script"), "{out:#?}");
}

/// Brushtool's brush: a clone goes into a temporary Model, then into the
/// Workspace, and the Model is destroyed; Undo of that step takes the
/// clone out of the DataModel although its old parent is gone.
#[test]
fn undo_takes_out_a_clone_whose_orphan_parent_is_gone() {
    use rustudio_core::doc::Change;
    let mut d = Document::new_place();
    let ws = d.workspace().unwrap();
    let none = rbx_dom_weak::types::Ref::none();
    d.chs_set_waypoint("Brushed");
    let temp = d.dom.insert(none, InstanceBuilder::new("Model"));
    let clone = d.dom.insert(temp, InstanceBuilder::new("Part").with_name("Tree"));
    d.dom.transfer_within(clone, ws);
    d.note_script_change(Change::Enter { r: clone, old: temp, new: ws });
    d.dom.destroy(temp);
    d.chs_set_waypoint("Brushed");
    assert!(d.undo().is_some());
    assert!(d.dom.get_by_ref(clone).unwrap().parent().is_none(), "the clone left the Workspace");
    assert!(d.redo().is_some());
    assert_eq!(d.dom.get_by_ref(clone).unwrap().parent(), ws);
}

#[test]
fn selection_fires_as_studio() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Sel",
        r#"
        local Sel = game:GetService("Selection")
        local n = 0
        Sel.SelectionChanged:Connect(function() n += 1 end)
        local a = Instance.new("Part") a.Parent = workspace
        local b = Instance.new("Part") b.Parent = workspace
        local function count(f) task.wait() n = 0 f() task.wait() return n end
        print("set", count(function() Sel:Set({a}) end))
        print("same", count(function() Sel:Set({a}) end))
        print("adddup", count(function() Sel:Add({a}) end))
        print("remove other", count(function() Sel:Remove({b}) end))
        print("destroy", count(function() a:Destroy() end), #Sel:Get())
        print("empty", count(function() Sel:Set({}) end))
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 20);
    let out = lines(&mut h);
    for want in ["set 1", "same 1", "adddup 0", "remove other 1", "destroy 1 0", "empty 0"] {
        assert!(has(&out, want), "missing {want:?} in {out:#?}");
    }
}

#[test]
fn script_documents_follow_the_editor() {
    temp_data();
    script_docs::reset();
    let mut doc = Document::new_place();
    let ws = doc.workspace().unwrap();
    let s = doc.insert(
        ws,
        InstanceBuilder::new("Script").with_name("DocScript").with_property("Source", "a\n-- b\n".to_string()),
    );
    let mut h = PluginHost::default();
    h.load(source(
        "Ses",
        r#"
        local SES = game:GetService("ScriptEditorService")
        SES.TextDocumentDidOpen:Connect(function(d) print("open", d.Name, d.Parent) end)
        SES.TextDocumentDidClose:Connect(function(d) print("close", d.Name, d.Parent) end)
        SES.TextDocumentDidChange:Connect(function(d, ch) print("change", ch[1].range.start.line, ch[1].range["end"].line, ch[1].text) end)
        print("ready")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    // The person opens the script's tab, types, and closes it.
    script_docs::sync_tabs(&[s]);
    frames(&mut h, &mut doc, 2);
    let old = "a\n-- b\n".to_string();
    let new = "ab\n-- b\n".to_string();
    doc.set_props("Edit script", vec![(s, rbx_dom_weak::ustr("Source"), rbx_dom_weak::types::Variant::String(new.clone()))], None);
    script_docs::changed(s, vec![script_docs::diff(&old, &new).unwrap()]);
    frames(&mut h, &mut doc, 2);
    script_docs::sync_tabs(&[]);
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    for want in ["open Workspace.DocScript ScriptEditorService", "change 1 1 b", "close Workspace.DocScript nil"] {
        assert!(has(&out, want), "missing {want:?} in {out:#?}");
    }
}

#[test]
fn prompts_answer_plugin_dialogs() {
    temp_data();
    let dir = std::env::temp_dir().join(format!("rustudio-prompt-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let txt = dir.join("hello.txt");
    std::fs::write(&txt, b"hi there").unwrap();
    let fbx = dir.join("Crate.fbx");
    std::fs::write(&fbx, rustudio_core::fbx::sample([200.0, 100.0, 50.0], [0.0, 0.0, 0.0])).unwrap();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "Prompts",
        r#"
        local SS = game:GetService("StudioService")
        local f = SS:PromptImportFile({"txt"})
        print("file", f.ClassName, f.Name, f.Size, f:GetBinaryContents())
        print("cancel", SS:PromptImportFile())
        print("asset", plugin:PromptForExistingAssetId("Animation"))
        print("badtype", pcall(function() return plugin:PromptForExistingAssetId("NotAType") end))
        print("fbx cancel", pcall(function() return plugin:ImportFbxRig() end))
        local m = plugin:ImportFbxRig()
        print("fbx", m.ClassName, m.Name, #m:GetDescendants() > 0)
        print("icon", SS:GetClassIcon("Part").ImageRectSize, (SS:GetClassIcon("BasePart").Image:match("Placeholder")))
        print("icon bad", pcall(function() return SS:GetClassIcon("Nope") end))
        "#,
    ))
    .unwrap();
    let answers = [txt.display().to_string(), "cancel".into(), "424242".into(), "cancel".into(), fbx.display().to_string()];
    let mut next = 0;
    for _ in 0..200 {
        frames(&mut h, &mut doc, 1);
        if prompts::any_open() && next < answers.len() {
            prompts::answer_oldest(&answers[next]);
            next += 1;
        }
    }
    let out = lines(&mut h);
    for want in [
        "file File hello.txt 8 hi there",
        "cancel nil",
        "asset 424242",
        "badtype false Requested asset type is not supported.",
        "fbx cancel false No file selected",
        "fbx Model Crate true",
        "icon 16, 16 Placeholder",
        "icon bad false GetClassIcon: Nope is not a Roblox class.",
    ] {
        assert!(has(&out, want), "missing {want:?} in {out:#?}");
    }
}

/// The probe's dump lines vs the fixture's expectations
/// (make_fixtures.py): same pose paths, weights, easing, times; numbers
/// within 2e-4 (Pose.CFrame is stored as float32).
/// The [FBXM] lines of one case in captures/fbxanim-studio.txt vs ours: the
/// same text, Pose CFrames within 2e-4 (relative above 1), "ser" chunks
/// (an rbxm, compared by checks/plugins/services/fbxanim_compare.py) left out.
pub(super) fn fbxanim_case_matches(studio: &str, got: &[String], label: &str) -> Result<(), String> {
    let pick = |ls: Vec<&str>| -> Vec<String> {
        ls.into_iter()
            .filter_map(|l| l.find("[FBXM] ").map(|i| &l[i + 7..]))
            .filter_map(|l| l.strip_prefix(label).and_then(|r| r.strip_prefix(' ')))
            .filter(|l| !l.starts_with("ser ") && !l.starts_with("serdone"))
            .map(|l| l.trim_end().to_string())
            .collect()
    };
    let want = pick(studio.lines().collect());
    let got = pick(got.iter().map(String::as_str).collect());
    if want.is_empty() || got.len() != want.len() {
        return Err(format!("{label}: {} lines, Studio {}\n{got:#?}", got.len(), want.len()));
    }
    for (g, w) in got.iter().zip(&want) {
        let ok = if let (Some((gh, gt)), Some((wh, wt))) = (g.split_once(" Pose "), w.split_once(" Pose ")) {
            let (gt, wt): (Vec<&str>, Vec<&str>) = (gt.split_whitespace().collect(), wt.split_whitespace().collect());
            gh == wh
                && gt.len() == wt.len()
                && gt.iter().zip(&wt).all(|(a, b)| match (a.parse::<f64>(), b.parse::<f64>()) {
                    (Ok(x), Ok(y)) => (x - y).abs() <= 2e-4 * y.abs().max(1.0),
                    _ => a == b,
                })
        } else {
            g == w
        };
        if !ok {
            return Err(format!("{label}:\n  rustudio {g}\n  studio   {w}"));
        }
    }
    Ok(())
}

/// Plugin:ImportFbxAnimation in the real plugin host, the probe Studio was
/// measured with, against Studio's lines (captures/fbxanim-studio.txt; the
/// cases whose files need no generator: the rest run in check.py fbxanim).
#[test]
fn import_fbx_animation_matches_studio() {
    temp_data();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("checks/plugins/services/fixtures/fbxanim");
    let dir = std::env::temp_dir().join(format!("rustudio-fbxanim-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, b"not a model\n").unwrap();
    let mesh = dir.join("mesh.fbx");
    std::fs::write(&mesh, rustudio_core::fbx::sample([100.0, 100.0, 100.0], [0.0, 0.0, 0.0])).unwrap();
    let wave = fixtures.join("wave.fbx").display().to_string();
    let cases = r#"[{"label":"nilrig","rig":"nil","r15":true},{"label":"cancel","rig":"Rig","r15":true},
        {"label":"text","rig":"Rig","r15":true},{"label":"mesh","rig":"Rig","r15":true},
        {"label":"part","rig":"Part","r15":true},{"label":"wave","rig":"Rig","r15":true},
        {"label":"wavef","rig":"Rig","r15":false},{"label":"wavec","rig":"RigC","r15":true},
        {"label":"crate","rig":"Crate","r15":true},{"label":"async","rig":"Rig","r15":true,"async":true},
        {"label":"undo","rig":"Rig","r15":true,"undo":true}]"#;
    let probe = include_str!("../../checks/plugins/services/probes/SvcM_fbxanim_measure.lua")
        .replacen("local AUTO = nil", &format!("local AUTO = [==[{cases}]==]"), 1);
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source("FbxAnim", &probe)).unwrap();
    let mut answers: Vec<String> = vec!["cancel".into(), txt.display().to_string(), mesh.display().to_string()];
    answers.extend(std::iter::repeat_n(wave, 7));
    let mut next = 0;
    let mut out = Vec::new();
    for _ in 0..2000 {
        frames(&mut h, &mut doc, 1);
        if prompts::any_open() && next < answers.len() {
            prompts::answer_oldest(&answers[next]);
            next += 1;
        }
        out.extend(lines(&mut h));
        if has(&out, "[FBXM] batch done auto") {
            break;
        }
    }
    let studio = include_str!("../../checks/plugins/services/captures/fbxanim-studio.txt");
    let mut bad = Vec::new();
    for label in ["nilrig", "cancel", "text", "mesh", "part", "wave", "wavef", "wavec", "crate", "async", "undo"] {
        if let Err(e) = fbxanim_case_matches(studio, &out, label) {
            bad.push(e);
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn held_source_write_waits_for_the_prompt() {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    permissions::set_injection_check(|plugin| {
        if plugin != "user_Inject.rbxmx" {
            return permissions::Decision::Allow;
        }
        match STATE.load(Ordering::SeqCst) {
            0 => permissions::Decision::Pending,
            _ => permissions::Decision::Allow,
        }
    });
    h.load(source(
        "Inject",
        r#"
        local s = Instance.new("Script") s.Source = "-- old" s.Parent = workspace
        s.Source = "-- new"
        print("right after", s.Source)
        task.wait(0.2)
        print("later", s.Source)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 4);
    STATE.store(1, Ordering::SeqCst);
    frames(&mut h, &mut doc, 20);
    let out = lines(&mut h);
    // (Both writes waited: the script still has no source right after.)
    assert!(has(&out, "right after "), "{out:#?}");
    assert!(has(&out, "later -- new"), "{out:#?}");
}

#[test]
fn theme_switch_fires_theme_changed() {
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    h.load(source(
        "ThemeWatch",
        r#"
        settings().Studio.ThemeChanged:Connect(function() print("theme changed") end)
        settings().Studio.Changed:Connect(function(p) print("changed", p) end)
        print("ready")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    assert!(has(&lines(&mut h), "ready"));
    // The person picks the other theme in Studio Settings.
    let other = if crate::plugin_host::api::theme::theme_name() == "Dark" { "Light" } else { "Dark" };
    h.with_edit(&mut doc, |rt| crate::plugin_host::api::theme::watch_theme_as(&rt.lua, other).unwrap());
    frames(&mut h, &mut doc, 1);
    let out = lines(&mut h);
    assert!(has(&out, "theme changed"), "{out:#?}");
    assert!(has(&out, "changed Theme"), "{out:#?}");
}

#[test]
fn editor_calls_autocomplete_and_analysis_callbacks() {
    temp_data();
    let mut doc = Document::new_place();
    let ws = doc.workspace().unwrap();
    let s = doc.insert(ws, InstanceBuilder::new("Script").with_name("AcScript").with_property("Source", "pri".to_string()));
    let mut h = PluginHost::default();
    h.load(source(
        "Ac",
        r#"
        local SES = game:GetService("ScriptEditorService")
        SES:RegisterAutocompleteCallback("ac", 1, function(req, resp)
            print("ac", req.position.line, req.position.character, req.textDocument.script.Name)
            table.insert(resp.items, { label = "printHello", textEdit = { newText = "printHello()" } })
            return resp
        end)
        SES:RegisterAutocompleteCallback("bad", 2, function() end)
        SES:RegisterScriptAnalysisCallback("lint", 1, function(req)
            return { diagnostics = { { range = { start = { line = 1, character = 1 }, ["end"] = { line = 1, character = 4 } }, message = "lint: " .. req.script.Name, severity = 2 } } }
        end)
        print("registered")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    assert!(has(&lines(&mut h), "registered"));
    // What the script editor does as the person types (script_editor.rs).
    let items = h
        .with_edit(&mut doc, |rt| crate::plugin_host::api::script_editor::autocomplete(&rt.lua, s, 1, 4).unwrap())
        .unwrap();
    let diags = h.with_edit(&mut doc, |rt| crate::plugin_host::api::script_editor::analyze(&rt.lua, s).unwrap()).unwrap();
    assert_eq!(items, vec![("printHello".to_string(), "printHello()".to_string())]);
    assert_eq!(diags, vec![(1, "lint: AcScript".to_string())]);
    let out = lines(&mut h);
    assert!(has(&out, "ac 1 4 AcScript"), "{out:#?}");
    assert!(
        has(&out, "ScriptEditorService: Callback bad did not return exactly one value, sending default response"),
        "{out:#?}"
    );
}

#[test]
fn http_request_waits_for_the_permission_prompt() {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    temp_data();
    let mut doc = Document::new_place();
    let mut h = PluginHost::default();
    permissions::set_http_check(|plugin, _url| {
        if plugin != "user_HttpWait.rbxmx" {
            return permissions::Decision::Allow;
        }
        match STATE.load(Ordering::SeqCst) {
            0 => permissions::Decision::Pending,
            _ => permissions::Decision::Deny("HttpError: denied by the test".into()),
        }
    });
    h.load(source(
        "HttpWait",
        r#"
        local t = 0
        task.spawn(function() while true do t += task.wait() end end)
        local ok, err = pcall(function() return game:GetService("HttpService"):GetAsync("http://127.0.0.1:9/") end)
        print("answered", ok, err, t > 0.1)
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 20);
    assert!(!lines(&mut h).iter().any(|l| l.starts_with("answered")), "the request didn't wait");
    STATE.store(1, Ordering::SeqCst);
    frames(&mut h, &mut doc, 5);
    let out = lines(&mut h);
    assert!(has(&out, "answered false HttpError: denied by the test true"), "{out:#?}");
}

#[test]
fn editor_removal_fires_descendant_removing_in_place() {
    temp_data();
    let mut doc = Document::new_place();
    let ws = doc.workspace().unwrap();
    let f = doc.insert(ws, InstanceBuilder::new("Folder").with_name("EdFolder"));
    let mut h = PluginHost::default();
    h.load(source(
        "EdRemove",
        r#"
        workspace.DescendantRemoving:Connect(function(d) print("removing", d.Name, d.Parent and d.Parent.Name) end)
        workspace.ChildRemoved:Connect(function(d) print("childremoved", d.Name, d.Parent) end)
        print("ready")
        "#,
    ))
    .unwrap();
    frames(&mut h, &mut doc, 2);
    assert!(has(&lines(&mut h), "ready"));
    doc.remove(&[f]); // the person deletes it (Immediate signals: rustudio's start-up place)
    frames(&mut h, &mut doc, 2);
    let out = lines(&mut h);
    assert!(has(&out, "removing EdFolder Workspace"), "{out:#?}");
    assert!(has(&out, "childremoved EdFolder nil"), "{out:#?}");
    // Undo brings it back from where the plugin could still see it.
    doc.undo();
    frames(&mut h, &mut doc, 1);
    assert!(doc.dom.get_by_ref(f).is_some_and(|i| i.parent() == ws));
}
