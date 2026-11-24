//! LocalizationTable / Translator / LocalizationService: real lookups over
//! the tables' `Contents` (the JSON Studio saves: `[{key, context,
//! examples, source, values{locale: text}}]`). Error texts, entry order and
//! the formatting rules were measured in Studio (checks/parity/players_chat.md).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use mlua::{Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};
use serde_json::{json, Value as J};

use crate::runtime::*;

static CONTENTS_GEN: AtomicU64 = AtomicU64::new(1);

/// Called whenever a LocalizationTable's `Contents` property is written.
pub fn bump_contents_gen() {
    CONTENTS_GEN.fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub key: String,
    pub source: String,
    pub context: String,
    pub example: String,
    pub values: BTreeMap<String, String>,
}

/// What a Translator translates with: one table (LocalizationTable:
/// GetTranslator) or every table under LocalizationService.
#[derive(Clone, Debug)]
pub struct Translator {
    pub table: Option<Ref>,
    pub locale: String,
}

#[derive(Clone, Debug)]
struct CachedTable {
    gen: u64,
    raw: String,
    entries: Vec<Entry>,
    by_key: HashMap<String, Vec<usize>>,
    by_source: HashMap<String, Vec<usize>>,
}

impl CachedTable {
    fn from_entries(gen: u64, raw: String, entries: Vec<Entry>) -> Self {
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_source: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            if !e.key.is_empty() {
                by_key.entry(e.key.clone()).or_default().push(i);
            }
            if !e.source.is_empty() {
                by_source.entry(e.source.clone()).or_default().push(i);
            }
        }
        Self { gen, raw, entries, by_key, by_source }
    }
}

#[derive(Default)]
pub struct LocState {
    pub translators: HashMap<Ref, Translator>,
    cache: HashMap<Ref, CachedTable>,
}

fn state(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, LocState>> {
    if lua.app_data_ref::<LocState>().is_none() {
        lua.set_app_data(LocState::default());
    }
    lua.app_data_mut::<LocState>().ok_or_else(|| rt_err("localization state"))
}

pub fn parse(contents: &str) -> Result<Vec<Entry>, String> {
    let v: J = serde_json::from_str(contents).map_err(|_| "JSON parse error: Invalid value.".to_string())?;
    let rows = v.as_array().ok_or("JSON parse error: Invalid value.")?;
    let s = |r: &J, k: &str| r[k].as_str().unwrap_or("").to_string();
    Ok(rows
        .iter()
        .map(|r| Entry {
            key: s(r, "key"),
            source: s(r, "source"),
            context: s(r, "context"),
            example: s(r, "examples"),
            values: r["values"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// Studio's order: keyed entries first, then by source and context.
fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        (a.key.is_empty(), &a.key, &a.source, &a.context).cmp(&(b.key.is_empty(), &b.key, &b.source, &b.context))
    });
}

pub fn serialize(entries: &[Entry]) -> String {
    let mut e = entries.to_vec();
    sort(&mut e);
    let rows: Vec<J> = e
        .iter()
        .map(|e| {
            let mut o = serde_json::Map::new();
            for (k, v) in [("key", &e.key), ("context", &e.context), ("examples", &e.example), ("source", &e.source)] {
                if !v.is_empty() {
                    o.insert(k.into(), J::String(v.clone()));
                }
            }
            o.insert("values".into(), json!(e.values));
            J::Object(o)
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
}

fn ensure_cached(lua: &Lua, r: Ref) -> mlua::Result<()> {
    let cur_gen = CONTENTS_GEN.load(Ordering::Relaxed);
    {
        let st = state(lua)?;
        if let Some(c) = st.cache.get(&r) {
            if c.gen == cur_gen {
                return Ok(());
            }
        }
    }
    let text_opt: Option<String> = {
        let w = world(lua)?;
        let d = w.d();
        let raw_ref = d
            .dom
            .get_by_ref(r)
            .and_then(|i| i.properties.get(&rbx_dom_weak::ustr("Contents")))
            .and_then(|v| match v {
                Variant::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("");
        let mut st = state(lua)?;
        if let Some(c) = st.cache.get_mut(&r) {
            if c.raw == raw_ref {
                c.gen = cur_gen;
                return Ok(());
            }
        }
        Some(raw_ref.to_string())
    };
    let text = text_opt.unwrap_or_default();
    let entries = if text.trim().is_empty() {
        Vec::new()
    } else {
        parse(&text).unwrap_or_default()
    };
    state(lua)?.cache.insert(r, CachedTable::from_entries(cur_gen, text, entries));
    Ok(())
}

pub fn entries_of(lua: &Lua, r: Ref) -> mlua::Result<Vec<Entry>> {
    ensure_cached(lua, r)?;
    Ok(state(lua)?.cache.get(&r).map(|c| c.entries.clone()).unwrap_or_default())
}

fn save(lua: &Lua, r: Ref, entries: &[Entry]) -> mlua::Result<()> {
    let raw = serialize(entries);
    store_prop(lua, r, "Contents", Variant::String(raw.clone()))?;
    let cur_gen = CONTENTS_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    state(lua)?
        .cache
        .insert(r, CachedTable::from_entries(cur_gen, raw, entries.to_vec()));
    Ok(())
}

/// Same entry: by key when one is given, else by source and context.
fn find<'a>(entries: &'a mut [Entry], key: &str, source: &str, context: &str) -> Option<&'a mut Entry> {
    entries.iter_mut().find(|e| {
        if !key.is_empty() {
            e.key == key
        } else {
            e.key.is_empty() && e.source == source && e.context == context
        }
    })
}

fn s_arg(args: &MultiValue, i: usize) -> mlua::Result<String> {
    match arg(args, i) {
        Value::String(s) => Ok(s.to_str()?.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        _ => Err(rt_err(format!("Unable to cast value to std::string"))),
    }
}

fn entry_to_lua(lua: &Lua, e: &Entry) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("Key", e.key.as_str())?;
    t.set("Source", e.source.as_str())?;
    t.set("Context", e.context.as_str())?;
    t.set("Example", e.example.as_str())?;
    let v = lua.create_table()?;
    for (k, x) in &e.values {
        v.set(k.as_str(), x.as_str())?;
    }
    t.set("Values", v)?;
    Ok(t)
}

fn entries_from_lua(v: &Value) -> mlua::Result<Vec<Entry>> {
    let Value::Table(t) = v else {
        return Err(rt_err("entries was not an array."));
    };
    let mut out = Vec::new();
    for i in 1..=t.raw_len() {
        let row: Value = t.raw_get(i)?;
        let Value::Table(row) = row else {
            return Err(rt_err(format!("Entry at index {i} was not a table.")));
        };
        let field = |name: &str| -> mlua::Result<String> {
            match row.raw_get::<Value>(name)? {
                Value::String(s) => Ok(s.to_str()?.to_string()),
                Value::Nil if matches!(name, "Example" | "Context" | "Key" | "Source") => Ok(String::new()),
                _ => Err(rt_err(format!(
                    "Entry at index {i} had invalid Key/Source/Context: Entry at index {i} \"{name}\" was not a string."
                ))),
            }
        };
        let mut e = Entry {
            key: field("Key")?,
            source: field("Source")?,
            context: field("Context")?,
            example: field("Example")?,
            values: BTreeMap::new(),
        };
        if let Value::Table(vals) = row.raw_get::<Value>("Values")? {
            for p in vals.pairs::<String, Value>() {
                let (k, x) = p?;
                if let Value::String(s) = x {
                    e.values.insert(k, s.to_str()?.to_string());
                }
            }
        }
        out.push(e);
    }
    Ok(out)
}

/// The tables a Translator reads: its own, or every LocalizationTable
/// under LocalizationService.
fn tables_for(lua: &Lua, t: &Translator) -> mlua::Result<Vec<Ref>> {
    if let Some(r) = t.table {
        return Ok(vec![r]);
    }
    let w = world(lua)?;
    let d = w.d();
    let Some(svc) = d.find_child(d.dom.root_ref(), "LocalizationService") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut stack = vec![svc];
    while let Some(r) = stack.pop() {
        if let Some(i) = d.dom.get_by_ref(r) {
            if i.class == "LocalizationTable" {
                out.push(r);
            }
            stack.extend(i.children().iter().rev().copied());
        }
    }
    Ok(out)
}

/// The value for `locale`: exact, then its language ("es-mx" → "es"),
/// then any locale of that language.
fn value_for<'a>(e: &'a Entry, locale: &str) -> Option<&'a str> {
    let l = locale.to_ascii_lowercase();
    if let Some(v) = e.values.iter().find(|(k, _)| k.to_ascii_lowercase() == l).map(|(_, v)| v.as_str()) {
        return Some(v);
    }
    let lang = l.split('-').next().unwrap_or("");
    e.values
        .iter()
        .find(|(k, _)| k.to_ascii_lowercase() == lang)
        .or_else(|| e.values.iter().find(|(k, _)| k.to_ascii_lowercase().split('-').next() == Some(lang)))
        .map(|(_, v)| v.as_str())
}

/// Fills `{name}` / `{1}` tokens (`{1:fmt}` too: the format is ignored).
pub fn format(pattern: &str, args: &Value) -> Result<String, String> {
    let has_token = pattern.contains('{');
    let table = match args {
        Value::Table(t) => Some(t),
        _ => None,
    };
    if !has_token {
        return Ok(pattern.to_string());
    }
    let Some(t) = table else {
        return Err("Found parameter token, but had no parameters".into());
    };
    let len = t.raw_len();
    let named = t
        .clone()
        .pairs::<Value, Value>()
        .flatten()
        .any(|(k, _)| matches!(k, Value::String(_)));
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(a) = rest.find('{') {
        out.push_str(&rest[..a]);
        let Some(b) = rest[a..].find('}') else {
            out.push_str(&rest[a..]);
            rest = "";
            break;
        };
        let token = &rest[a + 1..a + b];
        let name = token.split(':').next().unwrap_or("").trim();
        let value: Value = if let Ok(n) = name.parse::<usize>() {
            if named {
                return Err("Expected array table, but table contained named arguments.".into());
            }
            if n == 0 || n > len {
                return Err(format!("Parameter index \"{}\" was out of range of {}", n.saturating_sub(1), if len == 0 { "empty array".to_string() } else { format!("array of size {len}") }));
            }
            t.raw_get(n).unwrap_or(Value::Nil)
        } else {
            if !named {
                return Err(format!("Parameter index \"0\" was out of range of empty array"));
            }
            t.raw_get(name).unwrap_or(Value::Nil)
        };
        let text = match &value {
            Value::String(s) => s.to_string_lossy().to_string(),
            Value::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", *n as i64) } else { n.to_string() }
            }
            Value::Integer(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => return Err(format!("Missing parameter \"{name}\"")),
            other => format!("{other:?}"),
        };
        out.push_str(&text);
        rest = &rest[a + b + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// An instance's path as localization contexts write it ("game.Workspace.X").
fn context_path(lua: &Lua, r: Ref) -> mlua::Result<String> {
    let w = world(lua)?;
    let d = w.d();
    let mut parts = Vec::new();
    let mut cur = r;
    let root = d.dom.root_ref();
    while cur != root {
        let Some(i) = d.dom.get_by_ref(cur) else { break };
        parts.push(i.name.clone());
        cur = i.parent();
        if cur.is_none() {
            break;
        }
    }
    parts.push("game".into());
    parts.reverse();
    Ok(parts.join("."))
}

pub fn new_translator(lua: &Lua, table: Option<Ref>, locale: &str) -> mlua::Result<Value> {
    let r = crate::runtime::ext::create_any(lua, "Translator")?;
    // Studio's Translators are named "Instance" and unparented.
    if let Some(i) = world(lua)?.dm().dom.get_by_ref_mut(r) {
        i.name = "Instance".into();
    }
    state(lua)?.translators.insert(r, Translator { table, locale: locale.to_string() });
    inst_value(lua, r)
}

pub const METHODS: &[(&str, &str)] = &[
    ("GetContents", "LocalizationTable"),
    ("SetContents", "LocalizationTable"),
    ("GetEntries", "LocalizationTable"),
    ("SetEntries", "LocalizationTable"),
    ("GetString", "LocalizationTable"),
    ("GetTranslator", "LocalizationTable"),
    ("SetEntry", "LocalizationTable"),
    ("SetEntryValue", "LocalizationTable"),
    ("SetEntryKey", "LocalizationTable"),
    ("SetEntrySource", "LocalizationTable"),
    ("SetEntryContext", "LocalizationTable"),
    ("SetEntryExample", "LocalizationTable"),
    ("RemoveEntry", "LocalizationTable"),
    ("RemoveEntryValue", "LocalizationTable"),
    ("RemoveKey", "LocalizationTable"),
    ("RemoveTargetLocale", "LocalizationTable"),
    ("FormatByKey", "Translator"),
    ("Translate", "Translator"),
    ("RobloxOnlyTranslate", "Translator"),
    ("GetTableEntries", "LocalizationService"),
    ("GetCorescriptLocalizations", "LocalizationService"),
];

pub fn call(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if !METHODS.iter().any(|(m, _)| *m == name) {
        return None;
    }
    Some(call_inner(lua, name, this, args))
}

fn one(v: Value) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_vec(vec![v]))
}

fn call_inner(lua: &Lua, name: &str, this: Ref, args: &MultiValue) -> mlua::Result<MultiValue> {
    let edit = |f: &mut dyn FnMut(&mut Vec<Entry>) -> mlua::Result<()>| -> mlua::Result<MultiValue> {
        let mut e = entries_of(lua, this)?;
        f(&mut e)?;
        save(lua, this, &e)?;
        Ok(MultiValue::new())
    };
    match name {
        "GetContents" => {
            let e = entries_of(lua, this)?;
            one(Value::String(lua.create_string(serialize(&e))?))
        }
        "SetContents" => {
            let text = s_arg(args, 1)?;
            let e = parse(&text).map_err(rt_err)?;
            save(lua, this, &e)?;
            Ok(MultiValue::new())
        }
        "GetEntries" => {
            let mut e = entries_of(lua, this)?;
            sort(&mut e);
            let t = lua.create_table()?;
            for x in &e {
                t.raw_push(entry_to_lua(lua, x)?)?;
            }
            one(Value::Table(t))
        }
        "SetEntries" => {
            let e = entries_from_lua(&arg(args, 1))?;
            save(lua, this, &e)?;
            Ok(MultiValue::new())
        }
        "GetString" => {
            let locale = s_arg(args, 1)?;
            let key = s_arg(args, 2)?;
            ensure_cached(lua, this)?;
            let st = state(lua)?;
            let found = st.cache.get(&this).and_then(|c| {
                c.by_key.get(&key).and_then(|idxs| idxs.first().map(|&i| &c.entries[i]))
            });
            let Some(x) = found else {
                return Err(rt_err(format!("LocalizationTable does not contain key \"{key}\"")));
            };
            let val = x.values.iter().find(|(k, _)| k.eq_ignore_ascii_case(&locale)).map(|(_, v)| v.clone());
            drop(st);
            match val {
                Some(v) => one(Value::String(lua.create_string(v)?)),
                None => Err(rt_err(format!(
                    "LocalizationTable entry with key \"{key}\" does not contain locale \"{locale}\""
                ))),
            }
        }
        "GetTranslator" => {
            let locale = s_arg(args, 1)?;
            one(new_translator(lua, Some(this), &locale)?)
        }
        "SetEntry" => {
            let (key, source, context) = (s_arg(args, 1)?, s_arg(args, 2)?, s_arg(args, 3)?);
            let mut values = BTreeMap::new();
            if let Value::Table(t) = arg(args, 4) {
                for p in t.pairs::<String, Value>() {
                    let (k, v) = p?;
                    if let Value::String(s) = v {
                        values.insert(k, s.to_str()?.to_string());
                    }
                }
            }
            edit(&mut |e| {
                match find(e, &key, &source, &context) {
                    Some(x) => {
                        x.source = source.clone();
                        x.context = context.clone();
                        x.values = values.clone();
                    }
                    None => e.push(Entry { key: key.clone(), source: source.clone(), context: context.clone(), values: values.clone(), ..Default::default() }),
                }
                Ok(())
            })
        }
        "SetEntryValue" => {
            let (key, source, context, locale, text) =
                (s_arg(args, 1)?, s_arg(args, 2)?, s_arg(args, 3)?, s_arg(args, 4)?, s_arg(args, 5)?);
            edit(&mut |e| {
                match find(e, &key, &source, &context) {
                    Some(x) => {
                        x.values.insert(locale.clone(), text.clone());
                    }
                    None => {
                        let mut values = BTreeMap::new();
                        values.insert(locale.clone(), text.clone());
                        e.push(Entry { key: key.clone(), source: source.clone(), context: context.clone(), values, ..Default::default() });
                    }
                }
                Ok(())
            })
        }
        "SetEntryKey" | "SetEntrySource" | "SetEntryContext" | "SetEntryExample" => {
            let (key, source, context, new) = (s_arg(args, 1)?, s_arg(args, 2)?, s_arg(args, 3)?, s_arg(args, 4)?);
            edit(&mut |e| {
                if let Some(x) = find(e, &key, &source, &context) {
                    match name {
                        "SetEntryKey" => x.key = new.clone(),
                        "SetEntrySource" => x.source = new.clone(),
                        "SetEntryContext" => x.context = new.clone(),
                        _ => x.example = new.clone(),
                    }
                }
                Ok(())
            })
        }
        "RemoveEntry" => {
            let (key, source, context) = (s_arg(args, 1)?, s_arg(args, 2)?, s_arg(args, 3)?);
            edit(&mut |e| {
                let before = e.clone();
                e.clear();
                let mut gone = false;
                for x in before {
                    let same = if !key.is_empty() { x.key == key } else { x.key.is_empty() && x.source == source && x.context == context };
                    if same && !gone {
                        gone = true;
                    } else {
                        e.push(x);
                    }
                }
                Ok(())
            })
        }
        "RemoveEntryValue" => {
            let (key, source, context, locale) = (s_arg(args, 1)?, s_arg(args, 2)?, s_arg(args, 3)?, s_arg(args, 4)?);
            edit(&mut |e| {
                if let Some(x) = find(e, &key, &source, &context) {
                    x.values.remove(&locale);
                }
                Ok(())
            })
        }
        "RemoveKey" => {
            let key = s_arg(args, 1)?;
            edit(&mut |e| {
                e.retain(|x| x.key != key);
                Ok(())
            })
        }
        "RemoveTargetLocale" => {
            let locale = s_arg(args, 1)?;
            edit(&mut |e| {
                for x in e.iter_mut() {
                    x.values.remove(&locale);
                }
                Ok(())
            })
        }
        "FormatByKey" => {
            let key = s_arg(args, 1)?;
            let t = state(lua)?.translators.get(&this).cloned().unwrap_or(Translator { table: None, locale: "en-us".into() });
            for r in tables_for(lua, &t)? {
                let source_locale = match world(lua)?.d().prop(r, "SourceLocaleId") {
                    Some(Variant::String(s)) if !s.is_empty() => s,
                    _ => "en-us".into(),
                };
                ensure_cached(lua, r)?;
                let matched: Option<String> = {
                    let st = state(lua)?;
                    st.cache.get(&r).and_then(|c| {
                        c.by_key.get(&key).and_then(|idxs| {
                            idxs.iter().find_map(|&i| {
                                let e = &c.entries[i];
                                value_for(e, &t.locale)
                                    .or_else(|| {
                                        (source_locale.split('-').next()
                                            == t.locale.to_ascii_lowercase().split('-').next())
                                        .then_some(e.source.as_str())
                                    })
                                    .map(|s| s.to_string())
                            })
                        })
                    })
                };
                if let Some(p) = matched {
                    let s = format(&p, &arg(args, 2)).map_err(rt_err)?;
                    return one(Value::String(lua.create_string(s)?));
                }
            }
            Err(rt_err(format!("Key {key} not found for locale {}", t.locale)))
        }
        "Translate" | "RobloxOnlyTranslate" => {
            let ctx = opt_inst(&arg(args, 1));
            let text = s_arg(args, 2)?;
            let t = state(lua)?.translators.get(&this).cloned().unwrap_or(Translator { table: None, locale: "en-us".into() });
            let path = match ctx {
                Some(r) => context_path(lua, r)?,
                None => String::new(),
            };
            let mut best: Option<(u8, String)> = None;
            for r in tables_for(lua, &t)? {
                ensure_cached(lua, r)?;
                let st = state(lua)?;
                if let Some(c) = st.cache.get(&r) {
                    if let Some(idxs) = c.by_source.get(&text) {
                        for &i in idxs {
                            let e = &c.entries[i];
                            let Some(v) = value_for(e, &t.locale) else { continue };
                            // Exact context, then an entry under the instance's
                            // path, then one without context (Studio picked the
                            // "game.Workspace.Btn" entry for Translate(workspace)).
                            let rank = if !e.context.is_empty() && e.context == path {
                                3
                            } else if !e.context.is_empty() && !path.is_empty() && e.context.starts_with(&path) {
                                2
                            } else if e.context.is_empty() {
                                1
                            } else {
                                0
                            };
                            if rank > 0 && best.as_ref().is_none_or(|(b, _)| rank > *b) {
                                best = Some((rank, v.to_string()));
                            }
                        }
                    }
                }
            }
            let out = best.map(|(_, v)| v).unwrap_or(text);
            one(Value::String(lua.create_string(out)?))
        }
        "GetTableEntries" => {
            let t = Translator { table: None, locale: String::new() };
            let out = lua.create_table()?;
            for r in tables_for(lua, &t)? {
                let mut e = entries_of(lua, r)?;
                sort(&mut e);
                for x in &e {
                    out.raw_push(entry_to_lua(lua, x)?)?;
                }
            }
            one(Value::Table(out))
        }
        // Roblox's own CoreScript tables under CoreGui (core_scripts.rs).
        "GetCorescriptLocalizations" => {
            let refs: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                let root = d.dom.root_ref();
                d.find_child(root, "CoreGui")
                    .and_then(|cg| d.dom.get_by_ref(cg))
                    .map(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .filter(|c| d.class_of(*c).is_some_and(|k| k == "LocalizationTable"))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let out = lua.create_table()?;
            for r in refs {
                out.raw_push(inst_value(lua, r)?)?;
            }
            one(Value::Table(out))
        }
        _ => Ok(MultiValue::new()),
    }
}

pub fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    match (class, key) {
        ("Translator", "LocaleId") => {
            let l = state(lua)?.translators.get(&r).map(|t| t.locale.clone()).unwrap_or_else(|| "en-us".into());
            Ok(Some(Value::String(lua.create_string(l)?)))
        }
        // Studio's session locale (English Studio).
        ("LocalizationService", "RobloxLocaleId" | "SystemLocaleId") => {
            Ok(Some(Value::String(lua.create_string("en-us")?)))
        }
        ("LocalizationTable", "Root") => {
            let v = match world(lua)?.d().prop(r, "Root") {
                Some(Variant::Ref(x)) if !x.is_none() => Some(x),
                _ => None,
            };
            Ok(Some(opt_ref_value(lua, v)?))
        }
        _ => Ok(None),
    }
}

pub fn newindex(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value) -> mlua::Result<bool> {
    if class == "LocalizationTable" && key == "Root" {
        let v = match value {
            Value::Nil => Ref::none(),
            v => opt_inst(v).ok_or_else(|| rt_err("Unable to assign property Root. Object expected"))?,
        };
        store_prop(lua, r, "Root", Variant::Ref(v))?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_round_trip_in_studio_order() {
        let e = parse(r#"[{"source":"Play","values":{"es":"Juega"}},{"key":"g","context":"c","source":"Hi","values":{"es":"Hola"}}]"#).unwrap();
        assert_eq!(
            serialize(&e),
            r#"[{"key":"g","context":"c","source":"Hi","values":{"es":"Hola"}},{"source":"Play","values":{"es":"Juega"}}]"#
        );
        assert_eq!(parse("xx").unwrap_err(), "JSON parse error: Invalid value.");
    }

    #[test]
    fn format_matches_studio() {
        let lua = Lua::new();
        let named = lua.create_table().unwrap();
        named.set("name", "Bob").unwrap();
        assert_eq!(format("Hola {name}", &Value::Table(named.clone())).unwrap(), "Hola Bob");
        let pos = lua.create_table().unwrap();
        pos.raw_push("Al").unwrap();
        assert_eq!(format("Salut {1}", &Value::Table(pos)).unwrap(), "Salut Al");
        assert_eq!(format("Salut {1}", &Value::Table(named)).unwrap_err(), "Expected array table, but table contained named arguments.");
        assert_eq!(format("Hola {name}", &Value::Nil).unwrap_err(), "Found parameter token, but had no parameters");
        let empty = lua.create_table().unwrap();
        assert_eq!(format("Hola {name}", &Value::Table(empty)).unwrap_err(), "Parameter index \"0\" was out of range of empty array");
    }
}
