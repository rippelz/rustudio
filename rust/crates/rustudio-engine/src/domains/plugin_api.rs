//! The engine's side of the plugin API domain. The domain itself lives in
//! studio (`src/plugin_host/api.rs`, `plugin_host::api`): it calls the
//! engine, not the other way round, and studio registers it at start-up
//! through [`crate::runtime::domains::register`]. This file holds the few
//! items the engine names to talk to it: the id type, the permission
//! decisions of an HTTP request, the signed-in user id, and the slot
//! descriptions the engine's call sites reach through the registration.
//!
//! With no domain registered (the future player binary, engine-only tests)
//! every helper here is a sensible no-op: no plugin members, no permission
//! prompts, no plugin API answer.

use std::sync::atomic::{AtomicU64, Ordering};

use mlua::{Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::Ref;

/// A plugin's id in this rustudio session (stable across reloads).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginId(pub u32);

/// The signed-in user's id (0 signed out): the plugin settings folder and
/// GetStudioUserId. Studio sets it when the session check answers; the
/// engine reads it for StudioService.GetUserId.
static USER_ID: AtomicU64 = AtomicU64::new(0);

pub fn set_user_id(id: u64) {
    USER_ID.store(id, Ordering::Relaxed);
}

pub fn user_id() -> u64 {
    USER_ID.load(Ordering::Relaxed)
}

/// What an HTTP request or a script-source write from a plugin thread is
/// allowed to do (the store's prompt decides).
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Allow,
    /// Refused: the error message the request raises.
    Deny(String),
    /// The person hasn't answered yet; the caller waits and asks again.
    Pending,
}

/// The error a Pending decision raises: callers that can wait test it with
/// [`is_pending`] and ask again.
pub const PENDING: &str = "rustudio: plugin permission pending";

pub fn is_pending(e: &mlua::Error) -> bool {
    e.to_string().contains(PENDING)
}

/// A domain registered by the code that owns it (studio's plugin domain),
/// filling the slots the built-in domains fill and the callbacks the
/// engine's own code asks the plugin API for.
pub struct Domain {
    /// The name build.rs's member-claims tables use for it.
    pub name: &'static str,
    pub methods: &'static [(&'static str, &'static str)],
    pub lua_methods: &'static [(&'static str, &'static str)],
    pub events: &'static [(&'static str, &'static str)],
    /// The claim tables build.rs generates for the domain's own sources
    /// (plugin_claims.rs): the words its prop_get / call / newindex code
    /// names, sorted, and the (key, class) literal pairs (sorted by key).
    pub claims: &'static [&'static str],
    pub claim_pairs: &'static [(&'static str, &'static str)],
    pub call_claims: &'static [&'static str],
    pub call_claim_pairs: &'static [(&'static str, &'static str)],
    pub newindex_claims: &'static [&'static str],
    pub newindex_claim_pairs: &'static [(&'static str, &'static str)],
    pub prop_get: fn(&Lua, Ref, &str, &str) -> mlua::Result<Option<Value>>,
    pub newindex: fn(&Lua, Ref, &str, &str, &Value) -> mlua::Result<bool>,
    pub call: fn(&Lua, &str, Ref, &MultiValue) -> Option<mlua::Result<MultiValue>>,
    pub write_acts: fn(&str, &str) -> bool,
    pub install: fn(&Lua) -> mlua::Result<()>,
    pub step: fn(&Lua, f64) -> mlua::Result<()>,
    /// Before a plugin thread writes a script's Source: Ok(false) = go
    /// ahead, Ok(true) = held until the permission prompt is answered.
    pub hold_source_write: fn(&Lua, Ref, &Value) -> mlua::Result<bool>,
    /// A Script inside a loaded plugin also gets the `plugin` global.
    pub script_env: fn(&Lua, Ref, &Table) -> mlua::Result<()>,
    /// printidentity's "Current asset id" (-2 for a local plugin).
    pub running_asset_id: fn(&Lua) -> i64,
    /// Studio's PluginManagerInterface instance.
    pub plugin_manager: fn(&Lua) -> mlua::Result<Value>,
    /// Before a plugin's HttpService request.
    pub check_http: fn(&Lua, &str) -> mlua::Result<()>,
    /// The running plugin's Name, if the running code is a plugin's.
    pub running_plugin: fn(&Lua) -> Option<String>,
    /// The registered HTTP check's answer for (plugin, url), asked again
    /// each frame by a request that waits (None: no check, allowed).
    pub http_decision: fn(&str, &str) -> Option<Decision>,
    /// settings().Studio.Theme: the StudioTheme object of the chosen theme.
    pub theme_get: fn(&Lua) -> mlua::Result<Value>,
    /// settings().Studio.Theme = theme: true when the theme changed.
    pub theme_set: fn(&Lua, &Value) -> mlua::Result<bool>,
    /// A plugin's own Theme write fired ThemeChanged already.
    pub theme_note: fn(&Lua),
    /// The script whose tab is in front (StudioService.ActiveScript).
    pub active_script: fn() -> Option<Ref>,
}

fn no_hold_source_write(_lua: &Lua, _r: Ref, _value: &Value) -> mlua::Result<bool> {
    Ok(false)
}

fn no_script_env(_lua: &Lua, _r: Ref, _env: &Table) -> mlua::Result<()> {
    Ok(())
}

fn no_asset_id(_lua: &Lua) -> i64 {
    -2
}

fn no_plugin_manager(_lua: &Lua) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

fn no_check_http(_lua: &Lua, _url: &str) -> mlua::Result<()> {
    Ok(())
}

fn no_running_plugin(_lua: &Lua) -> Option<String> {
    None
}

fn no_http_decision(_plugin: &str, _url: &str) -> Option<Decision> {
    None
}

fn no_theme_get(_lua: &Lua) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

fn no_theme_set(_lua: &Lua, _value: &Value) -> mlua::Result<bool> {
    Ok(false)
}

fn no_theme_note(_lua: &Lua) {}

fn no_active_script() -> Option<Ref> {
    None
}

/// The first registration or VM creation fixes the domain for the process.
static REGISTERED: std::sync::OnceLock<Option<&'static Domain>> = std::sync::OnceLock::new();

pub fn registered() -> Option<&'static Domain> {
    REGISTERED.get().copied().flatten()
}

pub(crate) fn freeze() {
    REGISTERED.get_or_init(|| None);
}

pub fn register(domain: &'static Domain) {
    match REGISTERED.get_or_init(|| Some(domain)) {
        Some(existing) if std::ptr::eq(*existing, domain) => {}
        Some(_) => panic!("domains::register: a different domain was registered first"),
        None => panic!("domains::register: the first Runtime was created without a plugin domain"),
    }
}

pub fn hold_source_write(lua: &Lua, r: Ref, value: &Value) -> mlua::Result<bool> {
    match registered() {
        Some(d) => (d.hold_source_write)(lua, r, value),
        None => no_hold_source_write(lua, r, value),
    }
}

pub fn script_env(lua: &Lua, r: Ref, env: &Table) -> mlua::Result<()> {
    match registered() {
        Some(d) => (d.script_env)(lua, r, env),
        None => no_script_env(lua, r, env),
    }
}

pub fn running_asset_id(lua: &Lua) -> i64 {
    match registered() {
        Some(d) => (d.running_asset_id)(lua),
        None => no_asset_id(lua),
    }
}

pub fn plugin_manager(lua: &Lua) -> mlua::Result<Value> {
    match registered() {
        Some(d) => (d.plugin_manager)(lua),
        None => no_plugin_manager(lua),
    }
}

pub fn check_http(lua: &Lua, url: &str) -> mlua::Result<()> {
    match registered() {
        Some(d) => (d.check_http)(lua, url),
        None => no_check_http(lua, url),
    }
}

pub fn running_plugin(lua: &Lua) -> Option<String> {
    match registered() {
        Some(d) => (d.running_plugin)(lua),
        None => no_running_plugin(lua),
    }
}

pub fn http_decision(plugin: &str, url: &str) -> Option<Decision> {
    match registered() {
        Some(d) => (d.http_decision)(plugin, url),
        None => no_http_decision(plugin, url),
    }
}

pub fn theme_get(lua: &Lua) -> mlua::Result<Value> {
    match registered() {
        Some(d) => (d.theme_get)(lua),
        None => no_theme_get(lua),
    }
}

pub fn theme_set(lua: &Lua, value: &Value) -> mlua::Result<bool> {
    match registered() {
        Some(d) => (d.theme_set)(lua, value),
        None => no_theme_set(lua, value),
    }
}

pub fn theme_note(lua: &Lua) {
    if let Some(d) = registered() {
        (d.theme_note)(lua);
    } else {
        no_theme_note(lua);
    }
}

pub fn active_script() -> Option<Ref> {
    match registered() {
        Some(d) => (d.active_script)(),
        None => no_active_script(),
    }
}
