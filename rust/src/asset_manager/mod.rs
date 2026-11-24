//! The Asset Manager panel: Roblox Studio's unified asset browser,
//! organizer, places manager, and bulk importer.
//!
//! * `mod.rs`: state, folder hierarchy, persistence (`asset_manager.json`),
//!   in-experience DOM asset scanner, natural alphanumeric sorting, search
//!   query matching, and worker pool.
//! * `api.rs`: Roblox web API calls (universe places, user/group inventories
//!   and creations, groups, asset details lookup, and asset uploads).
//! * `ui.rs`: 1:1 Roblox Studio Asset Manager UI in `theme.rs` dark style.
//! * `glue.rs`: `Editor` integration (per-frame tick, session sync, DOM scan,
//!   insertion via `toolbox`, drag-and-drop, place opening, bulk import).

pub mod api;
pub mod glue;
pub mod ui;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The longest the In-Experience list lags edits (`scan_document`).
const EXPERIENCE_RESCAN: Duration = Duration::from_secs(1);

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_account::cloud::CloudPlace;
use rustudio_core::doc::Document;
use serde::{Deserialize, Serialize};

use crate::toolbox::api::{AnonHttp, Failure, Http, OfflineHttp, SessionHttp};
use crate::toolbox::categories::Kind;
use crate::toolbox::model::Asset;
use crate::toolbox::thumbs::Thumbs;

/// Asset types shown in the Asset Manager filter menu, badges, and columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetKind {
    Model,
    MeshPart,
    Image,
    Audio,
    Animation,
    Package,
    Video,
    Font,
    Plugin,
    Place,
}

impl AssetKind {
    /// Filterable asset kinds (excluding Place, which has its own Places view).
    pub const FILTERABLE: [AssetKind; 9] = [
        AssetKind::Model,
        AssetKind::MeshPart,
        AssetKind::Image,
        AssetKind::Audio,
        AssetKind::Animation,
        AssetKind::Package,
        AssetKind::Video,
        AssetKind::Font,
        AssetKind::Plugin,
    ];

    /// Primary kinds fetched for an Inventory / Creations view.
    pub const INVENTORY_KINDS: [AssetKind; 6] = [
        AssetKind::Model,
        AssetKind::MeshPart,
        AssetKind::Image,
        AssetKind::Audio,
        AssetKind::Animation,
        AssetKind::Package,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AssetKind::Model => "Model",
            AssetKind::MeshPart => "Mesh",
            AssetKind::Image => "Image",
            AssetKind::Audio => "Audio",
            AssetKind::Animation => "Animation",
            AssetKind::Package => "Package",
            AssetKind::Video => "Video",
            AssetKind::Font => "Font",
            AssetKind::Plugin => "Plugin",
            AssetKind::Place => "Place",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            AssetKind::Model => "Models",
            AssetKind::MeshPart => "Meshes",
            AssetKind::Image => "Images",
            AssetKind::Audio => "Audio",
            AssetKind::Animation => "Animations",
            AssetKind::Package => "Packages",
            AssetKind::Video => "Videos",
            AssetKind::Font => "Fonts",
            AssetKind::Plugin => "Plugins",
            AssetKind::Place => "Places",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            AssetKind::Model => "rs-model",
            AssetKind::MeshPart => "cube",
            AssetKind::Image => "photo",
            AssetKind::Audio => "volume",
            AssetKind::Animation => "run",
            AssetKind::Package => "package",
            AssetKind::Video => "movie",
            AssetKind::Font => "typography",
            AssetKind::Plugin => "puzzle",
            AssetKind::Place => "map-pin",
        }
    }

    pub fn asset_type_id(self) -> u32 {
        match self {
            AssetKind::Model => 10,
            AssetKind::MeshPart => 40,
            AssetKind::Image => 13,
            AssetKind::Audio => 3,
            AssetKind::Animation => 24,
            AssetKind::Package => 10,
            AssetKind::Video => 62,
            AssetKind::Font => 73,
            AssetKind::Plugin => 38,
            AssetKind::Place => 9,
        }
    }

    pub fn from_type_id(id: u32) -> AssetKind {
        match id {
            1 | 13 => AssetKind::Image,
            3 => AssetKind::Audio,
            4 | 40 => AssetKind::MeshPart,
            9 => AssetKind::Place,
            24 => AssetKind::Animation,
            38 => AssetKind::Plugin,
            62 => AssetKind::Video,
            73 => AssetKind::Font,
            _ => AssetKind::Model,
        }
    }

    pub fn to_toolbox_kind(self) -> Option<Kind> {
        Some(match self {
            AssetKind::Model => Kind::Model,
            AssetKind::MeshPart => Kind::MeshPart,
            AssetKind::Image => Kind::Decal,
            AssetKind::Audio => Kind::Audio,
            AssetKind::Animation => Kind::Animation,
            AssetKind::Package => Kind::Package,
            AssetKind::Video => Kind::Video,
            AssetKind::Font => Kind::Font,
            AssetKind::Plugin => Kind::Plugin,
            AssetKind::Place => return None,
        })
    }
}

/// Which root inventory / section is selected in the left sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    /// Project > Places (all places in the current Roblox universe).
    Places,
    /// Project > In Experience (all assets referenced by the open DataModel).
    Experience,
    /// Inventories > My Inventory (optionally inside a nested folder id).
    MyInventory { folder: Option<String> },
    /// Inventories > Group (optionally inside a nested folder id).
    Group { group_id: u64, folder: Option<String> },
    /// Recent > Uploads (recently uploaded & imported assets).
    RecentUploads,
}

impl Default for Scope {
    fn default() -> Self {
        Scope::Experience
    }
}

impl Scope {
    /// The inventory root key that owns custom folders for this scope.
    pub fn folder_root(&self) -> Option<String> {
        match self {
            Scope::MyInventory { .. } => Some("user".into()),
            Scope::Group { group_id, .. } => Some(format!("group:{group_id}")),
            _ => None,
        }
    }

    pub fn folder_id(&self) -> Option<&str> {
        match self {
            Scope::MyInventory { folder } | Scope::Group { folder, .. } => folder.as_deref(),
            _ => None,
        }
    }

    pub fn with_folder(&self, folder: Option<String>) -> Scope {
        match self {
            Scope::MyInventory { .. } => Scope::MyInventory { folder },
            Scope::Group { group_id, .. } => Scope::Group { group_id: *group_id, folder },
            other => other.clone(),
        }
    }
}

/// A user-created folder inside My Inventory or a Group Inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub root: String,
    pub parent: Option<String>,
    pub name: String,
    #[serde(default)]
    pub asset_ids: Vec<u64>,
}

/// One asset item displayed in the Asset Manager grid or list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetEntry {
    pub id: u64,
    pub name: String,
    pub kind: AssetKind,
    pub creator_name: String,
    pub creator_id: u64,
    pub creator_verified: bool,
    pub created: String,
    pub updated: String,
    pub description: String,
    /// True if this is the Start Place in the Places view.
    #[serde(default)]
    pub is_start_place: bool,
    /// For In-Experience assets: instances in the open document using this asset.
    #[serde(skip)]
    pub usages: Vec<Ref>,
    /// True if created by the signed-in user or selected group.
    #[serde(default)]
    pub is_creation: bool,
    /// Full Toolbox Asset metadata when available (used for insertion).
    #[serde(skip)]
    pub raw: Option<Asset>,
}

impl AssetEntry {
    pub fn from_toolbox_asset(a: &Asset, kind: AssetKind, is_creation: bool) -> AssetEntry {
        AssetEntry {
            id: a.id,
            name: if a.name.is_empty() { format!("Asset {}", a.id) } else { a.name.clone() },
            kind: if a.type_id != 0 { AssetKind::from_type_id(a.type_id) } else { kind },
            creator_name: a.creator.name.clone(),
            creator_id: a.creator.id,
            creator_verified: a.creator.verified,
            created: a.created.clone(),
            updated: a.updated.clone(),
            description: a.description.clone(),
            is_start_place: false,
            usages: Vec::new(),
            is_creation,
            raw: Some(a.clone()),
        }
    }

    pub fn from_place(p: &CloudPlace, is_start_place: bool) -> AssetEntry {
        AssetEntry {
            id: p.id,
            name: p.name.clone(),
            kind: AssetKind::Place,
            creator_name: String::new(),
            creator_id: 0,
            creator_verified: false,
            created: String::new(),
            updated: String::new(),
            description: String::new(),
            is_start_place,
            usages: Vec::new(),
            is_creation: true,
            raw: None,
        }
    }

    pub fn to_toolbox_asset(&self) -> Asset {
        if let Some(a) = &self.raw {
            return a.clone();
        }
        Asset {
            id: self.id,
            name: self.name.clone(),
            description: self.description.clone(),
            type_id: self.kind.asset_type_id(),
            creator: crate::toolbox::model::Creator {
                id: self.creator_id,
                name: self.creator_name.clone(),
                kind: 1,
                verified: self.creator_verified,
            },
            created: self.created.clone(),
            updated: self.updated.clone(),
            detailed: true,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    #[default]
    Grid,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortBy {
    #[default]
    Name,
    Modified,
    Created,
    AssetType,
    Creator,
    Id,
}

impl SortBy {
    pub const ALL: [SortBy; 6] = [
        SortBy::Name,
        SortBy::Modified,
        SortBy::Created,
        SortBy::AssetType,
        SortBy::Creator,
        SortBy::Id,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "Name",
            SortBy::Modified => "Date Modified",
            SortBy::Created => "Date Created",
            SortBy::AssetType => "Asset Type",
            SortBy::Creator => "Creator",
            SortBy::Id => "Asset ID",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

impl SortDir {
    pub fn flip(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OwnershipFilter {
    #[default]
    All,
    Creations,
    Inventory,
}

impl OwnershipFilter {
    pub fn label(self) -> &'static str {
        match self {
            OwnershipFilter::All => "All",
            OwnershipFilter::Creations => "Created by Me",
            OwnershipFilter::Inventory => "Inventory",
        }
    }
}

/// Which columns are visible in List View.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Columns {
    pub asset_type: bool,
    pub id: bool,
    pub creator: bool,
    pub modified: bool,
    pub usages: bool,
}

impl Default for Columns {
    fn default() -> Self {
        Columns {
            asset_type: true,
            id: true,
            creator: true,
            modified: true,
            usages: true,
        }
    }
}

/// One file queued for Bulk Import / Upload.
#[derive(Debug, Clone)]
pub struct ImportItem {
    pub path: PathBuf,
    pub display_name: String,
    pub description: String,
    pub asset_type: &'static str,
    pub kind: AssetKind,
    pub mime: &'static str,
    pub size_bytes: u64,
    pub status: ImportStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportStatus {
    Pending,
    Uploading,
    Done(u64),
    LocalImported,
    Failed(String),
}

/// Saved Asset Manager preferences and local folder hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Persist {
    pub scope: Scope,
    pub view: ViewMode,
    pub sort_by: SortBy,
    pub sort_dir: SortDir,
    pub tile_size: f32,
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub details_open: bool,
    pub type_filters: Vec<AssetKind>,
    pub ownership: OwnershipFilter,
    pub columns: Columns,
    pub folders: Vec<Folder>,
    pub hidden_groups: HashSet<u64>,
    pub recent_uploads: Vec<AssetEntry>,
    pub next_folder_seq: u64,
}

impl Default for Persist {
    fn default() -> Self {
        Persist {
            scope: Scope::Experience,
            view: ViewMode::Grid,
            sort_by: SortBy::Name,
            sort_dir: SortDir::Asc,
            tile_size: 104.0,
            sidebar_open: true,
            sidebar_width: 196.0,
            details_open: false,
            type_filters: Vec::new(),
            ownership: OwnershipFilter::All,
            columns: Columns::default(),
            folders: Vec::new(),
            hidden_groups: HashSet::new(),
            recent_uploads: Vec::new(),
            next_folder_seq: 1,
        }
    }
}

/// Cached cloud listing for an inventory root (`"user"` or `"group:<id>"`).
#[derive(Debug, Clone, Default)]
pub struct InventoryCache {
    pub items: Vec<AssetEntry>,
    pub cursors: HashMap<(AssetKind, bool), Option<String>>,
    pub loading: bool,
    pub error: Option<Failure>,
    pub loaded: bool,
    pub has_more: bool,
}

/// Cached places listing for a universe.
#[derive(Debug, Clone, Default)]
pub struct PlacesCache {
    pub universe_id: u64,
    pub places: Vec<AssetEntry>,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded: bool,
}

/// Worker thread messages returned to the UI thread.
pub enum Msg {
    Groups(Result<Vec<(u64, String)>, Failure>),
    Places {
        universe_id: u64,
        result: Result<Vec<CloudPlace>, String>,
    },
    InventoryBatch {
        root: String,
        append: bool,
        items: Vec<AssetEntry>,
        cursors: HashMap<(AssetKind, bool), Option<String>>,
        error: Option<Failure>,
    },
    LookupAsset {
        id: u64,
        result: Result<Option<AssetEntry>, Failure>,
    },
    UploadFinished {
        index: usize,
        result: Result<u64, String>,
    },
}

type Task = Box<dyn FnOnce(&dyn Http) -> Option<Msg> + Send>;

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
            http: Arc::new(Mutex::new(Arc::new(AnonHttp))),
            tx,
            rx,
            started: false,
            busy: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Pool {
    pub fn set_http(&mut self, h: Arc<dyn Http>) {
        *self.http.lock().unwrap() = h;
    }

    pub fn spawn(&mut self, task: impl FnOnce(&dyn Http) -> Option<Msg> + Send + 'static) {
        if !self.started {
            self.started = true;
            for n in 0..3 {
                let q = self.queue.clone();
                let http = self.http.clone();
                let tx = self.tx.clone();
                let busy = self.busy.clone();
                let _ = std::thread::Builder::new()
                    .name(format!("asset-mgr-{n}"))
                    .spawn(move || loop {
                        let job = {
                            let (lock, cv) = &*q;
                            let mut g = lock.lock().unwrap();
                            while g.is_empty() {
                                g = cv.wait(g).unwrap();
                            }
                            g.pop_front()
                        };
                        if let Some(job) = job {
                            busy.fetch_add(1, Ordering::Relaxed);
                            let h = http.lock().unwrap().clone();
                            let msg = job(&*h);
                            busy.fetch_sub(1, Ordering::Relaxed);
                            if let Some(m) = msg {
                                let _ = tx.send(m);
                            }
                        }
                    });
            }
        }
        let (lock, cv) = &*self.queue;
        lock.lock().unwrap().push_back(Box::new(task));
        cv.notify_one();
    }
}

/// Modal state for creating or renaming a folder.
#[derive(Debug, Clone)]
pub enum FolderDialog {
    Create { root: String, parent: Option<String>, name: String },
    Rename { folder_id: String, name: String },
}

/// Actions queued by the UI and executed by `glue.rs` against `Editor`.
#[derive(Debug, Clone)]
pub enum Action {
    Insert { entry: AssetEntry, into_selection: bool },
    OpenPlace(u64),
    SelectInstances(Vec<Ref>),
    CopyText(String, String),
    OpenImportDialog,
    SaveToRoblox,
    OpenAccount,
    PreviewAudio(u64),
    StopAudio,
    OpenUrl(String),
    ImportLocalModel(PathBuf),
}

pub struct AssetManager {
    pub persist: Persist,
    pub query: String,
    pub selected: Vec<u64>,
    pub anchor: Option<u64>,
    pub back: Vec<Scope>,
    pub forward: Vec<Scope>,
    pub expanded_folders: HashSet<String>,
    pub project_expanded: bool,
    pub inventories_expanded: bool,
    pub groups_expanded: bool,
    pub recent_expanded: bool,

    // Session & networking
    pub http: Arc<dyn Http>,
    pub session: Option<rustudio_account::session::Session>,
    pub user_id: u64,
    pub user_name: String,
    pub signed_in: bool,
    pub pool: Pool,
    pub thumbs: Thumbs,
    pub session_checked: Option<Instant>,

    // Cached data
    pub groups: Option<Result<Vec<(u64, String)>, Failure>>,
    pub groups_loading: bool,
    pub places: PlacesCache,
    pub inventories: HashMap<String, InventoryCache>,
    pub experience_assets: Vec<AssetEntry>,
    pub experience_rev: u64,
    pub experience_root: Ref,
    /// When the in-experience list was last rebuilt: edits (and Play, whose
    /// world changes every frame) rescan at most once a second.
    pub experience_scanned: Option<Instant>,
    pub lookup_cache: HashMap<u64, Option<AssetEntry>>,
    pub lookup_inflight: HashSet<u64>,

    // Popups / modals / transient UI state
    pub filter_popup: bool,
    pub sort_popup: bool,
    pub view_popup: bool,
    pub folder_dialog: Option<FolderDialog>,
    pub import_modal: Option<ImportModal>,
    pub actions: Vec<Action>,
    pub log: Vec<(bool, String)>,
    pub dirty_since: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    pub playing_audio: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ImportModal {
    pub items: Vec<ImportItem>,
    pub target_group: Option<u64>,
    pub target_folder: Option<String>,
    pub uploading: bool,
}

impl Default for AssetManager {
    fn default() -> Self {
        let mut persist = load_persist();
        if persist.tile_size < 64.0 || persist.tile_size > 200.0 {
            persist.tile_size = 104.0;
        }
        if persist.sidebar_width < 130.0 || persist.sidebar_width > 360.0 {
            persist.sidebar_width = 196.0;
        }
        AssetManager {
            persist,
            query: String::new(),
            selected: Vec::new(),
            anchor: None,
            back: Vec::new(),
            forward: Vec::new(),
            expanded_folders: HashSet::new(),
            project_expanded: true,
            inventories_expanded: true,
            groups_expanded: true,
            recent_expanded: true,
            http: Arc::new(AnonHttp),
            session: None,
            user_id: 0,
            user_name: String::new(),
            signed_in: false,
            pool: Pool::default(),
            thumbs: Thumbs::default(),
            session_checked: None,
            groups: None,
            groups_loading: false,
            places: PlacesCache::default(),
            inventories: HashMap::new(),
            experience_assets: Vec::new(),
            experience_rev: u64::MAX,
            experience_root: Ref::none(),
            experience_scanned: None,
            lookup_cache: HashMap::new(),
            lookup_inflight: HashSet::new(),
            filter_popup: false,
            sort_popup: false,
            view_popup: false,
            folder_dialog: None,
            import_modal: None,
            actions: Vec::new(),
            log: Vec::new(),
            dirty_since: None,
            cancel: Arc::new(AtomicBool::new(false)),
            playing_audio: None,
        }
    }
}

impl AssetManager {
    pub fn mark_dirty(&mut self) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
    }

    pub fn save_if_changed(&mut self, force: bool) {
        let Some(t) = self.dirty_since else { return };
        if !force && t.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.dirty_since = None;
        save_persist(&self.persist);
    }

    pub fn set_session(&mut self, session: Option<(rustudio_account::session::Session, u64)>) {
        let offline = std::env::var_os("RUSTUDIO_TOOLBOX_OFFLINE").is_some();
        let new_uid = session.as_ref().map(|(_, u)| *u).unwrap_or(0);
        let new_signed = session.is_some();
        if new_uid == self.user_id && new_signed == self.signed_in {
            return;
        }
        self.user_id = new_uid;
        self.signed_in = new_signed;
        self.user_name = session
            .as_ref()
            .map(|(s, _)| {
                if !s.profile.display_name.is_empty() {
                    s.profile.display_name.clone()
                } else {
                    s.profile.name.clone()
                }
            })
            .unwrap_or_default();
        self.session = session.as_ref().map(|(s, _)| s.downloader());
        let h: Arc<dyn Http> = match session {
            Some((_, u)) if offline => Arc::new(OfflineHttp(u)),
            Some((s, _)) => Arc::new(SessionHttp(s)),
            None => Arc::new(AnonHttp),
        };
        self.http = h.clone();
        self.pool.set_http(h.clone());
        self.thumbs.set_http(h);
        self.groups = None;
        self.groups_loading = false;
        self.inventories.clear();
        self.places = PlacesCache::default();
        if new_signed {
            self.fetch_groups();
        }
    }

    pub fn navigate(&mut self, scope: Scope) {
        if self.persist.scope == scope {
            return;
        }
        let prev = std::mem::replace(&mut self.persist.scope, scope);
        self.back.push(prev);
        if self.back.len() > 64 {
            self.back.remove(0);
        }
        self.forward.clear();
        self.selected.clear();
        self.anchor = None;
        self.mark_dirty();
    }

    pub fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            let cur = std::mem::replace(&mut self.persist.scope, prev);
            self.forward.push(cur);
            self.selected.clear();
            self.anchor = None;
            self.mark_dirty();
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            let cur = std::mem::replace(&mut self.persist.scope, next);
            self.back.push(cur);
            self.selected.clear();
            self.anchor = None;
            self.mark_dirty();
        }
    }

    pub fn fetch_groups(&mut self) {
        if !self.signed_in || self.groups_loading {
            return;
        }
        self.groups_loading = true;
        self.pool.spawn(|h| Some(Msg::Groups(crate::toolbox::api::fetch_groups(h))));
    }

    pub fn ensure_places(&mut self, universe_id: u64, force: bool) {
        if universe_id == 0 || !self.signed_in {
            return;
        }
        if !force && self.places.universe_id == universe_id && (self.places.loaded || self.places.loading) {
            return;
        }
        self.places.universe_id = universe_id;
        self.places.loading = true;
        self.places.error = None;
        let Some(sess) = self.session.as_ref().map(|s| s.downloader()) else {
            self.places.loading = false;
            return;
        };
        self.pool.spawn(move |_| {
            let res = api::fetch_places(&sess, universe_id);
            Some(Msg::Places { universe_id, result: res })
        });
    }

    pub fn ensure_inventory(&mut self, root: &str, group_id: Option<u64>, force: bool) {
        if !self.signed_in {
            return;
        }
        let cache = self.inventories.entry(root.to_string()).or_default();
        if !force && (cache.loaded || cache.loading) {
            return;
        }
        cache.loading = true;
        cache.error = None;
        if force {
            cache.items.clear();
            cache.cursors.clear();
            cache.loaded = false;
        }
        let root_owned = root.to_string();
        let cancel = self.cancel.clone();
        self.pool.spawn(move |h| {
            let (items, cursors, err) = api::fetch_inventory_first_page(h, group_id, &|| cancel.load(Ordering::Relaxed));
            Some(Msg::InventoryBatch {
                root: root_owned,
                append: false,
                items,
                cursors,
                error: err,
            })
        });
    }

    pub fn load_more_inventory(&mut self, root: &str, group_id: Option<u64>) {
        if !self.signed_in {
            return;
        }
        let Some(cache) = self.inventories.get_mut(root) else { return };
        if cache.loading || !cache.has_more {
            return;
        }
        cache.loading = true;
        let cursors = cache.cursors.clone();
        let root_owned = root.to_string();
        let cancel = self.cancel.clone();
        self.pool.spawn(move |h| {
            let (items, next_cursors, err) =
                api::fetch_inventory_next_page(h, group_id, &cursors, &|| cancel.load(Ordering::Relaxed));
            Some(Msg::InventoryBatch {
                root: root_owned,
                append: true,
                items,
                cursors: next_cursors,
                error: err,
            })
        });
    }

    pub fn lookup_asset_id(&mut self, id: u64) {
        if id == 0 || !self.signed_in || self.lookup_cache.contains_key(&id) || !self.lookup_inflight.insert(id) {
            return;
        }
        self.pool.spawn(move |h| {
            let res = api::lookup_asset(h, id);
            Some(Msg::LookupAsset { id, result: res })
        });
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.pool.rx.try_recv() {
            changed = true;
            match msg {
                Msg::Groups(res) => {
                    self.groups_loading = false;
                    self.groups = Some(res);
                }
                Msg::Places { universe_id, result } => {
                    if self.places.universe_id == universe_id {
                        self.places.loading = false;
                        self.places.loaded = true;
                        match result {
                            Ok(list) => {
                                self.places.error = None;
                                self.places.places = list
                                    .iter()
                                    .enumerate()
                                    .map(|(i, p)| AssetEntry::from_place(p, i == 0))
                                    .collect();
                            }
                            Err(e) => {
                                self.places.error = Some(e);
                            }
                        }
                    }
                }
                Msg::InventoryBatch { root, append, items, cursors, error } => {
                    let cache = self.inventories.entry(root).or_default();
                    cache.loading = false;
                    cache.loaded = true;
                    cache.error = error;
                    cache.has_more = cursors.values().any(|c| c.is_some());
                    cache.cursors = cursors;
                    if !append {
                        cache.items.clear();
                    }
                    let mut seen: HashSet<u64> = cache.items.iter().map(|a| a.id).collect();
                    for item in items {
                        if seen.insert(item.id) {
                            cache.items.push(item);
                        }
                    }
                }
                Msg::LookupAsset { id, result } => {
                    self.lookup_inflight.remove(&id);
                    if let Ok(entry) = result {
                        self.lookup_cache.insert(id, entry);
                    }
                }
                Msg::UploadFinished { index, result } => {
                    let mut finished_entry: Option<(AssetEntry, Option<String>)> = None;
                    if let Some(modal) = &mut self.import_modal {
                        if let Some(item) = modal.items.get_mut(index) {
                            match result {
                                Ok(new_id) => {
                                    item.status = ImportStatus::Done(new_id);
                                    let entry = AssetEntry {
                                        id: new_id,
                                        name: item.display_name.clone(),
                                        kind: item.kind,
                                        creator_name: self.user_name.clone(),
                                        creator_id: self.user_id,
                                        creator_verified: false,
                                        created: now_iso(),
                                        updated: now_iso(),
                                        description: item.description.clone(),
                                        is_start_place: false,
                                        usages: Vec::new(),
                                        is_creation: true,
                                        raw: None,
                                    };
                                    finished_entry = Some((entry, modal.target_folder.clone()));
                                    self.log.push((false, format!("Uploaded \"{}\" (rbxassetid://{new_id})", item.display_name)));
                                }
                                Err(e) => {
                                    item.status = ImportStatus::Failed(e.clone());
                                    self.log.push((true, format!("Upload failed for \"{}\": {e}", item.display_name)));
                                }
                            }
                        }
                        let any_pending = modal
                            .items
                            .iter()
                            .any(|i| matches!(i.status, ImportStatus::Pending | ImportStatus::Uploading));
                        if !any_pending {
                            modal.uploading = false;
                        }
                    }
                    if let Some((entry, folder_id)) = finished_entry {
                        self.persist.recent_uploads.retain(|a| a.id != entry.id);
                        self.persist.recent_uploads.insert(0, entry.clone());
                        if self.persist.recent_uploads.len() > 200 {
                            self.persist.recent_uploads.truncate(200);
                        }
                        if let Some(fid) = folder_id {
                            self.add_asset_to_folder(entry.id, &fid);
                        }
                        // Also add into the user's inventory cache if loaded.
                        if let Some(cache) = self.inventories.get_mut("user") {
                            if !cache.items.iter().any(|a| a.id == entry.id) {
                                cache.items.insert(0, entry);
                            }
                        }
                        self.mark_dirty();
                    }
                }
            }
        }
        changed
    }

    // ------------------------------------------------------------------ folders

    pub fn create_folder(&mut self, root: String, parent: Option<String>, name: String) -> String {
        let name = name.trim().to_string();
        let name = if name.is_empty() { "New Folder".into() } else { name };
        let id = format!("f_{}", self.persist.next_folder_seq);
        self.persist.next_folder_seq += 1;
        self.persist.folders.push(Folder {
            id: id.clone(),
            root,
            parent: parent.clone(),
            name,
            asset_ids: Vec::new(),
        });
        if let Some(p) = parent {
            self.expanded_folders.insert(p);
        }
        self.mark_dirty();
        id
    }

    pub fn rename_folder(&mut self, folder_id: &str, name: String) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(f) = self.persist.folders.iter_mut().find(|f| f.id == folder_id) {
            f.name = name.to_string();
            self.mark_dirty();
        }
    }

    pub fn delete_folder(&mut self, folder_id: &str) {
        let mut to_remove = HashSet::new();
        to_remove.insert(folder_id.to_string());
        loop {
            let before = to_remove.len();
            for f in &self.persist.folders {
                if f.parent.as_deref().is_some_and(|p| to_remove.contains(p)) {
                    to_remove.insert(f.id.clone());
                }
            }
            if to_remove.len() == before {
                break;
            }
        }
        let parent_of_target = self
            .persist
            .folders
            .iter()
            .find(|f| f.id == folder_id)
            .and_then(|f| f.parent.clone());
        self.persist.folders.retain(|f| !to_remove.contains(&f.id));
        if self.persist.scope.folder_id().is_some_and(|cur| to_remove.contains(cur)) {
            self.persist.scope = self.persist.scope.with_folder(parent_of_target);
        }
        self.mark_dirty();
    }

    pub fn add_asset_to_folder(&mut self, asset_id: u64, folder_id: &str) {
        if let Some(f) = self.persist.folders.iter_mut().find(|f| f.id == folder_id) {
            if !f.asset_ids.contains(&asset_id) {
                f.asset_ids.push(asset_id);
                self.mark_dirty();
            }
        }
    }

    pub fn remove_asset_from_folder(&mut self, asset_id: u64, folder_id: &str) {
        if let Some(f) = self.persist.folders.iter_mut().find(|f| f.id == folder_id) {
            let before = f.asset_ids.len();
            f.asset_ids.retain(|id| *id != asset_id);
            if f.asset_ids.len() != before {
                self.mark_dirty();
            }
        }
    }

    pub fn child_folders(&self, root: &str, parent: Option<&str>) -> Vec<Folder> {
        let mut out: Vec<Folder> = self
            .persist
            .folders
            .iter()
            .filter(|f| f.root == root && f.parent.as_deref() == parent)
            .cloned()
            .collect();
        out.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        out
    }

    pub fn folder_breadcrumbs(&self, folder_id: &str) -> Vec<(String, String)> {
        let mut chain = Vec::new();
        let mut cur = Some(folder_id.to_string());
        let mut guard = 0;
        while let Some(id) = cur {
            guard += 1;
            if guard > 32 {
                break;
            }
            if let Some(f) = self.persist.folders.iter().find(|f| f.id == id) {
                chain.push((f.id.clone(), f.name.clone()));
                cur = f.parent.clone();
            } else {
                break;
            }
        }
        chain.reverse();
        chain
    }

    /// Scans the open `Document` for every referenced Roblox asset ID.
    pub fn scan_document(&mut self, doc: &Document) {
        let rev = doc.revision;
        let root = doc.dom.root_ref();
        if self.experience_rev == rev && self.experience_root == root {
            return;
        }
        // (a walk of every instance: a big place in Play would pay it each frame)
        let forced = self.experience_rev == u64::MAX || self.experience_root != root;
        if !forced && self.experience_scanned.is_some_and(|t| t.elapsed() < EXPERIENCE_RESCAN) {
            return;
        }
        self.experience_rev = rev;
        self.experience_root = root;
        self.experience_scanned = Some(Instant::now());
        self.experience_assets = scan_dom_assets(doc);
    }

    /// Returns the filtered and sorted entries for the active scope.
    pub fn visible_entries(&self) -> Vec<AssetEntry> {
        let mut list: Vec<AssetEntry> = match &self.persist.scope {
            Scope::Places => self.places.places.clone(),
            Scope::Experience => self.experience_assets.clone(),
            Scope::RecentUploads => self.persist.recent_uploads.clone(),
            Scope::MyInventory { folder } => {
                let items = self.inventories.get("user").map(|c| c.items.as_slice()).unwrap_or(&[]);
                filter_by_folder(items, folder.as_deref(), &self.persist.folders)
            }
            Scope::Group { group_id, folder } => {
                let key = format!("group:{group_id}");
                let items = self.inventories.get(&key).map(|c| c.items.as_slice()).unwrap_or(&[]);
                filter_by_folder(items, folder.as_deref(), &self.persist.folders)
            }
        };

        // Ownership filter (applicable to Inventory scopes).
        if matches!(self.persist.scope, Scope::MyInventory { .. } | Scope::Group { .. }) {
            match self.persist.ownership {
                OwnershipFilter::All => {}
                OwnershipFilter::Creations => list.retain(|a| a.is_creation),
                OwnershipFilter::Inventory => list.retain(|a| !a.is_creation),
            }
        }

        // Asset type filter (not applied in Places view).
        if !self.persist.type_filters.is_empty() && !matches!(self.persist.scope, Scope::Places) {
            list.retain(|a| self.persist.type_filters.contains(&a.kind));
        }

        // Search query filter.
        let q = self.query.trim();
        if !q.is_empty() {
            let parsed_id = parse_asset_id(q);
            list.retain(|a| matches_query(a, q, parsed_id));
            // If user typed an explicit asset ID that isn't in the current list,
            // include the direct lookup result if fetched.
            if let Some(id) = parsed_id {
                if !list.iter().any(|a| a.id == id) {
                    if let Some(Some(found)) = self.lookup_cache.get(&id) {
                        list.insert(0, found.clone());
                    }
                }
            }
        }

        // Sort (Places keeps Start Place first when sorting by Name asc).
        let dir = self.persist.sort_dir;
        let by = self.persist.sort_by;
        list.sort_by(|a, b| {
            let ord = match by {
                SortBy::Name => natural_cmp(&a.name, &b.name),
                SortBy::Modified => a.updated.cmp(&b.updated).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Created => a.created.cmp(&b.created).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::AssetType => a.kind.label().cmp(b.kind.label()).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Creator => natural_cmp(&a.creator_name, &b.creator_name).then_with(|| natural_cmp(&a.name, &b.name)),
                SortBy::Id => a.id.cmp(&b.id),
            };
            match dir {
                SortDir::Asc => ord,
                SortDir::Desc => ord.reverse(),
            }
        });
        if matches!(self.persist.scope, Scope::Places) {
            list.sort_by_key(|a| !a.is_start_place);
        }
        list
    }

    pub fn active_filter_count(&self) -> usize {
        let mut n = self.persist.type_filters.len();
        if self.persist.ownership != OwnershipFilter::All {
            n += 1;
        }
        n
    }

    pub fn find_entry(&self, id: u64) -> Option<AssetEntry> {
        if let Some(e) = self.experience_assets.iter().find(|a| a.id == id) {
            return Some(e.clone());
        }
        if let Some(e) = self.places.places.iter().find(|a| a.id == id) {
            return Some(e.clone());
        }
        if let Some(e) = self.persist.recent_uploads.iter().find(|a| a.id == id) {
            return Some(e.clone());
        }
        for cache in self.inventories.values() {
            if let Some(e) = cache.items.iter().find(|a| a.id == id) {
                return Some(e.clone());
            }
        }
        self.lookup_cache.get(&id).and_then(|o| o.clone())
    }
}

fn filter_by_folder(items: &[AssetEntry], folder_id: Option<&str>, folders: &[Folder]) -> Vec<AssetEntry> {
    let Some(fid) = folder_id else {
        return items.to_vec();
    };
    let Some(folder) = folders.iter().find(|f| f.id == fid) else {
        return Vec::new();
    };
    let set: HashSet<u64> = folder.asset_ids.iter().copied().collect();
    items.iter().filter(|a| set.contains(&a.id)).cloned().collect()
}

/// Parses a bare numeric asset ID or `rbxassetid://<id>` or Roblox asset URL.
pub fn parse_asset_id(input: &str) -> Option<u64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(id) = s.parse::<u64>() {
        return (id > 0).then_some(id);
    }
    let lower = s.to_ascii_lowercase();
    for prefix in [
        "rbxassetid://",
        "http://www.roblox.com/asset/?id=",
        "https://www.roblox.com/asset/?id=",
        "https://create.roblox.com/store/asset/",
        "https://www.roblox.com/library/",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(id) = digits.parse::<u64>() {
                if id > 0 {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn matches_query(a: &AssetEntry, raw_query: &str, parsed_id: Option<u64>) -> bool {
    if let Some(id) = parsed_id {
        if a.id == id {
            return true;
        }
    }
    let q = raw_query.to_ascii_lowercase();
    for token in q.split_whitespace() {
        if let Some(t) = token.strip_prefix("type:") {
            if !a.kind.label().to_ascii_lowercase().contains(t) && !a.kind.plural().to_ascii_lowercase().contains(t) {
                return false;
            }
        } else if let Some(c) = token.strip_prefix("creator:") {
            if !a.creator_name.to_ascii_lowercase().contains(c) {
                return false;
            }
        } else {
            let hit = a.name.to_ascii_lowercase().contains(token)
                || a.creator_name.to_ascii_lowercase().contains(token)
                || a.kind.label().to_ascii_lowercase().contains(token)
                || a.id.to_string().contains(token);
            if !hit {
                return false;
            }
        }
    }
    true
}

/// Natural alphanumeric comparison (case-insensitive): "Part 2" < "Part 11".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ia = a.chars().peekable();
    let mut ib = b.chars().peekable();
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na: u128 = 0;
                let mut la = 0usize;
                while let Some(c) = ia.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na = na.saturating_mul(10).saturating_add((c as u8 - b'0') as u128);
                    la += 1;
                    ia.next();
                }
                let mut nb: u128 = 0;
                let mut lb = 0usize;
                while let Some(c) = ib.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb = nb.saturating_mul(10).saturating_add((c as u8 - b'0') as u128);
                    lb += 1;
                    ib.next();
                }
                let ord = na.cmp(&nb).then_with(|| la.cmp(&lb));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                let ord = ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase());
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
                ia.next();
                ib.next();
            }
        }
    }
}

/// Scans all instances in `doc` and extracts every referenced Roblox asset.
pub fn scan_dom_assets(doc: &Document) -> Vec<AssetEntry> {
    struct Acc {
        kind: AssetKind,
        name: String,
        usages: Vec<Ref>,
    }
    let mut map: BTreeMap<u64, Acc> = BTreeMap::new();
    let mut stack = vec![doc.dom.root_ref()];
    while let Some(r) = stack.pop() {
        let Some(inst) = doc.dom.get_by_ref(r) else { continue };
        let class = inst.class.as_str();
        for (prop, val) in &inst.properties {
            let pname = prop.as_str();
            let raw_str: Option<&str> = match val {
                Variant::Content(c) => c.as_uri(),
                Variant::ContentId(c) => Some(c.as_str()),
                Variant::String(s) if pname.ends_with("Id") || pname.contains("Texture") || pname == "Image" => {
                    Some(s.as_str())
                }
                _ => None,
            };
            let Some(s) = raw_str else { continue };
            let Some(id) = parse_asset_id(s) else { continue };
            let kind = infer_dom_asset_kind(class, pname);
            let entry = map.entry(id).or_insert_with(|| Acc {
                kind,
                name: format!("{} ({id})", inst.name),
                usages: Vec::new(),
            });
            if !entry.usages.contains(&r) {
                entry.usages.push(r);
            }
        }
        for &child in inst.children() {
            stack.push(child);
        }
    }
    map.into_iter()
        .map(|(id, acc)| AssetEntry {
            id,
            name: acc.name,
            kind: acc.kind,
            creator_name: "In Experience".into(),
            creator_id: 0,
            creator_verified: false,
            created: String::new(),
            updated: String::new(),
            description: format!("Referenced by {} instance(s) in this place.", acc.usages.len()),
            is_start_place: false,
            usages: acc.usages,
            is_creation: false,
            raw: None,
        })
        .collect()
}

fn infer_dom_asset_kind(class: &str, prop: &str) -> AssetKind {
    match (class, prop) {
        ("Sound", _) | (_, "SoundId") => AssetKind::Audio,
        ("Animation", _) | (_, "AnimationId") => AssetKind::Animation,
        ("VideoFrame", _) | (_, "Video") => AssetKind::Video,
        ("PackageLink", _) | (_, "PackageId") => AssetKind::Package,
        (_, "MeshId") => AssetKind::MeshPart,
        ("Decal" | "Texture" | "ImageLabel" | "ImageButton" | "Sky" | "ParticleEmitter" | "Beam" | "Trail"
        | "SurfaceAppearance" | "Shirt" | "Pants" | "ShirtGraphic" | "MaterialVariant", _) => AssetKind::Image,
        (_, p) if p.contains("Texture") || p.contains("Image") || p.ends_with("Map") => AssetKind::Image,
        _ => AssetKind::Model,
    }
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn persist_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("rustudio").join("asset_manager.json"))
}

fn load_persist() -> Persist {
    let Some(p) = persist_path() else { return Persist::default() };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_persist(p: &Persist) {
    let Some(path) = persist_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sorting_orders_numbers_properly() {
        let mut names = vec!["Part 11", "Part 2", "Part 1", "part 10", "Asset"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["Asset", "Part 1", "Part 2", "part 10", "Part 11"]);
    }

    #[test]
    fn parse_asset_ids_handles_uris_and_numbers() {
        assert_eq!(parse_asset_id("123456"), Some(123456));
        assert_eq!(parse_asset_id("rbxassetid://987654321"), Some(987654321));
        assert_eq!(parse_asset_id("http://www.roblox.com/asset/?id=42"), Some(42));
        assert_eq!(parse_asset_id("https://create.roblox.com/store/asset/555123/My-Model"), Some(555123));
        assert_eq!(parse_asset_id("not-an-id"), None);
        assert_eq!(parse_asset_id("0"), None);
    }

    #[test]
    fn folder_hierarchy_create_rename_delete_and_membership() {
        let mut am = AssetManager::default();
        am.persist.folders.clear();
        let root_id = am.create_folder("user".into(), None, "Props".into());
        let child_id = am.create_folder("user".into(), Some(root_id.clone()), "Trees".into());
        am.add_asset_to_folder(100, &child_id);
        am.add_asset_to_folder(100, &child_id);
        assert_eq!(am.persist.folders.iter().find(|f| f.id == child_id).unwrap().asset_ids, vec![100]);
        assert_eq!(am.folder_breadcrumbs(&child_id), vec![(root_id.clone(), "Props".into()), (child_id.clone(), "Trees".into())]);
        am.rename_folder(&child_id, "Pine Trees".into());
        assert_eq!(am.persist.folders.iter().find(|f| f.id == child_id).unwrap().name, "Pine Trees");
        am.remove_asset_from_folder(100, &child_id);
        assert!(am.persist.folders.iter().find(|f| f.id == child_id).unwrap().asset_ids.is_empty());
        am.delete_folder(&root_id);
        assert!(am.persist.folders.is_empty());
    }
}
