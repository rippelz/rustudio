//! ScriptEditorService and ScriptDocument for plugins, on the editor's
//! script tabs (script_docs.rs is the shared registry; the editor side is
//! in script_editor.rs). Measured in Studio 0.747 (REF's scenarios capture,
//! measured in Studio, api-walk):
//! - A document's Name is the script's full name and its Parent the
//!   service (`ScriptEditorService.Workspace.RefScnScript`); the command
//!   bar's document is "CommandBar" (IsCommandBar true, GetScript nil,
//!   selection 1,1,1,1, empty text).
//! - OpenScriptDocumentAsync(script) → true (the document can be found at
//!   once); TextDocumentDidOpen(doc) fires after the caller yields, as do
//!   DidChange(doc, {{range={start={line,character},end={...}},text}}) for
//!   UpdateSourceAsync / EditTextAsync (1-based, the end exclusive: the
//!   whole of "a\n-- b\n" is 1,1 to 3,1). CloseAsync yields a frame: its
//!   DidClose fires before it returns true.
//! - Text is the script's Source (Studio writes edits through to Source:
//!   after EditTextAsync, Source has them); writes go through the normal
//!   Source write (change history, script-injection permission).
//! - GetReadOnlyReasons / IsReadOnly / Add/RemoveReadOnlyReasonsAsync:
//!   "ScriptDocument:<Fn> is not yet enabled".
//! - RegisterAutocompleteCallback / RegisterScriptAnalysisCallback(name,
//!   priority, fn): the editor calls them (script_editor.rs) with Studio's
//!   request tables; a callback that doesn't return exactly one value
//!   prints "ScriptEditorService: Callback <name> did not return exactly
//!   one value, sending default response".

use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::InstanceBuilder;

use super::*;
use crate::plugin_host::script_docs::{self, DocEvent, TextChange};

#[derive(Default)]
struct Ses {
    /// script → its ScriptDocument instance (and back).
    docs: HashMap<Ref, Ref>,
    scripts: HashMap<Ref, Ref>,
    cmdbar: Option<Ref>,
    /// (name, priority, callback), Studio's order: priority, then name.
    autocomplete: Vec<(String, i64, Function)>,
    analysis: Vec<(String, i64, Function)>,
}

fn ses(lua: &Lua) -> mlua::AppDataRefMut<'_, Ses> {
    if lua.app_data_ref::<Ses>().is_none() {
        lua.set_app_data(Ses::default());
    }
    lua.app_data_mut::<Ses>().expect("ses")
}

fn service(lua: &Lua) -> mlua::Result<Ref> {
    let mut w = world(lua)?;
    let root = w.d().dom.root_ref();
    if let Some(s) = w.d().find_child(root, "ScriptEditorService") {
        return Ok(s);
    }
    let r = w.dm().dom.insert(root, InstanceBuilder::new("ScriptEditorService").with_name("ScriptEditorService"));
    w.dm().mark_tree(r);
    Ok(r)
}

fn full_name(lua: &Lua, r: Ref) -> mlua::Result<String> {
    let w = world(lua)?;
    let d = w.d();
    let root = d.dom.root_ref();
    let mut parts = Vec::new();
    let mut cur = r;
    while let Some(i) = d.dom.get_by_ref(cur) {
        if cur == root {
            break;
        }
        parts.push(i.name.clone());
        cur = i.parent();
    }
    parts.reverse();
    Ok(parts.join("."))
}

/// The ScriptDocument instance of an open script (made on first use).
fn doc_inst(lua: &Lua, script: Ref) -> mlua::Result<Ref> {
    if let Some(d) = ses(lua).docs.get(&script).copied() {
        if world(lua)?.d().exists(d) {
            return Ok(d);
        }
    }
    let name = full_name(lua, script)?;
    let svc = service(lua)?;
    let d = world(lua)?.dm().dom.insert(svc, InstanceBuilder::new("ScriptDocument").with_name(name));
    let mut s = ses(lua);
    s.docs.insert(script, d);
    s.scripts.insert(d, script);
    Ok(d)
}

fn cmdbar(lua: &Lua) -> mlua::Result<Ref> {
    if let Some(d) = ses(lua).cmdbar {
        if world(lua)?.d().exists(d) {
            return Ok(d);
        }
    }
    let svc = service(lua)?;
    let d = world(lua)?.dm().dom.insert(svc, InstanceBuilder::new("ScriptDocument").with_name("CommandBar"));
    ses(lua).cmdbar = Some(d);
    Ok(d)
}

fn unparent(lua: &Lua, d: Ref) -> mlua::Result<()> {
    let mut w = world(lua)?;
    if w.d().dom.get_by_ref(d).is_some_and(|i| i.parent().is_some()) {
        w.dm().dom.transfer_within(d, Ref::none());
        w.dm().mark_tree(d);
    }
    Ok(())
}

fn source_of(lua: &Lua, script: Ref) -> mlua::Result<String> {
    Ok(match world(lua)?.d().prop(script, "Source") {
        Some(Variant::String(s)) => s,
        _ => String::new(),
    })
}

/// `script.Source = text` as a script would write it (history, permission,
/// Changed).
fn write_source(lua: &Lua, script: Ref, text: &str) -> mlua::Result<()> {
    let f: Function = match lua.named_registry_value::<Function>("ses_write_source") {
        Ok(f) => f,
        Err(_) => {
            let f = lua.load("local s, v = ... s.Source = v").set_name("=ScriptEditorService").into_function()?;
            lua.set_named_registry_value("ses_write_source", f.clone())?;
            f
        }
    };
    f.call::<()>((inst_value(lua, script)?, text))
}

fn arg_script(lua: &Lua, args: &MultiValue, i: usize) -> mlua::Result<Ref> {
    let v = argv(args, i);
    let r = match &v {
        Value::Nil => return Err(rt_err(format!("Argument {i} missing or nil"))),
        v => opt_inst(v).ok_or_else(|| rt_err(format!("Unable to cast {} to Instance", v.type_name())))?,
    };
    let ok = world(lua)?.d().class_of(r).is_some_and(|c| rustudio_core::doc::is_a(&c, "LuaSourceContainer"));
    if !ok {
        return Err(rt_err(format!("Argument {i} is invalid: expected 'LuaSourceContainer' instance type")));
    }
    Ok(r)
}

fn int_arg(args: &MultiValue, i: usize) -> Option<i64> {
    match argv(args, i) {
        Value::Integer(n) => Some(n as i64),
        Value::Number(n) => Some(n as i64),
        _ => None,
    }
}

fn req_int(args: &MultiValue, i: usize) -> mlua::Result<i64> {
    match argv(args, i) {
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        v => int_arg(args, i).ok_or_else(|| rt_err(format!("Unable to cast {} to int", v.type_name()))),
    }
}

fn change_table(lua: &Lua, changes: &[TextChange]) -> mlua::Result<Value> {
    let arr = lua.create_table()?;
    for (i, c) in changes.iter().enumerate() {
        let pos = |p: (i64, i64)| -> mlua::Result<Table> {
            let t = lua.create_table()?;
            t.set("line", p.0)?;
            t.set("character", p.1)?;
            Ok(t)
        };
        let range = lua.create_table()?;
        range.set("start", pos(c.start)?)?;
        range.set("end", pos(c.end)?)?;
        let e = lua.create_table()?;
        e.set("range", range)?;
        e.set("text", c.text.as_str())?;
        arr.raw_set(i + 1, e)?;
    }
    Ok(Value::Table(arr))
}

/// Replaces the range of a script's text; the change, for DidChange.
fn edit(lua: &Lua, script: Ref, start: (i64, i64), end: (i64, i64), text: &str) -> mlua::Result<()> {
    let old = source_of(lua, script)?;
    let a = script_docs::index_of(&old, start.0, start.1);
    let b = script_docs::index_of(&old, end.0, end.1).max(a);
    let chars: Vec<char> = old.chars().collect();
    let new: String = chars[..a].iter().chain(text.chars().collect::<Vec<_>>().iter()).chain(chars[b..].iter()).collect();
    write_source(lua, script, &new)?;
    script_docs::changed(
        script,
        vec![TextChange { start: script_docs::pos_of(&old, a), end: script_docs::pos_of(&old, b), text: text.to_string() }],
    );
    Ok(())
}

/// A position inside the text (a line's characters, plus one past its end).
fn valid_pos(text: &str, p: (i64, i64)) -> bool {
    let Some(line) = (p.0 >= 1).then(|| text.split('\n').nth((p.0 - 1) as usize)).flatten() else { return false };
    p.1 >= 1 && p.1 <= line.chars().count() as i64 + 1
}

fn not_enabled(name: &str) -> mlua::Error {
    rt_err(format!("ScriptDocument:{name} is not yet enabled"))
}

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    match class {
        "ScriptEditorService" => service_call(lua, name, args),
        "ScriptDocument" => doc_call(lua, name, this, args),
        _ => None,
    }
}

fn service_call(lua: &Lua, name: &str, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let r = match name {
        "FindScriptDocument" => (|| {
            let s = arg_script(lua, args, 1)?;
            if script_docs::is_open(s) {
                ret1(inst_value(lua, doc_inst(lua, s)?)?)
            } else {
                ret1(Value::Nil)
            }
        })(),
        "GetScriptDocuments" => (|| {
            let mut refs = vec![cmdbar(lua)?];
            for d in script_docs::docs() {
                if world(lua)?.d().exists(d.script) {
                    refs.push(doc_inst(lua, d.script)?);
                }
            }
            ret1(list_value(lua, &refs)?)
        })(),
        "OpenScriptDocumentAsync" => (|| {
            let s = arg_script(lua, args, 1)?;
            script_docs::open(s, None, false);
            doc_inst(lua, s)?;
            ret1(Value::Boolean(true))
        })(),
        "GetEditorSource" => (|| {
            let s = arg_script(lua, args, 1)?;
            ret1(lstr(lua, &source_of(lua, s)?)?)
        })(),
        "UpdateSourceAsync" => (|| {
            require_args(args, 2)?;
            let s = arg_script(lua, args, 1)?;
            let f = match argv(args, 2) {
                Value::Function(f) => f,
                v => return Err(rt_err(format!("Unable to cast {} to Function", v.type_name()))),
            };
            let old = source_of(lua, s)?;
            // (A number becomes its text; nil leaves the source alone.)
            let new = match f.call::<Value>(old.as_str())? {
                Value::String(t) => t.to_string_lossy(),
                Value::Integer(n) => n.to_string(),
                Value::Number(n) => lua.globals().get::<Function>("tostring")?.call::<String>(n)?,
                Value::Nil => return ret0(),
                v => return Err(rt_err(format!("Unable to cast {} to string", v.type_name()))),
            };
            if new != old {
                write_source(lua, s, &new)?;
                script_docs::changed(s, vec![TextChange { start: (1, 1), end: script_docs::end_of(&old), text: new }]);
            }
            ret0()
        })(),
        "RegisterAutocompleteCallback" | "RegisterScriptAnalysisCallback" => (|| {
            require_args(args, 3)?;
            let n = arg_string(args, 1)?;
            let prio = req_int(args, 2)?;
            let f = match argv(args, 3) {
                Value::Function(f) => f,
                v => return Err(rt_err(format!("Unable to cast {} to Function", v.type_name()))),
            };
            let mut s = ses(lua);
            let list = if name == "RegisterAutocompleteCallback" { &mut s.autocomplete } else { &mut s.analysis };
            list.retain(|c| c.0 != n);
            list.push((n, prio, f));
            list.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            ret0()
        })(),
        "DeregisterAutocompleteCallback" | "DeregisterScriptAnalysisCallback" => (|| {
            let n = arg_string(args, 1)?;
            let mut s = ses(lua);
            let list = if name == "DeregisterAutocompleteCallback" { &mut s.autocomplete } else { &mut s.analysis };
            list.retain(|c| c.0 != n);
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}

/// The script behind a document, or None for the command bar.
fn doc_script(lua: &Lua, this: Ref) -> Option<Ref> {
    ses(lua).scripts.get(&this).copied()
}

fn doc_text(lua: &Lua, this: Ref) -> mlua::Result<String> {
    match doc_script(lua, this) {
        Some(s) => source_of(lua, s),
        None => Ok(String::new()),
    }
}

fn selection(lua: &Lua, this: Ref) -> (i64, i64, i64, i64) {
    doc_script(lua, this).and_then(script_docs::doc).map(|d| d.selection).unwrap_or((1, 1, 1, 1))
}

/// (start, end) of a selection, in text order.
fn ordered(sel: (i64, i64, i64, i64)) -> ((i64, i64), (i64, i64)) {
    let a = (sel.0, sel.1);
    let b = (sel.2, sel.3);
    if a <= b { (a, b) } else { (b, a) }
}

fn range_text(text: &str, a: (i64, i64), b: (i64, i64)) -> String {
    let i = script_docs::index_of(text, a.0, a.1);
    let j = script_docs::index_of(text, b.0, b.1).max(i);
    text.chars().skip(i).take(j - i).collect()
}

fn doc_call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    let two = |a: i64, b: i64| Ok(MultiValue::from_iter([Value::Integer(a as _), Value::Integer(b as _)]));
    let r = match name {
        "IsCommandBar" => ret1(Value::Boolean(doc_script(lua, this).is_none())),
        "GetScript" => (|| match doc_script(lua, this) {
            Some(s) => ret1(inst_value(lua, s)?),
            None => ret1(Value::Nil),
        })(),
        "GetLineCount" => (|| ret1(Value::Integer(script_docs::line_count(&doc_text(lua, this)?) as _)))(),
        "GetLine" => (|| {
            let text = doc_text(lua, this)?;
            let n = int_arg(args, 1).unwrap_or(selection(lua, this).0);
            match text.split('\n').nth((n.max(1) - 1) as usize) {
                Some(line) if n >= 1 => ret1(lstr(lua, line)?),
                _ => Err(rt_err("Attempt to access a document with an index out of bounds")),
            }
        })(),
        "GetText" => (|| {
            let text = doc_text(lua, this)?;
            let (Some(l1), Some(c1)) = (int_arg(args, 1), int_arg(args, 2)) else {
                return ret1(lstr(lua, &text)?);
            };
            let end = match (int_arg(args, 3), int_arg(args, 4)) {
                (Some(l2), Some(c2)) => (l2, c2),
                _ => script_docs::end_of(&text),
            };
            ret1(lstr(lua, &range_text(&text, (l1, c1), end))?)
        })(),
        "GetSelection" => {
            let s = selection(lua, this);
            Ok(MultiValue::from_iter([s.0, s.1, s.2, s.3].map(|x| Value::Integer(x as _))))
        }
        "GetSelectionStart" => {
            let (a, _) = ordered(selection(lua, this));
            two(a.0, a.1)
        }
        "GetSelectionEnd" => {
            let (_, b) = ordered(selection(lua, this));
            two(b.0, b.1)
        }
        "GetSelectedText" => (|| {
            let (a, b) = ordered(selection(lua, this));
            ret1(lstr(lua, &range_text(&doc_text(lua, this)?, a, b))?)
        })(),
        "HasSelectedText" => {
            let (a, b) = ordered(selection(lua, this));
            ret1(Value::Boolean(a != b))
        }
        "GetViewport" => {
            let v = doc_script(lua, this).and_then(script_docs::doc).map(|d| d.viewport).unwrap_or((1, 1));
            two(v.0, v.1)
        }
        "GetReadOnlyReasons" | "IsReadOnly" | "AddReadOnlyReasonsAsync" => Err(not_enabled(name)),
        "RemoveReadOnlyReasonsAsync" => (|| {
            if matches!(argv(args, 1), Value::Nil) {
                return Err(rt_err("Argument 1 missing or nil"));
            }
            Err(not_enabled(name))
        })(),
        "ReviewableTextEditsAsync" => (|| {
            match argv(args, 1) {
                Value::Nil => Err(rt_err("Argument 1 missing or nil")),
                Value::Table(t) if t.raw_len() == 0 => Err(rt_err("Changes array must not be empty")),
                _ => Err(not_enabled(name)),
            }
        })(),
        "EditTextAsync" => (|| {
            require_args(args, 5)?;
            let text = arg_string(args, 1)?;
            let (l1, c1, l2, c2) = (req_int(args, 2)?, req_int(args, 3)?, req_int(args, 4)?, req_int(args, 5)?);
            // (The command bar's document: false.)
            let Some(s) = doc_script(lua, this) else { return ret1(Value::Boolean(false)) };
            let src = source_of(lua, s)?;
            if !valid_pos(&src, (l1, c1)) || !valid_pos(&src, (l2, c2)) || (l2, c2) < (l1, c1) {
                return Err(rt_err("Range is not valid"));
            }
            edit(lua, s, (l1, c1), (l2, c2), &text)?;
            ret1(Value::Boolean(true))
        })(),
        "MultiEditTextAsync" => (|| {
            let Value::Table(list) = argv(args, 1) else { return Err(rt_err("Argument 1 missing or nil")) };
            let Some(s) = doc_script(lua, this) else {
                return Ok(MultiValue::from_iter([Value::Boolean(false), lstr(lua, "Cannot edit the command bar")?]));
            };
            // Each edit: {NewText, StartLine, StartCharacter, EndLine,
            // EndCharacter} (Studio's api docs); a missing key is Studio's
            // "invalid unordered_map<K, T> key". Ranges are in the original
            // text: applied from the last one back.
            let mut edits = Vec::new();
            for e in list.sequence_values::<Table>() {
                let e = e?;
                let key = |k: &str| -> mlua::Result<Value> {
                    match e.raw_get::<Value>(k)? {
                        Value::Nil => Err(rt_err("invalid unordered_map<K, T> key")),
                        v => Ok(v),
                    }
                };
                let num = |k: &str| -> mlua::Result<i64> {
                    match key(k)? {
                        Value::Integer(n) => Ok(n as i64),
                        Value::Number(n) => Ok(n as i64),
                        v => Err(rt_err(format!("Unable to cast {} to int", v.type_name()))),
                    }
                };
                let text = match key("NewText")? {
                    Value::String(t) => t.to_string_lossy(),
                    v => return Err(rt_err(format!("Unable to cast {} to string", v.type_name()))),
                };
                edits.push(((num("StartLine")?, num("StartCharacter")?), (num("EndLine")?, num("EndCharacter")?), text));
            }
            {
                let src = source_of(lua, s)?;
                if edits.iter().any(|(a, b, _)| !valid_pos(&src, *a) || !valid_pos(&src, *b) || b < a) {
                    return Err(rt_err("Range is not valid"));
                }
            }
            let old = source_of(lua, s)?;
            edits.sort_by(|a, b| b.0.cmp(&a.0));
            let mut chars: Vec<char> = old.chars().collect();
            let mut changes = Vec::new();
            for (a, b, t) in &edits {
                let i = script_docs::index_of(&old, a.0, a.1);
                let j = script_docs::index_of(&old, b.0, b.1).max(i);
                chars.splice(i..j, t.chars());
                changes.push(TextChange { start: *a, end: *b, text: t.clone() });
            }
            let new: String = chars.into_iter().collect();
            write_source(lua, s, &new)?;
            changes.reverse();
            script_docs::changed(s, changes);
            ret1(Value::Boolean(true))
        })(),
        "ForceSetSelectionAsync" | "RequestSetSelectionAsync" => (|| {
            require_args(args, 2)?;
            let (l, c) = (req_int(args, 1)?, req_int(args, 2)?);
            let (al, ac) = (int_arg(args, 3).unwrap_or(l), int_arg(args, 4).unwrap_or(c));
            if let Some(s) = doc_script(lua, this) {
                script_docs::set_selection(s, (l, c, al, ac), false);
            }
            ret1(Value::Boolean(true))
        })(),
        // (CloseAsync's yield is the Luau wrapper's: LUA_CODE.)
        "__CloseDocument" => (|| match doc_script(lua, this) {
            Some(s) => {
                script_docs::close(s, false);
                // A closed document has no parent (Studio, also in DidClose).
                unparent(lua, this)?;
                ret1(Value::Boolean(true))
            }
            // (raised by the Luau side, without a position)
            None => Ok(MultiValue::from_iter([Value::Nil, lstr(lua, "Cannot close the Command Bar")?])),
        })(),
        _ => return None,
    };
    Some(r)
}

/// Luau halves of the yielding members (set in `__rs_lua_methods`).
pub const LUA_CODE: &str = r#"
local close = ...
local M = __rs_lua_methods
M.CloseAsync = function(self)
	local ok, err = close(self)
	if ok == nil then error(err, 0) end
	task.wait()
	return ok
end
"#;

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let close = lua.create_function(|lua, d: Value| {
        let this = opt_inst(&d).ok_or_else(|| rt_err("Expected ':' not '.' calling member function CloseAsync"))?;
        match doc_call(lua, "__CloseDocument", this, &MultiValue::new()) {
            Some(r) => r,
            None => Ok(MultiValue::new()),
        }
    })?;
    lua.load(LUA_CODE).set_name("=rustudio.plugin.script_editor").call::<()>(close)
}

/// StudioService.ActiveScript (testing_studio.rs asks).
pub fn active_script() -> Option<Ref> {
    script_docs::active()
}

/// Fires the queued document events (end of the edit frame). The command
/// bar's document is the service's child as soon as the service exists
/// (Studio: ScriptEditorService has 1 child with no script open).
pub fn fire_events(lua: &Lua) -> mlua::Result<bool> {
    if ses(lua).cmdbar.is_none() {
        let has_service = {
            let w = world(lua)?;
            let d = w.d();
            d.find_child(d.dom.root_ref(), "ScriptEditorService").is_some()
        };
        if has_service {
            cmdbar(lua)?;
        }
    }
    if !script_docs::has_events() {
        return Ok(false);
    }
    let events = {
        // (Closed: a script already gone from the document still had a
        // document here.)
        let known: Vec<Ref> = ses(lua).docs.keys().copied().collect();
        let w = world(lua)?;
        let d = w.d();
        script_docs::take_events_where(|r| d.exists(r) || known.contains(&r))
    };
    if events.is_empty() {
        return Ok(false);
    }
    let svc = service(lua)?;
    for e in events {
        match e {
            DocEvent::Opened(s) => {
                if !world(lua)?.d().exists(s) {
                    continue;
                }
                let d = doc_inst(lua, s)?;
                let v = inst_value(lua, d)?;
                world(lua)?.fire(svc, "TextDocumentDidOpen", vec![v]);
            }
            DocEvent::Closed(s) => {
                let d = ses(lua).docs.remove(&s);
                if let Some(d) = d {
                    unparent(lua, d)?;
                    let v = inst_value(lua, d)?;
                    world(lua)?.fire(svc, "TextDocumentDidClose", vec![v]);
                }
            }
            DocEvent::Changed(s, ch) => {
                if !world(lua)?.d().exists(s) {
                    continue;
                }
                let d = doc_inst(lua, s)?;
                let t = change_table(lua, &ch)?;
                let v = inst_value(lua, d)?;
                world(lua)?.fire(svc, "TextDocumentDidChange", vec![v, t]);
            }
            DocEvent::Selection(s, sel) => {
                if let Some(d) = ses(lua).docs.get(&s).copied() {
                    let v = [sel.0, sel.1, sel.2, sel.3].map(|x| Value::Integer(x as _)).to_vec();
                    world(lua)?.fire(d, "SelectionChanged", v);
                }
            }
            DocEvent::Viewport(s, vp) => {
                if let Some(d) = ses(lua).docs.get(&s).copied() {
                    world(lua)?.fire(d, "ViewportChanged", vec![Value::Integer(vp.0 as _), Value::Integer(vp.1 as _)]);
                }
            }
        }
    }
    Ok(true)
}

// ------------------------------------------- the editor's callbacks

/// Calls the registered autocomplete callbacks in order (each gets the
/// previous one's response), as Studio's script editor does; returns the
/// final items' labels (with what each inserts).
pub fn autocomplete(lua: &Lua, script: Ref, line: i64, character: i64) -> mlua::Result<Vec<(String, String)>> {
    let cbs: Vec<(String, Function)> = ses(lua).autocomplete.iter().map(|c| (c.0.clone(), c.2.clone())).collect();
    if cbs.is_empty() {
        return Ok(Vec::new());
    }
    let doc = doc_inst(lua, script)?;
    let req = lua.create_table()?;
    let pos = lua.create_table()?;
    pos.set("line", line)?;
    pos.set("character", character)?;
    req.set("position", pos)?;
    let td = lua.create_table()?;
    td.set("document", inst_value(lua, doc)?)?;
    td.set("script", inst_value(lua, script)?)?;
    req.set("textDocument", td)?;
    let mut resp = lua.create_table()?;
    resp.set("items", lua.create_table()?)?;
    for (name, f) in cbs {
        match f.call::<MultiValue>((req.clone(), resp.clone())) {
            Ok(r) if r.len() == 1 => {
                if let Some(Value::Table(t)) = r.into_iter().next() {
                    resp = t;
                }
            }
            Ok(_) => {
                world(lua)?.output.push((
                    rustudio_core::log::Level::Error,
                    format!("ScriptEditorService: Callback {name} did not return exactly one value, sending default response"),
                ));
            }
            Err(e) => report_error_in(lua, &e),
        }
    }
    let mut out = Vec::new();
    if let Ok(items) = resp.get::<Table>("items") {
        for it in items.sequence_values::<Table>().flatten() {
            let label: String = it.get("label").unwrap_or_default();
            let insert = it
                .get::<Table>("textEdit")
                .ok()
                .and_then(|te| te.get::<String>("newText").ok())
                .unwrap_or_else(|| label.clone());
            if !label.is_empty() {
                out.push((label, insert));
            }
        }
    }
    Ok(out)
}

/// Calls the registered script analysis callbacks; their diagnostics as
/// (line, message).
pub fn analyze(lua: &Lua, script: Ref) -> mlua::Result<Vec<(i64, String)>> {
    let cbs: Vec<(String, Function)> = ses(lua).analysis.iter().map(|c| (c.0.clone(), c.2.clone())).collect();
    let mut out = Vec::new();
    for (name, f) in cbs {
        let req = lua.create_table()?;
        req.set("script", inst_value(lua, script)?)?;
        match f.call::<MultiValue>(req) {
            Ok(r) if r.len() == 1 => {
                let Some(Value::Table(t)) = r.into_iter().next() else { continue };
                let Ok(diags) = t.get::<Table>("diagnostics") else { continue };
                for d in diags.sequence_values::<Table>().flatten() {
                    let msg: String = d.get("message").unwrap_or_default();
                    let line = d
                        .get::<Table>("range")
                        .ok()
                        .and_then(|r| r.get::<Table>("start").ok())
                        .and_then(|s| s.get::<i64>("line").ok())
                        .unwrap_or(1);
                    out.push((line, msg));
                }
            }
            Ok(_) => world(lua)?.output.push((
                rustudio_core::log::Level::Error,
                format!("ScriptEditorService: Callback {name} did not return exactly one value, sending default response"),
            )),
            Err(e) => report_error_in(lua, &e),
        }
    }
    Ok(out)
}

fn report_error_in(lua: &Lua, e: &mlua::Error) {
    if let Ok(mut w) = world(lua) {
        w.output.push((rustudio_core::log::Level::Error, rustudio_engine::runtime::describe_error(e)));
    }
}

/// Whether any plugin registered an autocomplete / analysis callback.
pub fn has_callbacks(lua: &Lua) -> (bool, bool) {
    lua.app_data_ref::<Ses>().map(|s| (!s.autocomplete.is_empty(), !s.analysis.is_empty())).unwrap_or((false, false))
}
