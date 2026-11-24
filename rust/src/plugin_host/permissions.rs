//! Hook for plugin permissions (the store prompts): HttpService
//! requests and script-source writes from plugin threads ask the check
//! STORE registers. With none registered everything is allowed, as for a
//! local plugin in Studio (measured in Studio: no prompt, HttpEnabled off).
//!
//! `plugin` is the Plugin's Name (`user_X.lua`, `cloud_123`); `key` is
//! host[:port] for HTTP, as Studio stores it (`localhost:8000`).

use std::sync::RwLock;

use mlua::Lua;

/// What a permission check decides (the engine's type: services.rs reads
/// it for a request that waits).
pub use rustudio_engine::runtime::domains::plugin_api::Decision;

pub type HttpCheck = fn(plugin: &str, url: &str) -> Decision;
pub type InjectionCheck = fn(plugin: &str) -> Decision;

static HTTP: RwLock<Option<HttpCheck>> = RwLock::new(None);
static INJECTION: RwLock<Option<InjectionCheck>> = RwLock::new(None);

pub fn set_http_check(f: HttpCheck) {
    if let Ok(mut h) = HTTP.write() {
        *h = Some(f);
    }
}

pub fn set_injection_check(f: InjectionCheck) {
    if let Ok(mut h) = INJECTION.write() {
        *h = Some(f);
    }
}

/// The running plugin's Name, if the running code is a plugin's.
pub fn running_plugin(lua: &Lua) -> Option<String> {
    if rustudio_engine::runtime::security::current(lua) != rustudio_engine::runtime::security::PLUGIN_IDENT {
        return None;
    }
    let script = rustudio_engine::runtime::running_script(lua)?;
    let root = super::vm::plugin_root_of(lua, script)?;
    lua.app_data_ref::<super::vm::PluginVm>()?.by_root(root).map(|p| p.name.clone())
}

/// The error a Pending decision raises: callers that can wait
/// (HttpService's Luau side) test it with [`is_pending`] and ask again.
/// (The text lives in the engine: services.rs, above the plugin host,
/// tests it.)
pub use rustudio_engine::runtime::domains::plugin_api::{is_pending, PENDING};

fn decide(d: Decision, _what: &str) -> mlua::Result<()> {
    match d {
        Decision::Allow => Ok(()),
        Decision::Deny(m) => Err(mlua::Error::runtime(m)),
        Decision::Pending => Err(mlua::Error::runtime(PENDING)),
    }
}

/// Registers the plugin store's checks (src/plugin_store/permissions.rs:
/// per installed plugin, HTTP per domain and script injection; Studio's
/// prompts) as the host's. Local plugins are never gated there (F49).
/// A run with no one to answer a prompt (--no-ui, and no `--action
/// plugin-prompt:…` scheduled): an unanswered permission is refused for the
/// run instead of waiting forever (Rojo's start-up update check held its
/// toolbar back), and Output says so. Nothing is stored.
static AUTO_DENY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NOTES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn set_auto_deny(on: bool) {
    AUTO_DENY.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Output lines about refused prompts (the host's log takes them).
pub fn take_notes() -> Vec<String> {
    std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

pub fn use_store_checks() {
    use crate::plugin_store::permissions::{self as store, Access};
    fn map(a: Access, deny: String) -> Decision {
        match a {
            Access::Allowed => Decision::Allow,
            Access::Denied => Decision::Deny(deny),
            Access::Pending if AUTO_DENY.load(std::sync::atomic::Ordering::Relaxed) => {
                let what = store::global()
                    .lock()
                    .ok()
                    .and_then(|g| g.prompts.first().map(|p| format!("{} ({:?})", p.plugin_id, p.ask)))
                    .unwrap_or_default();
                let note = format!(
                    "rustudio: a plugin permission prompt nobody can answer in this run (--no-ui): {what} refused for this run (--action plugin-prompt:allow@S answers it)"
                );
                let mut n = NOTES.lock().unwrap_or_else(|e| e.into_inner());
                if !n.contains(&note) {
                    n.push(note);
                }
                Decision::Deny(deny)
            }
            Access::Pending => Decision::Pending,
        }
    }
    set_http_check(|plugin, url| {
        map(
            store::check_http(plugin, plugin, url),
            format!("HttpError: {} is not allowed to reach {}", plugin, store::domain_of(url)),
        )
    });
    set_injection_check(|plugin| {
        map(store::check_script_injection(plugin, plugin), format!("{plugin} is not allowed to inject scripts"))
    });
}

/// Before a plugin's HttpService request (services.rs http_start).
pub fn check_http(lua: &Lua, url: &str) -> mlua::Result<()> {
    let Some(plugin) = running_plugin(lua) else { return Ok(()) };
    let Some(f) = HTTP.read().ok().and_then(|h| *h) else { return Ok(()) };
    decide(f(&plugin, url), "HTTP")
}

/// The registered HTTP check's answer for `plugin` (a plugin's Name) and
/// `url`, asked again each frame by a request that waits (None: no check
/// registered, everything allowed).
pub fn http_decision(plugin: &str, url: &str) -> Option<Decision> {
    let f = HTTP.read().ok().and_then(|h| *h)?;
    Some(f(plugin, url))
}

/// Before a plugin writes a script's Source or makes a script.
pub fn check_script_injection(lua: &Lua) -> mlua::Result<()> {
    let Some(plugin) = running_plugin(lua) else { return Ok(()) };
    let Some(f) = INJECTION.read().ok().and_then(|h| *h) else { return Ok(()) };
    decide(f(&plugin), "script injection")
}

/// A plugin's Source write waiting for its script-injection prompt.
struct HeldWrite {
    plugin: String,
    script: rbx_dom_weak::types::Ref,
    source: String,
}

static HELD: std::sync::Mutex<Vec<HeldWrite>> = std::sync::Mutex::new(Vec::new());

/// Before a plugin's `Source` write (runtime.rs): Ok(false) = go ahead,
/// Ok(true) = held until the person answers the script-injection prompt
/// (the write happens then, or fails with the denial in Output), Err =
/// denied. A property write can't yield, so the wait happens after it.
pub fn hold_source_write(lua: &Lua, script: rbx_dom_weak::types::Ref, value: &mlua::Value) -> mlua::Result<bool> {
    let Some(plugin) = running_plugin(lua) else { return Ok(false) };
    let Some(f) = INJECTION.read().ok().and_then(|h| *h) else { return Ok(false) };
    match f(&plugin) {
        Decision::Allow => Ok(false),
        Decision::Deny(m) => Err(mlua::Error::runtime(m)),
        Decision::Pending => {
            let source = match value {
                mlua::Value::String(s) => s.to_string_lossy(),
                _ => return Ok(false), // (the write's own type error)
            };
            if let Ok(mut h) = HELD.lock() {
                h.retain(|w| !(w.plugin == plugin && w.script == script));
                h.push(HeldWrite { plugin, script, source });
            }
            Ok(true)
        }
    }
}

/// Each edit frame: held writes whose prompt was answered go through (as
/// the plugin, so the normal write path runs) or report the denial.
pub fn retry_source_writes(lua: &Lua) -> bool {
    let Some(f) = INJECTION.read().ok().and_then(|h| *h) else { return false };
    let ready: Vec<(HeldWrite, Decision)> = {
        let Ok(mut h) = HELD.lock() else { return false };
        if h.is_empty() {
            return false;
        }
        let mut ready = Vec::new();
        let mut keep = Vec::new();
        let here = |r| rustudio_engine::runtime::world(lua).is_ok_and(|wd| wd.d().exists(r));
        for w in h.drain(..) {
            // (Another runtime's write waits for that one.)
            if !here(w.script) {
                keep.push(w);
                continue;
            }
            match f(&w.plugin) {
                Decision::Pending => keep.push(w),
                d => ready.push((w, d)),
            }
        }
        *h = keep;
        ready
    };
    let mut any = false;
    for (w, d) in ready {
        match d {
            Decision::Allow => {
                let r = (|| -> mlua::Result<()> {
                    let f = lua.load("local s, v = ... s.Source = v").set_name(format!("={}", w.plugin)).into_function()?;
                    let th = lua.create_thread(f)?;
                    rustudio_engine::runtime::security::set(&th, rustudio_engine::runtime::security::PLUGIN_IDENT);
                    th.resume::<()>((rustudio_engine::runtime::inst_value(lua, w.script)?, w.source))?;
                    Ok(())
                })();
                if let Err(e) = r {
                    if let Ok(mut wd) = rustudio_engine::runtime::world(lua) {
                        wd.output.push((crate::app::Level::Error, rustudio_engine::runtime::describe_error(&e)));
                    }
                }
                any = true;
            }
            Decision::Deny(m) => {
                if let Ok(mut wd) = rustudio_engine::runtime::world(lua) {
                    wd.output.push((crate::app::Level::Error, m));
                }
            }
            Decision::Pending => {}
        }
    }
    any
}
