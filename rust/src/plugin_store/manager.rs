//! The editor's plugin manager: the store, the runtime it feeds, the
//! folder watcher, and the Roblox jobs (sync, install, update, search,
//! Get, publish) on background threads.
//!
//! Startup (Studio's order, from its log): plugins already on disk are
//! handed to the runtime one per frame from the second frame on, so the
//! first frame never waits on them; when the session is signed in, one
//! background sync reads the account's plugin permissions, its installed
//! list and the plugins' details and versions (batches of 50), installs
//! what is missing, and runs auto-updates. Nothing else touches the
//! network unless the person asks.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};

use rustudio_account::plugins::{self as rp, Failure};
use rustudio_account::session::Session;

use super::permissions;
use super::runtime::{NoRuntime, PluginRuntime};
use super::watcher::{self, Watcher};
use super::{InstalledMeta, PluginKind, PluginSource, Store};
use rustudio_core::assets::SharedClient;

pub struct Synced {
    pub user_id: u64,
    pub rows: Vec<rp::CloudPlugin>,
    pub info: HashMap<u64, rp::PluginInfo>,
    pub details: HashMap<u64, rp::ItemDetails>,
    pub perms: Vec<rp::CloudPermission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Install,
    Update,
    Fetch,
}

pub enum Msg {
    Synced(Result<Box<Synced>, Failure>),
    Downloaded { meta: InstalledMeta, bytes: Vec<u8>, why: Why },
    Failed { id: String, what: &'static str, failure: Failure },
    Search { query: String, result: Result<rp::SearchPage, Failure> },
    Icon { asset_id: u64, png: Vec<u8> },
    Published { result: Result<u64, Failure>, overwrite: Option<u64>, name: String },
    Note(String),
}

/// A line for Output.
pub struct Line {
    pub level: watcher::Level,
    pub text: String,
}

pub struct PluginManager {
    pub store: Store,
    pub runtime: Box<dyn PluginRuntime>,
    pub watcher: Option<Watcher>,
    pub tx: Sender<Msg>,
    rx: Receiver<Msg>,
    started: bool,
    startup: VecDeque<PluginSource>,
    pub synced: Option<Result<(), String>>,
    sync_requested_for: Option<u64>,
    /// Plugin ids with a job running (install / update / fetch).
    pub busy: HashSet<String>,
    /// Last failure per plugin id (shown on its card).
    pub errors: HashMap<String, String>,
    pub search: Option<(String, Result<rp::SearchPage, String>)>,
    pub searching: bool,
    pub icons: HashMap<u64, Vec<u8>>,
    icons_requested: HashSet<u64>,
    pub published: Vec<(String, Result<u64, String>)>,
    pub publishing: bool,
    /// Signed in (as of the last tick).
    pub signed_in: bool,
    last_user_check: Option<std::time::Instant>,
    pub lines: Vec<Line>,
    pub ui: super::ui::ManageUi,
}

impl Default for PluginManager {
    fn default() -> Self {
        PluginManager::new(Store::open_default(), Box::new(NoRuntime::default()))
    }
}

/// The session for a background job (a copy that doesn't hold the
/// account's lock) and the account id.
pub fn session_of(client: &SharedClient) -> Option<(Session, u64)> {
    let g = client.lock().ok()?;
    let c = g.as_ref()?;
    Some((c.session.downloader(), c.user_id()))
}

impl PluginManager {
    pub fn new(store: Store, runtime: Box<dyn PluginRuntime>) -> PluginManager {
        permissions::init(store.installed_dir().join("PluginPermissions.json"));
        let (tx, rx) = channel();
        PluginManager {
            store,
            runtime,
            watcher: None,
            tx,
            rx,
            started: false,
            startup: VecDeque::new(),
            synced: None,
            sync_requested_for: None,
            busy: HashSet::new(),
            errors: HashMap::new(),
            search: None,
            searching: false,
            icons: HashMap::new(),
            icons_requested: HashSet::new(),
            published: Vec::new(),
            publishing: false,
            signed_in: false,
            last_user_check: None,
            lines: Vec::new(),
            ui: Default::default(),
        }
    }

    fn info(&mut self, t: impl Into<String>) {
        self.lines.push(Line { level: watcher::Level::Info, text: t.into() });
    }
    fn error(&mut self, t: impl Into<String>) {
        self.lines.push(Line { level: watcher::Level::Error, text: t.into() });
    }

    /// Plugins load in this process (the runtime's switch).
    pub fn loading_on(&self) -> bool {
        // RUSTUDIO_NO_INSTALLED_PLUGINS=1 (checks): the account's installed
        // plugins and the Plugins folder stay out; --plugin / --plugins-dir
        // still load, the session still serves assets.
        self.runtime.enabled() && std::env::var_os("RUSTUDIO_NO_INSTALLED_PLUGINS").is_none_or(|v| v.is_empty() || v == "0")
    }

    /// Queues every enabled plugin on disk for loading (start, place open).
    pub fn queue_startup(&mut self) {
        if !self.loading_on() {
            return;
        }
        self.startup = self.store.enabled().into_iter().collect();
    }

    /// A place was opened: plugins run again in the new place (Studio
    /// unloads them with the old DataModel and loads them for the new one).
    pub fn on_place_opened(&mut self) {
        if self.runtime.follows_documents() {
            return;
        }
        for id in self.runtime.loaded() {
            self.runtime.unload(&id);
        }
        self.queue_startup();
    }

    /// Switches to another account's installed plugins (sign in / out).
    fn switch_user(&mut self, user_id: u64) {
        if user_id == self.store.user_id {
            return;
        }
        // RUSTUDIO_PLUGIN_USER names the account whose installed plugins
        // load: signing in doesn't swap them for the session's.
        if std::env::var("RUSTUDIO_PLUGIN_USER").ok().and_then(|v| v.parse::<u64>().ok()).is_some() {
            return;
        }
        for p in self.store.list().into_iter().filter(|p| p.kind == PluginKind::Installed) {
            if self.runtime.loaded().contains(&p.id) {
                self.runtime.unload(&p.id);
            }
        }
        self.store = Store::open(self.store.root.clone(), self.store.plugins_dir.clone(), user_id);
        permissions::init(self.store.installed_dir().join("PluginPermissions.json"));
        self.synced = None;
        if self.loading_on() {
            for p in self.store.enabled().into_iter().filter(|p| p.kind == PluginKind::Installed) {
                self.startup.push_back(p);
            }
        }
    }

    /// Once a frame.
    pub fn tick(&mut self, client: Option<SharedClient>, frame_no: u64) {
        if !self.started {
            self.started = true;
            self.queue_startup();
            if self.loading_on() {
                self.watcher = Some(Watcher::new(&self.store.plugins_dir));
            }
        }
        // One plugin a frame, from the second frame.
        if frame_no >= 2 {
            if let Some(p) = self.startup.pop_front() {
                if !self.runtime.loaded().contains(&p.id) {
                    if let Err(e) = self.runtime.load(&p) {
                        self.error(format!("Plugin {} failed to load: {e}", p.id));
                    }
                }
            }
        }
        if let Some(w) = self.watcher.as_mut() {
            let changes = w.tick();
            if !changes.is_empty() {
                let lines = watcher::apply(&self.store, self.runtime.as_mut(), &changes, watcher::reload_on_change_setting());
                for (level, text) in lines {
                    self.lines.push(Line { level, text });
                }
            }
        }
        // The account: check who is signed in about once a second.
        let check = self.last_user_check.is_none_or(|t| t.elapsed().as_millis() > 1000);
        if check {
            self.last_user_check = Some(std::time::Instant::now());
            // Account workers can be using the session during a network request.
            // Keep the last sign-in state until the next tick if the lock is busy.
            let who = match client.as_ref() {
                None => Some(None),
                Some(client) => client.try_lock().ok().map(|g| g.as_ref().map(|c| c.user_id())),
            };
            if let Some(current) = who {
                self.signed_in = current.is_some();
            }
            if let Some(Some(uid)) = who {
                self.switch_user(uid);
                if self.sync_requested_for != Some(uid) && self.loading_on() {
                    self.sync_requested_for = Some(uid);
                    if let Some(c) = client.clone() {
                        self.sync(c);
                    }
                }
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m, client.clone());
        }
    }

    fn spawn(&self, f: impl FnOnce(Sender<Msg>) + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::Builder::new().name("plugins".into()).spawn(move || f(tx)).ok();
    }

    /// Reads the account's plugins from Roblox (background).
    pub fn sync(&mut self, client: SharedClient) {
        self.synced = None;
        self.spawn(move |tx| {
            let r = (|| {
                let (s, user_id) = session_of(&client).ok_or(Failure::SignedOut)?;
                let perms = rp::permissions(&s).unwrap_or_default();
                let rows = rp::installed(&s)?;
                let ids: Vec<u64> = rows.iter().map(|r| r.plugin_asset_id).collect();
                let details = rp::item_details(&s, &ids).unwrap_or_default();
                let info = rp::plugin_info(&s, &ids).unwrap_or_default();
                Ok(Box::new(Synced { user_id, rows, info, details, perms }))
            })();
            let _ = tx.send(Msg::Synced(r));
        });
    }

    /// Downloads and installs a plugin (Find Plugins > Install, `--install-plugin`):
    /// takes a free one into the inventory first when the account doesn't
    /// own it.
    pub fn install(&mut self, client: Option<SharedClient>, asset_id: u64) {
        let id = super::cloud_id(asset_id);
        let Some(client) = client else {
            self.errors.insert(id, Failure::SignedOut.describe());
            return;
        };
        if !self.busy.insert(id.clone()) {
            return;
        }
        self.errors.remove(&id);
        self.spawn(move |tx| {
            let r = (|| {
                let (s, uid) = session_of(&client).ok_or(Failure::SignedOut)?;
                fetch_for_install(&s, uid, asset_id, true)
            })();
            let _ = tx.send(match r {
                Ok((meta, bytes)) => Msg::Downloaded { meta, bytes, why: Why::Install },
                Err(failure) => Msg::Failed { id, what: "install", failure },
            });
        });
    }

    /// Updates an installed plugin to its newest version (Update button).
    pub fn update(&mut self, client: Option<SharedClient>, asset_id: u64) {
        self.download_current(client, asset_id, Why::Update);
    }

    fn download_current(&mut self, client: Option<SharedClient>, asset_id: u64, why: Why) {
        let id = super::cloud_id(asset_id);
        let Some(client) = client else {
            self.errors.insert(id, Failure::SignedOut.describe());
            return;
        };
        if !self.busy.insert(id.clone()) {
            return;
        }
        let known = self.store.meta.plugin_metadata.iter().find(|m| m.plugin_asset_id == asset_id).cloned();
        self.spawn(move |tx| {
            let r = (|| {
                let (s, _) = session_of(&client).ok_or(Failure::SignedOut)?;
                let mut meta = known.unwrap_or(InstalledMeta { plugin_asset_id: asset_id, enabled: true, ..Default::default() });
                // Fetch: the version the account installed; Update: the newest.
                let want = if why == Why::Fetch && meta.asset_version_id != 0 {
                    meta.asset_version_id
                } else {
                    let info = rp::plugin_info(&s, &[asset_id])?;
                    let i = info.get(&asset_id).ok_or(Failure::NotFound)?;
                    meta.latest_version = i.version_id;
                    meta.updated = i.updated.clone();
                    i.version_id
                };
                // The newest version comes by asset id (asset delivery's
                // assetVersionId route refuses some current versions with 429).
                let bytes = rp::download(&s, asset_id, if why == Why::Fetch { Some(want) } else { None })?;
                meta.asset_version_id = want;
                Ok((meta, bytes))
            })();
            let _ = tx.send(match r {
                Ok((meta, bytes)) => Msg::Downloaded { meta, bytes, why },
                Err(failure) => Msg::Failed { id, what: if why == Why::Update { "update" } else { "download" }, failure },
            });
        });
    }

    /// Downloads the account's installed plugins that aren't on disk yet,
    /// one after another on one thread (as Studio does; a burst of parallel
    /// requests gets HTTP 429 from asset delivery).
    pub fn fetch_all(&mut self, client: Option<SharedClient>, ids: Vec<u64>) {
        let Some(client) = client else { return };
        let metas: Vec<InstalledMeta> = ids
            .iter()
            .filter(|a| self.busy.insert(super::cloud_id(**a)))
            .map(|a| self.store.meta.plugin_metadata.iter().find(|m| m.plugin_asset_id == *a).cloned().unwrap_or(InstalledMeta { plugin_asset_id: *a, enabled: true, ..Default::default() }))
            .collect();
        self.spawn(move |tx| {
            for mut meta in metas {
                let id = super::cloud_id(meta.plugin_asset_id);
                let r = session_of(&client).ok_or(Failure::SignedOut).and_then(|(s, _)| {
                    let want = meta.asset_version_id;
                    // The account's version; by asset id when that is the newest.
                    let by_id = want == 0 || meta.latest_version == want;
                    let bytes = rp::download(&s, meta.plugin_asset_id, if by_id { None } else { Some(want) })?;
                    if want == 0 {
                        meta.asset_version_id = meta.latest_version;
                    }
                    Ok(bytes)
                });
                let _ = tx.send(match r {
                    Ok(bytes) => Msg::Downloaded { meta, bytes, why: Why::Fetch },
                    Err(failure) => Msg::Failed { id, what: "download", failure },
                });
            }
        });
    }

    /// Updates every installed plugin that has one (Update All).
    pub fn update_all(&mut self, client: Option<SharedClient>) {
        let ids: Vec<u64> = self.store.list().iter().filter(|p| p.has_update()).filter_map(|p| p.asset_id).collect();
        for a in ids {
            self.update(client.clone(), a);
        }
    }

    /// Turns a plugin on or off, live: loads or unloads it now.
    pub fn set_enabled(&mut self, client: Option<SharedClient>, id: &str, on: bool) {
        match self.store.set_enabled(id, on) {
            Ok(p) => {
                let loaded = self.runtime.loaded().contains(&p.id);
                if on && !loaded && self.loading_on() {
                    if p.present {
                        if let Err(e) = self.runtime.load(&p) {
                            self.error(format!("Plugin {} failed to load: {e}", p.id));
                        }
                    } else if let Some(a) = p.asset_id {
                        self.download_current(client, a, Why::Fetch);
                    }
                } else if !on && loaded {
                    self.runtime.unload(&p.id);
                }
            }
            Err(e) => self.error(e),
        }
    }

    /// Removes an installed plugin (… > Remove).
    pub fn remove(&mut self, asset_id: u64) {
        let id = super::cloud_id(asset_id);
        if self.runtime.loaded().contains(&id) {
            self.runtime.unload(&id);
        }
        match self.store.uninstall(asset_id) {
            Ok(()) => {
                if let Ok(mut g) = permissions::global().lock() {
                    g.forget(&id);
                }
                self.info(format!("Removed plugin {id}"));
            }
            Err(e) => self.error(e),
        }
    }

    pub fn search(&mut self, client: Option<SharedClient>, query: String) {
        let Some(client) = client else {
            self.search = Some((query, Err(Failure::SignedOut.describe())));
            return;
        };
        self.searching = true;
        self.spawn(move |tx| {
            let result = session_of(&client).ok_or(Failure::SignedOut).and_then(|(s, _)| rp::search(&s, &query, None));
            let _ = tx.send(Msg::Search { query, result });
        });
    }

    /// Plugin icons (Manage Plugins cards), once per plugin.
    pub fn want_icons(&mut self, client: Option<SharedClient>, ids: &[u64]) {
        let Some(client) = client else { return };
        let ids: Vec<u64> = ids.iter().copied().filter(|i| self.icons_requested.insert(*i)).collect();
        if ids.is_empty() {
            return;
        }
        self.spawn(move |tx| {
            let Some((s, _)) = session_of(&client) else { return };
            for chunk in ids.chunks(50) {
                let list = chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
                let url = format!("https://thumbnails.roblox.com/v1/assets?assetIds={list}&returnPolicy=PlaceHolder&size=150x150&format=Png&isCircular=false");
                let Ok((200, t)) = s.get(&url) else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
                for e in v.get("data").and_then(|d| d.as_array()).into_iter().flatten() {
                    let (Some(id), Some(u)) = (e.get("targetId").and_then(|x| x.as_u64()), e.get("imageUrl").and_then(|x| x.as_str())) else { continue };
                    if let Ok((200, png)) = s.fetch_bytes(u, false) {
                        let _ = tx.send(Msg::Icon { asset_id: id, png });
                    }
                }
            }
        });
    }

    /// Publish as Plugin (background): uploads `rbxm` as a new plugin or a
    /// new version of `req.overwrite`.
    pub fn publish(&mut self, client: Option<SharedClient>, req: rp::PublishRequest, rbxm: Vec<u8>) {
        let Some(client) = client else {
            self.published.push((req.name, Err(Failure::SignedOut.describe())));
            return;
        };
        self.publishing = true;
        self.spawn(move |tx| {
            let result = session_of(&client).ok_or(Failure::SignedOut).and_then(|(s, _)| rp::publish(&s, &req, &rbxm));
            let _ = tx.send(Msg::Published { result, overwrite: req.overwrite, name: req.name });
        });
    }

    fn handle(&mut self, m: Msg, client: Option<SharedClient>) {
        match m {
            Msg::Synced(Ok(s)) => {
                if s.user_id != self.store.user_id {
                    return;
                }
                self.store.merge_cloud(&s.rows);
                self.store.merge_details(&s.info, &s.details);
                let _ = self.store.save();
                if let Ok(mut g) = permissions::global().lock() {
                    g.merge_cloud(&s.perms);
                }
                self.synced = Some(Ok(()));
                // Missing enabled plugins download; auto-update ones update.
                let list = self.store.list();
                let fetch: Vec<u64> = list.iter().filter(|p| p.kind == PluginKind::Installed && p.enabled && !p.present).filter_map(|p| p.asset_id).collect();
                let auto: Vec<u64> = list.iter().filter(|p| p.auto_update && p.present && p.has_update()).filter_map(|p| p.asset_id).collect();
                if !fetch.is_empty() {
                    self.info(format!("Plugins: downloading {} installed plugin(s)", fetch.len()));
                }
                self.fetch_all(client.clone(), fetch);
                if !auto.is_empty() {
                    self.info(format!("Plugins: auto-updating {} plugin(s)", auto.len()));
                }
                for a in auto {
                    self.update(client.clone(), a);
                }
            }
            Msg::Synced(Err(f)) => {
                let d = f.describe();
                self.synced = Some(Err(d.clone()));
                self.error(format!("Plugins: couldn't read your installed plugins: {d}"));
            }
            Msg::Downloaded { meta, bytes, why } => {
                let id = super::cloud_id(meta.plugin_asset_id);
                self.busy.remove(&id);
                let was_loaded = self.runtime.loaded().contains(&id);
                match self.store.install_bytes(meta, &bytes) {
                    Ok(p) => {
                        self.errors.remove(&id);
                        match why {
                            Why::Install => self.info(format!("Installed plugin {} ({id})", p.name)),
                            Why::Update => self.info(format!("Updated plugin {} ({id})", p.name)),
                            Why::Fetch => {}
                        }
                        if p.enabled && self.loading_on() {
                            let r = if was_loaded { self.runtime.reload(&p) } else { self.runtime.load(&p) };
                            if let Err(e) = r {
                                self.error(format!("Plugin {id} failed to load: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        self.errors.insert(id.clone(), e.clone());
                        self.error(format!("Plugin {id}: {e}"));
                    }
                }
            }
            Msg::Failed { id, what, failure } => {
                self.busy.remove(&id);
                let d = failure.describe();
                self.error(format!("Plugin {id} {what} failed: {d}"));
                self.errors.insert(id, d);
            }
            Msg::Search { query, result } => {
                self.searching = false;
                self.search = Some((query, result.map_err(|f| f.describe())));
            }
            Msg::Icon { asset_id, png } => {
                self.icons.insert(asset_id, png);
            }
            Msg::Published { result, overwrite, name } => {
                self.publishing = false;
                match &result {
                    Ok(id) if overwrite.is_some() => self.info(format!("Published a new version of plugin {name} ({id})")),
                    Ok(id) => self.info(format!("Published plugin {name} as asset {id}")),
                    Err(f) => self.error(format!("Publish as Plugin failed: {}", f.describe())),
                }
                self.published.push((name, result.map_err(|f| f.describe())));
            }
            Msg::Note(t) => self.info(t),
        }
    }

    /// Writes a model as a local plugin file and loads it now (Save as
    /// Local Plugin).
    pub fn save_local(&mut self, path: &std::path::Path, bytes: &[u8]) -> Result<PluginSource, String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let id = super::local_id(&self.store.plugins_dir, path);
        let p = self.store.get(&id).ok_or_else(|| format!("{} is not in the Plugins folder", path.display()))?;
        // The watcher would see it as new: tell it it's known.
        if let Some(w) = self.watcher.as_mut() {
            let _ = w.poll();
            let _ = w.poll();
        }
        if p.enabled && self.loading_on() {
            let r = if self.runtime.loaded().contains(&p.id) { self.runtime.reload(&p) } else { self.runtime.load(&p) };
            r.map_err(|e| format!("saved, but it failed to load: {e}"))?;
        }
        self.info(format!("Saved local plugin {} ({})", p.name, path.display()));
        Ok(p)
    }
}

/// Install steps on a background thread: check it's a plugin, Get it when
/// free and not owned, download its current version.
pub fn fetch_for_install(s: &Session, user_id: u64, asset_id: u64, acquire: bool) -> Result<(InstalledMeta, Vec<u8>), Failure> {
    let det = rp::item_details(s, &[asset_id])?;
    let d = det.get(&asset_id).cloned().ok_or(Failure::NotFound)?;
    if d.type_id != rp::PLUGIN_ASSET_TYPE {
        return Err(Failure::NotAPlugin);
    }
    if !rp::owns(s, user_id, asset_id).unwrap_or(false) {
        if acquire && d.is_free == Some(true) {
            rp::acquire_free(s, asset_id)?;
        } else {
            return Err(Failure::NotOwned);
        }
    }
    let info = rp::plugin_info(s, &[asset_id])?;
    let i = info.get(&asset_id).cloned().unwrap_or_default();
    let bytes = rp::download(s, asset_id, None)?;
    let meta = InstalledMeta {
        plugin_asset_id: asset_id,
        asset_version_id: i.version_id,
        latest_version: i.version_id,
        enabled: true,
        plugin_name: if i.name.is_empty() { d.name.clone() } else { i.name.clone() },
        creator_name: d.creator_name.clone(),
        updated: i.updated.clone(),
        description: d.description.clone(),
        local_only: true,
        ..Default::default()
    };
    Ok((meta, bytes))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{tiny_rbxm, tmp};
    use super::*;

    #[test]
    fn frame_keeps_account_state_while_session_is_busy() {
        let root = tmp("busy-session");
        let store = Store::open(root.clone(), root.join("Plugins"), 0);
        let mut manager = PluginManager::new(store, Box::new(NoRuntime::default()));
        manager.signed_in = true;
        let client = SharedClient::default();
        let held = client.clone();
        let (ready, locked) = channel();
        let (done, release) = channel();
        let worker = std::thread::spawn(move || {
            let _guard = held.lock().unwrap();
            ready.send(()).unwrap();
            let _ = release.recv_timeout(std::time::Duration::from_secs(3));
        });
        locked.recv().unwrap();
        manager.tick(Some(client), 1);
        let _ = done.send(());
        worker.join().unwrap();
        assert!(manager.signed_in);
    }

    #[test]
    fn startup_loads_one_per_frame_in_order_and_toggles_live() {
        let root = tmp("mgr");
        let dir = root.join("Plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.lua"), "print(2)").unwrap();
        std::fs::write(dir.join("a.lua"), "print(1)").unwrap();
        std::fs::write(dir.join("off.lua"), "print(0)").unwrap();
        let mut store = Store::open(root.clone(), dir.clone(), 0);
        store.set_enabled("off.lua", false).unwrap();
        store.install_bytes(InstalledMeta { plugin_asset_id: 9, asset_version_id: 90, enabled: true, ..Default::default() }, &tiny_rbxm()).unwrap();
        let mut m = PluginManager::new(store, Box::new(NoRuntime::default()));
        m.tick(None, 1);
        assert!(m.runtime.loaded().is_empty(), "nothing on the first frame");
        for f in 2..10 {
            m.tick(None, f);
        }
        assert_eq!(m.runtime.loaded(), ["user_a.lua", "user_b.lua", "cloud_9"]);
        m.set_enabled(None, "user_b.lua", false);
        assert_eq!(m.runtime.loaded(), ["user_a.lua", "cloud_9"]);
        m.set_enabled(None, "user_off.lua", true);
        assert!(m.runtime.loaded().contains(&"user_off.lua".to_string()));
        m.remove(9);
        assert!(!m.runtime.loaded().contains(&"cloud_9".to_string()));
        // Save as Local Plugin: written and loaded at once.
        let p = m.save_local(&dir.join("Saved.rbxmx"), &tiny_rbxm()).unwrap();
        assert_eq!(p.id, "user_Saved.rbxmx");
        assert!(m.runtime.loaded().contains(&p.id));
        // Place open: everything runs again in the new place.
        m.on_place_opened();
        assert!(m.runtime.loaded().is_empty());
        for f in 10..20 {
            m.tick(None, f);
        }
        // a, off (turned on), Saved; b is off and 9 was removed.
        assert_eq!(m.runtime.loaded(), ["user_a.lua", "user_off.lua", "user_Saved.rbxmx"]);
    }
}
