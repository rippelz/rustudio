//! plugin:GetSetting / plugin:SetSetting, kept as Studio keeps them: one
//! JSON object per settings file, `<user id>/InstalledPlugins/<key>/
//! settings.json`, where the key is "0" for every local plugin (they share
//! one file) and the asset id for an installed one. Measured in Studio
//! 0.747 (2026-11-14): a SetSetting is on disk at once and read by the
//! plugin's copies in the Play server and client; numbers, booleans,
//! strings and tables are stored as JSON; other values as text: a Vector3
//! or Color3 as its tostring ("1, 2, 3"), an EnumItem as its Name
//! ("Grass"), anything else (an Instance, a function) as its type name.
//! GetSetting of a missing key is nil.
//!
//! The files live under rustudio's own data folder (paths::data()/
//! plugin-data; RUSTUDIO_PLUGIN_DATA moves it, for tests), never in a
//! Studio or Vinegar folder.

use std::path::PathBuf;
use std::sync::Mutex;

use mlua::{Lua, Value};
use serde_json::{Map, Value as Json};

/// The signed-in user's id (0 signed out): the settings folder, and
/// GetStudioUserId. (It lives in the engine: StudioService.GetUserId reads
/// it there too.)
pub use rustudio_engine::runtime::domains::plugin_api::{set_user_id, user_id};

/// Serializes reads and writes of the settings files (the edit and Play
/// VMs share them).
static LOCK: Mutex<()> = Mutex::new(());

pub fn data_root() -> PathBuf {
    match std::env::var_os("RUSTUDIO_PLUGIN_DATA") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => rustudio_core::paths::data().join("plugin-data"),
    }
}

pub fn file(key: &str) -> PathBuf {
    data_root()
        .join(user_id().to_string())
        .join("InstalledPlugins")
        .join(key)
        .join("settings.json")
}

fn read_all(key: &str) -> Map<String, Json> {
    std::fs::read(file(key))
        .ok()
        .and_then(|b| serde_json::from_slice::<Json>(&b).ok())
        .and_then(|v| match v {
            Json::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

pub fn get(key: &str, name: &str) -> Option<Json> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read_all(key).remove(name)
}

/// Sets (or with None removes) one setting and writes the file.
pub fn set(key: &str, name: &str, value: Option<Json>) -> Result<(), String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = read_all(key);
    match value {
        Some(v) => {
            all.insert(name.to_string(), v);
        }
        None => {
            all.remove(name);
        }
    }
    let path = file(key);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = studio_json(&Json::Object(all), true);
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Studio's own layout for the file: `{ "a":1, "t":{  "x":[ true,"y" ] } }`.
fn studio_json(v: &Json, top: bool) -> String {
    match v {
        Json::Object(m) if m.is_empty() => if top { "{}".into() } else { "{  }".into() },
        Json::Object(m) => {
            let body: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}:{}", Json::String(k.clone()), studio_json(v, false)))
                .collect();
            if top {
                format!("{{ {} }}", body.join(", "))
            } else {
                format!("{{  {} }}", body.join(", "))
            }
        }
        Json::Array(a) if a.is_empty() => "[  ]".into(),
        Json::Array(a) => {
            let body: Vec<String> = a.iter().map(|v| studio_json(v, false)).collect();
            format!("[ {} ]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// A Lua value as Studio stores it (see the header). None
/// for nil (the key is removed). A table with an array part keeps only that
/// part (`{1,2,x=3}` → `[1,2]`, `{[1]=1,[3]=3}` → `[1]`); otherwise its
/// keys become strings (`{[10]="a"}` → `{"10":"a"}`). NaN and infinities:
/// Studio writes a bare `NAN` / `INF` that makes the whole shared file
/// unreadable (F25, every setting lost); rustudio stores nothing for that
/// key instead, so the other plugins' settings survive.
pub fn to_json(lua: &Lua, v: &Value, depth: usize) -> Option<Json> {
    Some(match v {
        Value::Nil => return None,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => {
            if !n.is_finite() {
                return None;
            }
            if n.fract() == 0.0 && n.abs() <= 9.007_199_254_740_992e15 {
                Json::from(*n as i64)
            } else {
                serde_json::Number::from_f64(*n).map(Json::Number)?
            }
        }
        Value::String(s) => Json::String(s.to_string_lossy()),
        Value::Table(t) if depth < 64 => {
            let mut arr = Vec::new();
            let mut k = 1usize;
            loop {
                let item: Value = t.raw_get(k).unwrap_or(Value::Nil);
                if item.is_nil() {
                    break;
                }
                arr.push(to_json(lua, &item, depth + 1).unwrap_or(Json::Null));
                k += 1;
            }
            if !arr.is_empty() {
                Json::Array(arr)
            } else {
                let mut m = Map::new();
                for (k, v) in t.clone().pairs::<Value, Value>().flatten() {
                    let key = match &k {
                        Value::String(s) => s.to_string_lossy(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) if n.fract() == 0.0 => format!("{}", *n as i64),
                        other => lua_tostring(lua, other),
                    };
                    if let Some(j) = to_json(lua, &v, depth + 1) {
                        m.insert(key.replace('.', "_"), j);
                    }
                }
                Json::Object(m)
            }
        }
        Value::UserData(ud) => {
            let ty = lua_typeof(lua, v);
            match ty.as_str() {
                "EnumItem" => {
                    let name = ud.borrow::<rustudio_engine::ltypes::LEnumItem>().map(|e| e.name.clone()).ok();
                    Json::String(name.unwrap_or_else(|| lua_tostring(lua, v)))
                }
                "Instance" | "RBXScriptSignal" | "RBXScriptConnection" => Json::String(ty),
                _ => Json::String(lua_tostring(lua, v)),
            }
        }
        Value::Buffer(b) => Json::String(format!("buffer {{ Size = {} }}", b.len())),
        // (Vector3 is Luau's native vector: "1, 2, 3" as tostring gives.)
        Value::Function(_) | Value::Thread(_) => Json::String(lua_typeof(lua, v)),
        other => Json::String(lua_tostring(lua, other)),
    })
}

pub fn from_json(lua: &Lua, j: &Json) -> mlua::Result<Value> {
    Ok(match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(a) => {
            let t = lua.create_table()?;
            for (i, v) in a.iter().enumerate() {
                t.raw_set(i + 1, from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
        Json::Object(m) => {
            let t = lua.create_table()?;
            for (k, v) in m {
                t.raw_set(k.as_str(), from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}

fn lua_typeof(lua: &Lua, v: &Value) -> String {
    lua.globals()
        .get::<mlua::Function>("typeof")
        .and_then(|f| f.call::<String>(v.clone()))
        .unwrap_or_else(|_| v.type_name().to_string())
}

fn lua_tostring(lua: &Lua, v: &Value) -> String {
    lua.globals()
        .get::<mlua::Function>("tostring")
        .and_then(|f| f.call::<String>(v.clone()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_layout() {
        let v: Json = serde_json::from_str(r#"{"a":1,"t":{"x":[true,"y"],"e":[]}}"#).unwrap();
        assert_eq!(studio_json(&v, true), r#"{ "a":1, "t":{  "x":[ true,"y" ], "e":[  ] } }"#);
    }
}
