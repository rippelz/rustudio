//! Studio Settings, as the engine sees them: the `settings()` members the
//! `services` domain answers from Studio's settings store
//! (src/studio_settings.rs) and the one number the runtime reads from it
//! (Studio > ScriptTimeoutLength).
//!
//! The store is studio code, which sits above the engine: it installs this
//! hook once, at start-up (`studio_settings::init`). With nothing installed
//! the engine still runs (an engine-only build, the future rustudio-player
//! binary): those members are left unanswered, as settings() leaves a member
//! it has no answerer for, and the runtime keeps its own default for the
//! script timeout. Values travel as Luau values and "did it change", because
//! the rows, their kinds and their defaults are the dialog's
//! (`studio_settings_schema.rs`), not the engine's.

use std::sync::OnceLock;

use mlua::{Lua, Value};

/// Studio's settings store, as the engine sees it.
pub trait StudioSettingsHost: Sync {
    /// `settings().<class>.<key>` for a member the store has a row for; None
    /// for one it has no row for (the generic path answers that).
    fn lua_get(&self, lua: &Lua, class: &str, key: &str) -> mlua::Result<Option<Value>>;
    /// `settings().<class>.<key> = value`: None for a member without a row,
    /// Some(changed) when the store took the write (changed: the stored value
    /// differs from the old one, so the caller fires Changed as Studio does).
    fn lua_set(&self, class: &str, key: &str, value: &Value) -> mlua::Result<Option<bool>>;
    /// A number from one of the store's pages, `or` when it has none.
    fn num(&self, class: &str, prop: &str, or: f64) -> f64;
}

static HOST: OnceLock<&'static dyn StudioSettingsHost> = OnceLock::new();

/// Installs the store, once, at start-up (`studio_settings::init`).
pub fn install(host: &'static dyn StudioSettingsHost) {
    let _ = HOST.set(host);
}

/// The settings() classes the store has rows for: Studio's own five pages'
/// items (GameSettings, LuaSettings and TaskScheduler are the engine's).
pub fn is_item_class(class: &str) -> bool {
    matches!(class, "Studio" | "DebugSettings" | "NetworkSettings" | "PhysicsSettings" | "RenderSettings")
}

pub fn lua_get(lua: &Lua, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    match HOST.get() {
        Some(host) => host.lua_get(lua, class, key),
        None => Ok(None),
    }
}

pub fn lua_set(class: &str, key: &str, value: &Value) -> mlua::Result<Option<bool>> {
    match HOST.get() {
        Some(host) => host.lua_set(class, key, value),
        None => Ok(None),
    }
}

pub fn num(class: &str, prop: &str, or: f64) -> f64 {
    match HOST.get() {
        Some(host) => host.num(class, prop, or),
        None => or,
    }
}
