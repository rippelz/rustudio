//! Plugin permissions, as Studio has them: per plugin, HTTP requests per
//! domain (HttpService) and script injection (setting Script.Source,
//! creating scripts). Studio keeps them on the account
//! (studio-user-settings/plugin-permissions, types HttpService and
//! ScriptInjection) and shows each as a row under the plugin in Manage
//! Plugins; the first time a plugin needs one, Studio asks with a prompt.
//!
//! rustudio keeps them per account in
//! `<root>/<userId>/InstalledPlugins/PluginPermissions.json`, seeded from
//! the account's rows for installed plugins, and edited by the prompts and
//! Manage Plugins. One store per process ([`global`]) so the host's
//! HttpService and script-source paths can ask without a handle (see the
//! store module).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// The answer for one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Access {
    Allowed,
    Denied,
    /// Never asked: the prompt is up (or about to be); ask again later.
    Pending,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PluginPerms {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_injection: Option<bool>,
    /// Domain (lower case, no scheme / path) → allowed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub http: BTreeMap<String, bool>,
}

/// What a prompt asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Http(String),
    ScriptInjection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub plugin_id: String,
    pub plugin_name: String,
    pub ask: Ask,
}

#[derive(Debug, Default)]
pub struct Permissions {
    pub file: Option<PathBuf>,
    pub plugins: BTreeMap<String, PluginPerms>,
    pub prompts: Vec<Prompt>,
}

/// The key Studio stores an HTTP permission under: host and, when given,
/// port, lower case (Manage Plugins lists `127.0.0.1:8791`, `localhost:8000`):
/// `https://Api.Example.com/x` → `api.example.com`, `http://localhost:8000/a` → `localhost:8000`.
pub fn domain_of(url: &str) -> String {
    let s = url.trim();
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map(|(_, h)| h).unwrap_or(s);
    s.to_ascii_lowercase()
}

impl Permissions {
    pub fn load(file: PathBuf) -> Permissions {
        let plugins = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Permissions { file: Some(file), plugins, prompts: Vec::new() }
    }

    fn save(&self) {
        if let Some(f) = &self.file {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(b) = serde_json::to_vec_pretty(&self.plugins) {
                let _ = std::fs::write(f, b);
            }
        }
    }

    pub fn http(&self, plugin_id: &str, domain: &str) -> Option<bool> {
        self.plugins.get(plugin_id).and_then(|p| p.http.get(&domain_of(domain)).copied())
    }

    pub fn script_injection(&self, plugin_id: &str) -> Option<bool> {
        self.plugins.get(plugin_id).and_then(|p| p.script_injection)
    }

    /// Asks for HTTP to a domain: the stored answer, else Pending with a
    /// prompt queued (once).
    pub fn request_http(&mut self, plugin_id: &str, plugin_name: &str, url: &str) -> Access {
        let d = domain_of(url);
        match self.http(plugin_id, &d) {
            Some(true) => Access::Allowed,
            Some(false) => Access::Denied,
            None => {
                self.queue(Prompt { plugin_id: plugin_id.into(), plugin_name: plugin_name.into(), ask: Ask::Http(d) });
                Access::Pending
            }
        }
    }

    pub fn request_script_injection(&mut self, plugin_id: &str, plugin_name: &str) -> Access {
        match self.script_injection(plugin_id) {
            Some(true) => Access::Allowed,
            Some(false) => Access::Denied,
            None => {
                self.queue(Prompt { plugin_id: plugin_id.into(), plugin_name: plugin_name.into(), ask: Ask::ScriptInjection });
                Access::Pending
            }
        }
    }

    fn queue(&mut self, p: Prompt) {
        if !self.prompts.contains(&p) {
            self.prompts.push(p);
        }
    }

    pub fn set_http(&mut self, plugin_id: &str, domain: &str, allowed: bool) {
        self.plugins.entry(plugin_id.into()).or_default().http.insert(domain_of(domain), allowed);
        let d = domain_of(domain);
        self.prompts.retain(|p| !(p.plugin_id == plugin_id && p.ask == Ask::Http(d.clone())));
        self.save();
    }

    pub fn set_script_injection(&mut self, plugin_id: &str, allowed: bool) {
        self.plugins.entry(plugin_id.into()).or_default().script_injection = Some(allowed);
        self.prompts.retain(|p| !(p.plugin_id == plugin_id && p.ask == Ask::ScriptInjection));
        self.save();
    }

    /// Answers a prompt.
    pub fn answer(&mut self, p: &Prompt, allowed: bool) {
        match &p.ask {
            Ask::Http(d) => self.set_http(&p.plugin_id, d, allowed),
            Ask::ScriptInjection => self.set_script_injection(&p.plugin_id, allowed),
        }
    }

    /// Forgets a plugin's permissions (Remove in Manage Plugins).
    pub fn forget(&mut self, plugin_id: &str) {
        if self.plugins.remove(plugin_id).is_some() {
            self.save();
        }
        self.prompts.retain(|p| p.plugin_id != plugin_id);
    }

    /// Seeds from the account's rows (installed plugins): rows rustudio has
    /// no answer for yet are taken.
    pub fn merge_cloud(&mut self, rows: &[rustudio_account::plugins::CloudPermission]) {
        let mut changed = false;
        for r in rows {
            let e = self.plugins.entry(super::cloud_id(r.plugin_id)).or_default();
            match (r.kind.as_str(), &r.domain) {
                ("ScriptInjection", _) if e.script_injection.is_none() => {
                    e.script_injection = Some(r.allowed);
                    changed = true;
                }
                ("HttpService", Some(d)) => {
                    let d = domain_of(d);
                    if !e.http.contains_key(&d) {
                        e.http.insert(d, r.allowed);
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            self.save();
        }
    }
}

/// The process's permission store (empty and unsaved until [`init`]).
pub fn global() -> &'static Mutex<Permissions> {
    static P: OnceLock<Mutex<Permissions>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(Permissions::default()))
}

/// Points the process's store at an account's file.
pub fn init(file: PathBuf) {
    if let Ok(mut g) = global().lock() {
        let prompts = std::mem::take(&mut g.prompts);
        *g = Permissions::load(file);
        g.prompts = prompts;
    }
}

/// For the host's HttpService: may `plugin_id` reach `url`? Pending means a
/// prompt was queued; the host keeps the request waiting and asks again
/// (every frame / resume) until it is Allowed or Denied.
///
/// Local plugins (`user_…`) are never gated: Studio lets them make requests
/// with no prompt; permissions are an installed-plugin
/// (`cloud_…`) thing.
pub fn check_http(plugin_id: &str, plugin_name: &str, url: &str) -> Access {
    if !plugin_id.starts_with("cloud_") {
        return Access::Allowed;
    }
    global().lock().map(|mut g| g.request_http(plugin_id, plugin_name, url)).unwrap_or(Access::Denied)
}

/// For the host's script-source paths: may `plugin_id` inject scripts?
/// Local plugins are not gated (as HTTP, F49).
pub fn check_script_injection(plugin_id: &str, plugin_name: &str) -> Access {
    if !plugin_id.starts_with("cloud_") {
        return Access::Allowed;
    }
    global().lock().map(|mut g| g.request_script_injection(plugin_id, plugin_name)).unwrap_or(Access::Denied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_plugins_are_not_gated() {
        assert_eq!(check_http("user_x.lua", "x", "https://example.com"), Access::Allowed);
        assert_eq!(check_script_injection("user_x.lua", "x"), Access::Allowed);
    }

    #[test]
    fn domains() {
        assert_eq!(domain_of("https://Api.Example.com/x?y"), "api.example.com");
        assert_eq!(domain_of("http://localhost:8000/api"), "localhost:8000");
        assert_eq!(domain_of("127.0.0.1:8791"), "127.0.0.1:8791");
        assert_eq!(domain_of("example.com"), "example.com");
        assert_eq!(domain_of("http://user@host.io/p"), "host.io");
    }

    #[test]
    fn prompts_and_persistence() {
        let d = super::super::tests::tmp("perms");
        let f = d.join("PluginPermissions.json");
        let mut p = Permissions::load(f.clone());
        assert_eq!(p.request_http("cloud_1", "One", "https://a.com/x"), Access::Pending);
        assert_eq!(p.request_http("cloud_1", "One", "https://a.com/y"), Access::Pending);
        assert_eq!(p.prompts.len(), 1, "one prompt per domain");
        let pr = p.prompts[0].clone();
        p.answer(&pr, true);
        assert!(p.prompts.is_empty());
        assert_eq!(p.request_http("cloud_1", "One", "http://A.com"), Access::Allowed);
        assert_eq!(p.request_http("cloud_1", "One", "b.com"), Access::Pending);
        assert_eq!(p.request_script_injection("user_x.lua", "x"), Access::Pending);
        p.set_script_injection("user_x.lua", false);
        let mut q = Permissions::load(f);
        assert_eq!(q.request_http("cloud_1", "One", "a.com"), Access::Allowed);
        assert_eq!(q.request_script_injection("user_x.lua", "x"), Access::Denied);
        q.merge_cloud(&[rustudio_account::plugins::CloudPermission { plugin_id: 1, kind: "ScriptInjection".into(), allowed: true, domain: None }]);
        assert_eq!(q.script_injection("cloud_1"), Some(true));
        q.forget("cloud_1");
        assert_eq!(q.http("cloud_1", "a.com"), None);
    }
}
