//! Luau run for an outside caller (the editor's MCP server) that wants the
//! chunk's return values back, not only its errors: the command bar's way
//! (a thread at the given identity, scripts' `_G` / `shared`, free to yield
//! with `task.wait`), with the outcome kept in a registry table the caller
//! polls each frame until the thread has finished.
//!
//! [`start`] loads the chunk and runs it up to its first yield; [`poll`]
//! reads the outcome once it is there (and drops the registry entry). The
//! values come back as JSON: numbers, strings, booleans and nil as they are,
//! tables as arrays or objects (nested up to [`MAX_DEPTH`], cycles cut),
//! Instances as their full name, other userdata as `tostring` gives them.

use mlua::{Lua, Table, Value};
use serde_json::{json, Map, Value as J};

use rustudio_engine::runtime::Side;

/// Nesting kept when a returned table is turned into JSON.
pub const MAX_DEPTH: usize = 8;
/// Entries kept per returned table.
pub const MAX_ITEMS: usize = 2000;

const WRAPPER: &str = r##"local f, sink = ...
local function keep(ok, ...)
	sink.n = select("#", ...)
	sink.values = { ... }
	sink.ok = ok
	sink.done = true
end
keep(xpcall(f, function(e)
	return debug.traceback(tostring(e), 2)
end))"##;

fn key(id: u64) -> String {
    format!("rustudio_mcp_eval_{id}")
}

/// Loads `code` and runs it on a new thread at identity `identity` until it
/// finishes or first yields. The runtime must be entered; call
/// `Runtime::dispatch` after.
pub fn start(lua: &Lua, code: &str, id: u64, identity: u8) -> Result<(), String> {
    rustudio_engine::runtime::security::with_main_identity(lua, identity, || -> mlua::Result<()> {
        let env = rustudio_engine::globals_parity::harness_env(lua)?;
        let f = lua.load(code).set_name("=mcp").set_environment(env).into_function()?;
        let sink = lua.create_table()?;
        lua.set_named_registry_value(&key(id), sink.clone())?;
        let wrapper = lua.load(WRAPPER).set_name("=mcp").into_function()?;
        let th = rustudio_engine::runtime::domains::assets_cloud::scene::create_thread(lua, wrapper)?;
        th.resume::<mlua::MultiValue>((f, sink)).map(|_| ())
    })
    .map_err(|e| rustudio_engine::runtime::describe_error(&e))
}

/// The outcome of a [`start`]ed run once its thread is done:
/// `{"ok": true, "values": [...]}` or `{"ok": false, "error": "..."}`.
/// None while it is still running (yielded).
pub fn poll(lua: &Lua, id: u64) -> Option<J> {
    let sink: Table = lua.named_registry_value(&key(id)).ok()?;
    if !sink.raw_get::<bool>("done").unwrap_or(false) {
        return None;
    }
    let _ = lua.unset_named_registry_value(&key(id));
    let ok = sink.raw_get::<bool>("ok").unwrap_or(false);
    let n = sink.raw_get::<i64>("n").unwrap_or(0).max(0) as usize;
    let values: Option<Table> = sink.raw_get("values").ok();
    let list: Vec<Value> = (1..=n).map(|i| values.as_ref().and_then(|t| t.raw_get(i).ok()).unwrap_or(Value::Nil)).collect();
    if ok {
        Some(json!({ "ok": true, "values": list.iter().map(|v| to_json(lua, v, 0, &mut Vec::new())).collect::<Vec<_>>() }))
    } else {
        let error = list.first().map(|v| display(lua, v)).unwrap_or_else(|| "error".into());
        Some(json!({ "ok": false, "error": error }))
    }
}

/// Forgets a run (the caller gave up waiting); its thread may still finish.
pub fn forget(lua: &Lua, id: u64) {
    let _ = lua.unset_named_registry_value(&key(id));
}

fn display(lua: &Lua, v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        _ => lua
            .globals()
            .get::<mlua::Function>("tostring")
            .and_then(|f| f.call::<String>(v.clone()))
            .unwrap_or_else(|_| format!("{v:?}")),
    }
}

fn typeof_name(lua: &Lua, v: &Value) -> Option<String> {
    lua.globals().get::<mlua::Function>("typeof").ok()?.call::<String>(v.clone()).ok()
}

/// A Lua value as JSON (see the module doc).
pub fn to_json(lua: &Lua, v: &Value, depth: usize, seen: &mut Vec<*const std::ffi::c_void>) -> J {
    match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => json!(i),
        Value::Number(n) if n.is_finite() => json!(n),
        Value::Number(n) => J::String(n.to_string()),
        Value::String(s) => J::String(s.to_string_lossy()),
        Value::Table(t) => {
            let ptr = t.to_pointer();
            if depth >= MAX_DEPTH || seen.contains(&ptr) {
                return J::String(if depth >= MAX_DEPTH { "<table: nested too deep>".into() } else { "<table: cycle>".into() });
            }
            seen.push(ptr);
            let len = t.raw_len();
            let mut count = 0usize;
            let mut sequence = true;
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for pair in t.pairs::<Value, Value>() {
                let Ok((k, val)) = pair else { continue };
                count += 1;
                if !matches!(k, Value::Integer(i) if i >= 1 && (i as usize) <= len) {
                    sequence = false;
                }
                if entries.len() < MAX_ITEMS {
                    entries.push((k, val));
                }
            }
            let out = if sequence && count == len {
                let mut items = vec![J::Null; len.min(MAX_ITEMS)];
                for (k, val) in &entries {
                    if let Value::Integer(i) = k {
                        if let Some(slot) = items.get_mut(*i as usize - 1) {
                            *slot = to_json(lua, val, depth + 1, seen);
                        }
                    }
                }
                if len > MAX_ITEMS {
                    items.push(J::String(format!("<{} more>", len - MAX_ITEMS)));
                }
                J::Array(items)
            } else {
                let mut map = Map::new();
                for (k, val) in &entries {
                    map.insert(display(lua, k), to_json(lua, val, depth + 1, seen));
                }
                if count > MAX_ITEMS {
                    map.insert("<more>".into(), json!(count - MAX_ITEMS));
                }
                J::Object(map)
            };
            seen.pop();
            out
        }
        Value::UserData(_) => match typeof_name(lua, v).as_deref() {
            Some("Instance") => {
                let full = lua
                    .load("local v = ... return v:GetFullName()")
                    .into_function()
                    .and_then(|f| f.call::<String>(v.clone()))
                    .ok();
                J::String(full.unwrap_or_else(|| display(lua, v)))
            }
            Some(t) => J::String(format!("{t}({})", display(lua, v))),
            None => J::String(display(lua, v)),
        },
        _ => J::String(display(lua, v)),
    }
}

impl crate::game::Game {
    /// [`start`] on a Play side (the worker's game when Play is threaded).
    pub fn mcp_eval_start(&mut self, side: Side, code: &str, id: u64) -> Result<(), String> {
        if self.play_thread.is_some() {
            let code = code.to_owned();
            return self.on_play_worker(move |game| game.mcp_eval_start(side, &code, id)).unwrap_or_else(|| Err("Play stopped".into()));
        }
        let identity = rustudio_engine::runtime::security::COMMAND;
        self.with_play_side(side, |rt, _, _| {
            let r = start(&rt.lua, code, id, identity);
            rt.dispatch();
            r
        })
        .unwrap_or_else(|| Err(format!("Play has no {side:?} side")))
    }

    /// [`poll`] on a Play side: Some(outcome) once done, or an error outcome
    /// when the side is gone.
    pub fn mcp_eval_poll(&mut self, side: Side, id: u64) -> Option<J> {
        if self.play_thread.is_some() {
            return self.on_play_worker(move |game| game.mcp_eval_poll(side, id)).unwrap_or_else(|| Some(json!({ "ok": false, "error": "Play stopped" })));
        }
        if !self.is_playing() {
            return Some(json!({ "ok": false, "error": "Play stopped before the chunk finished" }));
        }
        self.with_play_side(side, |rt, _, _| poll(&rt.lua, id)).unwrap_or_else(|| Some(json!({ "ok": false, "error": format!("Play has no {side:?} side") })))
    }

    /// Drops a run's outcome slot (the caller timed out).
    pub fn mcp_eval_forget(&mut self, side: Side, id: u64) {
        if self.play_thread.is_some() {
            let _ = self.on_play_worker(move |game| game.mcp_eval_forget(side, id));
            return;
        }
        let _ = self.with_play_side(side, |rt, _, _| forget(&rt.lua, id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_become_json() {
        let lua = Lua::new();
        let t: Value = lua.load(r#"return { 1, "two", true, { x = 1.5 } }"#).eval().unwrap();
        let j = to_json(&lua, &t, 0, &mut Vec::new());
        assert_eq!(j, json!([1, "two", true, { "x": 1.5 }]));
        let m: Value = lua.load(r#"local t = { a = 1 } t.self = t return t"#).eval().unwrap();
        let j = to_json(&lua, &m, 0, &mut Vec::new());
        assert_eq!(j["a"], json!(1));
        assert_eq!(j["self"], json!("<table: cycle>"));
        let holes: Value = lua.load(r#"return { [1] = 1, [3] = 3 }"#).eval().unwrap();
        assert!(to_json(&lua, &holes, 0, &mut Vec::new()).is_object());
    }
}
