//! The Toolbox panel: Studio's Creator Store / Inventory / Recent /
//! Creations browser, native in egui (checks/toolbox/ref/NOTES.md is the
//! reference: Studio 0.747's built-in Toolbox, recorded in a hidden
//! Studio).
//!
//! * `api.rs`: the web calls over the signed-in session, `model.rs` the
//!   defensive parsers, `categories.rs` the tabs and categories;
//! * this file: the panel's state (kept while hidden, saved to
//!   `<config>/rustudio/toolbox.json` across restarts), a small worker
//!   pool so nothing waits on the network on the UI thread, the list cache
//!   (one `List` per request key, paged), and the deep links
//!   `toolbox:search:<category>:<query>` / `toolbox:insert:<assetId>`;
//! * `thumbs.rs`: thumbnails (disk + memory cache, off the UI thread);
//! * `insert.rs` + `glue.rs`: inserting, as Studio does it;
//! * `ui.rs` / `preview.rs`: drawing.

pub mod api;
pub mod categories;
pub mod cli;
pub mod glue;
pub mod insert;
pub mod model;
pub mod preview;
pub mod thumbs;
pub mod ui;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use api::{Failure, Filters, Http, ListReq, Source};
use categories::{Kind, Tab};
use model::{Asset, Page};

/// The Creator Store's page stack (Back pops).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Nav {
    Home,
    /// A top category's page (pills + swimlanes).
    Category(String),
    /// Results: a category key ("models" from Home), the query, and the
    /// audio tile it came from if any.
    Results { cat: String, query: String, tile: Option<String> },
    /// The asset detail page.
    Asset(u64),
}

/// What survives a restart (and the panel being hidden).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Persist {
    pub tab: Tab,
    pub nav: Vec<Nav>,
    /// The search box's text (Creator Store).
    pub query: String,
    pub filters: Filters,
    pub inv_cat: usize,
    pub recent_cat: usize,
    pub creations_cat: usize,
    pub group: Option<u64>,
    pub inv_query: String,
    /// Scroll offset per view key.
    pub scroll: HashMap<String, f32>,
    pub banner_closed: bool,
    /// Assets inserted in rustudio, newest first (Recent; Roblox's own
    /// recent list is account-side and merged in front of these).
    pub recent_local: Vec<(u64, u32)>,
    pub recent_searches: Vec<String>,
}

impl Default for Persist {
    fn default() -> Self {
        Persist {
            tab: Tab::Store,
            nav: vec![Nav::Home],
            query: String::new(),
            filters: Filters::default(),
            inv_cat: 0,
            recent_cat: 0,
            creations_cat: 0,
            group: None,
            inv_query: String::new(),
            scroll: HashMap::new(),
            banner_closed: false,
            recent_local: Vec::new(),
            recent_searches: Vec::new(),
        }
    }
}

/// One list (search results, a swimlane, an inventory page set).
#[derive(Debug, Clone, Default)]
pub struct List {
    pub ids: Vec<u64>,
    pub next: Option<String>,
    /// No more pages.
    pub done: bool,
    pub loading: bool,
    pub error: Option<Failure>,
    pub total: Option<u64>,
    pub facets: Vec<String>,
    pub corrected: Option<String>,
    pub pages: usize,
    pub asked: Option<Instant>,
}

impl List {
    pub fn can_load_more(&self) -> bool {
        !self.done && !self.loading && self.error.is_none()
    }
}

/// Messages from the workers.
pub enum Msg {
    Page { req: ListReq, cursor: Option<String>, result: Result<Page, Failure> },
    /// Autocomplete replies for (category key, text): the current
    /// category's, then each other category's.
    Suggest { key: (String, String), current: Vec<String>, others: Vec<(&'static str, Vec<String>)> },
    Groups(Result<Vec<(u64, String)>, Failure>),
    Details(Vec<u64>, Vec<Asset>),
    Favorites { id: u64, count: Option<u64>, mine: Option<bool> },
    Owned { id: u64, owned: bool },
    Acquired { id: u64, result: Result<bool, Failure> },
    Insert(Result<insert::Ready, (u64, u64, Failure)>),
    PreviewModel { id: u64, result: Result<rbx_dom_weak::WeakDom, Failure> },
    AudioBytes { id: u64, result: Result<Arc<Vec<u8>>, Failure> },
    Sent { what: String, result: Result<(), Failure> },
}

type Task = Box<dyn FnOnce(&dyn Http) -> Option<Msg> + Send>;

/// A fixed pool of worker threads running tasks against the current Http.
pub struct Pool {
    queue: Arc<(Mutex<VecDeque<Task>>, Condvar)>,
    http: Arc<Mutex<Arc<dyn Http>>>,
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    started: bool,
    pub busy: Arc<AtomicUsize>,
}

impl Default for Pool {
    fn default() -> Self {
        let (tx, rx) = channel();
        Pool {
            queue: Arc::new((Mutex::new(VecDeque::new()), Condvar::new())),
            http: Arc::new(Mutex::new(Arc::new(api::AnonHttp))),
            tx,
            rx,
            started: false,
            busy: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Pool {
    const WORKERS: usize = 4;

    pub fn set_http(&self, h: Arc<dyn Http>) {
        if let Ok(mut g) = self.http.lock() {
            *g = h;
        }
    }

    pub fn http(&self) -> Arc<dyn Http> {
        self.http.lock().map(|g| g.clone()).unwrap_or_else(|_| Arc::new(api::AnonHttp))
    }

    pub fn run(&mut self, t: impl FnOnce(&dyn Http) -> Option<Msg> + Send + 'static) {
        if !self.started {
            self.started = true;
            for i in 0..Self::WORKERS {
                let q = self.queue.clone();
                let tx = self.tx.clone();
                let http = self.http.clone();
                let busy = self.busy.clone();
                let _ = std::thread::Builder::new().name(format!("toolbox-{i}")).spawn(move || loop {
                    let task = {
                        let (lock, cv) = &*q;
                        let Ok(mut g) = lock.lock() else { return };
                        loop {
                            if let Some(t) = g.pop_front() {
                                break t;
                            }
                            g = match cv.wait(g) {
                                Ok(g) => g,
                                Err(_) => return,
                            };
                        }
                    };
                    let h = http.lock().map(|g| g.clone()).unwrap_or_else(|_| Arc::new(api::AnonHttp));
                    busy.fetch_add(1, Ordering::SeqCst);
                    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(&*h)));
                    busy.fetch_sub(1, Ordering::SeqCst);
                    if let Ok(Some(m)) = out {
                        if tx.send(m).is_err() {
                            return;
                        }
                    }
                });
            }
        }
        busy_hint(&self.busy);
        let (lock, cv) = &*self.queue;
        if let Ok(mut g) = lock.lock() {
            g.push_back(Box::new(t));
        }
        cv.notify_one();
    }

    pub fn queued(&self) -> usize {
        self.queue.0.lock().map(|g| g.len()).unwrap_or(0)
    }
}

fn busy_hint(_b: &AtomicUsize) {}

/// An insert in progress (the line under the search bar).
#[derive(Debug)]
pub struct InsertJob {
    pub job: u64,
    pub id: u64,
    pub name: String,
    pub stage: Arc<AtomicUsize>,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
}

/// A question the insert asks before it goes on (Studio's dialogs).
pub enum Ask {
    /// "This asset contains N scripts": Insert / Cancel.
    Scripts { ready: insert::Ready, count: usize },
    /// A Tool: Workspace / StarterPack / Cancel.
    ToolHome { ready: insert::Ready },
    /// A plugin the account doesn't own yet: Get (free) / Cancel.
    GetPlugin { asset: Asset },
    /// A paid asset: Studio's Buy dialog; rustudio opens the store page.
    Paid { asset: Asset },
}

/// The audio card / detail player.
#[derive(Default)]
pub struct Player {
    pub audio: Option<rustudio_engine::audio::Audio>,
    pub playing: Option<u64>,
    pub loading: Option<u64>,
    pub bytes: HashMap<u64, Arc<Vec<u8>>>,
    pub paused: bool,
    pub error: Option<(u64, String)>,
}

impl Player {
    pub fn position(&self, id: u64) -> f64 {
        match (&self.audio, self.playing) {
            (Some(a), Some(p)) if p == id => a.position(id),
            _ => 0.0,
        }
    }
    pub fn stop(&mut self) {
        if let (Some(a), Some(p)) = (self.audio.as_mut(), self.playing.take()) {
            a.stop(p);
        }
        self.paused = false;
    }
}

pub struct Toolbox {
    pub st: Persist,
    saved: Option<String>,
    last_save: Option<Instant>,
    pub lists: HashMap<ListReq, List>,
    pub assets: HashMap<u64, Asset>,
    pub pool: Pool,
    pub thumbs: thumbs::Thumbs,
    pub signed_in: bool,
    pub user_id: u64,
    /// Suggestion rows for the (category key, text) they were asked for.
    pub suggest: ((String, String), Vec<Suggestion>),
    suggest_asked: (String, String),
    /// Whether the reply to `suggest_asked` (with the current Http) came.
    pub suggest_replied: bool,
    pub groups: Option<Result<Vec<(u64, String)>, Failure>>,
    pub favorites: HashMap<u64, (Option<u64>, Option<bool>)>,
    pub owned: HashMap<u64, bool>,
    /// Ids details were asked for and Roblox didn't return.
    pub missing: std::collections::HashSet<u64>,
    pub inserts: Vec<InsertJob>,
    next_job: u64,
    pub ask: Option<Ask>,
    /// A card dragged into the 3D view while a plugin holds the mouse
    /// (inserted as it enters: PluginMouse.DragEnter).
    pub drag_in: Option<insert::DragIn>,
    pub player: Player,
    /// Detail pages' 3D previews.
    pub preview: preview::Previews,
    /// Lines for Output (insert results), taken by the editor.
    pub log: Vec<(bool, String)>,
    /// Transient UI state (popups, the filter sheet, hover).
    pub ui: ui::UiState,
    /// Account-changing requests are logged instead of sent
    /// (RUSTUDIO_TOOLBOX_DRY_RUN=1, the default in checks).
    pub dry_run: bool,
    pub sent: Vec<api::Mutation>,
    /// Results of `toolbox:*` deep links for `--result` (the check reads them).
    pub harness: serde_json::Map<String, serde_json::Value>,
    pub frame_ms: Vec<f32>,
    /// Time spent drawing the panel per frame (ms), for the perf report.
    pub ui_ms: Vec<f32>,
}

impl Default for Toolbox {
    fn default() -> Self {
        let st = load_persist();
        Toolbox {
            saved: serde_json::to_string(&st).ok(),
            st,
            last_save: None,
            lists: HashMap::new(),
            assets: HashMap::new(),
            pool: Pool::default(),
            thumbs: thumbs::Thumbs::default(),
            signed_in: false,
            user_id: 0,
            suggest: Default::default(),
            suggest_asked: Default::default(),
            suggest_replied: false,
            groups: None,
            favorites: HashMap::new(),
            owned: HashMap::new(),
            missing: Default::default(),
            inserts: Vec::new(),
            next_job: 1,
            ask: None,
            drag_in: None,
            player: Player::default(),
            preview: preview::Previews::default(),
            log: Vec::new(),
            ui: ui::UiState::default(),
            dry_run: std::env::var_os("RUSTUDIO_TOOLBOX_DRY_RUN").is_some_and(|v| v != "0"),
            sent: Vec::new(),
            harness: serde_json::Map::new(),
            frame_ms: Vec::new(),
            ui_ms: Vec::new(),
        }
    }
}

fn persist_path() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("RUSTUDIO_TOOLBOX_STATE") {
        return p.into();
    }
    rustudio_core::paths::config().join("rustudio").join("toolbox.json")
}

fn load_persist() -> Persist {
    let mut p: Persist = std::fs::read_to_string(persist_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if p.nav.is_empty() {
        p.nav.push(Nav::Home);
    }
    // A detail page isn't reopened at start (Studio opens on the list it showed).
    while matches!(p.nav.last(), Some(Nav::Asset(_))) && p.nav.len() > 1 {
        p.nav.pop();
    }
    p.recent_local.truncate(100);
    p.recent_searches.truncate(10);
    p
}

impl Toolbox {
    /// The current Http (session or anonymous) for workers.
    pub fn set_session(&mut self, s: Option<(rustudio_account::session::Session, u64)>) {
        let now_in = s.is_some();
        let uid = s.as_ref().map(|x| x.1).unwrap_or(0);
        if now_in == self.signed_in && uid == self.user_id {
            return;
        }
        let h: Arc<dyn Http> = match s {
            Some((_, uid)) if std::env::var_os("RUSTUDIO_TOOLBOX_OFFLINE").is_some() => Arc::new(api::OfflineHttp(uid)),
            Some((s, _)) => Arc::new(api::SessionHttp(s)),
            None => Arc::new(api::AnonHttp),
        };
        self.pool.set_http(h.clone());
        self.thumbs.set_http(h);
        self.signed_in = now_in;
        self.user_id = uid;
        // Everything listed was for the other account (or none).
        self.lists.clear();
        self.groups = None;
        self.owned.clear();
        self.reask_suggestions();
    }

    /// The text typed before the session came (or for the other account)
    /// was asked with the old Http (signed out: no autocomplete at all):
    /// ask the same (category, text) again with the new one.
    fn reask_suggestions(&mut self) {
        let (cat, text) = std::mem::take(&mut self.suggest_asked);
        if !text.trim().is_empty() {
            self.want_suggestions(Some(&cat), &text);
        }
    }

    /// Test seam: a fixed Http (fixtures).
    pub fn set_http(&mut self, h: Arc<dyn Http>) {
        self.signed_in = h.signed_in();
        self.user_id = h.user_id();
        self.pool.set_http(h.clone());
        self.thumbs.set_http(h);
        self.lists.clear();
        self.reask_suggestions();
    }

    pub fn list(&self, r: &ListReq) -> Option<&List> {
        self.lists.get(r)
    }

    /// The list for `r`, asking for its first page if it has none.
    pub fn ensure(&mut self, r: &ListReq) -> &List {
        if !self.lists.contains_key(r) {
            self.lists.insert(r.clone(), List::default());
            self.load_more(r);
        }
        &self.lists[r]
    }

    pub fn load_more(&mut self, r: &ListReq) {
        let Some(l) = self.lists.get_mut(r) else { return };
        if !l.can_load_more() {
            return;
        }
        l.loading = true;
        l.asked = Some(Instant::now());
        let cursor = if l.pages == 0 { None } else { l.next.clone() };
        if l.pages > 0 && cursor.is_none() {
            l.loading = false;
            l.done = true;
            return;
        }
        let req = r.clone();
        self.pool.run(move |h| {
            let result = api::fetch_page(h, &req, cursor.as_deref(), &|| false);
            Some(Msg::Page { req, cursor, result })
        });
    }

    /// Retry after an error (keeps the pages loaded so far).
    pub fn retry(&mut self, r: &ListReq) {
        if let Some(l) = self.lists.get_mut(r) {
            l.error = None;
            l.loading = false;
        }
        if !self.lists.contains_key(r) {
            self.ensure(r);
        } else {
            self.load_more(r);
        }
        self.thumbs.retry_failed();
    }

    /// Drop a list so it is asked again (Refresh / after an insert for Recent).
    pub fn forget(&mut self, pred: impl Fn(&ListReq) -> bool) {
        self.lists.retain(|k, _| !pred(k));
    }

    pub fn asset(&self, id: u64) -> Option<&Asset> {
        self.assets.get(&id)
    }

    /// Details of assets not known yet (deep links, detail pages).
    pub fn want_details(&mut self, ids: Vec<u64>) {
        let ids: Vec<u64> = ids.into_iter().filter(|i| !self.assets.get(i).is_some_and(|a| a.detailed)).collect();
        if ids.is_empty() {
            return;
        }
        for &i in &ids {
            self.assets.entry(i).or_insert_with(|| Asset { id: i, ..Default::default() });
        }
        self.pool.run(move |h| {
            let found = api::fetch_details(h, &ids, &|| false).unwrap_or_default();
            Some(Msg::Details(ids, found))
        });
    }

    /// The search box's suggestions for `text` typed in category `cat`
    /// (None: Home). Asked again only when the category or the text changes;
    /// a reply for anything but the latest (category, text) is dropped.
    pub fn want_suggestions(&mut self, cat: Option<&str>, text: &str) {
        let cat = cat.unwrap_or("models").to_string();
        let key = (cat.clone(), text.to_string());
        if key == self.suggest_asked {
            return;
        }
        self.suggest_asked = key.clone();
        self.suggest_replied = false;
        if text.trim().is_empty() {
            self.suggest = Default::default();
            self.suggest_replied = true;
            return;
        }
        let kind = categories::find(&cat).map(|c| c.kind).unwrap_or(Kind::Model);
        let others: Vec<(&'static str, Kind)> = SUGGEST_OTHERS.iter().filter(|o| categories::top_of(&cat).key != **o && !(cat == "models" && **o == "3d")).filter_map(|o| Some((*o, categories::find(o)?.kind))).collect();
        let t = text.to_string();
        self.pool.run(move |h| {
            let current = api::fetch_suggestions(h, kind, &t);
            let others = others.into_iter().map(|(k, kind)| (k, api::fetch_suggestions(h, kind, &t))).collect();
            Some(Msg::Suggest { key, current, others })
        });
    }

    /// An autocomplete reply: kept only if it answers the latest
    /// (category, text) asked (a slower reply for older text, or for the
    /// same text in the category left, is dropped).
    fn accept_suggestions(&mut self, key: (String, String), current: &[String], others: &[(&'static str, Vec<String>)]) {
        if key == self.suggest_asked {
            let rows = suggestion_rows(&key.0, &key.1, current, others);
            self.suggest = (key, rows);
            self.suggest_replied = true;
        }
    }

    /// The rows shown for the search box now (empty while the reply for the
    /// current category and text hasn't come).
    pub fn suggestions_for(&self, text: &str) -> &[Suggestion] {
        let cat = self.current_cat().unwrap_or_else(|| "models".into());
        if self.suggest.0 .0 == cat && self.suggest.0 .1 == text {
            &self.suggest.1
        } else {
            &[]
        }
    }

    /// A suggestion chosen: its query searched in its category (the current
    /// one: as Enter; another: that category's results, as a deep link).
    pub fn choose_suggestion(&mut self, s: &Suggestion) {
        self.suggest = Default::default();
        let cur = self.current_cat().unwrap_or_else(|| "models".into());
        if s.cat == cur {
            self.search(&s.query);
            return;
        }
        self.note_search(&s.query);
        self.st.filters.facets.clear();
        let _ = self.deep_search(&s.cat, &s.query);
    }

    pub fn want_groups(&mut self) {
        if self.groups.is_some() {
            return;
        }
        self.groups = Some(Ok(Vec::new()));
        self.pool.run(|h| Some(Msg::Groups(api::fetch_groups(h))));
    }

    pub fn want_favorites(&mut self, id: u64) {
        if self.favorites.contains_key(&id) {
            return;
        }
        self.favorites.insert(id, (None, None));
        self.pool.run(move |h| Some(Msg::Favorites { id, count: api::favorite_count(h, id), mine: api::is_favorited(h, id) }));
    }

    pub fn want_owned(&mut self, id: u64) {
        if self.owned.contains_key(&id) || !self.signed_in {
            return;
        }
        self.owned.insert(id, false);
        self.pool.run(move |h| api::owns(h, id).ok().map(|owned| Msg::Owned { id, owned }));
    }

    /// Account-changing buttons (vote, favorite): built exactly; sent only
    /// when not a dry run.
    pub fn mutate(&mut self, m: api::Mutation, what: &str) {
        self.sent.push(m.clone());
        if self.dry_run {
            self.log.push((false, format!("Toolbox: {what} not sent (dry run): {} {}", m.method, m.url)));
            return;
        }
        let what = what.to_string();
        self.pool.run(move |h| {
            let r = h.send(m.method, &m.url, "application/json", m.body.as_bytes());
            let result = match r {
                Ok((st, _)) if (200..300).contains(&st) => Ok(()),
                Ok((st, t)) => Err(Failure::from_status(st, &t)),
                Err(e) => Err(Failure::Offline(e)),
            };
            Some(Msg::Sent { what, result })
        });
    }

    /// Starts an insert: details + download + decode on a worker.
    pub fn start_insert(&mut self, id: u64, target: insert::Target) -> u64 {
        let job = self.next_job;
        self.next_job += 1;
        let asset = self.assets.get(&id).cloned().unwrap_or(Asset { id, ..Default::default() });
        let stage = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        self.inserts.push(InsertJob { job, id, name: asset.name.clone(), stage: stage.clone(), cancel: cancel.clone(), started: Instant::now() });
        self.pool.run(move |h| {
            let c = cancel.clone();
            let r = insert::prepare(h, job, asset, target, &move || c.load(Ordering::Relaxed), &|s| stage.store(s, Ordering::Relaxed));
            Some(Msg::Insert(r.map_err(|e| (job, id, e))))
        });
        job
    }

    pub fn cancel_insert(&mut self, job: u64) {
        if let Some(j) = self.inserts.iter().find(|j| j.job == job) {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn note_recent(&mut self, id: u64, type_id: u32) {
        self.st.recent_local.retain(|r| r.0 != id);
        self.st.recent_local.insert(0, (id, type_id));
        self.st.recent_local.truncate(100);
        self.forget(|k| k.source == Source::Recent);
    }

    pub fn note_search(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() {
            return;
        }
        self.st.recent_searches.retain(|s| s != q);
        self.st.recent_searches.insert(0, q.to_string());
        self.st.recent_searches.truncate(10);
    }

    /// Worker replies, per frame. Returns ready inserts for the editor.
    pub fn poll(&mut self) -> Vec<Result<insert::Ready, (u64, u64, Failure)>> {
        let mut ready = Vec::new();
        while let Ok(m) = self.pool.rx.try_recv() {
            match m {
                Msg::Page { req, cursor, result } => {
                    let Some(l) = self.lists.get_mut(&req) else { continue };
                    // A reply for a page we no longer wait for (list reset).
                    if cursor.is_some() && l.pages == 0 {
                        continue;
                    }
                    l.loading = false;
                    match result {
                        Ok(p) => {
                            l.pages += 1;
                            for a in p.assets {
                                match self.assets.get_mut(&a.id) {
                                    Some(e) => {
                                        let mut n = a.clone();
                                        n.merge(e);
                                        *e = n;
                                    }
                                    None => {
                                        self.assets.insert(a.id, a);
                                    }
                                }
                            }
                            for id in p.ids {
                                if !l.ids.contains(&id) {
                                    l.ids.push(id);
                                }
                            }
                            l.done = p.next.is_none();
                            l.next = p.next;
                            if l.pages == 1 {
                                l.total = p.total;
                                l.facets = p.facets;
                                l.corrected = p.corrected;
                            }
                        }
                        Err(e) => l.error = Some(e),
                    }
                }
                Msg::Suggest { key, current, others } => self.accept_suggestions(key, &current, &others),
                Msg::Groups(g) => self.groups = Some(g),
                Msg::Details(asked, list) => {
                    // Asked for and not in the reply: removed, private or never existed.
                    for id in asked {
                        if !list.iter().any(|a| a.id == id) {
                            self.missing.insert(id);
                        }
                    }
                    for a in list {
                        let e = self.assets.entry(a.id).or_default();
                        let mut n = a.clone();
                        n.merge(e);
                        *e = n;
                    }
                }
                Msg::Favorites { id, count, mine } => {
                    self.favorites.insert(id, (count, mine));
                }
                Msg::Owned { id, owned } => {
                    self.owned.insert(id, owned);
                }
                Msg::Acquired { id, result } => match result {
                    Ok(_) => {
                        self.owned.insert(id, true);
                    }
                    Err(e) => self.log.push((true, format!("Toolbox: couldn't get asset {id}: {}", e.describe()))),
                },
                Msg::Insert(r) => ready.push(r),
                Msg::PreviewModel { id, result } => self.preview.loaded(id, result),
                Msg::AudioBytes { id, result } => {
                    if self.player.loading == Some(id) {
                        self.player.loading = None;
                    }
                    match result {
                        Ok(b) => {
                            self.player.bytes.insert(id, b);
                            self.play(id);
                        }
                        Err(e) => self.player.error = Some((id, e.describe())),
                    }
                }
                Msg::Sent { what, result } => {
                    if let Err(e) = result {
                        self.log.push((true, format!("Toolbox: {what} failed: {}", e.describe())));
                    }
                }
            }
        }
        ready
    }

    /// Plays (or resumes) an audio asset in the card / detail player.
    pub fn play(&mut self, id: u64) {
        if self.player.playing == Some(id) && self.player.paused {
            if let Some(a) = self.player.audio.as_mut() {
                a.resume(id);
            }
            self.player.paused = false;
            return;
        }
        self.player.stop();
        let Some(b) = self.player.bytes.get(&id).cloned() else {
            if self.player.loading != Some(id) {
                self.player.loading = Some(id);
                self.player.error = None;
                self.pool.run(move |h| {
                    let r = (|| {
                        let a = api::fetch_details(h, &[id], &|| false).ok().and_then(|d| d.into_iter().next());
                        if a.as_ref().is_some_and(|a| a.type_id != 0 && a.type_id != 3) {
                            return Err(Failure::Other("Not an audio asset.".into()));
                        }
                        insert::download(h, id, &|| false).map(Arc::new)
                    })();
                    Some(Msg::AudioBytes { id, result: r })
                });
            }
            return;
        };
        let a = self.player.audio.get_or_insert_with(rustudio_engine::audio::Audio::new);
        match a.play(id, b, 0.5, 1.0, false, 0.0, Default::default()) {
            Ok(()) => {
                self.player.playing = Some(id);
                self.player.paused = false;
            }
            Err(e) => self.player.error = Some((id, e)),
        }
    }

    pub fn pause(&mut self, id: u64) {
        if self.player.playing == Some(id) {
            if let Some(a) = self.player.audio.as_mut() {
                a.pause(id);
            }
            self.player.paused = true;
        }
    }

    /// Saves the persistent state when it changed (at most every 2 s).
    pub fn save_if_changed(&mut self, force: bool) {
        if !force && self.last_save.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
            return;
        }
        self.last_save = Some(Instant::now());
        let Ok(t) = serde_json::to_string(&self.st) else { return };
        if self.saved.as_deref() == Some(t.as_str()) {
            return;
        }
        let p = persist_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, &t).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
            self.saved = Some(t);
        }
    }

    // ------------------------------------------------------------ requests

    /// The list the current Creator Store results page shows.
    pub fn store_req(&self, cat_key: &str, query: &str, tile: Option<&str>) -> ListReq {
        let c = categories::find(cat_key).unwrap_or_else(|| categories::find("models").unwrap());
        let mut r = ListReq {
            source: Source::Store,
            kind: c.kind,
            query: query.trim().to_string(),
            category_path: c.category_path.map(str::to_string),
            sub_type: c.sub_type.map(str::to_string),
            filters: self.st.filters.clone(),
        };
        if c.kind != Kind::Audio {
            r.filters.audio_min_s = None;
            r.filters.audio_max_s = None;
            r.filters.audio_type = None;
            r.filters.artist.clear();
            r.filters.album.clear();
        }
        if let Some(t) = tile.and_then(audio_tile) {
            if r.query.is_empty() {
                r.query = t.keyword.to_string();
            }
            if let Some(a) = t.audio_type {
                r.filters.audio_type = Some(if a == 0 { "Music".into() } else { "SoundEffect".into() });
            }
            if t.sort_new && r.filters.sort == api::Sort::Relevance {
                r.filters.sort = api::Sort::Newest;
            }
        }
        r
    }

    pub fn home_req(section: &str) -> ListReq {
        ListReq { source: Source::Home(section.to_string()), ..ListReq::store(Kind::Model, "") }
    }

    /// The Inventory / Recent / Creations tab's list.
    pub fn tab_req(&self, tab: Tab) -> Option<ListReq> {
        let cats = categories::list_cats(tab);
        let i = match tab {
            Tab::Inventory => self.st.inv_cat,
            Tab::Recent => self.st.recent_cat,
            Tab::Creations => self.st.creations_cat,
            Tab::Store => return None,
        };
        let c = cats.get(i).copied().unwrap_or(cats[0]);
        let group = if c.group { Some(self.st.group?) } else { None };
        let source = match tab {
            Tab::Inventory => Source::Inventory { group },
            Tab::Recent => Source::Recent,
            _ => Source::Creations { group },
        };
        let query = if tab == Tab::Inventory { self.st.inv_query.trim().to_string() } else { String::new() };
        Some(ListReq { source, kind: c.kind, query, category_path: None, sub_type: None, filters: Filters::default() })
    }

    // ------------------------------------------------------------ navigation

    pub fn nav(&self) -> &Nav {
        self.st.nav.last().unwrap_or(&Nav::Home)
    }

    pub fn push(&mut self, n: Nav) {
        if self.st.nav.last() != Some(&n) {
            self.st.nav.push(n);
        }
        self.ui.scroll_to_top = true;
    }

    pub fn back(&mut self) {
        if self.st.nav.len() > 1 {
            self.st.nav.pop();
        }
        if let Some(Nav::Results { query, .. }) = self.st.nav.last() {
            self.st.query = query.clone();
        } else {
            self.st.query.clear();
        }
        self.ui.scroll_to_top = false;
    }

    pub fn home(&mut self) {
        self.st.nav = vec![Nav::Home];
        self.st.query.clear();
    }

    /// The category the store is in (for the search box: searching keeps it).
    pub fn current_cat(&self) -> Option<String> {
        for n in self.st.nav.iter().rev() {
            match n {
                Nav::Results { cat, .. } => return Some(cat.clone()),
                Nav::Category(c) => return Some(c.clone()),
                _ => {}
            }
        }
        None
    }

    /// Enter in the search box: results in the current category (Home: Models).
    pub fn search(&mut self, q: &str) {
        self.note_search(q);
        let cat = self.current_cat().unwrap_or_else(|| "models".into());
        self.st.query = q.to_string();
        self.st.filters.facets.clear();
        match self.st.nav.last() {
            Some(Nav::Results { .. }) => {
                self.st.nav.pop();
            }
            Some(Nav::Asset(_)) => {
                while matches!(self.st.nav.last(), Some(Nav::Asset(_))) {
                    self.st.nav.pop();
                }
                if matches!(self.st.nav.last(), Some(Nav::Results { .. })) {
                    self.st.nav.pop();
                }
            }
            _ => {}
        }
        self.push(Nav::Results { cat, query: q.trim().to_string(), tile: None });
    }

    /// `toolbox:search:<category>:<query>`.
    pub fn deep_search(&mut self, cat: &str, q: &str) -> Result<ListReq, String> {
        let c = categories::find(cat).ok_or_else(|| format!("unknown Toolbox category '{cat}'"))?;
        self.st.tab = Tab::Store;
        self.st.nav = vec![Nav::Home];
        if c.key != "models" {
            self.st.nav.push(Nav::Category(categories::top_of(c.key).key.to_string()));
        }
        self.st.query = q.to_string();
        self.st.nav.push(Nav::Results { cat: c.key.to_string(), query: q.to_string(), tile: None });
        let r = self.store_req(c.key, q, None);
        if self.signed_in {
            self.ensure(&r);
        }
        Ok(r)
    }
}

impl Toolbox {
    /// `--result`'s "toolbox": deep-link results, perf, what was sent.
    pub fn result_json(&self) -> serde_json::Value {
        let stats = |v: &[f32]| {
            let mut s = v.to_vec();
            s.sort_by(f32::total_cmp);
            let p = |q: f32| s.get(((s.len() as f32 - 1.0) * q).round() as usize).copied().unwrap_or(0.0);
            serde_json::json!({"count": s.len(), "mean_ms": if s.is_empty() { 0.0 } else { s.iter().sum::<f32>() / s.len() as f32 }, "p50_ms": p(0.5), "p99_ms": p(0.99), "max_ms": s.last().copied().unwrap_or(0.0)})
        };
        serde_json::json!({
            "signed_in": self.signed_in,
            "harness": serde_json::Value::Object(self.harness.clone()),
            "tick": stats(&self.frame_ms),
            "panel": stats(&self.ui_ms),
            "lists": self.lists.len(),
            "assets": self.assets.len(),
            "sent": self.sent.iter().map(|m| format!("{} {}", m.method, m.url)).collect::<Vec<_>>(),
            "pending_question": self.ask.is_some(),
        })
    }
}

pub fn audio_tile(key: &str) -> Option<&'static categories::AudioTile> {
    categories::AUDIO_SECTIONS.iter().flat_map(|s| s.1.iter()).find(|t| t.key == key)
}

/// One row of the search box's dropdown: "<query>" with "in <label>"
/// under it and the category's icon; choosing it searches `cat`.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub query: String,
    pub cat: String,
    pub label: &'static str,
    pub icon: &'static str,
}

/// The other categories whose suggestions follow the current one's.
/// Studio 0.747 (checks/toolbox/ref/NOTES.md, typing "tre" on Home) ends the
/// list with "build a boat for treasure in Plugins", the plugin
/// autocomplete's first answer for "tre" (fixtures/autocomplete_tre_plugin.json);
/// 3D Assets is the Home search's own. Only these two are recorded.
pub const SUGGEST_OTHERS: [&str; 2] = ["3d", "plugins"];

/// Suggestions shown from the current category: Studio's "tre in 3D Assets,
/// tree in 3D Assets, trees in 3D Assets, tre in Structures …" shows the
/// typed text and then two of the category's suggestions.
pub const SUGGEST_CURRENT: usize = 2;

/// "in <label>" for a category: Home's search (all Models) reads
/// "3D Assets" in Studio; a category its own name.
fn suggest_label(cat: &str) -> (&'static str, &'static str) {
    let top = categories::top_of(cat);
    match categories::find(cat) {
        Some(c) if cat != "models" => (c.label, top.icon),
        _ => (top.label, top.icon),
    }
}

/// The dropdown's rows: the typed text and the current category's first
/// suggestions, then each other category's first suggestion.
pub fn suggestion_rows(cat: &str, text: &str, current: &[String], others: &[(&'static str, Vec<String>)]) -> Vec<Suggestion> {
    let typed = text.trim();
    if typed.is_empty() {
        return Vec::new();
    }
    let (label, icon) = suggest_label(cat);
    let mut rows = vec![Suggestion { query: typed.to_string(), cat: cat.to_string(), label, icon }];
    for q in current.iter().filter(|q| !q.trim().eq_ignore_ascii_case(typed)).take(SUGGEST_CURRENT) {
        rows.push(Suggestion { query: q.trim().to_string(), cat: cat.to_string(), label, icon });
    }
    for (key, qs) in others {
        if let Some(q) = qs.iter().find(|q| !q.trim().is_empty()) {
            let (label, icon) = suggest_label(key);
            rows.push(Suggestion { query: q.trim().to_string(), cat: key.to_string(), label, icon });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::tests::Mock;

    fn wait(tb: &mut Toolbox, r: &ListReq) {
        for _ in 0..200 {
            tb.poll();
            if !tb.lists[r].loading {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("no reply");
    }

    #[test]
    fn paging_and_cache() {
        std::env::set_var("RUSTUDIO_TOOLBOX_STATE", std::env::temp_dir().join("rustudio-toolbox-test.json"));
        let mut tb = Toolbox::default();
        let m = Arc::new(Mock::default().route("/inventory/", "v1_inventory_model.json").raw("items/details", 200, "{\"data\":[]}"));
        tb.set_http(m.clone());
        let r = tb.tab_req(Tab::Inventory).unwrap();
        tb.ensure(&r);
        wait(&mut tb, &r);
        assert_eq!(tb.lists[&r].ids.len(), 30);
        assert!(tb.lists[&r].can_load_more());
        // cached: asking again doesn't fetch
        let calls = m.calls.lock().unwrap().len();
        tb.ensure(&r);
        assert_eq!(m.calls.lock().unwrap().len(), calls);
        // page 2 (same fixture: ids deduplicated, cursor sent)
        tb.load_more(&r);
        wait(&mut tb, &r);
        assert_eq!(tb.lists[&r].pages, 2);
        assert_eq!(tb.lists[&r].ids.len(), 30);
        assert!(m.calls.lock().unwrap().iter().any(|c| c.contains("&cursor=CURSOR2")));
    }

    #[test]
    fn errors_and_retry() {
        let mut tb = Toolbox::default();
        let m = Arc::new(Mock::default().raw("search", 500, "{\"errors\":[{\"code\":0,\"message\":\"InternalServerError\"}]}"));
        tb.set_http(m);
        let r = tb.store_req("models", "tree", None);
        tb.ensure(&r);
        wait(&mut tb, &r);
        assert!(tb.lists[&r].error.is_some());
        tb.set_http(Arc::new(Mock::default().route("search", "v2_search_model_tree.json")));
        tb.ensure(&r);
        wait(&mut tb, &r);
        assert!(tb.lists[&r].error.is_none());
        assert_eq!(tb.lists[&r].ids.len(), 30);
        assert_eq!(tb.asset(580221169).unwrap().name, "Tree");
    }

    fn autocomplete(name: &str) -> Vec<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("checks/toolbox/fixtures").join(name);
        model::parse_autocomplete(&serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
    }

    #[test]
    fn suggestions_across_categories() {
        // Studio, "tre" typed on Home: "tre in 3D Assets, tree in 3D Assets,
        // trees in 3D Assets, ... build a boat for treasure in Plugins".
        let (model, plugin) = (autocomplete("autocomplete_tre.json"), autocomplete("autocomplete_tre_plugin.json"));
        let rows = suggestion_rows("models", "tre", &model, &[("plugins", plugin.clone())]);
        let got: Vec<(String, &str, String)> = rows.iter().map(|r| (r.query.clone(), r.label, r.cat.clone())).collect();
        assert_eq!(
            got,
            vec![
                ("tre".into(), "3D Assets", "models".into()),
                ("tree".into(), "3D Assets", "models".into()),
                ("trees".into(), "3D Assets", "models".into()),
                ("build a boat for treasure".into(), "Plugins", "plugins".into()),
            ]
        );
        assert_eq!(rows[0].icon, "cube");
        assert_eq!(rows[3].icon, "plugin");
        // In a subcategory: its own name; other categories after it.
        let rows = suggestion_rows("3d__vehicles", "tre", &model, &[("plugins", plugin.clone())]);
        assert_eq!(rows[0].label, "Vehicles");
        assert_eq!(rows[0].cat, "3d__vehicles");
        assert_eq!(rows.last().unwrap().label, "Plugins");
        // From Plugins: the plugin ones first, then 3D Assets.
        let rows = suggestion_rows("plugins", "tre", &plugin, &[("3d", model.clone())]);
        assert_eq!(rows.iter().map(|r| r.label).collect::<Vec<_>>(), vec!["Plugins", "Plugins", "Plugins", "3D Assets"]);
        assert_eq!(rows[1].query, "build a boat for treasure");
        // The typed text isn't repeated; blank text: no rows; nothing back: the typed row only.
        assert_eq!(suggestion_rows("models", "tree", &model, &[]).iter().filter(|r| r.query == "tree").count(), 1);
        assert!(suggestion_rows("models", "  ", &model, &[]).is_empty());
        assert_eq!(suggestion_rows("audio", "zzz", &[], &[("plugins", vec![])]).len(), 1);
    }

    #[test]
    fn suggestion_races() {
        std::env::set_var("RUSTUDIO_TOOLBOX_STATE", std::env::temp_dir().join("rustudio-toolbox-test-suggest.json"));
        let mut tb = Toolbox::default();
        tb.st = Persist::default();
        tb.set_http(Arc::new(Mock::default()));
        let (model, plugin) = (autocomplete("autocomplete_tre.json"), autocomplete("autocomplete_tre_plugin.json"));
        // Typed "tr", then "tre": the "tr" reply coming last is dropped.
        tb.want_suggestions(None, "tr");
        tb.want_suggestions(None, "tre");
        tb.accept_suggestions(("models".into(), "tre".into()), &model, &[("plugins", plugin.clone())]);
        tb.accept_suggestions(("models".into(), "tr".into()), &["trap".into()], &[]);
        assert_eq!(tb.suggestions_for("tre")[1].query, "tree");
        assert!(tb.suggestions_for("tr").is_empty());
        // Same text in another category: asked again, and Home's late reply
        // isn't shown under Plugins.
        tb.push(Nav::Category("plugins".into()));
        assert!(tb.suggestions_for("tre").is_empty(), "Home's rows aren't the Plugins page's");
        tb.want_suggestions(Some("plugins"), "tre");
        assert_eq!(tb.suggest_asked, ("plugins".to_string(), "tre".to_string()));
        tb.accept_suggestions(("models".into(), "tre".into()), &model, &[("plugins", plugin.clone())]);
        assert!(tb.suggestions_for("tre").is_empty());
        tb.accept_suggestions(("plugins".into(), "tre".into()), &plugin, &[("3d", model.clone())]);
        assert_eq!(tb.suggestions_for("tre")[0].label, "Plugins");
        // Back on Home with the same text: asked again.
        tb.home();
        tb.want_suggestions(None, "tre");
        assert_eq!(tb.suggest_asked, ("models".to_string(), "tre".to_string()));
        assert!(tb.suggestions_for("tre").is_empty());
    }

    /// Text typed before the session came (the account's client busy at
    /// start): asked signed out (no autocomplete), then asked again once
    /// signed in, so the rows aren't left at the typed text alone.
    #[test]
    fn suggestions_asked_again_when_signed_in() {
        std::env::set_var("RUSTUDIO_TOOLBOX_STATE", std::env::temp_dir().join("rustudio-toolbox-test-suggest-session.json"));
        let mut tb = Toolbox::default();
        tb.st = Persist::default();
        tb.set_http(Arc::new(api::AnonHttp));
        tb.st.query = "tre".into();
        tb.want_suggestions(None, "tre");
        let poll_until = |tb: &mut Toolbox, n: usize| {
            for _ in 0..400 {
                tb.poll();
                if tb.suggestions_for("tre").len() >= n {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        poll_until(&mut tb, 1);
        assert!(tb.suggest_replied);
        assert_eq!(tb.suggestions_for("tre").len(), 1, "signed out: the typed text only");
        let mock = Mock::default().route("cat=plugin&", "autocomplete_tre_plugin.json").route("cat=model&", "autocomplete_tre.json");
        tb.set_http(Arc::new(mock));
        assert_eq!(tb.suggest_asked, ("models".to_string(), "tre".to_string()));
        assert!(!tb.suggest_replied, "asked again: waiting for the signed-in reply");
        poll_until(&mut tb, 4);
        assert!(tb.suggest_replied);
        let rows: Vec<(&str, &str)> = tb.suggestions_for("tre").iter().map(|r| (r.query.as_str(), r.label)).collect();
        assert_eq!(rows, [("tre", "3D Assets"), ("tree", "3D Assets"), ("trees", "3D Assets"), ("build a boat for treasure", "Plugins")]);
    }

    #[test]
    fn choosing_a_suggestion() {
        std::env::set_var("RUSTUDIO_TOOLBOX_STATE", std::env::temp_dir().join("rustudio-toolbox-test-choose.json"));
        let mut tb = Toolbox::default();
        tb.st = Persist::default();
        let (model, plugin) = (autocomplete("autocomplete_tre.json"), autocomplete("autocomplete_tre_plugin.json"));
        let rows = suggestion_rows("models", "tre", &model, &[("plugins", plugin)]);
        // Same category: as Enter (Home searches Models).
        tb.choose_suggestion(&rows[1]);
        assert_eq!(tb.nav(), &Nav::Results { cat: "models".into(), query: "tree".into(), tile: None });
        assert_eq!(tb.st.query, "tree");
        // Another category: its results; Back leads to that category's page.
        tb.home();
        tb.choose_suggestion(&rows[3]);
        assert_eq!(tb.nav(), &Nav::Results { cat: "plugins".into(), query: "build a boat for treasure".into(), tile: None });
        assert_eq!(tb.st.query, "build a boat for treasure");
        assert_eq!(tb.st.recent_searches[0], "build a boat for treasure");
        tb.back();
        assert_eq!(tb.nav(), &Nav::Category("plugins".into()));
    }

    #[test]
    fn navigation_keeps_category() {
        let mut tb = Toolbox::default();
        tb.st = Persist::default();
        tb.push(Nav::Category("3d".into()));
        tb.search("tree");
        assert_eq!(tb.nav(), &Nav::Results { cat: "3d".into(), query: "tree".into(), tile: None });
        tb.search("rock");
        assert_eq!(tb.st.nav.len(), 3);
        tb.back();
        assert_eq!(tb.nav(), &Nav::Category("3d".into()));
        tb.home();
        tb.search("car");
        assert_eq!(tb.nav(), &Nav::Results { cat: "models".into(), query: "car".into(), tile: None });
        let r = tb.store_req("audio", "", Some("trending-sfx"));
        assert_eq!(r.filters.audio_type.as_deref(), Some("SoundEffect"));
        let r = tb.store_req("3d__vehicles", "tree", None);
        assert_eq!(r.category_path.as_deref(), Some("3d__vehicles"));
    }
}
