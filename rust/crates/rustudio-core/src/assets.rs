//! Assets for meshes, textures, decals and clothing: resolving content ids,
//! downloading cloud assets through the account crate (Open Cloud Asset
//! Delivery only), a disk cache, parsing, and GPU upload caches.
//!
//! Sources:
//! * `rbxassetid://N`, `rbxassetid://N?version=V`, `…/asset/?id=N`, or a bare
//!   number: a cloud asset, read from the disk cache or downloaded (needs a
//!   signed-in account or API key with `legacy-asset:manage`).
//! * `rbxasset://path`: inside the content folder the person chooses
//!   (RUSTUDIO_CONTENT_DIR, else ~/.local/share/rustudio/content).
//! * `file://` URLs and plain paths (relative ones next to the open file).

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rustudio_account::api::{public_asset_typed, AssetDownloadError};

use glam::{Vec2, Vec3};

use crate::mesh;
use crate::texture::{self, Rgba};

// Shared by render, audio and script preload requests. 0 = None, 1 = Loading,
// 2 = Success, 3 = Failure; keys use one canonical asset URI.
static CONTENT_STATUS: std::sync::LazyLock<Mutex<HashMap<String, u8>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
fn content_key(id: &str) -> String {
    match asset_ref(id) { Some(a) => format!("rbxassetid://{}{}",a.id,a.version.map(|v|format!("?version={v}")).unwrap_or_default()), None => id.to_string() }
}
/// Counts every change of a content status: who polls statuses (the
/// AssetFetchStatus signals, domains/assets_cloud.rs) looks again only when
/// this moved.
static CONTENT_STATUS_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub fn content_status_generation() -> u64 {
    CONTENT_STATUS_GEN.load(std::sync::atomic::Ordering::Relaxed)
}
pub fn content_fetch_status(id: &str) -> u8 {
    if id.is_empty() {return 3}
    CONTENT_STATUS.lock().ok().and_then(|s|s.get(&content_key(id)).copied()).unwrap_or(0)
}
pub fn content_queue_size() -> usize {
    CONTENT_STATUS.lock().map(|s|s.values().filter(|v|**v==1).count()).unwrap_or(0)
}
static CONTENT_FAILURES: Mutex<(u64,Vec<(u64,String)>)> = Mutex::new((0,Vec::new()));
pub fn content_failures_since(cursor: u64) -> (u64,Vec<String>) {
    let f=CONTENT_FAILURES.lock().unwrap_or_else(|e|e.into_inner());
    (f.0,f.1.iter().filter(|(seq,_)|*seq>cursor).map(|(_,id)|id.clone()).collect())
}
pub fn content_track(id: &str, status: u8) {
    if let Ok(mut s)=CONTENT_STATUS.lock(){
        let old=s.insert(content_key(id),status);
        if old!=Some(status) {
            CONTENT_STATUS_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if status==3 && old!=Some(3) && asset_ref(id).is_some() {
            let mut f=CONTENT_FAILURES.lock().unwrap_or_else(|e|e.into_inner());
            f.0+=1;let seq=f.0;f.1.push((seq,id.to_string()));
            if f.1.len()>256 {f.1.remove(0);}
        }
    }
}
fn track_fetch(id: &str, result: &Fetch) {
    content_track(id,match result {Fetch::Ready(_)=>2,Fetch::Pending=>1,Fetch::Unavailable(_)=>3});
}

pub type SharedClient = Arc<Mutex<Option<rustudio_account::Client>>>;

pub fn content_dir() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTUDIO_CONTENT_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let link = crate::paths::data().join("rustudio/content");
    if !link.exists() {
        // No link to a Studio install: Studio installed on this Mac, else
        // the copy rustudio downloaded.
        if let Some(d) = crate::paths::installed_studio().or_else(crate::studio_content::installed) {
            return d.join("content");
        }
    }
    link
}

/// The asset cache folder: RUSTUDIO_CACHE_DIR, else ~/.cache/rustudio.
pub fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTUDIO_CACHE_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::paths::cache().join("rustudio")
}

/// An optional local asset store: a folder of `<id>` or `<id>.<ext>` files,
/// named by `RUSTUDIO_LOCAL_ASSETS`. Off unless that is set. Read only.
pub fn local_assets_dir() -> Option<PathBuf> {
    // (or the rustudio option: crates/rustudio-core/src/rustudio_settings.rs)
    crate::rustudio_settings::local_assets()
}

fn local_asset(id: u64) -> Option<Vec<u8>> {
    local_asset_path(id).and_then(|p| std::fs::read(p).ok())
}

/// The file behind an asset id on this machine: the local store
/// (`RUSTUDIO_LOCAL_ASSETS`, if set), then the rbxasset upload ledger
/// (~/.local/share/rbxasset/ledger.json: your uploads → their original files).
pub fn local_asset_path(id: u64) -> Option<PathBuf> {
    local_store_path(id).or_else(|| ledger_path(id))
}

/// The local store's entry for `id` (`local_assets_dir`). Used for images only
/// when Roblox's copy can't be had (`fetch_cached`): such a folder may hold
/// stand-ins of another size, and a smaller image drawn in place of a sprite
/// sheet puts every ImageRect frame in the wrong cell.
fn local_store_path(id: u64) -> Option<PathBuf> {
    if let Some(dir) = local_assets_dir() {
        for ext in [
            "",
            ".mesh",
            ".png",
            ".jpg",
            ".dds",
            ".wav",
            ".ogg",
            ".mp3",
            ".webm",
            ".rbxm",
            ".rbxmx",
            ".anim.rbxm",
            ".decal.rbxmx",
            ".fbx",
        ] {
            let p = dir.join(format!("{id}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// An upload's source file on this machine: rbxasset's upload ledger
/// (`path` per entry), else rustudio's own `asset-sources.json`
/// (`{"<asset id>": "<file>"}`, ~/.local/share/rustudio/) for uploads whose
/// recorded source is gone (a /tmp upload copy) but an equivalent file
/// exists. Needed because Roblox refuses another creator's private models to
/// a collaborator's session (a live Team Create join), so the place's own
/// InsertService:LoadAsset packs can only come from disk.
fn ledger_path(id: u64) -> Option<PathBuf> {
    let data = crate::paths::data();
    let ledger = std::env::var("RUSTUDIO_ASSET_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data.join("rbxasset/ledger.json"));
    let sources = std::env::var("RUSTUDIO_ASSET_SOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data.join("rustudio/asset-sources.json"));
    static LEDGER: Mutex<Option<(std::time::SystemTime, HashMap<u64, PathBuf>)>> = Mutex::new(None);
    static SOURCES: Mutex<Option<(std::time::SystemTime, HashMap<u64, PathBuf>)>> = Mutex::new(None);
    path_map(&LEDGER, &ledger, id)
        .or_else(|| path_map(&SOURCES, &sources, id))
}

/// One id → file map from a JSON file, reloaded when the file changes. Takes
/// either the ledger's shape (objects with `assetId` + `path`) or plain
/// `"id": "path"` pairs. Only files that exist count.
fn path_map(
    cache: &Mutex<Option<(std::time::SystemTime, HashMap<u64, PathBuf>)>>,
    file: &std::path::Path,
    id: u64,
) -> Option<PathBuf> {
    let mtime = std::fs::metadata(file).and_then(|m| m.modified()).ok()?;
    let mut guard = cache.lock().ok()?;
    if guard.as_ref().is_none_or(|(t, _)| *t != mtime) {
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        let mut map = HashMap::new();
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if let Some(path) = v.as_str() {
                    if let Ok(aid) = k.parse() {
                        map.insert(aid, PathBuf::from(path));
                    }
                    continue;
                }
                let aid = v.get("assetId").and_then(|a| {
                    a.as_u64()
                        .or_else(|| a.as_str().and_then(|s| s.parse().ok()))
                });
                if let (Some(aid), Some(path)) = (aid, v.get("path").and_then(|p| p.as_str())) {
                    map.insert(aid, PathBuf::from(path));
                }
            }
        }
        *guard = Some((mtime, map));
    }
    guard.as_ref()?.1.get(&id).filter(|p| p.is_file()).cloned()
}

/// Content made while the program runs (meshes and textures from imported
/// FBX models), under `rbxtemp://rustudio/...` ids.
static GENERATED: Mutex<Option<HashMap<String, Arc<Vec<u8>>>>> = Mutex::new(None);

pub fn register_generated(id: &str, bytes: Vec<u8>) {
    if let Ok(mut g) = GENERATED.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(id.to_string(), Arc::new(bytes));
    }
}

/// Release content whose owning Play DataModel has ended.
pub fn unregister_generated(id:&str){
    if let Ok(mut generated)=GENERATED.lock(){if let Some(generated)=generated.as_mut(){generated.remove(id);}}
}

pub fn generated(id: &str) -> Option<Arc<Vec<u8>>> {
    if !id.starts_with("rbxtemp://") {
        return None;
    }
    GENERATED.lock().ok()?.as_ref()?.get(id).cloned()
}

/// A cloud asset reference: id and optional version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetRef {
    pub id: u64,
    pub version: Option<u64>,
}

/// Parses the asset id forms Roblox content uses.
pub fn asset_ref(content: &str) -> Option<AssetRef> {
    let s = content.trim();
    let num = |t: &str| -> Option<u64> {
        t.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    };
    let version = |t: &str| t.find("version=").and_then(|i| num(&t[i + 8..]));
    if let Some(rest) = s.strip_prefix("rbxassetid://") {
        return num(rest).map(|id| AssetRef {
            id,
            version: version(rest),
        });
    }
    // `?ID=` counts too (old models: `http://www.roblox.com/asset/?ID=94746105`).
    let lower = s.to_ascii_lowercase();
    if let Some(i) = lower.find("?id=").or_else(|| lower.find("&id=")) {
        if lower.contains("roblox.com") {
            return num(s[i + 4..].trim_ascii_start()).map(|id| AssetRef {
                id,
                version: version(s),
            });
        }
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse().ok().map(|id| AssetRef { id, version: None });
    }
    if let Some(rest) = s.strip_prefix("rbxasset://") {
        return legacy_content(rest).map(|id| AssetRef { id, version: None });
    }
    None
}

/// The asset id Studio loads for an old built-in file that content/ no
/// longer ships (`rbxasset://fonts/sword.mesh` is asset 12221720): Studio's
/// LegacyContentTable, `assets/legacy-content.tsv`. Paths match without case
/// (Studio's content folder is on Windows). A file that is there locally
/// (content/ or PlatformContent/pc) still wins.
fn legacy_content(rest: &str) -> Option<u64> {
    static TABLE: std::sync::OnceLock<HashMap<String, u64>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| legacy_table(&content_dir()));
    legacy_lookup(table, rest)
}

/// The legacy table less the files `content` (or its PlatformContent/pc)
/// still has.
fn legacy_table(content: &Path) -> HashMap<String, u64> {
    let platform = content
        .canonicalize()
        .ok()
        .and_then(|c| c.parent().map(|p| p.join("PlatformContent/pc")));
    include_str!("../../../assets/legacy-content.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .filter(|(p, _)| !content.join(p).is_file() && !platform.as_ref().is_some_and(|d| d.join(p).is_file()))
        .filter_map(|(p, id)| Some((p.to_ascii_lowercase(), id.trim().parse().ok()?)))
        .collect()
}

fn normalize_rbxasset(rest: &str) -> String {
    rest.replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

fn legacy_lookup(table: &HashMap<String, u64>, rest: &str) -> Option<u64> {
    let key = normalize_rbxasset(rest).to_ascii_lowercase();
    table.get(&key).copied()
}

/// A local file for a non-cloud content id.
pub fn resolve(id: &str, doc_dir: Option<&Path>) -> Option<PathBuf> {
    let id = id.trim();
    if let Some(rest) = id.strip_prefix("rbxasset://") {
        let rest_norm = normalize_rbxasset(rest);
        let rest = rest_norm.as_str();
        // rbxasset:// names a file in the content folder, nothing outside it.
        if !Path::new(rest)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        let p = content_dir().join(rest);
        // Like Studio: what isn't in content/ may be in StudioContent (built-in
        // plugin & DeveloperFramework UI textures), PlatformContent/pc (default
        // skybox, terrain), or ExtraContent beside it.
        if !p.exists() {
            if let Some(alt) = content_dir()
                .canonicalize()
                .ok()
                .and_then(|c| c.parent().map(|s| s.to_path_buf()))
                .into_iter()
                .chain(crate::default_scripts::studio_dir())
                .chain(crate::paths::installed_studio())
                .chain(crate::paths::windows_studio_install())
                .flat_map(|s| [
                    s.join("content").join(rest),
                    s.join("StudioContent").join(rest),
                    s.join("PlatformContent/pc").join(rest),
                    s.join("ExtraContent").join(rest),
                ])
                .find(|a| a.is_file())
            {
                return Some(alt);
            }
            // The retired death sound: Studio plays sounds/oof.ogg (its 2022
            // replacement) for it; rustudio plays the classic Oof once fetched.
            if matches!(rest.to_ascii_lowercase().as_str(), "sounds/uuhhh.mp3" | "sounds/uuhhh.wav") {
                let classic = crate::studio_content::classic_oof();
                if classic.is_file() && crate::rustudio_settings::classic_death_sound_use() {
                    return Some(classic);
                }
                crate::studio_content::fetch_classic_oof_once();
                let oof = content_dir().join("sounds/oof.ogg");
                if oof.is_file() {
                    return Some(oof);
                }
            }
            // An old built-in file Studio loads by asset id instead.
            if legacy_content(rest).is_some() {
                return None;
            }
        }
        return Some(p);
    }
    if let Some(rest) = id.strip_prefix("rbxlocaltheme://") {
        let rest_norm = normalize_rbxasset(rest);
        let name = rest_norm.trim_end_matches(".png");
        if name.is_empty()
            || !Path::new(name)
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return None;
        }
        let svg_roots: Vec<PathBuf> = [
            content_dir().join("studio_svg_textures/Shared"),
            content_dir().join("studio_svg_textures/Lua"),
        ]
        .into_iter()
        .chain(
            content_dir()
                .canonicalize()
                .ok()
                .and_then(|c| c.parent().map(|s| s.to_path_buf()))
                .into_iter()
                .chain(crate::default_scripts::studio_dir())
                .chain(crate::paths::installed_studio())
                .chain(crate::paths::windows_studio_install())
                .flat_map(|s| {
                    [
                        s.join("content/studio_svg_textures/Shared"),
                        s.join("content/studio_svg_textures/Lua"),
                    ]
                }),
        )
        .filter(|d| d.is_dir())
        .collect();
        for root in &svg_roots {
            for theme in ["Dark", "Light"] {
                for rel in [
                    format!("WidgetIcons/{theme}/Large/{name}.png"),
                    format!("WidgetIcons/{theme}/Standard/{name}.png"),
                    format!("InsertableObjects/{theme}/Large/{name}.png"),
                    format!("InsertableObjects/{theme}/Standard/{name}.png"),
                    format!("Ribbon/{theme}/Medium/Ribbon{name}.png"),
                    format!("Ribbon/{theme}/Large/Ribbon{name}.png"),
                    format!("Ribbon/{theme}/Standard/Ribbon{name}.png"),
                    format!("Ribbon/{theme}/Standard/Ribbon{name}Small.png"),
                    format!("Ribbon/{theme}/Medium/{name}.png"),
                    format!("Ribbon/{theme}/Large/{name}.png"),
                    format!("Ribbon/{theme}/Standard/{name}.png"),
                ] {
                    let p = root.join(rel);
                    if p.is_file() {
                        return Some(p);
                    }
                }
                if let Ok(rd) = std::fs::read_dir(root) {
                    for entry in rd.flatten() {
                        let cat = entry.path();
                        if !cat.is_dir() {
                            continue;
                        }
                        for size in ["Large", "Medium", "Standard"] {
                            for fname in [
                                format!("{name}.png"),
                                format!("Ribbon{name}.png"),
                                format!("Ribbon{name}Small.png"),
                            ] {
                                let p = cat.join(theme).join(size).join(fname);
                                if p.is_file() {
                                    return Some(p);
                                }
                            }
                        }
                    }
                }
            }
        }
        return None;
    }
    if let Some(rest) = id.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    if id.contains("://") || asset_ref(id).is_some() {
        return None;
    }
    let p = PathBuf::from(id);
    if p.is_absolute() {
        Some(p)
    } else if let Some(d) = doc_dir {
        Some(d.join(p))
    } else {
        // Play's DataModel copies have no file: an opted-in place folder
        // (note_opened_place) still names where its relative content lives.
        let dirs = opted_in_folders().lock().ok()?.clone();
        dirs.iter().map(|d| d.join(&p)).find(|f| f.is_file())
    }
}

pub enum Fetch {
    Ready(Arc<Vec<u8>>),
    Pending,
    Unavailable(String),
}

/// Downloads cloud assets in the background into the disk cache.
pub struct AssetStore {
    client: Option<SharedClient>,
    /// Treat scripts as running whatever the live state (tests).
    pub scripts_running: bool,
    pending: HashSet<AssetRef>,
    failed: HashMap<AssetRef, String>,
    memory: HashMap<AssetRef, Arc<Vec<u8>>>,
    tx: Sender<(AssetRef, Result<(), String>)>,
    rx: Receiver<(AssetRef, Result<(), String>)>,
    /// Lines for the Output panel.
    pub messages: Vec<String>,
    /// Said once: Roblox wants a signed-in session for these downloads.
    auth_told: bool,
    /// Asked for while the session check was running, or failed before a
    /// sign-in: handed back from `poll` for the caches to retry.
    deferred: Vec<AssetRef>,
    /// Results `wait_pending` took off the channel, for the next `poll`.
    early: Vec<(AssetRef, Result<(), String>)>,
}

impl Default for AssetStore {
    fn default() -> Self {
        let (tx, rx) = channel();
        AssetStore {
            client: None,
            scripts_running: false,
            pending: HashSet::new(),
            failed: HashMap::new(),
            memory: HashMap::new(),
            tx,
            rx,
            messages: Vec::new(),
            auth_told: false,
            deferred: Vec::new(),
            early: Vec::new(),
        }
    }
}

/// The signed-in session for background loads (set with the asset store's).
static SESSION: Mutex<Option<SharedClient>> = Mutex::new(None);

pub fn session_client() -> Option<SharedClient> {
    SESSION.lock().ok()?.clone()
}

/// Whether a session is signed in (for downloads that wait).
pub fn signed_in() -> bool {
    session_client().is_some()
}

/// A session check is under way (set when it starts).
static CHECKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn session_checking(on: bool) {
    CHECKING.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// The session check ended (from its worker thread): signed in or not.
pub fn session_ready(client: Option<SharedClient>) {
    let signed_in = client.is_some();
    if let Ok(mut g) = SESSION.lock() {
        *g = client;
    }
    if signed_in {
        forget_refusals();
    }
    session_checking(false);
}

// ------------------------------------------------------------ refusals

/// Assets delivery refused this process (private to their owner, deleted,
/// moderated): asking again costs a request and gets the same answer. A
/// place's preload lists name its owner's private animations by the
/// hundred; without this every PreloadAsync (the server's, then each
/// client's) and every play of them asked again. A login session can
/// change the answer, so it empties the list.
static REFUSED: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);

fn refused(id: u64) -> Option<String> {
    REFUSED.lock().ok()?.as_ref()?.get(&id).cloned()
}

fn note_refused(id: u64, why: &str) {
    if let Ok(mut g) = REFUSED.lock() {
        g.get_or_insert_with(HashMap::new).insert(id, why.to_string());
    }
}

/// A session arrived (or changed): what was private may be served now.
pub fn forget_refusals() {
    if let Ok(mut g) = REFUSED.lock() {
        *g = None;
    }
}

/// Notes a refusal and gives the error back as text.
fn download_error(id: u64, error: AssetDownloadError) -> String {
    if let AssetDownloadError::Refused(_) = &error {
        note_refused(id, &error.to_string());
    }
    error.to_string()
}

// ------------------------------------------------------- shared downloads

/// One download per asset at a time: a second asker for the same asset
/// (a preload and the first play of an animation; the server's and the
/// client's preloads of one list; a part's texture asked by the scene while
/// a script preloads it) waits for the first's bytes instead of asking
/// delivery again.
static INFLIGHT: LazyLock<Mutex<HashMap<AssetRef, Arc<Inflight>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Default)]
struct Inflight {
    done: Mutex<Option<Result<Arc<Vec<u8>>, String>>>,
    changed: Condvar,
}

/// Runs `download` (which also writes the cache file) unless another
/// thread is already downloading `a`, in which case its result is shared.
fn download_shared(a: AssetRef, download: impl FnOnce() -> Result<Vec<u8>, String>) -> Result<Arc<Vec<u8>>, String> {
    let (slot, first) = {
        let mut m = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(&a) {
            Some(slot) => (slot.clone(), false),
            None => {
                let slot = Arc::new(Inflight::default());
                m.insert(a, slot.clone());
                (slot, true)
            }
        }
    };
    if !first {
        let mut done = slot.done.lock().unwrap_or_else(|e| e.into_inner());
        while done.is_none() {
            done = slot.changed.wait(done).unwrap_or_else(|e| e.into_inner());
        }
        return done.clone().unwrap();
    }
    // Publishes on every exit, a panic in `download` included: nobody
    // waits forever on a download that never finished.
    struct Publish(AssetRef, Arc<Inflight>);
    impl Drop for Publish {
        fn drop(&mut self) {
            let mut done = self.1.done.lock().unwrap_or_else(|e| e.into_inner());
            if done.is_none() {
                *done = Some(Err("download did not finish".into()));
            }
            drop(done);
            self.1.changed.notify_all();
            if let Ok(mut m) = INFLIGHT.lock() {
                m.remove(&self.0);
            }
        }
    }
    let publish = Publish(a, slot.clone());
    let result = download().map(Arc::new);
    *slot.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
    drop(publish);
    result
}

/// How soon a download is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Something waits for it: a part on screen, a sound told to play, an
    /// animation a track was made for.
    Needed,
    /// ContentProvider:PreloadAsync, warming the cache: after everything
    /// needed, and never on more than a few of the download slots.
    Preload,
}

/// Preloads downloading at once, out of [`MAX_ASSET_DOWNLOADS`]: the rest
/// of the slots stay free for what is needed now.
pub const MAX_PRELOAD_DOWNLOADS: usize = 4;

struct Scheduler {
    state: Mutex<Queues>,
    changed: Condvar,
}

#[derive(Default)]
struct Queues {
    needed: std::collections::VecDeque<DownloadJob>,
    preload: std::collections::VecDeque<DownloadJob>,
    preload_active: usize,
}

impl Scheduler {
    fn push(&self, priority: Priority, job: DownloadJob) {
        let mut q = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match priority {
            Priority::Needed => q.needed.push_back(job),
            Priority::Preload => q.preload.push_back(job),
        }
        drop(q);
        self.changed.notify_one();
    }

    /// The next job: a needed one, else a preload while fewer than
    /// [`MAX_PRELOAD_DOWNLOADS`] run.
    fn next(&self) -> (DownloadJob, Priority) {
        let mut q = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(job) = q.needed.pop_front() {
                return (job, Priority::Needed);
            }
            if q.preload_active < MAX_PRELOAD_DOWNLOADS {
                if let Some(job) = q.preload.pop_front() {
                    q.preload_active += 1;
                    return (job, Priority::Preload);
                }
            }
            q = self.changed.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finished(&self, priority: Priority) {
        if priority == Priority::Preload {
            let mut q = self.state.lock().unwrap_or_else(|e| e.into_inner());
            q.preload_active -= 1;
            drop(q);
            self.changed.notify_all();
        }
    }

    #[cfg(test)]
    fn queued(&self) -> (usize, usize) {
        let q = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (q.needed.len(), q.preload.len())
    }
}

/// Queues a download job for the asset workers.
pub fn schedule(priority: Priority, job: impl FnOnce() + Send + 'static) {
    LazyLock::force(&ASSET_WORKERS);
    SCHEDULER.push(priority, Box::new(job));
}

static SCHEDULER: LazyLock<Scheduler> = LazyLock::new(|| Scheduler {
    state: Mutex::new(Queues::default()),
    changed: Condvar::new(),
});

/// How long ContentProvider:PreloadAsync waits for its list, in seconds
/// (RUSTUDIO_PRELOAD_DEADLINE overrides; domains/assets_cloud.rs). Roblox
/// waits for each asset to arrive or time out; a place that preloads 2000
/// assets from the server script that then starts its game would wait
/// minutes on a slow link. What is still queued at the deadline is
/// reported TimedOut and goes on downloading.
pub fn preload_deadline() -> f64 {
    std::env::var("RUSTUDIO_PRELOAD_DEADLINE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(20.0)
}

/// A download held open (tests): every asker of `a` waits on it until
/// [`DownloadHold::release`] hands them the bytes, as a slow link would.
#[doc(hidden)]
pub struct DownloadHold(AssetRef, Arc<Inflight>);

#[doc(hidden)]
pub fn hold_download(a: AssetRef) -> DownloadHold {
    let slot = Arc::new(Inflight::default());
    INFLIGHT.lock().unwrap_or_else(|e| e.into_inner()).insert(a, slot.clone());
    DownloadHold(a, slot)
}

impl DownloadHold {
    pub fn release(self, result: Result<Vec<u8>, String>) {
        *self.1.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.map(Arc::new));
        self.1.changed.notify_all();
        if let Ok(mut m) = INFLIGHT.lock() {
            m.remove(&self.0);
        }
    }
}

/// ContentProvider:PreloadAsync's fetch of one content id, after what the
/// scene and the scripts need now; `done` gets true once the bytes are
/// cached (false: can't be had). One queued download serves every asker
/// of the same id (the server's preload, then the client's).
pub fn preload_content(id: &str, done: Arc<Mutex<Option<bool>>>) {
    static WAITERS: Mutex<Option<HashMap<String, Vec<Arc<Mutex<Option<bool>>>>>>> = Mutex::new(None);
    let key = content_key(id);
    {
        let mut g = WAITERS.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(list) = map.get_mut(&key) {
            list.push(done);
            return;
        }
        map.insert(key.clone(), vec![done]);
    }
    let id = id.to_string();
    schedule(Priority::Preload, move || {
        let ok = script_content_blocking(&id).is_ok();
        let waiters = WAITERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|m| m.remove(&key))
            .unwrap_or_default();
        for w in waiters {
            *w.lock().unwrap_or_else(|e| e.into_inner()) = Some(ok);
        }
    });
}

/// Waits (up to `max`) for a session check that is still running, so a
/// download made right at Play start sees the session (LoadAsset in a
/// server script's first frame).
pub fn wait_for_session(max: std::time::Duration) -> bool {
    let t0 = std::time::Instant::now();
    while CHECKING.load(std::sync::atomic::Ordering::SeqCst) && t0.elapsed() < max {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    signed_in()
}

/// Whether an asset can be had: already on disk, or a session can fetch it.
pub fn reachable(id: u64) -> bool {
    cache_path(AssetRef { id, version: None }).exists()
        || local_asset_path(id).is_some()
        || session_client().is_some()
}

/// A public asset's bytes, blocking: the disk cache, the local asset store,
/// else public asset delivery (anonymous; docs/MATERIALS.md), saved to the cache.
/// For background threads (material packs, animations, avatar parts).
pub fn public_cached(id: u64) -> Result<Arc<Vec<u8>>, String> {
    let key=format!("rbxassetid://{id}");
    content_track(&key,1);
    let result=fetch_cached(id,true);
    content_track(&key,if result.is_ok(){2}else{3});
    result
}

/// Roblox's own copy of an asset (disk cache, else download), skipping the
/// local store: the upload ledger holds an upload's source file (a decal's
/// .png), not the asset Roblox made of it (a Model holding the Decal).
pub fn remote_cached(id: u64) -> Result<Arc<Vec<u8>>, String> {
    let key=format!("rbxassetid://{id}");
    content_track(&key,1);
    let result=fetch_cached(id,false);
    content_track(&key,if result.is_ok(){2}else{3});
    result
}

fn fetch_cached(id: u64, local: bool) -> Result<Arc<Vec<u8>>, String> {
    let a = AssetRef { id, version: None };
    if let Ok(b) = std::fs::read(cache_path(a)) {
        return Ok(Arc::new(b));
    }
    // The upload ledger's source files first (the person's own uploads);
    // the local store only after Roblox's copy (see `local_store_path`).
    if let Some(b) = local.then(|| ledger_path(id).and_then(|p| std::fs::read(p).ok())).flatten() {
        return Ok(Arc::new(b));
    }
    let fetched = match refused(id) {
        Some(why) => Err(why),
        None => download_shared(a, || {
            if let Ok(b) = std::fs::read(cache_path(a)) {
                return Ok(b);
            }
            let b = download_asset_bytes(id)?;
            write_cache(a, &b);
            Ok(b)
        }),
    };
    match fetched {
        Ok(b) => Ok(b),
        Err(e) => {
            if let Some(b) = local.then(|| local_store_path(id).and_then(|p| std::fs::read(p).ok())).flatten() {
                return Ok(Arc::new(b));
            }
            Err(e)
        }
    }
}

/// Saves downloaded bytes under the asset's cache path.
fn write_cache(a: AssetRef, bytes: &[u8]) {
    let path = cache_path(a);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, bytes);
}

const MAX_ASSET_DOWNLOADS: usize = 12;
const ASSET_BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

fn retry_after_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let delay = if let Ok(seconds) = value.parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        // Retry-After can also be an HTTP date.
        let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        SystemTime::from(date)
            .duration_since(now)
            .unwrap_or_default()
    };
    Some(delay.min(MAX_RETRY_AFTER))
}

type DownloadResult = Result<Vec<u8>, AssetDownloadError>;
type DownloadJob = Box<dyn FnOnce() + Send>;

// Only queued jobs (`schedule`) use these workers; they take needed
// downloads before preloads. Blocking callers use the same download
// permits directly, including when waiting out a shared pause.
static ASSET_WORKERS: LazyLock<()> = LazyLock::new(|| {
    for i in 0..MAX_ASSET_DOWNLOADS {
        let _ = std::thread::Builder::new().name(format!("asset-download-{i}")).spawn(move || loop {
            let (job, priority) = SCHEDULER.next();
            {
                let _p = crate::profiler::scope("asset_download", crate::profiler::Cat::Assets);
                job();
            }
            SCHEDULER.finished(priority);
        });
    }
});

static ASSET_DOWNLOADS: LazyLock<AssetDownloads> =
    LazyLock::new(|| AssetDownloads::new(ASSET_BACKOFF));

struct AssetDownloads {
    state: Mutex<DownloadState>,
    changed: Condvar,
    backoff: [Duration; ASSET_BACKOFF.len()],
}

struct DownloadState {
    active: usize,
    not_before: Instant,
}

struct DownloadPermit<'a> {
    downloads: &'a AssetDownloads,
    pause_until: Option<Instant>,
}

impl Drop for DownloadPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.downloads.state.lock().unwrap();
        if let Some(until) = self.pause_until {
            state.not_before = state.not_before.max(until);
        }
        state.active -= 1;
        self.downloads.changed.notify_all();
    }
}

impl AssetDownloads {
    fn new(backoff: [Duration; ASSET_BACKOFF.len()]) -> Self {
        Self {
            state: Mutex::new(DownloadState {
                active: 0,
                not_before: Instant::now(),
            }),
            changed: Condvar::new(),
            backoff,
        }
    }

    fn acquire(&self, ready_at: Instant) -> DownloadPermit<'_> {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.active == MAX_ASSET_DOWNLOADS {
                state = self.changed.wait(state).unwrap();
                continue;
            }
            let wait = state
                .not_before
                .max(ready_at)
                .saturating_duration_since(Instant::now());
            if wait.is_zero() {
                state.active += 1;
                return DownloadPermit {
                    downloads: self,
                    pause_until: None,
                };
            }
            (state, _) = self.changed.wait_timeout(state, wait).unwrap();
        }
    }

    fn retry(&self, mut download: impl FnMut() -> DownloadResult) -> DownloadResult {
        let mut ready_at = Instant::now();
        for attempt in 0..=self.backoff.len() {
            let mut permit = self.acquire(ready_at);
            let result = download();
            let Err(AssetDownloadError::Transient {
                status,
                retry_after,
                ..
            }) = &result
            else {
                return result;
            };
            // Even a 429 on the last attempt pauses everyone else.
            let backoff = self.backoff[attempt.min(self.backoff.len() - 1)];
            let delay = retry_after
                .as_ref()
                .and_then(|header| header.to_str().ok())
                .and_then(|value| retry_after_delay(value, SystemTime::now()))
                .map_or(backoff, |delay| backoff.max(delay));
            ready_at = Instant::now() + delay;
            if *status == 429 {
                permit.pause_until = Some(ready_at);
            }
            if attempt == self.backoff.len() {
                return result;
            }
        }
        unreachable!()
    }

    fn editor_download(
        &self,
        version: Option<u64>,
        account: Option<impl FnMut() -> DownloadResult>,
        public: impl FnMut() -> DownloadResult,
    ) -> DownloadResult {
        match account.map(|download| self.retry(download)) {
            Some(Ok(bytes)) => Ok(bytes),
            Some(Err(error @ AssetDownloadError::Transient { .. })) => Err(error),
            Some(Err(error)) if version.is_some() => Err(error),
            Some(Err(error)) => self.retry(public).map_err(|public| {
                let both = rustudio_account::Error::new(format!("{error}; {public}"));
                match public {
                    AssetDownloadError::Transient { status, retry_after, .. } => {
                        AssetDownloadError::Transient { status, retry_after, error: both }
                    }
                    AssetDownloadError::Refused(_) => AssetDownloadError::Refused(both),
                    AssetDownloadError::Other(_) => AssetDownloadError::Other(both),
                }
            }),
            None if version.is_some() => Err(AssetDownloadError::Other(rustudio_account::Error::new(
                "sign in (Account) to download asset versions",
            ))),
            None => self.retry(public),
        }
    }
}

/// Roblox's copy of an asset: Studio's login session, else public asset
/// delivery (waiting a little for a session check still running).
fn download_asset_bytes(id: u64) -> Result<Vec<u8>, String> {
    // Prefer Studio's login session. Its downloader preserves Roblox-Place-Id,
    // which asset delivery requires for assets private to the current place.
    let public = || ASSET_DOWNLOADS.retry(|| public_asset_typed(id));
    let result = if let Some(c) = session_client() {
        let downloader = {
            let guard = c.lock().map_err(|_| "account busy".to_string())?;
            guard.as_ref().map(|c| c.session.downloader())
        };
        match downloader {
            Some(d) => ASSET_DOWNLOADS.retry(|| d.download_asset_typed(id, None)),
            None => public(),
        }
    } else {
        match public() {
            Err(error @ (AssetDownloadError::Refused(_) | AssetDownloadError::Other(_))) => {
                let session = wait_for_session(Duration::from_secs(30))
                    .then(session_client)
                    .flatten();
                let Some(c) = session else {
                    return Err(download_error(id, error));
                };
                let d = {
                    let guard = c.lock().map_err(|_| "account busy".to_string())?;
                    match guard.as_ref() {
                        Some(c) => c.session.downloader(),
                        None => return Err(download_error(id, error)),
                    }
                };
                ASSET_DOWNLOADS.retry(|| d.download_asset_typed(id, None))
            }
            result => result,
        }
    };
    result.map_err(|error| download_error(id, error))
}

/// Any content id's bytes, blocking (background threads): generated
/// meshes, local files (rbxasset://, paths), else a public asset.
pub fn content_blocking(id: &str) -> Result<Arc<Vec<u8>>, String> {
    content_tracked(id, true)
}

// ------------------------------------------------- place content in Play

/// Play/Run sessions alive (each holds a [`ScriptsRunning`]).
static SCRIPTS_RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Held while a place's scripts run (a Play/Run session): place content then
/// loads only Roblox content, see [`place_content_ok`].
pub struct ScriptsRunning(());
impl ScriptsRunning {
    pub fn new() -> Self {
        SCRIPTS_RUNNING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ScriptsRunning(())
    }
}
impl Drop for ScriptsRunning {
    fn drop(&mut self) {
        SCRIPTS_RUNNING.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
pub fn scripts_running() -> bool {
    SCRIPTS_RUNNING.load(std::sync::atomic::Ordering::SeqCst) > 0
}

/// RUSTUDIO_ALLOW_LOCAL_CONTENT=1: local development, place properties may
/// name files on this computer in Play too (scripts can then read them back
/// through audio analysis or captures, so only for your own places).
pub fn local_content_opt_in() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RUSTUDIO_ALLOW_LOCAL_CONTENT").is_ok_and(|v| v == "1"))
}

/// Whether `id` names a file on this computer (`file://`, an absolute or a
/// relative path) rather than Roblox content (asset ids, `rbxasset://` in
/// the content folder, engine-generated `rbxtemp://`).
pub fn is_local_path(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && (id.starts_with("file://") || (!id.contains("://") && asset_ref(id).is_none()))
}

/// The marker that opts one folder's files in as place content during Play.
pub const LOCAL_CONTENT_MARKER: &str = ".rustudio-local-content";

/// Folders of opened places that carry [`LOCAL_CONTENT_MARKER`] (canonical).
fn opted_in_folders() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static DIRS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    DIRS.get_or_init(Default::default)
}

/// Called when a place file opens: if its folder holds [`LOCAL_CONTENT_MARKER`]
/// (e.g. a converted level with its meshes/ and textures/ beside it), files
/// inside that folder stay loadable while Play runs. Only that folder: a place
/// saved straight into the home folder without the marker exposes nothing.
pub fn note_opened_place(path: &Path) {
    // Canonicalise the file first: a bare "level.rbxlx" has an empty parent.
    let Some(dir) = path.canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else { return };
    if dir.join(LOCAL_CONTENT_MARKER).is_file() {
        let mut dirs = opted_in_folders().lock().unwrap_or_else(|e| e.into_inner());
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
}

/// `id`'s file when it lies inside an opted-in place folder (relative ids
/// resolve against those folders; Play's DataModel copies have no file path).
fn opted_in_file(id: &str) -> Option<PathBuf> {
    let dirs = opted_in_folders().lock().ok()?.clone();
    if dirs.is_empty() || !is_local_path(id) {
        return None;
    }
    let direct = resolve(id, None).into_iter();
    let relative = dirs.iter().filter_map(|d| resolve(id, Some(d)));
    direct.chain(relative).filter_map(|p| p.canonicalize().ok())
        .find(|p| p.is_file() && dirs.iter().any(|d| p.starts_with(d)))
}

/// Whether place content (a property's image, mesh or sound) may load given
/// whether scripts are running. Roblox never loads local files for a place;
/// rustudio's editor does (docs/MECHANICS.md), but not while scripts run: what is
/// rendered or played can be read back (CaptureService +
/// CreateEditableImageAsync, GetWaveformAsync, AudioAnalyzer, raycasts and
/// unions against a mesh). Checked before any cache keyed by id (#16).
pub fn place_content_allowed(id: &str, running: bool) -> bool {
    !running || !is_local_path(id) || local_content_opt_in() || opted_in_file(id).is_some()
}

/// [`place_content_allowed`] for the live Play state.
pub fn place_content_ok(id: &str) -> bool {
    place_content_allowed(id, scripts_running())
}

const LOCAL_IN_PLAY: &str = "local files aren't place content while scripts run (RUSTUDIO_ALLOW_LOCAL_CONTENT=1 allows them)";

/// [`content_blocking`] for a running script that gets back what it loads
/// (EditableImage pixels, EditableMesh vertices, video frames): Roblox content
/// only. `file://` and plain paths would let a place's scripts read the
/// person's own files. One exception (rustudio only; Roblox has no local files):
/// a file inside the folder of an opened place that carries
/// [`LOCAL_CONTENT_MARKER`], named as its MeshId / TextureId would name it
/// (a path relative to that folder, or absolute inside it). The same folders
/// already load as place content in Play; nothing else on disk is reachable
/// (`..` out of the folder and symlinks pointing out are refused by the
/// canonical-prefix check in [`opted_in_file`]).
pub fn script_content_blocking(id: &str) -> Result<Arc<Vec<u8>>, String> {
    if is_local_path(id) {
        if let Some(p) = opted_in_file(id) {
            content_track(id, 1);
            let r = std::fs::read(&p).map(Arc::new).map_err(|e| format!("{}: {e}", p.display()));
            content_track(id, if r.is_ok() { 2 } else { 3 });
            return r;
        }
    }
    content_tracked(id, false)
}

fn content_tracked(id: &str, paths: bool) -> Result<Arc<Vec<u8>>, String> {
    content_track(id,1);
    let result=content_blocking_inner(id, paths);
    content_track(id,if result.is_ok(){2}else{3});
    result
}
fn content_blocking_inner(id: &str, paths: bool) -> Result<Arc<Vec<u8>>, String> {
    if paths && !place_content_ok(id) {
        return Err(format!("{id}: {LOCAL_IN_PLAY}"));
    }
    if let Some(b) = generated(id) {
        return Ok(b);
    }
    if let Some(a) = asset_ref(id) {
        return public_cached(a.id);
    }
    match resolve(id, None) {
        Some(p) if paths || id.trim().starts_with("rbxasset://") => std::fs::read(&p)
            .map(Arc::new)
            .map_err(|e| format!("{}: {e}", p.display())),
        _ => Err(format!("{id} is not a local or cloud asset id")),
    }
}

/// Non-blocking [`public_cached`]: Ready once the bytes are on disk (or in
/// the local store), Pending while a background thread downloads them.
/// Scripts use it so an asset load never stalls a frame (Roblox loads
/// animations the same way: a track's Length is 0 until its asset arrives).
pub fn public_prefetch(id: u64) -> Fetch {
    let key=format!("rbxassetid://{id}");
    content_track(&key,1);
    let result=public_prefetch_inner(id);
    // A fast worker may already have completed before Pending is returned.
    if !matches!(&result,Fetch::Pending){track_fetch(&key,&result);}
    result
}
fn public_prefetch_inner(id: u64) -> Fetch {
    use std::sync::Mutex;
    static STATES: Mutex<Option<HashMap<u64, Option<Result<(), String>>>>> = Mutex::new(None);
    let a = AssetRef { id, version: None };
    // (a local store file isn't "on disk" here: Roblox's copy comes first)
    if cache_path(a).exists() || ledger_path(id).is_some() {
        return match public_cached(id) {
            Ok(b) => Fetch::Ready(b),
            Err(e) => Fetch::Unavailable(e),
        };
    }
    let mut g = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    match map.get(&id) {
        Some(Some(Err(e))) => return Fetch::Unavailable(e.clone()),
        Some(Some(Ok(()))) => {
            return match public_cached(id) {
                Ok(b) => Fetch::Ready(b),
                Err(e) => Fetch::Unavailable(e),
            }
        }
        Some(None) => return Fetch::Pending,
        None => {}
    }
    // --fixed-dt: fetched here and now, so a script polling for it (the
    // animation library's task.wait loop) waits the same frames every run.
    if crate::sim_clock::on() {
        drop(g);
        let r = public_cached(id);
        content_track(&format!("rbxassetid://{id}"), if r.is_ok() { 2 } else { 3 });
        if let Ok(mut g) = STATES.lock() {
            g.get_or_insert_with(HashMap::new).insert(id, Some(r.as_ref().map(|_| ()).map_err(|e| e.clone())));
        }
        return match r {
            Ok(b) => Fetch::Ready(b),
            Err(e) => Fetch::Unavailable(e),
        };
    }
    map.insert(id, None);
    drop(g);
    // On the asset workers, ahead of preloads (a track was made for this
    // clip, a sound was told to play): a thread per id made a client
    // warming 1400 animations spawn 1400 threads.
    schedule(Priority::Needed, move || {
        let r = public_cached(id).map(|_| ());
        content_track(&format!("rbxassetid://{id}"),if r.is_ok(){2}else{3});
        if let Ok(mut g) = STATES.lock() {
            g.get_or_insert_with(HashMap::new).insert(id, Some(r));
        }
    });
    Fetch::Pending
}

/// Fetches a `rbxthumb://type=...&id=...&w=...&h=...` image via Roblox's
/// public thumbnails v1 API, cached in memory and under `cache_dir()/thumbs/`.
fn fetch_rbxthumb(uri: &str) -> Fetch {
    type States = HashMap<String, Option<Result<Arc<Vec<u8>>, String>>>;
    static STATES: Mutex<Option<States>> = Mutex::new(None);
    let Some(rest) = uri.strip_prefix("rbxthumb://") else {
        return Fetch::Unavailable("not an rbxthumb uri".into());
    };
    let (mut kind, mut id, mut w, mut h) = ("", 0u64, 150u32, 150u32);
    for pair in rest.split('&') {
        let Some((k, v)) = pair.split_once('=') else { continue };
        match k {
            "type" => kind = v,
            "id" => id = v.parse().unwrap_or(0),
            "w" => w = v.parse().unwrap_or(150),
            "h" => h = v.parse().unwrap_or(150),
            _ => {}
        }
    }
    if id == 0 || kind.is_empty() {
        return Fetch::Unavailable(format!("invalid rbxthumb id ({kind}:{id})"));
    }
    // Snap to standard Roblox thumbnail square sizes so the API never returns 400.
    let valid: &[u32] = match kind {
        "AvatarHeadShot" | "AvatarBust" => &[48, 60, 75, 100, 110, 150, 180, 352, 420, 720],
        "Avatar" | "Outfit" => &[30, 48, 60, 75, 100, 110, 140, 150, 180, 250, 352, 420, 720],
        _ => &[50, 150, 256, 420, 512],
    };
    let req_sz = w.max(h);
    let sz = *valid
        .iter()
        .find(|&&v| v >= req_sz)
        .unwrap_or(valid.last().unwrap_or(&150));
    let key = format!("{kind}_{id}_{sz}x{sz}");
    let mut g = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    match map.get(&key) {
        Some(Some(Ok(b))) => return Fetch::Ready(b.clone()),
        Some(Some(Err(e))) => return Fetch::Unavailable(e.clone()),
        Some(None) => return Fetch::Pending,
        None => {}
    }
    let disk = cache_dir().join("thumbs").join(format!("{key}.png"));
    if let Ok(b) = std::fs::read(&disk) {
        if !b.is_empty() {
            let b = Arc::new(b);
            map.insert(key, Some(Ok(b.clone())));
            return Fetch::Ready(b);
        }
    }
    let api_url = match kind {
        "AvatarHeadShot" => format!(
            "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "AvatarBust" => format!(
            "https://thumbnails.roblox.com/v1/users/avatar-bust?userIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "Avatar" => format!(
            "https://thumbnails.roblox.com/v1/users/avatar?userIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "Asset" => format!(
            "https://thumbnails.roblox.com/v1/assets?assetIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "GameIcon" => format!(
            "https://thumbnails.roblox.com/v1/games/icons?universeIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "BadgeIcon" => format!(
            "https://thumbnails.roblox.com/v1/badges/icons?badgeIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "GroupIcon" => format!(
            "https://thumbnails.roblox.com/v1/groups/icons?groupIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        "Outfit" => format!(
            "https://thumbnails.roblox.com/v1/users/outfits?userOutfitIds={id}&size={sz}x{sz}&format=Png&isCircular=false"
        ),
        _ => return Fetch::Unavailable(format!("unsupported rbxthumb type '{kind}'")),
    };
    map.insert(key.clone(), None);
    std::thread::spawn(move || {
        let r = (|| -> Result<Arc<Vec<u8>>, String> {
            let agent = rustudio_account::api::agent();
            let mut resp = agent.get(&api_url).call().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status().as_u16()));
            }
            let body = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
            let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            let img_url = v
                .get("data")
                .and_then(|d| d.as_array())
                .and_then(|a| a.first())
                .and_then(|e| e.get("imageUrl"))
                .and_then(|u| u.as_str())
                .filter(|u| !u.is_empty())
                .ok_or_else(|| "thumbnail not ready".to_string())?;
            let mut img_resp = agent.get(img_url).call().map_err(|e| e.to_string())?;
            if !img_resp.status().is_success() {
                return Err(format!("image HTTP {}", img_resp.status().as_u16()));
            }
            let bytes = img_resp
                .body_mut()
                .with_config()
                .limit(8 * 1024 * 1024)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            if let Some(parent) = disk.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&disk, &bytes);
            Ok(Arc::new(bytes))
        })();
        if let Ok(mut g) = STATES.lock() {
            g.get_or_insert_with(HashMap::new).insert(key, Some(r));
        }
    });
    Fetch::Pending
}

/// MarketplaceService:GetProductInfo's catalog record, non-blocking like
/// [`public_prefetch`]: Ready with the JSON body, Pending while a background
/// thread asks the web, or the error Roblox raises (the HTTP status). Kind is
/// Enum.InfoType's name; only Asset and GamePass have public endpoints.
/// Roblox's reply for an asset is economy's details record, field for field
/// (Name, Creator, AssetTypeId, IsForSale, …).
pub fn product_info(id: u64, kind: &str) -> Fetch {
    type States = HashMap<(u64, String), Option<Result<Arc<Vec<u8>>, String>>>;
    static STATES: Mutex<Option<States>> = Mutex::new(None);
    let key = (id, kind.to_string());
    let mut g = STATES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    match map.get(&key) {
        Some(Some(Ok(b))) => return Fetch::Ready(b.clone()),
        Some(Some(Err(e))) => return Fetch::Unavailable(e.clone()),
        Some(None) => return Fetch::Pending,
        None => {}
    }
    let url = match kind {
        "Asset" => format!("https://economy.roblox.com/v2/assets/{id}/details"),
        "GamePass" => format!("https://apis.roblox.com/game-passes/v1/game-passes/{id}/product-info"),
        // Developer products, bundles, subscriptions: their endpoints need
        // the experience's universe; an empty record, as before round 26.
        _ => return Fetch::Ready(Arc::new(b"{}".to_vec())),
    };
    map.insert(key.clone(), None);
    std::thread::spawn(move || {
        let r = (|| {
            let mut r = rustudio_account::api::agent()
                .get(&url)
                .call()
                .map_err(|e| e.to_string())?;
            let status = r.status();
            if !status.is_success() {
                return Err(format!(
                    "HTTP {} ({})",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Error")
                ));
            }
            let body = r.body_mut().read_to_string().map_err(|e| e.to_string())?;
            Ok(Arc::new(body.into_bytes()))
        })();
        if let Ok(mut g) = STATES.lock() {
            g.get_or_insert_with(HashMap::new).insert(key, Some(r));
        }
    });
    Fetch::Pending
}

pub fn cache_path(a: AssetRef) -> PathBuf {
    let v = a
        .version
        .map(|v| v.to_string())
        .unwrap_or_else(|| "latest".into());
    cache_dir()
        .join("assets")
        .join(format!("{}_v{v}.bin", a.id))
}

impl AssetStore {
    /// Gives the store the account's client (None when signed out).
    pub fn set_client(&mut self, client: Option<SharedClient>) {
        let had = self.client.is_some();
        if had != client.is_some() {
            if let Ok(mut g) = SESSION.lock() {
                *g = client.clone();
            }
        }
        self.client = client;
        if !had && self.client.is_some() {
            // Retry what failed only because nobody was signed in.
            forget_refusals();
            self.deferred.extend(self.failed.drain().map(|(a, _)| a));
            self.auth_told = false;
        }
    }

    /// The bytes of a cloud asset: from memory, the disk cache, or a
    /// background download.
    pub fn fetch(&mut self, a: AssetRef) -> Fetch {
        let key=format!("rbxassetid://{}{}",a.id,a.version.map(|v|format!("?version={v}")).unwrap_or_default());
        content_track(&key,1);
        let result=self.fetch_inner(a);
        if !matches!(&result,Fetch::Pending){track_fetch(&key,&result);}
        result
    }
    fn fetch_inner(&mut self, a: AssetRef) -> Fetch {
        if let Some(b) = self.memory.get(&a) {
            return Fetch::Ready(b.clone());
        }
        // Downloading: its bytes reach the disk cache only when it's done.
        // (Asked first: every part waiting on it asks again each time any
        // asset arrives, and the disk cache read and local asset lookup
        // below were ~100k instructions per ask in Place B.)
        if self.pending.contains(&a) {
            return Fetch::Pending;
        }
        if let Ok(b) = std::fs::read(cache_path(a)) {
            let b = Arc::new(b);
            self.memory.insert(a, b.clone());
            return Fetch::Ready(b);
        }
        // The person's own upload's source file; the local store only once
        // Roblox's copy failed (`poll`, `local_store_path`).
        if let Some(b) = ledger_path(a.id).and_then(|p| std::fs::read(p).ok()) {
            let b = Arc::new(b);
            self.memory.insert(a, b.clone());
            return Fetch::Ready(b);
        }
        if let Some(why) = self.failed.get(&a) {
            return Fetch::Unavailable(why.clone());
        }
        if a.version.is_none() {
            if let Some(why) = refused(a.id) {
                self.failed.insert(a, why.clone());
                return Fetch::Unavailable(why);
            }
        }
        // The session check is still running: wait for it rather than
        // fail anonymously (most assets need the session).
        if self.client.is_none() && CHECKING.load(std::sync::atomic::Ordering::SeqCst) {
            if !self.deferred.contains(&a) {
                self.deferred.push(a);
            }
            return Fetch::Pending;
        }
        // Try the account first when signed in. Unversioned assets can
        // also use anonymous delivery after a non-transient refusal.
        // The check's worker sets SESSION a frame before `set_client` runs.
        let client = self.client.clone().or_else(session_client);
        self.pending.insert(a);
        let tx = self.tx.clone();
        schedule(Priority::Needed, move || {
            let result = download_shared(a, || {
                // (a preload may have cached it while this job queued)
                if let Ok(bytes) = std::fs::read(cache_path(a)) {
                    return Ok(bytes);
                }
                // Copy the session out so downloads run side by side.
                let downloader =
                    client.and_then(|client| Some(client.lock().ok()?.as_ref()?.session.downloader()));
                let bytes = ASSET_DOWNLOADS
                    .editor_download(
                        a.version,
                        downloader.map(|d| move || d.download_asset_typed(a.id, a.version)),
                        || public_asset_typed(a.id),
                    )
                    .map_err(|error| download_error(a.id, error))?;
                let path = cache_path(a);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
                Ok(bytes)
            })
            .map(|_| ());
            content_track(
                &format!(
                    "rbxassetid://{}{}",
                    a.id,
                    a.version.map(|v| format!("?version={v}")).unwrap_or_default()
                ),
                if result.is_ok() { 2 } else { 3 },
            );
            let _ = tx.send((a, result));
        });
        Fetch::Pending
    }

    /// Finished downloads since last call (callers retry those ids).
    pub fn poll(&mut self) -> Vec<AssetRef> {
        let mut done = Vec::new();
        if !self.deferred.is_empty() && !CHECKING.load(std::sync::atomic::Ordering::SeqCst) {
            done.append(&mut self.deferred);
        }
        let mut got = std::mem::take(&mut self.early);
        got.extend(self.rx.try_iter());
        for (a, r) in got {
            self.pending.retain(|p| *p != a);
            match r {
                Ok(()) => {
                    self.messages.push(format!("Downloaded asset {}", a.id));
                    done.push(a);
                }
                Err(e) => {
                    if let Some(b) = a
                        .version
                        .is_none()
                        .then(|| local_store_path(a.id).and_then(|p| std::fs::read(p).ok()))
                        .flatten()
                    {
                        self.memory.insert(a, Arc::new(b));
                        done.push(a);
                        continue;
                    }
                    if !self.auth_told && self.client.is_none() && (e.contains("Authentication required") || e.contains("not authorized")) {
                        self.auth_told = true;
                        self.messages.push(
                            "Roblox only gives most assets to a signed-in session: sign in (Account) \
                             to see this place's meshes and textures. Until then they're left out, \
                             as Studio shows assets it can't load."
                                .to_string(),
                        );
                    }
                    self.messages
                        .push(format!("Asset {} could not be downloaded: {e}", a.id));
                    self.failed.insert(a, e);
                }
            }
        }
        done
    }

    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Downloads asked for and not finished.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// --fixed-dt (crates/rustudio-core/src/sim_clock.rs): waits until every download in flight
    /// has finished (at most `max`), so an asset asked for in one frame is
    /// handed out by the next `poll`, however slow the network or machine.
    pub fn wait_pending(&mut self, max: std::time::Duration) {
        let until = std::time::Instant::now() + max;
        let mut left: HashSet<AssetRef> = self.pending.clone();
        for (a, _) in &self.early {
            left.remove(a);
        }
        while !left.is_empty() {
            let now = std::time::Instant::now();
            if now >= until {
                break;
            }
            match self.rx.recv_timeout(until - now) {
                Ok(r) => {
                    left.remove(&r.0);
                    self.early.push(r);
                }
                Err(_) => break,
            }
        }
    }

    /// Why a cloud asset isn't here (round 23's asset report): the
    /// download error, "downloading", or None when it is (or wasn't asked).
    pub fn why_not(&self, id: &str) -> Option<String> {
        let a = asset_ref(id)?;
        if let Some(e) = self.failed.get(&a) {
            return Some(e.clone());
        }
        self.pending.contains(&a).then(|| "still downloading".to_string())
    }

    /// Whether place content `id` may load now ([`place_content_allowed`]):
    /// callers with a cache keyed by id ask before looking in it.
    pub fn allows(&self, id: &str) -> bool {
        place_content_allowed(id, self.scripts_running || scripts_running())
    }

    /// Bytes for any content id (cloud or local; local only while no
    /// scripts run).
    pub fn content(&mut self, id: &str, doc_dir: Option<&Path>) -> Fetch {
        if !self.allows(id) {
            return Fetch::Unavailable(format!("{id}: {LOCAL_IN_PLAY}"));
        }
        let result=self.content_inner(id,doc_dir);
        track_fetch(id,&result);
        result
    }
    fn content_inner(&mut self, id: &str, doc_dir: Option<&Path>) -> Fetch {
        if let Some(b) = generated(id) {
            return Fetch::Ready(b);
        }
        if id.starts_with("rbxthumb://") {
            return fetch_rbxthumb(id);
        }
        if let Some(a) = asset_ref(id) {
            return self.fetch(a);
        }
        match resolve(id, doc_dir) {
            Some(p) => match std::fs::read(&p) {
                Ok(b) => Fetch::Ready(Arc::new(b)),
                Err(e) => Fetch::Unavailable(format!("{}: {e}", p.display())),
            },
            None => Fetch::Unavailable(format!("{id} is not a local or cloud asset id")),
        }
    }
}

/// A mesh normalised to the unit box (-0.5..0.5) so it can be stretched to
/// any size, plus its original extent.
#[derive(Debug, Clone, Copy)]
pub struct LoadedMesh {
    pub index: u32,
    pub extent: Vec3,
    /// The centre of the mesh's own bounds (what the unit box was centred on).
    pub center: Vec3,
}

/// Parses mesh bytes and normalises them into the unit box.
pub fn load_mesh(bytes: &[u8]) -> Result<(Vec<Vec3>, Vec<Vec3>, Vec<Vec2>, Vec3, Vec<glam::Vec4>), String> {
    let (p,n,u,e,c,_,_) = load_mesh_projected(bytes)?;
    Ok((p,n,u,e,c))
}

/// `load_mesh` plus each corner's projection plane and the centre of the
/// mesh's own bounds (what the unit box is centred on).
fn load_mesh_projected(bytes: &[u8]) -> Result<(Vec<Vec3>, Vec<Vec3>, Vec<Vec2>, Vec3, Vec<glam::Vec4>, Vec<glam::Vec4>, Vec3), String> {
    normalize_mesh(mesh::parse(bytes)?)
}

fn normalize_mesh(m: mesh::MeshData) -> Result<(Vec<Vec3>, Vec<Vec3>, Vec<Vec2>, Vec3, Vec<glam::Vec4>, Vec<glam::Vec4>, Vec3), String> {
    if m.positions.is_empty() {
        return Err("the mesh has no triangles".into());
    }
    let extent = (m.max - m.min).max(Vec3::splat(1e-4));
    let center = (m.max + m.min) * 0.5;
    if std::env::var_os("RUSTUDIO_MESH_DENSITY_TRACE").is_some() {
        eprintln!("MESH_BOUNDS {:?}..{:?} extent={:?} authored_uvs={} projection_tags={}", m.min, m.max, extent, m.uvs.len(), m.projection_tags.len());
    }
    let positions = m.positions.iter().map(|p| (*p - center) / extent).collect();
    // Normals are pre-scaled by the extent; the shader divides by the drawn size.
    let normals = m.normals.iter().map(|n| *n * extent).collect();
    let planes = (0..m.positions.len()).map(|i| extent.extend(m.projection_tags.get(i).copied().unwrap_or(0) as f32)).collect();
    let colors = m.gpu_colors();
    Ok((positions, normals, m.uvs, extent, colors, planes, center))
}

/// Uploads positions, normals, UVs and (possibly empty) per-corner colours.
pub type MeshUpload<'a> = dyn FnMut(&[Vec3], &[Vec3], &[Vec2], &[glam::Vec4], &[glam::Vec4], &mesh::MeshLods) -> u32 + 'a;
/// Uploads an image; the second argument is the texture slot it replaces
/// (`None`: a new one), so the slot itself stays with the renderer.
pub type TextureUpload<'a> = dyn FnMut(&Rgba, Option<u32>) -> u32 + 'a;

/// Loaded meshes by content id (None = failed for good).
#[derive(Default)]
pub struct MeshCache {
    entries: HashMap<String, Option<LoadedMesh>>,
    /// Per id: its bytes are in a Roblox mesh format (mesh::studio_format).
    roblox_format: HashMap<String, bool>,
    /// Why an entry is None (fetch or parse error).
    reasons: HashMap<String, String>,
    /// Loaded ids whose mesh carries no UVs (every corner at 0,0): a
    /// SolidMesh union stores none (its UV array is empty in all 5658 of
    /// a game's solid mesh holders), so there are none to keep.
    uvless: std::collections::HashSet<String>,
    /// Loaded ids whose mesh file contains skinning data (bones & weights).
    skinned: std::collections::HashSet<String>,
    /// Messages for the Output panel (one per new failure).
    pub messages: Vec<String>,
}

impl MeshCache {
    /// `id`'s mesh; `colors` false: without its vertex colours when the
    /// mesh is in a Roblox format (a SpecialMesh's, scene.rs
    /// MeshAsset::vertex_colors: Studio drops them), cached apart. Decided
    /// by the bytes, once per id: a local file only rustudio can read (OBJ)
    /// keeps its colours (the exception noted at MeshAsset::vertex_colors).
    pub fn get_colored(
        &mut self,
        id: &str,
        colors: bool,
        doc_dir: Option<&Path>,
        store: &mut AssetStore,
        upload: &mut MeshUpload,
    ) -> Option<LoadedMesh> {
        if colors {
            return self.get(id, doc_dir, store, upload);
        }
        let roblox = match self.roblox_format.get(id) {
            Some(r) => *r,
            None => match store.content(id, doc_dir) {
                Fetch::Ready(bytes) => {
                    let r = crate::mesh::studio_format(&bytes);
                    self.roblox_format.insert(id.to_string(), r);
                    r
                }
                Fetch::Pending => return None,
                // (`get` records the failure)
                Fetch::Unavailable(_) => true,
            },
        };
        if !roblox {
            return self.get(id, doc_dir, store, upload);
        }
        let key = format!("{id}#novc");
        if let Some(e) = self.entries.get(&key) {
            return *e;
        }
        let mut dropped = |p: &[Vec3], n: &[Vec3], u: &[Vec2], _c: &[glam::Vec4], planes: &[glam::Vec4], lods: &mesh::MeshLods| upload(p, n, u, &[], planes, lods);
        // (loaded apart from a MeshPart's coloured copy of the same mesh)
        let had = self.entries.remove(id);
        bump_mesh_generation();
        let got = self.get(id, doc_dir, store, &mut dropped);
        let failed = self.entries.contains_key(id);
        self.entries.remove(id);
        bump_mesh_generation();
        if let Some(h) = had {
            self.entries.insert(id.to_string(), h);
            bump_mesh_generation();
        }
        if got.is_some() || failed {
            self.entries.insert(key, got);
            bump_mesh_generation();
        }
        got
    }

    pub fn get(
        &mut self,
        id: &str,
        doc_dir: Option<&Path>,
        store: &mut AssetStore,
        upload: &mut MeshUpload,
    ) -> Option<LoadedMesh> {
        if let Some(spec) = id.strip_prefix("rustudio-truss:") {
            if let Some(cached) = self.entries.get(id) { return *cached; }
            let fields: Vec<_> = spec.split(':').collect();
            if fields.len() != 4 { return None; }
            let size = Vec3::new(
                f32::from_bits(u32::from_str_radix(fields[0], 16).ok()?),
                f32::from_bits(u32::from_str_radix(fields[1], 16).ok()?),
                f32::from_bits(u32::from_str_radix(fields[2], 16).ok()?),
            );
            let (p, n, uv) = crate::truss::generate(size, fields[3].parse().ok()?);
            let mesh = LoadedMesh { index: upload(&p, &n, &uv, &[], &[], &mesh::MeshLods::default()), extent: size, center: Vec3::ZERO };
            self.entries.insert(id.to_owned(), Some(mesh));
            bump_mesh_generation();
            return Some(mesh);
        }
        if !store.allows(id) {
            return None;
        }
        if let Some(e) = self.entries.get(id) {
            return *e;
        }
        let loaded = match store.content(id, doc_dir) {
            Fetch::Pending => return None,
            Fetch::Unavailable(why) => {
                if asset_ref(id).is_none() {
                    self.messages.push(format!(
                        "Mesh {id} not loaded ({why}); showing a placeholder"
                    ));
                }
                self.reasons.insert(id.to_string(), format!("not fetched: {why}"));
                None
            }
            Fetch::Ready(bytes) => match mesh::parse_with_lods(&bytes)
                .or_else(|_| mesh::parse(&bytes).map(|mesh| (mesh, mesh::MeshLods::default())))
                .and_then(|(mesh, lods)| {
                    let is_skinned = mesh.skin.is_some();
                    normalize_mesh(mesh).map(|geometry| (geometry, lods, is_skinned))
                }) {
                Ok(((pos, nrm, uv, extent, mut colors, planes, center), lods, is_skinned)) => {
                    // A union with UsePartColor: its own Color, not its children's.
                    if id.ends_with("#partcolor") {
                        colors.clear();
                    }
                    if uv.iter().all(|t| *t == Vec2::ZERO) {
                        self.uvless.insert(id.to_string());
                    }
                    if is_skinned {
                        self.skinned.insert(id.to_string());
                    }
                    Some(LoadedMesh {
                        index: upload(&pos, &nrm, &uv, &colors, &planes, &lods),
                        extent,
                        center,
                    })
                }
                Err(e) => {
                    self.messages
                        .push(format!("Mesh {id} not loaded: {e}; showing a placeholder"));
                    self.reasons.insert(id.to_string(), format!("not decoded: {e}"));
                    None
                }
            },
        };
        self.entries.insert(id.to_string(), loaded);
        bump_mesh_generation();
        loaded
    }

    /// A live EditableMesh copy (runtime/editable.rs): built from the mesh
    /// itself the first time, then patched in place by the viewport. None
    /// when `id` isn't live (or its mesh is gone: then its bytes hold it).
    pub fn live(&mut self, id: &str, upload: &mut dyn FnMut(&[crate::vertex::Vertex]) -> u32) -> Option<LoadedMesh> {
        if !crate::editable_source::is_live(id) {
            return None;
        }
        if let Some(Some(m)) = self.entries.get(id) {
            return Some(*m);
        }
        let (verts, extent) = crate::editable_source::live_build(id)?;
        if verts.is_empty() {
            return None;
        }
        let m = LoadedMesh { index: upload(&verts), extent, center: Vec3::ZERO };
        self.entries.insert(id.to_string(), Some(m));
        bump_mesh_generation();
        Some(m)
    }

    /// Whether `id`'s loaded mesh has UVs of its own (not all at 0,0).
    pub fn authored_uvs(&self, id: &str) -> bool {
        !self.uvless.contains(id)
    }

    /// Whether `id`'s loaded mesh file contains skinning data (bones & weights).
    pub fn has_skin(&self, id: &str) -> bool {
        self.skinned.contains(id)
    }

    /// Meshes asked for so far (loaded or failed).
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// The loaded mesh of `id`, if any.
    pub fn loaded(&self, id: &str) -> Option<LoadedMesh> {
        self.entries.get(id).copied().flatten()
    }

    /// Drops `id` so it loads again.
    pub fn forget(&mut self, id: &str) {
        self.entries.remove(id);
        bump_mesh_generation();
        self.uvless.remove(id);
    }

    /// Forget entries for assets that just finished downloading.
    pub fn retry_studio_content(&mut self) {
        self.entries.retain(|id, value| value.is_some() || !id.starts_with("rbxasset://"));
    }

    /// Forget entries for assets that just finished downloading.
    pub fn retry(&mut self, done: &[AssetRef]) {
        self.entries
            .retain(|id, _| asset_ref(id).is_none_or(|a| !done.contains(&a)));
        bump_mesh_generation();
    }

    /// Loaded (Ok), failed (Err with why), or never asked for (None).
    pub fn state(&self, id: &str) -> Option<Result<(), String>> {
        // (or its copy without vertex colours, `get_colored`)
        match self.entries.get(id).or_else(|| self.entries.get(&format!("{id}#novc")))? {
            Some(_) => Some(Ok(())),
            None => Some(Err(self.reasons.get(id).cloned().unwrap_or_else(|| "failed".into()))),
        }
    }
}

/// RUSTUDIO_SYNC_DECODE=1 (and --fixed-dt) decodes in the requesting frame.
/// Texture upload time per frame before further uploads wait a frame.
const UPLOAD_BUDGET_MS: f32 = 6.0;

/// Texture bytes (level 0 and mip chain) uploaded per frame before further
/// uploads wait a frame. With the mip chains made on the decode workers an
/// upload is only a copy, so the time budget alone let a map load push
/// hundreds of MB through the upload staging memory in a few frames.
/// RUSTUDIO_UPLOAD_BUDGET_MB overrides it (0: no byte budget).
const UPLOAD_BUDGET_BYTES: usize = 16 << 20;

fn upload_budget_bytes() -> usize {
    static B: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *B.get_or_init(|| parse_upload_budget(std::env::var("RUSTUDIO_UPLOAD_BUDGET_MB").ok().as_deref()))
}

/// At least 1 byte: a budget of 0 would hold every texture back for good
/// (the first upload of a frame starts at 0 bytes), e.g. "1e-9" or "nan".
fn parse_upload_budget(v: Option<&str>) -> usize {
    match v.and_then(|v| v.trim().parse::<f64>().ok()) {
        Some(mb) if mb.is_nan() => UPLOAD_BUDGET_BYTES,
        Some(mb) if mb <= 0.0 => usize::MAX,
        Some(mb) => ((mb * 1048576.0) as usize).max(1),
        None => UPLOAD_BUDGET_BYTES,
    }
}

fn sync_decode() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Fixed-step captures must not race wall-clock workers. Decode and upload
    // in the requesting frame, rather than blocking workers whose memory
    // budget can only be released by subsequent render passes.
    crate::sim_clock::on()
        || *ON.get_or_init(|| std::env::var_os("RUSTUDIO_SYNC_DECODE").is_some())
}

/// A texture on the GPU plus its pixels (clothing compositing reads them).
/// A texture on the GPU. Its pixels are not kept in memory after the upload
/// (a place's few hundred 1024² images held 4 MB each for good): what needs
/// them again asks [`TextureCache::pixels`].
#[derive(Clone, Copy)]
pub struct LoadedTexture {
    pub index: u32,
    pub width: u32,
    pub height: u32,
}

/// Released pixels kept around for the next composite that shares them.
const LOOSE_PIXELS_BYTES: usize = 192 << 20;

#[derive(Default)]
pub struct TextureCache {
    entries: HashMap<String, Option<LoadedTexture>>,
    /// Why an entry is None (fetch or decode error).
    reasons: HashMap<String, String>,
    /// Composites (clothing / SurfaceAppearance): key → GPU index.
    composites: HashMap<String, u32>,
    pub messages: Vec<String>,
    /// Decodes running on worker threads, and finished ones not yet uploaded.
    decoding: std::collections::HashSet<String>,
    decoded: HashMap<String, (texture::Decoded, u64)>,
    decode_frame: u64,
    /// Upload time spent this frame (mip chain + GPU copy), and whether a
    /// decoded texture was held back for the next frame by that budget.
    upload_ms: f32,
    upload_bytes: usize,
    deferred: bool,
    decode_chan: Option<(
        std::sync::mpsc::Sender<(String, texture::Decoded)>,
        std::sync::mpsc::Receiver<(String, texture::Decoded)>,
    )>,
    /// EditableImage textures and composites made from them, by key without
    /// generations (runtime/editable.rs): the generation they hold. A newer
    /// one is uploaded into the same GPU slot.
    editable_gens: HashMap<String, u64>,
    editable_composites: HashMap<String, (String, u32)>,
    /// Decoded pixels still wanted after their upload: of ids a composite is
    /// about to be made from (`pinned`), and released ones kept while recently
    /// wanted or under LOOSE_PIXELS_BYTES (`loose`).
    pixels: HashMap<String, Arc<Rgba>>,
    pinned: std::collections::HashSet<String>,
    last_wanted: HashMap<String, u64>,
    loose: std::collections::VecDeque<String>,
    loose_set: std::collections::HashSet<String>,
    loose_bytes: usize,
}

impl TextureCache {
    /// (decoding or waiting to upload, asked for so far).
    pub fn counts(&self) -> (usize, usize) {
        (self.decoding.len() + self.decoded.len(), self.entries.len())
    }

    /// An EditableImage's pixels (`rbxtemp://rustudio/editable/img/<n>@<gen>`),
    /// re-uploaded in place when they changed.
    fn editable_image(&mut self, id: &str, upload: &mut TextureUpload) -> Option<LoadedTexture> {
        let (base, gen) = crate::editable_source::image_key(id)?;
        let held = self.editable_gens.get(base).copied();
        if held.is_some_and(|h| h >= gen) {
            if let Some(Some(t)) = self.entries.get(base) {
                return Some(t.clone());
            }
        }
        let Some((now, img)) = crate::editable_source::image_pixels(id) else {
            return self.entries.get(base).cloned().flatten();
        };
        let reuse = self.entries.get(base).and_then(|t| t.map(|t| t.index));
        let index = upload(&img, reuse);
        let t = LoadedTexture { index, width: img.width, height: img.height };
        self.entries.insert(base.to_string(), Some(t));
        self.editable_gens.insert(base.to_string(), now.max(gen));
        Some(t)
    }

    pub fn image(
        &mut self,
        id: &str,
        doc_dir: Option<&Path>,
        store: &mut AssetStore,
        upload: &mut TextureUpload,
    ) -> Option<LoadedTexture> {
        if id.starts_with("rbxtemp://rustudio/editable/") {
            return self.editable_image(id, upload);
        }
        if !store.allows(id) {
            return None;
        }
        if let Some(e) = self.entries.get(id) {
            return *e;
        }
        if self.decoding.contains(id) {
            return None;
        }
        let decoded = match self.decoded.remove(id) {
            Some((d, _)) => Some(d),
            None => match store.content(id, doc_dir) {
                Fetch::Pending => return None,
                Fetch::Unavailable(why) => {
                    if asset_ref(id).is_none() && !id.starts_with("rbxthumb://") {
                        self.messages
                            .push(format!("Texture {id} not loaded ({why})"));
                    }
                    self.reasons.insert(id.to_string(), format!("not fetched: {why}"));
                    self.entries.insert(id.to_string(), None);
                    return None;
                }
                // Small images decode here; bigger ones on the decode
                // workers (the part waits and is redone on arrival).
                Fetch::Ready(bytes) if bytes.len() <= 16 * 1024 || sync_decode() => Some(texture::decode(&bytes).map(|i| (i, None, None))),
                Fetch::Ready(bytes) => {
                    let (tx, _) = self.decode_chan.get_or_insert_with(std::sync::mpsc::channel);
                    texture::decode_async(bytes, tx.clone(), id.to_string());
                    self.decoding.insert(id.to_string());
                    return None;
                }
            },
        };
        if decoded.is_some()
            && (self.upload_ms > UPLOAD_BUDGET_MS || self.upload_bytes >= upload_budget_bytes())
            && !sync_decode()
        {
            // Over this frame's budget: upload it next frame.
            if let Some(d) = decoded {
                self.decoded.insert(id.to_string(), (d, self.decode_frame));
            }
            self.deferred = true;
            return None;
        }
        let loaded = match decoded {
            None => None,
            Some(result) => match result {
                Ok((img, mips, _waiting)) => {
                    let t0 = std::time::Instant::now();
                    // (the chain holds level 0 too; made in add_texture otherwise)
                    self.upload_bytes += mips.as_ref().map_or(img.pixels.len() * 4 / 3, |m| m.len());
                    let index = crate::texture::with_mips(&img.pixels, mips, || upload(&img, None));
                    self.upload_ms += t0.elapsed().as_secs_f32() * 1000.0;
                    let t = LoadedTexture { index, width: img.width, height: img.height };
                    if self.pinned.contains(id) {
                        self.pixels.insert(id.to_string(), Arc::new(img));
                    }
                    Some(t)
                }
                Err(e) => {
                    self.messages.push(format!("Texture {id} not loaded: {e}"));
                    self.reasons.insert(id.to_string(), format!("not decoded: {e}"));
                    None
                }
            },
        };
        self.entries.insert(id.to_string(), loaded);
        loaded
    }

    /// Keep `id`'s pixels when it is uploaded: a composite will be made from
    /// them (ask before `image`; `release_pixels` once it is made).
    pub fn want_pixels(&mut self, id: &str) {
        if !self.pinned.contains(id) {
            self.pinned.insert(id.to_string());
        }
        if let Some(f) = self.last_wanted.get_mut(id) {
            *f = self.decode_frame;
        } else {
            self.last_wanted.insert(id.to_string(), self.decode_frame);
        }
    }

    fn queue_loose(&mut self, id: &str) {
        if self.loose_set.contains(id) {
            return;
        }
        if let Some(px) = self.pixels.get(id) {
            self.loose_bytes += px.pixels.len();
            self.loose_set.insert(id.to_string());
            self.loose.push_back(id.to_string());
        }
    }

    fn evict_loose(&mut self) {
        let mut checked = 0;
        let max_checks = self.loose.len();
        while checked < max_checks {
            let Some(id) = self.loose.pop_front() else {
                self.loose_bytes = 0;
                break;
            };
            checked += 1;
            let age = self
                .last_wanted
                .get(&id)
                .map_or(u64::MAX, |f| self.decode_frame.saturating_sub(*f));
            // Keep shared composite inputs alive for a few frames so other
            // waiting parts don't thrash re-decoding them when one part finishes.
            if self.pinned.contains(&id) || age <= 4 {
                self.loose.push_back(id);
                continue;
            }
            if self.loose_bytes <= LOOSE_PIXELS_BYTES && age <= 12 {
                self.loose.push_back(id);
                continue;
            }
            self.loose_set.remove(&id);
            if let Some(px) = self.pixels.remove(&id) {
                self.loose_bytes = self.loose_bytes.saturating_sub(px.pixels.len());
            }
            self.last_wanted.remove(&id);
        }
        if self.loose.is_empty() {
            self.loose_bytes = 0;
        }
    }

    /// The composite is made: the pixels wait a while for another one that
    /// shares them, then go.
    pub fn release_pixels<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        for id in ids {
            if self.pinned.remove(id) {
                self.queue_loose(id);
            }
        }
        if self.loose_bytes > LOOSE_PIXELS_BYTES {
            self.evict_loose();
        }
    }

    /// True when this frame has already spent its texture upload budget (used to
    /// spread SurfaceAppearance composite uploads across frames).
    pub fn over_upload_budget(&self) -> bool {
        (self.upload_ms > UPLOAD_BUDGET_MS || self.upload_bytes >= upload_budget_bytes())
            && !sync_decode()
    }

    /// Mark that a composite upload was deferred to the next frame by the upload budget.
    pub fn defer_upload(&mut self) {
        self.deferred = true;
    }

    /// Account for a composite's GPU upload against this frame's upload budget.
    pub fn record_upload(&mut self, bytes: usize, ms: f32) {
        self.upload_bytes += bytes;
        self.upload_ms += ms;
    }

    /// Fetch and decode an image's CPU pixels without uploading a standalone GPU
    /// texture (for SurfaceAppearance / emissive composite inputs). Uses the same
    /// async decode pool and failure tracking as `image()`.
    pub fn fetch_pixels(
        &mut self,
        id: &str,
        doc_dir: Option<&Path>,
        store: &mut AssetStore,
    ) -> Option<Arc<Rgba>> {
        if id.starts_with("rbxtemp://rustudio/editable/") {
            return crate::editable_source::image_pixels(id).map(|(_, img)| Arc::new(img));
        }
        if !store.allows(id) || self.failed(id) {
            return None;
        }
        if let Some(px) = self.pixels.get(id) {
            if let Some(f) = self.last_wanted.get_mut(id) {
                *f = self.decode_frame;
            }
            return Some(px.clone());
        }
        if self.decoding.contains(id) {
            return None;
        }
        let decoded = match self.decoded.remove(id) {
            Some((d, _)) => d,
            None => match store.content(id, doc_dir) {
                Fetch::Pending => return None,
                Fetch::Unavailable(why) => {
                    if asset_ref(id).is_none() {
                        self.messages.push(format!("Texture {id} not loaded ({why})"));
                    }
                    self.reasons.insert(id.to_string(), format!("not fetched: {why}"));
                    self.entries.insert(id.to_string(), None);
                    return None;
                }
                Fetch::Ready(bytes) if bytes.len() <= 16 * 1024 || sync_decode() => {
                    texture::decode(&bytes).map(|i| (i, None, None))
                }
                Fetch::Ready(bytes) => {
                    let (tx, _) = self.decode_chan.get_or_insert_with(std::sync::mpsc::channel);
                    texture::decode_async(bytes, tx.clone(), id.to_string());
                    self.decoding.insert(id.to_string());
                    return None;
                }
            },
        };
        match decoded {
            Ok((img, _mips, _waiting)) => {
                let px = Arc::new(img);
                self.pixels.insert(id.to_string(), px.clone());
                if !self.pinned.contains(id) {
                    self.queue_loose(id);
                    if self.loose_bytes > LOOSE_PIXELS_BYTES {
                        self.evict_loose();
                    }
                }
                Some(px)
            }
            Err(e) => {
                self.messages.push(format!("Texture {id} not loaded: {e}"));
                self.reasons.insert(id.to_string(), format!("not decoded: {e}"));
                self.entries.insert(id.to_string(), None);
                None
            }
        }
    }

    /// A loaded texture's pixels: the kept ones, else decoded again from its
    /// file (in this frame: rare, a composite made after its inputs went).
    pub fn pixels(&mut self, id: &str, doc_dir: Option<&Path>, store: &mut AssetStore) -> Option<Arc<Rgba>> {
        if id.starts_with("rbxtemp://rustudio/editable/") {
            return crate::editable_source::image_pixels(id).map(|(_, img)| Arc::new(img));
        }
        if let Some(px) = self.pixels.get(id) {
            return Some(px.clone());
        }
        let Fetch::Ready(bytes) = store.content(id, doc_dir) else { return None };
        let px = Arc::new(texture::decode(&bytes).ok()?);
        self.pixels.insert(id.to_string(), px.clone());
        if !self.pinned.contains(id) {
            self.queue_loose(id);
            if self.loose_bytes > LOOSE_PIXELS_BYTES {
                self.evict_loose();
            }
        }
        Some(px)
    }

    /// Collects finished background decodes; true when any arrived (the
    /// caller bumps its asset generation so waiting parts are redone).
    pub fn poll_decoded(&mut self) -> bool {
        self.decode_frame += 1;
        if !self.loose.is_empty() {
            self.evict_loose();
        }
        // Completed results for parts that left the scene must release their
        // Waiting tokens. Otherwise they permanently consume the worker budget.
        // Give arrivals several render passes to be requested; upload-budget
        // deferrals refresh their age in image(), so live textures keep waiting.
        self.decoded.retain(|_, (_, frame)| self.decode_frame - *frame <= 3);
        self.upload_ms = 0.0;
        self.upload_bytes = 0;
        let mut any = std::mem::take(&mut self.deferred);
        let Some((_, rx)) = &self.decode_chan else { return any };
        while let Ok((id, r)) = rx.try_recv() {
            self.decoding.remove(&id);
            self.decoded.insert(id, (r, self.decode_frame));
            any = true;
        }
        any
    }

    /// Decodes still running.
    pub fn decoding(&self) -> bool {
        !self.decoding.is_empty()
    }

    /// Loaded (Ok), failed (Err with why), or never asked for (None).
    pub fn state(&self, id: &str) -> Option<Result<(), String>> {
        match self.entries.get(id)? {
            Some(_) => Some(Ok(())),
            None => Some(Err(self.reasons.get(id).cloned().unwrap_or_else(|| "failed".into()))),
        }
    }

    /// Failed for good (not fetched or not decoded): a part showing it has
    /// nothing to wait for (Studio draws it without, and doesn't ask again).
    pub fn failed(&self, id: &str) -> bool {
        matches!(self.entries.get(id), Some(None))
    }

    /// Whether asking for `id` again can get further: false while its
    /// download or decode is still running (a part waiting on it is left
    /// alone until then).
    pub fn can_progress(&self, id: &str, store: &AssetStore) -> bool {
        if self.entries.contains_key(id) || self.decoded.contains_key(id) {
            return true;
        }
        if self.decoding.contains(id) {
            return false;
        }
        !asset_ref(id).is_some_and(|a| store.pending.contains(&a))
    }

    /// The composite `key`, if it is made already.
    pub fn composite_index(&self, key: &str) -> Option<u32> {
        if let Some(i) = self.composites.get(key) {
            return Some(*i);
        }
        let base = crate::editable_source::strip_generations(key)?;
        self.editable_composites.get(&base).filter(|(full, _)| full == key).map(|(_, i)| *i)
    }

    /// A cached composite, building it the first time.
    pub fn composite(
        &mut self,
        key: &str,
        build: impl FnOnce() -> Option<Rgba>,
        upload: &mut TextureUpload,
    ) -> Option<u32> {
        if let Some(i) = self.composites.get(key) {
            return Some(*i);
        }
        // Made from an EditableImage: one GPU slot per composite, whatever
        // the generation (the old one is replaced, not leaked).
        let stripped = crate::editable_source::strip_generations(key);
        let mut reuse = None;
        if let Some(base) = &stripped {
            if let Some((full, i)) = self.editable_composites.get(base) {
                if full == key {
                    return Some(*i);
                }
                reuse = Some(*i);
            }
        }
        let img = build()?;
        let i = upload(&img, reuse);
        match stripped {
            Some(base) => {
                self.editable_composites.insert(base, (key.to_string(), i));
            }
            None => {
                self.composites.insert(key.to_string(), i);
            }
        }
        Some(i)
    }

    pub fn retry_studio_content(&mut self) {
        self.entries.retain(|id, value| value.is_some() || !id.starts_with("rbxasset://"));
        self.composites.clear();
    }

    pub fn retry(&mut self, done: &[AssetRef]) {
        self.entries
            .retain(|id, _| asset_ref(id).is_none_or(|a| !done.contains(&a)));
        if !done.is_empty() {
            let done_ids: Vec<String> = done.iter().map(|a| a.id.to_string()).collect();
            self.composites
                .retain(|k, _| !done_ids.iter().any(|id| k.contains(id.as_str())));
        }
    }
}

// ------------------------------------------------------------------ clothing

/// The classic clothing template is 585×559; regions are (x, y, w, h).
pub const TEMPLATE_W: f32 = 585.0;
pub const TEMPLATE_H: f32 = 559.0;

/// Template regions for one box: front (−Z), back (+Z), +X side, −X side, top, bottom.
#[derive(Debug, Clone, Copy)]
pub struct BoxRegions {
    pub front: [f32; 4],
    pub back: [f32; 4],
    pub px: [f32; 4],
    pub nx: [f32; 4],
    pub top: [f32; 4],
    pub bottom: [f32; 4],
}

pub const TORSO: BoxRegions = BoxRegions {
    front: [231.0, 74.0, 128.0, 128.0],
    back: [427.0, 74.0, 128.0, 128.0],
    px: [165.0, 74.0, 64.0, 128.0],
    nx: [361.0, 74.0, 64.0, 128.0],
    top: [231.0, 8.0, 128.0, 64.0],
    bottom: [231.0, 204.0, 128.0, 64.0],
};

/// The character's right limb (+X side): right arm on shirts, right leg on pants.
pub const RIGHT_LIMB: BoxRegions = BoxRegions {
    front: [217.0, 355.0, 64.0, 128.0],
    back: [85.0, 355.0, 64.0, 128.0],
    px: [151.0, 355.0, 64.0, 128.0],
    nx: [19.0, 355.0, 64.0, 128.0],
    top: [217.0, 289.0, 64.0, 64.0],
    bottom: [217.0, 485.0, 64.0, 64.0],
};

pub const LEFT_LIMB: BoxRegions = BoxRegions {
    front: [308.0, 355.0, 64.0, 128.0],
    back: [440.0, 355.0, 64.0, 128.0],
    px: [506.0, 355.0, 64.0, 128.0],
    nx: [374.0, 355.0, 64.0, 128.0],
    top: [308.0, 289.0, 64.0, 64.0],
    bottom: [308.0, 485.0, 64.0, 64.0],
};

/// Which clothing image a body part uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClothingLayer {
    /// Pants under the shirt (torso parts).
    Torso,
    Shirt,
    Pants,
}

/// A body part's template mapping: regions, the vertical slice [t0, t1] of
/// the side regions (R15 parts split a limb), and the image to use.
#[derive(Debug, Clone, Copy)]
pub struct ClothingRole {
    pub regions: BoxRegions,
    pub t0: f32,
    pub t1: f32,
    pub layer: ClothingLayer,
}

/// The template role of a body part by its R6 / R15 name.
pub fn clothing_role(part: &str) -> Option<ClothingRole> {
    use ClothingLayer::*;
    let r = |regions, t0, t1, layer| {
        Some(ClothingRole {
            regions,
            t0,
            t1,
            layer,
        })
    };
    match part {
        "Torso" => r(TORSO, 0.0, 1.0, Torso),
        "UpperTorso" => r(TORSO, 0.0, 0.8, Torso),
        "LowerTorso" => r(TORSO, 0.8, 1.0, Torso),
        "Right Arm" => r(RIGHT_LIMB, 0.0, 1.0, Shirt),
        "Left Arm" => r(LEFT_LIMB, 0.0, 1.0, Shirt),
        "Right Leg" => r(RIGHT_LIMB, 0.0, 1.0, Pants),
        "Left Leg" => r(LEFT_LIMB, 0.0, 1.0, Pants),
        "RightUpperArm" => r(RIGHT_LIMB, 0.0, 0.4, Shirt),
        "RightLowerArm" => r(RIGHT_LIMB, 0.4, 0.85, Shirt),
        "RightHand" => r(RIGHT_LIMB, 0.85, 1.0, Shirt),
        "LeftUpperArm" => r(LEFT_LIMB, 0.0, 0.4, Shirt),
        "LeftLowerArm" => r(LEFT_LIMB, 0.4, 0.85, Shirt),
        "LeftHand" => r(LEFT_LIMB, 0.85, 1.0, Shirt),
        "RightUpperLeg" => r(RIGHT_LIMB, 0.0, 0.4, Pants),
        "RightLowerLeg" => r(RIGHT_LIMB, 0.4, 0.85, Pants),
        "RightFoot" => r(RIGHT_LIMB, 0.85, 1.0, Pants),
        "LeftUpperLeg" => r(LEFT_LIMB, 0.0, 0.4, Pants),
        "LeftLowerLeg" => r(LEFT_LIMB, 0.4, 0.85, Pants),
        "LeftFoot" => r(LEFT_LIMB, 0.85, 1.0, Pants),
        _ => None,
    }
}

/// A unit box whose faces map into the clothing template regions.
pub fn template_box(role: &ClothingRole) -> (Vec<Vec3>, Vec<Vec3>, Vec<Vec2>) {
    let h = 0.5;
    let mut pos = Vec::new();
    let mut nrm = Vec::new();
    let mut uvs = Vec::new();
    let to_uv = |rect: [f32; 4], s: f32, t: f32| {
        Vec2::new(
            (rect[0] + s * rect[2]) / TEMPLATE_W,
            (rect[1] + t * rect[3]) / TEMPLATE_H,
        )
    };
    let side_t = |t: f32| role.t0 + t * (role.t1 - role.t0);
    // (normal, region, s(p), t(p)) per face, same orientation as the renderer's box UVs.
    type Face = (Vec3, [f32; 4], fn(Vec3) -> f32, fn(Vec3) -> f32, bool);
    let faces: [Face; 6] = [
        (
            Vec3::NEG_Z,
            role.regions.front,
            |p| 0.5 - p.x,
            |p| 0.5 - p.y,
            true,
        ),
        (
            Vec3::Z,
            role.regions.back,
            |p| p.x + 0.5,
            |p| 0.5 - p.y,
            true,
        ),
        (Vec3::X, role.regions.px, |p| 0.5 - p.z, |p| 0.5 - p.y, true),
        (
            Vec3::NEG_X,
            role.regions.nx,
            |p| p.z + 0.5,
            |p| 0.5 - p.y,
            true,
        ),
        (
            Vec3::Y,
            role.regions.top,
            |p| 0.5 - p.x,
            |p| 0.5 - p.z,
            false,
        ),
        (
            Vec3::NEG_Y,
            role.regions.bottom,
            |p| 0.5 - p.x,
            |p| p.z + 0.5,
            false,
        ),
    ];
    for (n, rect, fs, ft, side) in faces {
        // Two axes spanning the face.
        let (u, v) = if n.x.abs() > 0.5 {
            (Vec3::Z, Vec3::Y)
        } else if n.y.abs() > 0.5 {
            (Vec3::X, Vec3::Z)
        } else {
            (Vec3::X, Vec3::Y)
        };
        let c = n * h;
        let corners = [
            c - u * h - v * h,
            c + u * h - v * h,
            c + u * h + v * h,
            c - u * h + v * h,
        ];
        // Outward winding.
        let (a, b, cc, d) = if (corners[1] - corners[0])
            .cross(corners[2] - corners[0])
            .dot(n)
            > 0.0
        {
            (corners[0], corners[1], corners[2], corners[3])
        } else {
            (corners[0], corners[3], corners[2], corners[1])
        };
        for p in [a, b, cc, a, cc, d] {
            let t = ft(p);
            let t = if side { side_t(t) } else { t };
            pos.push(p);
            nrm.push(n);
            uvs.push(to_uv(rect, fs(p), t));
        }
    }
    (pos, nrm, uvs)
}

/// Where a point on a body part's unit box (-0.5..0.5) with outward normal
/// `n` lands in the clothing template (the same faces as `template_box`).
pub fn template_uv(role: &ClothingRole, n: Vec3, p: Vec3) -> Vec2 {
    let p = p.clamp(Vec3::splat(-0.5), Vec3::splat(0.5));
    let a = n.abs();
    let r = &role.regions;
    let (rect, s, t, side) = if a.x >= a.y && a.x >= a.z {
        if n.x > 0.0 {
            (r.px, 0.5 - p.z, 0.5 - p.y, true)
        } else {
            (r.nx, p.z + 0.5, 0.5 - p.y, true)
        }
    } else if a.y >= a.z {
        if n.y > 0.0 {
            (r.top, 0.5 - p.x, 0.5 - p.z, false)
        } else {
            (r.bottom, 0.5 - p.x, p.z + 0.5, false)
        }
    } else if n.z < 0.0 {
        (r.front, 0.5 - p.x, 0.5 - p.y, true)
    } else {
        (r.back, p.x + 0.5, 0.5 - p.y, true)
    };
    let t = if side { role.t0 + t * (role.t1 - role.t0) } else { t };
    Vec2::new(
        (rect[0] + s * rect[2]) / TEMPLATE_W,
        (rect[1] + t * rect[3]) / TEMPLATE_H,
    )
}

/// Body part meshes (R6: Studio's shipped ones and package CharacterMeshes;
/// R15 MeshParts) are UV-mapped to Roblox's composited clothing atlases,
/// not to the 585×559 template; those atlases are the template's box faces
/// laid out again. So each triangle of such a mesh (positions in its unit
/// box) is box-projected onto the template by its own face normal: all
/// three corners land in one template region, never smeared across two.
pub fn body_template_uvs(part: &str, positions: &[Vec3]) -> Option<Vec<Vec2>> {
    let role = clothing_role(part)?;
    let mut out = Vec::with_capacity(positions.len());
    for tri in positions.chunks(3) {
        if tri.len() < 3 {
            out.extend(tri.iter().map(|_| Vec2::ZERO));
            continue;
        }
        // Outward: the positions are centred, so the face points away
        // from the middle of the box.
        let mut n = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
        if n.dot(tri[0] + tri[1] + tri[2]) < 0.0 {
            n = -n;
        }
        out.extend(tri.iter().map(|p| template_uv(&role, n, *p)));
    }
    Some(out)
}

/// A mesh's positions / normals / UVs as uploaded (normalized to its unit box).
pub fn mesh_for_upload(bytes: &[u8]) -> Option<(Vec<Vec3>, Vec<Vec3>, Vec<Vec2>)> {
    let (p, n, uv, _, _, _, _) = load_mesh_projected(bytes).ok()?;
    Some((p, n, uv))
}

// ------------------------------------------------------------- R15 composit
//
// Studio composites an R15 character's classic clothing on the GPU into one
// 916 × 568 atlas, and the R15 body MeshParts sample their own cell of it
// with their own mesh UVs (Studio 0.744):
// - layer list Studio: a CompositQuad body-colour fill per cell
//   (Studio: type 1 → TexCompFS, blend One/Zero), then the Pants layer
//   (), the Shirt layer (), the T-shirt (), each through
//   avatar/compositing/R15Composit*Base.mesh (Studio: type 0 →
//   TexCompPMAFS, blend One/InvSrcAlpha, colour = the clothing's Color3);
// - draw Studio: TexCompOversampleVS (pos·scale + offset in an ortho
//   over the target, uv as authored), linear filter, LOD bias −10 (mip 0);
// - cells Studio (table Studio, 1/916 and 1/568 at Studio):
//   UpperTorso/LowerTorso (0,0,388,272), left arm (388,0), right arm
//   (652,0), left leg (388,284), right leg (652,284), each 264 × 284; rows
//   counted from the top as the body meshes' UVs are (the GL draws sit at
//   y = 568 − y − h).

/// The R15 clothing atlas Studio composites (Studio: 1/916, 1/568).
pub const R15_ATLAS_W: u32 = 916;
pub const R15_ATLAS_H: u32 = 568;

/// An R15 body part's cell of the atlas (x, y, w, h in atlas pixels).
pub fn r15_cell(part: &str) -> Option<[f32; 4]> {
    Some(match part {
        "UpperTorso" | "LowerTorso" => [0.0, 0.0, 388.0, 272.0],
        "LeftUpperArm" | "LeftLowerArm" | "LeftHand" => [388.0, 0.0, 264.0, 284.0],
        "RightUpperArm" | "RightLowerArm" | "RightHand" => [652.0, 0.0, 264.0, 284.0],
        "LeftUpperLeg" | "LeftLowerLeg" | "LeftFoot" => [388.0, 284.0, 264.0, 284.0],
        "RightUpperLeg" | "RightLowerLeg" | "RightFoot" => [652.0, 284.0, 264.0, 284.0],
        _ => return None,
    })
}

/// The groups whose first part colours each cell (Studio checks the
/// parts in this order, Studio: a MeshPart with a loadable TextureID;
/// none such → the first part present): torso, left arm, right arm, left
/// leg, right leg.
pub const R15_CELL_GROUPS: [&[&str]; 5] = [
    &["UpperTorso", "LowerTorso"],
    &["LeftUpperArm", "LeftLowerArm", "LeftHand"],
    &["RightUpperArm", "RightLowerArm", "RightHand"],
    &["LeftUpperLeg", "LeftLowerLeg", "LeftFoot"],
    &["RightUpperLeg", "RightLowerLeg", "RightFoot"],
];

/// What an R15 character's atlas is made of besides the clothing images.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct R15Look {
    /// Body colour per cell group (R15_CELL_GROUPS), sRGB bytes.
    pub cell_colors: [[u8; 3]; 5],
    /// Shirt / Pants Color3 (multiplies the template).
    pub shirt_color: [f32; 3],
    pub pants_color: [f32; 3],
}

impl R15Look {
    /// A cache key for the composite.
    pub fn key(&self) -> String {
        format!("{:?}{:?}{:?}", self.cell_colors, self.shirt_color, self.pants_color)
    }
}

/// An R15 body part mesh's UVs moved into its atlas cell (Studio with
/// the normalise flag: (x + pad) / 916, (y + pad) / 568, (w − 2 pad) / 916,
/// (h − 2 pad) / 568; pad 0 for body cells).
pub fn r15_atlas_uvs(part: &str, uvs: &[Vec2]) -> Option<Vec<Vec2>> {
    let [x, y, w, h] = r15_cell(part)?;
    let (aw, ah) = (R15_ATLAS_W as f32, R15_ATLAS_H as f32);
    Some(uvs.iter().map(|uv| Vec2::new((x + uv.x * w) / aw, (y + uv.y * h) / ah)).collect())
}

/// Studio's three R15 composit meshes (torso, left arm, right arm): pixel
/// positions (x, y up) and template UVs, as triangle lists.
struct CompositMeshes {
    torso: (Vec<Vec3>, Vec<Vec2>),
    left: (Vec<Vec3>, Vec<Vec2>),
    right: (Vec<Vec3>, Vec<Vec2>),
}

fn composit_meshes() -> Option<&'static CompositMeshes> {
    static M: std::sync::OnceLock<Option<CompositMeshes>> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let dir = crate::default_scripts::studio_dir()?.join("content/avatar/compositing");
        let load = |n: &str| {
            let m = crate::mesh::parse(&std::fs::read(dir.join(n)).ok()?).ok()?;
            Some((m.positions, m.uvs))
        };
        Some(CompositMeshes {
            torso: load("R15CompositTorsoBase.mesh")?,
            left: load("R15CompositLeftArmBase.mesh")?,
            right: load("R15CompositRightArmBase.mesh")?,
        })
    })
    .as_ref()
}

/// The 916 × 568 atlas being composited: RGBA8 rows from the top, as the
/// GPU target holds them (premultiplied by the PMA layers).
struct Target {
    px: Vec<u8>,
}

impl Target {
    /// One triangle rasterized like the GPU does it (pixel centres, top-left
    /// fill rule), `shade` giving the source colour at a pixel from the
    /// interpolated uv and `blend` writing it.
    fn triangle(&mut self, p: [Vec2; 3], uv: [Vec2; 3], shade: &dyn Fn(Vec2) -> [f32; 4], pma: bool) {
        let (w, h) = (R15_ATLAS_W as i32, R15_ATLAS_H as i32);
        let area = (p[1] - p[0]).perp_dot(p[2] - p[0]);
        if area == 0.0 {
            return;
        }
        // Counter-clockwise either way: flip to a positive area.
        let (p, uv) = if area < 0.0 { ([p[0], p[2], p[1]], [uv[0], uv[2], uv[1]]) } else { (p, uv) };
        let area = area.abs();
        let min = p[0].min(p[1]).min(p[2]);
        let max = p[0].max(p[1]).max(p[2]);
        let x0 = (min.x - 0.5).ceil().max(0.0) as i32;
        let y0 = (min.y - 0.5).ceil().max(0.0) as i32;
        let x1 = ((max.x - 0.5).floor() as i32).min(w - 1);
        let y1 = ((max.y - 0.5).floor() as i32).min(h - 1);
        // Top-left rule: with rows down and a positive perp-dot area the
        // winding is clockwise on screen, so a top edge runs +x and a left
        // edge runs −y; pixels exactly on those edges are the triangle's.
        let owns = |a: Vec2, b: Vec2| {
            let e = b - a;
            (e.y == 0.0 && e.x > 0.0) || e.y < 0.0
        };
        let edges = [(p[1], p[2], owns(p[1], p[2])), (p[2], p[0], owns(p[2], p[0])), (p[0], p[1], owns(p[0], p[1]))];
        for y in y0..=y1.max(y0 - 1) {
            for x in x0..=x1.max(x0 - 1) {
                let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                let mut l = [0f32; 3];
                let mut inside = true;
                for (k, (a, b, own)) in edges.iter().enumerate() {
                    let e = (*b - *a).perp_dot(c - *a);
                    if e < 0.0 || (e == 0.0 && !own) {
                        inside = false;
                        break;
                    }
                    l[k] = e / area;
                }
                if !inside {
                    continue;
                }
                let t = uv[0] * l[0] + uv[1] * l[1] + uv[2] * l[2];
                let s = shade(t);
                let o = ((y * w + x) * 4) as usize;
                let d = &mut self.px[o..o + 4];
                for k in 0..4 {
                    let dst = d[k] as f32 / 255.0;
                    let v = if pma { s[k] + dst * (1.0 - s[3]) } else { s[k] };
                    d[k] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
    }
}

/// Bilinear, clamped to the edge, at mip 0 (TexComp's LOD bias −10), the
/// image's straight RGBA in 0..1.
fn sample_bilinear(im: &Rgba, uv: Vec2) -> [f32; 4] {
    let (w, h) = (im.width.max(1) as i64, im.height.max(1) as i64);
    let x = uv.x * w as f32 - 0.5;
    let y = uv.y * h as f32 - 0.5;
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |xi: i64, yi: i64| {
        let (xi, yi) = (xi.clamp(0, w - 1), yi.clamp(0, h - 1));
        let o = ((yi * w + xi) * 4) as usize;
        let p = im.pixels.get(o..o + 4).unwrap_or(&[0, 0, 0, 0]);
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    };
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut out = [0f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        out[k] = (top + (bot - top) * fy) / 255.0;
    }
    out
}

/// Composites an R15 character's clothing atlas as Studio does: the five
/// cells filled with their body colours, then Pants, then Shirt through the
/// R15 composit meshes. None when Studio's composit meshes aren't installed.
pub fn r15_composite(look: &R15Look, shirt: Option<&Rgba>, pants: Option<&Rgba>) -> Option<Rgba> {
    let meshes = composit_meshes()?;
    let (w, h) = (R15_ATLAS_W, R15_ATLAS_H);
    let mut t = Target { px: vec![0; (w * h * 4) as usize] };
    // Body colours: CompositQuad (0..1024, uv 0..1) scaled to the cell,
    // replace (Studio, TexCompFS with a white texture × (colour, 1)).
    let cells: [[f32; 4]; 5] = [
        [0.0, 0.0, 388.0, 272.0],
        [388.0, 0.0, 264.0, 284.0],
        [652.0, 0.0, 264.0, 284.0],
        [388.0, 284.0, 264.0, 284.0],
        [652.0, 284.0, 264.0, 284.0],
    ];
    for (cell, col) in cells.iter().zip(look.cell_colors) {
        let c = [col[0] as f32 / 255.0, col[1] as f32 / 255.0, col[2] as f32 / 255.0, 1.0];
        let [x, y, cw, ch] = *cell;
        let q = [Vec2::new(x, y), Vec2::new(x + cw, y), Vec2::new(x + cw, y + ch), Vec2::new(x, y + ch)];
        let shade = move |_: Vec2| c;
        t.triangle([q[0], q[1], q[2]], [Vec2::ZERO; 3], &shade, false);
        t.triangle([q[0], q[2], q[3]], [Vec2::ZERO; 3], &shade, false);
    }
    // A clothing layer: mesh pixels (y up) at a GL offset, so atlas row
    // = 568 − (offset y + mesh y); TexCompPMAFS: (rgb·a·colour, a), over.
    let mut layer = |mesh: &(Vec<Vec3>, Vec<Vec2>), off: Vec2, im: &Rgba, col: [f32; 3]| {
        let shade = |uv: Vec2| {
            let s = sample_bilinear(im, uv);
            [s[0] * s[3] * col[0], s[1] * s[3] * col[1], s[2] * s[3] * col[2], s[3]]
        };
        for (tri, tuv) in mesh.0.chunks_exact(3).zip(mesh.1.chunks_exact(3)) {
            let p = |v: Vec3| Vec2::new(off.x + v.x, h as f32 - (off.y + v.y));
            t.triangle([p(tri[0]), p(tri[1]), p(tri[2])], [tuv[0], tuv[1], tuv[2]], &shade, true);
        }
    };
    if let Some(im) = pants {
        layer(&meshes.torso, Vec2::new(0.0, 296.0), im, look.pants_color);
        layer(&meshes.right, Vec2::new(652.0, 0.0), im, look.pants_color);
        layer(&meshes.left, Vec2::new(388.0, 0.0), im, look.pants_color);
    }
    if let Some(im) = shirt {
        layer(&meshes.torso, Vec2::new(0.0, 296.0), im, look.shirt_color);
        layer(&meshes.right, Vec2::new(652.0, 284.0), im, look.shirt_color);
        layer(&meshes.left, Vec2::new(388.0, 284.0), im, look.shirt_color);
    }
    Some(Rgba { width: w, height: h, pixels: t.px })
}

/// Shirt over pants (alpha "over"), for torso parts.
pub fn composite_over(under: Option<&Rgba>, over: Option<&Rgba>) -> Option<Rgba> {
    match (under, over) {
        (None, None) => None,
        (Some(u), None) => Some(u.clone()),
        (None, Some(o)) => Some(o.clone()),
        (Some(u), Some(o)) => {
            let (w, h) = (o.width, o.height);
            let mut out = o.clone();
            for y in 0..h {
                for x in 0..w {
                    // Sample the under-image at the same relative position.
                    let ux = (x as u64 * u.width as u64 / w.max(1) as u64) as u32;
                    let uy = (y as u64 * u.height as u64 / h.max(1) as u64) as u32;
                    let ui = ((uy * u.width + ux) * 4) as usize;
                    let oi = ((y * w + x) * 4) as usize;
                    let (Some(us), Some(os)) =
                        (u.pixels.get(ui..ui + 4), out.pixels.get(oi..oi + 4))
                    else {
                        continue;
                    };
                    let (us, os) = ([us[0], us[1], us[2], us[3]], [os[0], os[1], os[2], os[3]]);
                    let oa = os[3] as f32 / 255.0;
                    let ua = us[3] as f32 / 255.0;
                    let a = oa + ua * (1.0 - oa);
                    let mut px = [0u8; 4];
                    for c in 0..3 {
                        let v = if a > 0.0 {
                            (os[c] as f32 * oa + us[c] as f32 * ua * (1.0 - oa)) / a
                        } else {
                            0.0
                        };
                        px[c] = v.round().clamp(0.0, 255.0) as u8;
                    }
                    px[3] = (a * 255.0).round() as u8;
                    out.pixels[oi..oi + 4].copy_from_slice(&px);
                }
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit box is centred on the mesh's bounds; their centre comes
    /// back so a CharacterMesh can sit by the mesh's own origin.
    #[test]
    fn loaded_mesh_keeps_its_bounds_centre() {
        let v1 = b"version 1.01\n1\n[0,1,0][0,0,1][0,0,0][2,1,0][0,0,1][1,0,0][0,3,1][0,0,1][0,1,0]";
        let (pos, _, _, extent, _, _, center) = load_mesh_projected(v1).unwrap();
        assert!((center - Vec3::new(1.0, 2.0, 0.5)).length() < 1e-5, "{center:?}");
        assert!((extent - Vec3::new(2.0, 2.0, 1.0)).length() < 1e-5, "{extent:?}");
        assert!((pos[0] - Vec3::new(-0.5, -0.5, -0.5)).length() < 1e-5, "{:?}", pos[0]);
    }

    fn transient(status: u16, retry_after: Option<&str>) -> AssetDownloadError {
        AssetDownloadError::Transient {
            status,
            retry_after: retry_after.map(|value| value.parse().unwrap()),
            error: rustudio_account::Error::new(format!("HTTP {status}")),
        }
    }

    #[test]
    fn download_retries_then_succeeds() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        let mut tries = 0;
        let result = downloads.retry(|| {
            tries += 1;
            match tries {
                1 => Err(transient(502, None)),
                2 => Err(transient(503, None)),
                _ => Ok(vec![42]),
            }
        });
        assert_eq!(result.unwrap(), vec![42]);
        assert_eq!(tries, 3);
    }

    #[test]
    fn download_gives_up_after_last_try() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        let mut tries = 0;
        let result = downloads.retry(|| {
            tries += 1;
            Err(transient(503, None))
        });
        assert!(matches!(
            result,
            Err(AssetDownloadError::Transient { status: 503, .. })
        ));
        assert_eq!(tries, ASSET_BACKOFF.len() + 1);
    }

    #[test]
    fn download_retry_after_pauses_the_whole_queue() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        std::thread::scope(|scope| {
            let (send, receive) = channel();
            let downloads = &downloads;
            let retry = scope.spawn(move || {
                let mut tries = 0;
                let mut pause_started = None;
                downloads
                    .retry(|| {
                        tries += 1;
                        if tries <= ASSET_BACKOFF.len() {
                            return Err(transient(429, None));
                        }
                        pause_started = Some(Instant::now());
                        Err(transient(429, Some("1")))
                    })
                    .unwrap_err();
                // The terminal 429 must still pause an unrelated download.
                send.send(pause_started.unwrap()).unwrap();
            });
            let start = receive.recv_timeout(Duration::from_secs(5)).unwrap();
            downloads
                .retry(|| {
                    assert!(start.elapsed() >= Duration::from_secs(1));
                    Ok(vec![])
                })
                .unwrap();
            retry.join().unwrap();
        });
    }

    #[test]
    fn download_retry_honors_retry_after_and_clamps_it() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        let start = Instant::now();
        let mut tries = 0;
        downloads
            .retry(|| {
                tries += 1;
                if tries == 1 {
                    return Err(transient(503, Some("1")));
                }
                assert!(start.elapsed() >= Duration::from_secs(1));
                Ok(vec![])
            })
            .unwrap();
        assert_eq!(tries, 2);
        assert_eq!(
            retry_after_delay("120", SystemTime::UNIX_EPOCH),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            retry_after_delay("Thu, 01 Jan 1970 00:00:02 GMT", SystemTime::UNIX_EPOCH),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn editor_download_does_not_fall_back_after_a_transient_failure() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        for status in [429, 502, 503] {
            let mut tries = 0;
            let error = downloads
                .editor_download(
                    None,
                    Some(|| {
                        tries += 1;
                        Err(transient(status, None))
                    }),
                    || panic!("transient failure must not use public delivery"),
                )
                .unwrap_err();
            assert_eq!(error.to_string(), format!("HTTP {status}"));
            assert_eq!(tries, ASSET_BACKOFF.len() + 1);
        }
    }

    #[test]
    fn editor_download_falls_back_on_auth_refusal_only_for_unversioned_assets() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        let auth = || Err(rustudio_account::Error::new("Authentication required").into());
        assert_eq!(
            downloads
                .editor_download(None, Some(auth), || Ok(vec![42]))
                .unwrap(),
            vec![42]
        );
        assert_eq!(
            downloads
                .editor_download(Some(3), Some(auth), || panic!(
                    "versioned asset used public delivery"
                ))
                .unwrap_err()
                .to_string(),
            "Authentication required"
        );
    }

    #[test]
    fn needed_downloads_go_before_preloads_which_keep_slots_free() {
        let scheduler = Scheduler {
            state: Mutex::new(Queues::default()),
            changed: Condvar::new(),
        };
        let order = Arc::new(Mutex::new(Vec::new()));
        let note = |what: &'static str| {
            let order = order.clone();
            Box::new(move || order.lock().unwrap().push(what)) as DownloadJob
        };
        for _ in 0..MAX_PRELOAD_DOWNLOADS + 1 {
            scheduler.push(Priority::Preload, note("preload"));
        }
        scheduler.push(Priority::Needed, note("needed"));
        assert_eq!(scheduler.queued(), (1, MAX_PRELOAD_DOWNLOADS + 1));
        let (job, priority) = scheduler.next();
        assert_eq!(priority, Priority::Needed);
        job();
        assert_eq!(*order.lock().unwrap(), ["needed"]);
        // Preloads take at most MAX_PRELOAD_DOWNLOADS slots while running.
        for _ in 0..MAX_PRELOAD_DOWNLOADS {
            let (job, priority) = scheduler.next();
            assert_eq!(priority, Priority::Preload);
            job();
        }
        std::thread::scope(|scope| {
            let (send, receive) = channel();
            let scheduler = &scheduler;
            scope.spawn(move || {
                let (job, priority) = scheduler.next();
                job();
                send.send(priority).unwrap();
            });
            assert!(
                receive.recv_timeout(Duration::from_millis(200)).is_err(),
                "a fifth preload started while four were running"
            );
            // A needed download still goes through at once.
            scheduler.push(Priority::Needed, note("needed-late"));
            let (job, priority) = scheduler.next();
            assert_eq!(priority, Priority::Needed);
            job();
            scheduler.finished(Priority::Preload);
            assert_eq!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), Priority::Preload);
        });
        assert_eq!(order.lock().unwrap().len(), MAX_PRELOAD_DOWNLOADS + 3);
    }

    /// Until a second asker holds the in-flight slot (the map's, the
    /// downloader's and its publisher's references, plus one).
    fn wait_for_waiter(a: AssetRef) {
        let t0 = Instant::now();
        loop {
            let waiting = INFLIGHT.lock().unwrap().get(&a).map(Arc::strong_count).unwrap_or(0) >= 4;
            if waiting || t0.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn one_download_serves_every_asker_of_an_asset() {
        let a = AssetRef { id: 7_000_000_001, version: Some(1) };
        let downloads = std::sync::atomic::AtomicUsize::new(0);
        let gate = (Mutex::new(false), Condvar::new());
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                download_shared(a, || {
                    downloads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _open = gate.1.wait_while(gate.0.lock().unwrap(), |open| !*open).unwrap();
                    Ok(vec![1, 2, 3])
                })
            });
            // The second asker arrives while the first downloads.
            while !INFLIGHT.lock().unwrap().contains_key(&a) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let second = scope.spawn(|| download_shared(a, || panic!("downloaded twice")));
            wait_for_waiter(a);
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            assert_eq!(*first.join().unwrap().unwrap(), vec![1, 2, 3]);
            assert_eq!(*second.join().unwrap().unwrap(), vec![1, 2, 3]);
        });
        assert_eq!(downloads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!INFLIGHT.lock().unwrap().contains_key(&a));
        // A download that panics still answers its waiters.
        let b = AssetRef { id: 7_000_000_002, version: Some(1) };
        let gate = (Mutex::new(false), Condvar::new());
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                std::panic::catch_unwind(|| {
                    download_shared(b, || {
                        let _open = gate.1.wait_while(gate.0.lock().unwrap(), |open| !*open).unwrap();
                        panic!("boom")
                    })
                })
                .unwrap_err()
            });
            while !INFLIGHT.lock().unwrap().contains_key(&b) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let second = scope.spawn(|| download_shared(b, || Ok(vec![])));
            wait_for_waiter(b);
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
            first.join().unwrap();
            assert_eq!(second.join().unwrap().unwrap_err(), "download did not finish");
        });
    }

    #[test]
    fn refusals_are_remembered_until_a_session_arrives() {
        let id = 7_000_000_003;
        assert_eq!(refused(id), None);
        let refusal = AssetDownloadError::Refused(rustudio_account::Error::new(
            "asset 7000000003: public asset delivery: Authentication required to access Asset.",
        ));
        assert_eq!(
            download_error(id, refusal),
            "asset 7000000003: public asset delivery: Authentication required to access Asset."
        );
        assert!(refused(id).is_some());
        // Other failures (the link was down) are asked again.
        download_error(7_000_000_004, AssetDownloadError::Other(rustudio_account::Error::new("io")));
        assert_eq!(refused(7_000_000_004), None);
        download_error(7_000_000_005, transient(429, None));
        assert_eq!(refused(7_000_000_005), None);
        forget_refusals();
        assert_eq!(refused(id), None);
    }

    #[test]
    fn preloads_wait_while_the_preload_slots_are_busy() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        for _ in 0..MAX_PRELOAD_DOWNLOADS {
            let gate = gate.clone();
            schedule(Priority::Preload, move || {
                let _open = gate.1.wait_while(gate.0.lock().unwrap(), |open| !*open).unwrap();
            });
        }
        let id = "rbxtemp://rustudio/preload-slots-test";
        register_generated(id, vec![1]);
        let done = Arc::new(Mutex::new(None));
        preload_content(id, done.clone());
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*done.lock().unwrap(), None, "a preload ran on a busy slot: {:?}", SCHEDULER.queued());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let t0 = Instant::now();
        while done.lock().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*done.lock().unwrap(), Some(true));
        unregister_generated(id);
    }

    #[test]
    fn preload_content_answers_every_asker_through_the_workers() {
        let id = "rbxtemp://rustudio/preload-content-test";
        register_generated(id, vec![9; 16]);
        let first = Arc::new(Mutex::new(None));
        let second = Arc::new(Mutex::new(None));
        preload_content(id, first.clone());
        preload_content(id, second.clone());
        let t0 = Instant::now();
        while (first.lock().unwrap().is_none() || second.lock().unwrap().is_none())
            && t0.elapsed() < Duration::from_secs(10)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*first.lock().unwrap(), Some(true));
        assert_eq!(*second.lock().unwrap(), Some(true));
        assert_eq!(content_fetch_status(id), 2);
        unregister_generated(id);
        let missing = Arc::new(Mutex::new(None));
        preload_content("rbxtemp://rustudio/preload-content-missing", missing.clone());
        let t0 = Instant::now();
        while missing.lock().unwrap().is_none() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*missing.lock().unwrap(), Some(false));
    }

    #[test]
    fn download_concurrency_is_capped_across_callers() {
        let downloads = AssetDownloads::new([Duration::ZERO; ASSET_BACKOFF.len()]);
        let (entered_tx, entered_rx) = channel();
        let release = (Mutex::new(false), Condvar::new());
        let active = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        use std::sync::atomic::Ordering::Relaxed;

        std::thread::scope(|scope| {
            for index in 0..2 * MAX_ASSET_DOWNLOADS {
                let download = || {
                    let count = active.fetch_add(1, Relaxed) + 1;
                    peak.fetch_max(count, Relaxed);
                    entered_tx.send(()).unwrap();
                    let _released = release
                        .1
                        .wait_while(release.0.lock().unwrap(), |released| !*released)
                        .unwrap();
                    active.fetch_sub(1, Relaxed);
                    Ok(vec![])
                };
                let downloads = &downloads;
                scope.spawn(move || {
                    if index % 2 == 0 {
                        downloads
                            .editor_download(None, Some(download), || unreachable!())
                            .unwrap();
                    } else {
                        downloads.retry(download).unwrap();
                    }
                });
            }

            let filled = (0..MAX_ASSET_DOWNLOADS)
                .all(|_| entered_rx.recv_timeout(Duration::from_secs(5)).is_ok());
            let exceeded = filled && entered_rx.recv_timeout(Duration::from_millis(50)).is_ok();
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            assert!(filled, "cap was never reached");
            assert!(
                !exceeded,
                "another caller started while all permits were held"
            );
        });
        assert_eq!(peak.load(Relaxed), MAX_ASSET_DOWNLOADS);
        assert_eq!(entered_rx.try_iter().count(), MAX_ASSET_DOWNLOADS);
    }

    #[test]
    fn abandoned_decode_results_expire_but_recent_results_survive() {
        let mut cache = TextureCache::default();
        let (tx, rx) = std::sync::mpsc::channel();
        cache.decode_chan = Some((tx.clone(), rx));
        cache.decoding.insert("old-area".into());
        tx.send(("old-area".into(), Err("fixture".into()))).unwrap();
        assert!(cache.poll_decoded());
        assert!(!cache.decoding());
        for _ in 0..3 {
            cache.poll_decoded();
            assert!(cache.decoded.contains_key("old-area"));
        }
        tx.send(("new-area".into(), Err("fixture".into()))).unwrap();
        cache.poll_decoded();
        assert!(!cache.decoded.contains_key("old-area"));
        assert!(cache.decoded.contains_key("new-area"));
        assert!(!cache.entries.contains_key("old-area"));
        // Expiration also runs when there are no new completions.
        for _ in 0..4 {
            cache.poll_decoded();
        }
        assert!(cache.decoded.is_empty());
    }

    #[test]
    fn upload_deferred_results_do_not_expire_while_requested() {
        if sync_decode() { return; }
        let mut cache = TextureCache::default();
        let mut store = AssetStore::default();
        cache.decoded.insert("rbxassetid://987654321".into(), (Err("fixture".into()), 0));
        for _ in 0..10 {
            cache.poll_decoded();
            cache.upload_ms = UPLOAD_BUDGET_MS + 1.0;
            assert!(cache.image("rbxassetid://987654321", None, &mut store, &mut |_, _| panic!("over budget")).is_none());
            assert!(cache.decoded.contains_key("rbxassetid://987654321"));
        }
        cache.poll_decoded();
        cache.image("rbxassetid://987654321", None, &mut store, &mut |_, _| panic!("failed decode"));
        assert!(cache.decoded.is_empty());
        assert!(cache.failed("rbxassetid://987654321"));
    }

    #[test]
    fn upload_budget_parse() {
        assert_eq!(parse_upload_budget(None), UPLOAD_BUDGET_BYTES);
        assert_eq!(parse_upload_budget(Some("x")), UPLOAD_BUDGET_BYTES);
        assert_eq!(parse_upload_budget(Some("nan")), UPLOAD_BUDGET_BYTES);
        assert_eq!(parse_upload_budget(Some("0")), usize::MAX);
        assert_eq!(parse_upload_budget(Some("-3")), usize::MAX);
        assert_eq!(parse_upload_budget(Some(" 2 ")), 2 << 20);
        assert_eq!(parse_upload_budget(Some("1e-9")), 1);
    }

    #[test]
    fn asset_id_forms() {
        assert_eq!(
            asset_ref("rbxassetid://12345"),
            Some(AssetRef {
                id: 12345,
                version: None
            })
        );
        assert_eq!(
            asset_ref("http://www.roblox.com/asset/?id=987&version=3"),
            Some(AssetRef {
                id: 987,
                version: Some(3)
            })
        );
        assert_eq!(
            asset_ref("42"),
            Some(AssetRef {
                id: 42,
                version: None
            })
        );
        assert_eq!(asset_ref("rbxasset://fonts/x.mesh"), None);
        assert_eq!(
            asset_ref("http://www.roblox.com/asset/?id= 276090533"),
            Some(AssetRef { id: 276090533, version: None })
        );
    }

    #[test]
    fn legacy_content_maps_to_asset_ids() {
        // Studio's LegacyContentTable: these files left content/ long ago.
        // (Against an empty content folder: a Studio content download that
        // still ships one of them, as the Windows one does SwordTexture.png,
        // wins over the table.)
        let empty = std::env::temp_dir().join(format!("rustudio-legacy-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty);
        let table = legacy_table(&empty);
        let id = |s: &str| legacy_lookup(&table, s.strip_prefix("rbxasset://").unwrap());
        assert_eq!(id("rbxasset://fonts/sword.mesh"), Some(12221720));
        assert_eq!(id("rbxasset://Fonts/Torso.mesh"), Some(12221758));
        assert_eq!(id("rbxasset://fonts//leftarm.mesh"), Some(12221505));
        assert_eq!(id("rbxasset://fonts//torso.mesh"), Some(12221758));
        assert_eq!(asset_ref("rbxasset://fonts//leftarm.mesh").map(|a| a.id), Some(12221505));
        assert_eq!(resolve("rbxasset://fonts//leftarm.mesh", None), None);
        assert_eq!(id("rbxasset://textures/SwordTexture.png"), Some(12224218));
        assert_eq!(id("rbxasset://textures/rocketlaunchertex.png"), Some(12224070));
        assert_eq!(id("rbxasset://fonts/clonewand.mesh"), Some(12221344));
        // A file the content folder has is not mapped.
        let _ = std::fs::create_dir_all(empty.join("fonts"));
        let _ = std::fs::write(empty.join("fonts/sword.mesh"), b"x");
        assert_eq!(legacy_lookup(&legacy_table(&empty), "fonts/sword.mesh"), None);
        let _ = std::fs::remove_dir_all(&empty);
        // Old models spell the query `?ID=`.
        assert_eq!(asset_ref("http://www.roblox.com/asset/?ID=94746105").map(|a| a.id), Some(94746105));
    }

    /// #16: while scripts run, a place's images and meshes come only from
    /// Roblox content (a local file rendered could be captured and read
    /// back), even when Edit already cached them; Roblox ids, the content
    /// folder and generated images still load, and Edit keeps local files.
    #[test]
    fn local_place_content_is_not_loaded_while_scripts_run() {
        let dir = std::env::temp_dir().join(format!("rustudio-play-content-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("private.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([12, 34, 56, 255])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        std::fs::write(&path, &bytes).unwrap();
        let locals = [format!("file://{}", path.display()), path.display().to_string(), "private.png".to_string()];
        let mut store = AssetStore::default();
        let mut textures = TextureCache::default();
        let mut up = |_: &Rgba, _: Option<u32>| 7u32;
        // Edit: local files render (and land in the texture cache).
        for id in &locals {
            assert!(matches!(store.content(id, Some(&dir)), Fetch::Ready(b) if *b == bytes), "{id}");
            assert!(textures.image(id, Some(&dir), &mut store, &mut up).is_some(), "{id}");
        }
        // Play: neither a fresh load nor the cache gives them.
        store.scripts_running = true;
        for id in &locals {
            assert!(matches!(store.content(id, Some(&dir)), Fetch::Unavailable(_)), "{id}");
            assert!(textures.image(id, Some(&dir), &mut store, &mut up).is_none(), "{id}");
            assert!(!store.allows(id), "{id}");
        }
        // Generated (capture) images and Roblox ids still do.
        let id = "rbxtemp://rustudio/test-play-content.png";
        register_generated(id, bytes.clone());
        let generated_ok = matches!(store.content(id, None), Fetch::Ready(b) if *b == bytes)
            && textures.image(id, None, &mut store, &mut up).is_some();
        unregister_generated(id);
        assert!(generated_ok);
        for id in ["rbxassetid://12345", "rbxasset://textures/white.png", "http://www.roblox.com/asset/?id=5", "12345"] {
            assert!(store.allows(id), "{id}");
        }
        // Back in Edit: the cached texture is there again.
        store.scripts_running = false;
        assert!(textures.image(&locals[0], Some(&dir), &mut store, &mut up).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn place_content_policy() {
        for id in ["file:///tmp/a.png", "/tmp/a.png", "a.png", "meshes/a.obj", " /x.wav "] {
            assert!(is_local_path(id), "{id}");
            assert!(place_content_allowed(id, false), "{id}");
            assert!(!place_content_allowed(id, true) || local_content_opt_in(), "{id}");
        }
        for id in ["rbxassetid://1", "rbxasset://sounds/x.wav", "rbxtemp://rustudio/x", "1234", "rbxthumb://type=Asset&id=1&w=150&h=150", ""] {
            assert!(!is_local_path(id), "{id}");
            assert!(place_content_allowed(id, true), "{id}");
        }
    }

    #[test]
    fn marked_place_folder_is_play_content() {
        // canonicalize: macOS temp dirs sit behind the /var → /private/var link.
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!("rs-local-content-{}", std::process::id()));
        let marked = root.join("marked");
        let plain = root.join("plain");
        std::fs::create_dir_all(marked.join("meshes")).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(marked.join(LOCAL_CONTENT_MARKER), "").unwrap();
        // Not meshes/a.obj: place_content_policy runs in parallel and checks that name.
        std::fs::write(marked.join("meshes/marked-a.obj"), "v 0 0 0").unwrap();
        std::fs::write(plain.join("b.obj"), "v 0 0 0").unwrap();
        std::fs::write(marked.join("level.rbxlx"), "").unwrap();
        std::fs::write(plain.join("other.rbxlx"), "").unwrap();
        note_opened_place(&marked.join("level.rbxlx"));
        note_opened_place(&plain.join("other.rbxlx"));
        assert!(place_content_allowed("meshes/marked-a.obj", true));
        let abs = format!("file://{}", marked.join("meshes/marked-a.obj").display());
        assert!(place_content_allowed(&abs, true));
        assert_eq!(resolve("meshes/marked-a.obj", None), Some(marked.join("meshes/marked-a.obj")));
        // Unmarked folders, and escapes out of a marked one, stay blocked.
        assert!(!place_content_allowed(&format!("file://{}", plain.join("b.obj").display()), true)
            || local_content_opt_in());
        assert!(!place_content_allowed("../plain/b.obj", true) || local_content_opt_in());
        // Scripts (CreateEditableMeshAsync / ImageAsync) read the marked
        // folder's files, by relative or absolute path, and nothing else.
        assert_eq!(script_content_blocking("meshes/marked-a.obj").unwrap().as_slice(), b"v 0 0 0");
        assert!(script_content_blocking(&abs).is_ok());
        assert!(script_content_blocking("../plain/b.obj").is_err());
        assert!(script_content_blocking(&format!("file://{}", plain.join("b.obj").display())).is_err());
        assert!(script_content_blocking("meshes/../../plain/b.obj").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_local_ids_only() {
        assert!(resolve("rbxassetid://12345", None).is_none());
        assert!(resolve("http://example.com/x.mesh", None).is_none());
        assert_eq!(
            resolve("file:///tmp/a.obj", None),
            Some(PathBuf::from("/tmp/a.obj"))
        );
        assert!(resolve("rbxasset://fonts/x.mesh", None)
            .unwrap()
            .ends_with("fonts/x.mesh"));
        assert_eq!(
            resolve("meshes/a.obj", Some(Path::new("/w"))),
            Some(PathBuf::from("/w/meshes/a.obj"))
        );
    }

    #[test]
    fn rbxasset_ids_stay_in_the_content_folder() {
        for id in [
            "rbxasset://../../secret.png",
            "rbxasset:///../secret.png",
            "rbxasset://textures/../../secret.png",
        ] {
            assert_eq!(resolve(id, None), None, "{id}");
        }
        assert!(resolve("rbxasset://textures/./a.png", None).is_some());
    }

    #[test]
    fn loads_obj_into_unit_box() {
        let (pos, _, uvs, extent, _) =
            load_mesh(b"v 0 0 0\nv 4 0 0\nv 0 2 0\nvt 0 0\nvt 1 0\nvt 0 1\nf 1/1 2/2 3/3\n")
                .unwrap();
        assert_eq!(pos.len(), 3);
        assert_eq!(uvs.len(), 3);
        assert!((extent - Vec3::new(4.0, 2.0, 1e-4)).length() < 1e-3);
        assert!(pos
            .iter()
            .all(|v| v.x.abs() <= 0.5001 && v.y.abs() <= 0.5001));
    }

    #[test]
    fn template_box_front_maps_into_torso_front() {
        let role = clothing_role("Torso").unwrap();
        let (pos, _, uvs) = template_box(&role);
        assert_eq!(pos.len(), 36);
        // The front face's top-left texel (as seen from the front) is the region's corner.
        let i = pos
            .iter()
            .position(|p| {
                (p.z + 0.5).abs() < 1e-6 && (p.x - 0.5).abs() < 1e-6 && (p.y - 0.5).abs() < 1e-6
            })
            .unwrap();
        assert!((uvs[i] - Vec2::new(231.0 / TEMPLATE_W, 74.0 / TEMPLATE_H)).length() < 1e-5);
    }
}

/// A mesh's triangles in its unit box (-0.5..0.5, stretched to a part's
/// Size), read straight from local files (generated, local store / ledger,
/// disk cache, content folder) for ray tests; None when it isn't here.
/// A mesh's bounds size (its unit-box triangles × this = mesh space).
pub fn mesh_extent(id: &str) -> Option<Vec3> {
    static CACHE: Mutex<Option<HashMap<String, Vec3>>> = Mutex::new(None);
    if let Some(hit) = CACHE.lock().ok()?.get_or_insert_with(HashMap::new).get(id) {
        return Some(*hit);
    }
    mesh_triangles(id)?;
    let bytes: Option<Vec<u8>> = generated(id)
        .map(|b| b.to_vec())
        .or_else(|| match asset_ref(id) {
            Some(a) => local_asset(a.id).or_else(|| std::fs::read(cache_path(a)).ok()),
            None => resolve(id, None).and_then(|p| std::fs::read(p).ok()),
        });
    let ext = bytes.and_then(|b| mesh::parse(&b).ok()).map(|m| (m.max - m.min).max(Vec3::splat(1e-4)))?;
    if let Ok(mut g) = CACHE.lock() {
        g.get_or_insert_with(HashMap::new).insert(id.to_string(), ext);
    }
    Some(ext)
}

static MESH_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Bumped whenever a loaded mesh, a mesh's outline triangles or a skinning
/// base appears, changes or is forgotten: what an outline drawn from them
/// depends on beyond the document and the renderer's draws.
pub fn mesh_generation() -> u64 {
    MESH_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

pub fn bump_mesh_generation() {
    MESH_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

pub fn mesh_triangles(id: &str) -> Option<Arc<Vec<[Vec3; 3]>>> {
    static CACHE: Mutex<Option<HashMap<String, Option<Arc<Vec<[Vec3; 3]>>>>>> = Mutex::new(None);
    if !place_content_ok(id) {
        return None;
    }
    if let Some(hit) = CACHE.lock().ok()?.get_or_insert_with(HashMap::new).get(id) {
        return hit.clone();
    }
    let bytes: Option<Vec<u8>> =
        generated(id)
            .map(|b| b.to_vec())
            .or_else(|| match asset_ref(id) {
                Some(a) => local_asset(a.id).or_else(|| std::fs::read(cache_path(a)).ok()),
                None => resolve(id, None).and_then(|p| std::fs::read(p).ok()),
            });
    let tris = bytes.and_then(|b| mesh::parse(&b).ok()).map(|m| {
        let ext = (m.max - m.min).max(Vec3::splat(1e-4));
        let c = (m.max + m.min) * 0.5;
        Arc::new(
            m.positions
                .chunks_exact(3)
                .map(|t| [(t[0] - c) / ext, (t[1] - c) / ext, (t[2] - c) / ext])
                .collect::<Vec<_>>(),
        )
    });
    // A mesh can still be downloading when decals first ask for geometry.
    // Retain successful parses only, so the overlay can follow the mesh once
    // its bytes arrive instead of permanently keeping the box fallback.
    if tris.is_some() {
        if let Ok(mut g) = CACHE.lock() {
            g.get_or_insert_with(HashMap::new)
                .insert(id.to_string(), tris.clone());
        }
        bump_mesh_generation();
    }
    tris
}

/// Nearest hit of a ray (origin, dir; dir's length is the unit) with
/// triangles: the ray parameter t.
/// Nearest triangle hit along a ray: distance and the triangle's normal,
/// turned to face the ray (Roblox reports the surface normal of the hit).
pub fn ray_triangles_normal(origin: Vec3, dir: Vec3, tris: &[[Vec3; 3]]) -> Option<(f32, Vec3)> {
    let mut best: Option<(f32, Vec3)> = None;
    for t in tris {
        let e1 = t[1] - t[0];
        let e2 = t[2] - t[0];
        let p = dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-12 {
            continue;
        }
        let inv = 1.0 / det;
        let s = origin - t[0];
        let u = s.dot(p) * inv;
        if !(0.0..=1.0).contains(&u) {
            continue;
        }
        let q = s.cross(e1);
        let v = dir.dot(q) * inv;
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let tt = e2.dot(q) * inv;
        if tt > 0.0 && best.is_none_or(|(b, _)| tt < b) {
            let n = e1.cross(e2).normalize_or_zero();
            best = Some((tt, if n.dot(dir) > 0.0 { -n } else { n }));
        }
    }
    best
}
