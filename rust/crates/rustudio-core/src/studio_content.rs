//! Roblox Studio's built-in content, downloaded on demand.
//!
//! rustudio ships no Roblox files. When this machine has no Studio install
//! (no `RUSTUDIO_STUDIO_DIR`, no `~/.local/share/rustudio/content` link),
//! rustudio downloads the parts of Studio it reads from Roblox's own
//! deployment CDN (`setup.rbxcdn.com`), the same packages the Studio
//! installer and Linux launchers such as Vinegar fetch, and unpacks them in
//! Studio's folder layout under `~/.local/share/rustudio/studio`:
//!
//! - `content/{avatar,fonts,sky,sounds,textures,models,configs}`: default
//!   characters, fonts, sky, sounds and `rbxasset://` textures,
//! - `PlatformContent/pc/{textures,terrain,fonts}`: studs, terrain colours,
//! - `ExtraContent/{scripts,models,textures}`: the default PlayerModule
//!   (camera + controls), character sounds and other built-in scripts,
//! - `ReflectionMetadata.xml`, taken alone out of `RobloxStudio.zip` with
//!   HTTP range requests (about 30 KB instead of the 120 MB package).
//!
//! About 60 MB to download. `rustudio --install-studio-content` does it in
//! the terminal; a normal launch does it in the background the first time
//! (`RUSTUDIO_NO_STUDIO_FETCH=1` turns that off).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// (package, folder it unpacks into, relative to the Studio root).
const PACKAGES: &[(&str, &str)] = &[
    ("content-avatar.zip", "content/avatar"),
    ("content-configs.zip", "content/configs"),
    ("content-fonts.zip", "content/fonts"),
    ("content-models.zip", "content/models"),
    ("content-sky.zip", "content/sky"),
    ("content-sounds.zip", "content/sounds"),
    ("content-textures2.zip", "content/textures"),
    ("content-textures3.zip", "PlatformContent/pc/textures"),
    ("content-terrain.zip", "PlatformContent/pc/terrain"),
    ("content-platform-fonts.zip", "PlatformContent/pc/fonts"),
    ("extracontent-models.zip", "ExtraContent/models"),
    ("extracontent-textures.zip", "ExtraContent/textures"),
];
const SCRIPTS_PACKAGE: &str = "extracontent-scripts.zip";
const SCRIPTS_FOLDER: &str = "ExtraContent/scripts";
/// Single files pulled out of a big package by range requests.
const SINGLE_FILES: &[(&str, &str)] = &[("RobloxStudio.zip", "ReflectionMetadata.xml")];

const VERSION_URL: &str = "https://clientsettingscdn.roblox.com/v2/client-version/WindowsStudio64";
const CDN: &str = "https://setup.rbxcdn.com";
const MARKER: &str = ".rustudio-studio-version";
// Bump when the installed content layout changes, even within one CDN version.
const CONTENT_REVISION: &str = "legacy-textures-1";

/// Where the downloaded content lives.
pub fn install_dir() -> PathBuf {
    if let Ok(p) = std::env::var("RUSTUDIO_STUDIO_DOWNLOAD_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::paths::data().join("rustudio/studio")
}

/// The downloaded Studio root, when a complete download is there.
pub fn installed() -> Option<PathBuf> {
    let d = install_dir();
    d.join(MARKER).is_file().then_some(d)
}

static MESSAGES: Mutex<Vec<(bool, String)>> = Mutex::new(Vec::new());

#[derive(Clone)]
pub enum DownloadState {
    Idle,
    Downloading(String),
    Finished,
    Failed(String),
    Disabled,
}

static DOWNLOAD: Mutex<DownloadState> = Mutex::new(DownloadState::Idle);
static CONTENT_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn generation() -> u64 {
    CONTENT_GENERATION.load(Ordering::Acquire)
}

pub fn download_state() -> DownloadState {
    if std::env::var_os("RUSTUDIO_NO_STUDIO_FETCH").is_some() {
        return DownloadState::Disabled;
    }
    DOWNLOAD.lock().unwrap().clone()
}

/// Controls can be used while the larger Studio content download continues.
pub(crate) fn installed_player_scripts() -> Option<PathBuf> {
    player_scripts_installed_at(&install_dir())
}

fn player_scripts_installed_at(dest: &Path) -> Option<PathBuf> {
    let root = dest.with_extension("player-scripts");
    (root.join(MARKER).is_file() && crate::default_scripts::has_player_scripts(&root)).then_some(root)
}

/// Messages for Output from the background download: (is_error, text).
pub fn take_messages() -> Vec<(bool, String)> {
    MESSAGES.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default()
}
fn post(err: bool, t: String) {
    if let Ok(mut m) = MESSAGES.lock() {
        m.push((err, t));
    }
}

/// On a normal launch with no Studio content anywhere, download it in the
/// background and report to Output.
pub fn spawn_background_install_if_missing() {
    fetch_classic_oof_once();
    if std::env::var_os("RUSTUDIO_NO_STUDIO_FETCH").is_some() {
        return;
    }
    if let Some(root) = crate::default_scripts::studio_dir() {
        // Upgrade older rustudio downloads without replacing the rest of their content.
        let need_vs = !has_view_selector(&root);
        let need_lp = !has_lua_package_assets(&root);
        let need_ui = !has_studio_ui_textures(&root);
        if root == install_dir() && (need_vs || need_lp || need_ui) {
            if let Some(marker) = std::fs::read_to_string(root.join(MARKER)).ok() {
                if let Some(version) = marker.split_whitespace().next().and_then(studio_version) {
                    let version = version.to_owned();
                    let _ = std::thread::Builder::new().name("studio-content-upgrade".into()).spawn(move || {
                        let a = agent();
                        if need_vs {
                            if let Err(e) = install_view_selector(&a, &version, &root) {
                                post(true, format!("Couldn't download Studio's View Selector: {e}"));
                            }
                        }
                        if need_lp {
                            if let Err(e) = install_lua_package_assets(&a, &version, &root) {
                                post(true, format!("Couldn't download Studio's LuaPackages assets: {e}"));
                            }
                        }
                        if need_ui {
                            if let Err(e) = install_studio_ui_textures(&a, &version, &root) {
                                post(true, format!("Couldn't download Studio's UI textures: {e}"));
                            }
                        }
                    });
                }
            }
        }
        return;
    }
    request_download();
}

/// Starts or retries a download. A second request joins the existing one.
pub fn request_download() {
    if std::env::var_os("RUSTUDIO_NO_STUDIO_FETCH").is_some() {
        return;
    }
    let mut state = DOWNLOAD.lock().unwrap();
    if matches!(*state, DownloadState::Downloading(_)) {
        return;
    }
    *state = DownloadState::Downloading("Consultando o CDN do Roblox".into());
    if let Err(err) = std::thread::Builder::new().name("studio-content".into()).spawn(|| {
        post(false, "Downloading Studio content, starting with the Play controls.".into());
        let result = install(&|step| {
            if !step.starts_with("fetching legacy") && !step.starts_with("legacy ") {
                *DOWNLOAD.lock().unwrap() = DownloadState::Downloading(step.to_owned());
                post(false, step.to_owned());
            }
        });
        match result {
            Ok(v) => {
                *DOWNLOAD.lock().unwrap() = DownloadState::Finished;
                post(false, format!("Studio content {v} installed."));
            }
            Err(err) => {
                *DOWNLOAD.lock().unwrap() = DownloadState::Failed(err.clone());
                eprintln!("rustudio: Studio content download failed: {err}");
                post(true, format!("Couldn't download Studio's built-in content: {err}"));
            }
        }
    }) {
        let err = format!("Couldn't start the Studio content downloader: {err}");
        *state = DownloadState::Failed(err.clone());
        post(true, err);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .user_agent(concat!("rustudio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, String> {
    let mut req = agent.get(url);
    if let Some((a, b)) = range {
        req = req.header("Range", &format!("bytes={a}-{b}"));
    }
    let mut resp = req.call().map_err(|e| format!("{url}: {e}"))?;
    if range.is_some() && resp.status().as_u16() != 206 {
        return Err(format!("{url}: no range support (HTTP {})", resp.status()));
    }
    resp.body_mut()
        .with_config()
        .limit(1 << 30)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

fn content_length(agent: &ureq::Agent, url: &str) -> Result<u64, String> {
    let resp = agent.head(url).call().map_err(|e| format!("{url}: {e}"))?;
    resp.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("{url}: no Content-Length"))
}

/// Download and unpack everything. Returns the Studio version installed.
/// `progress` gets one line per step.
/// Studio 0.533 (June 2022), the last build whose sounds package has the
/// classic death sound `uuhhh.mp3` ("Oof"); later Studios play a replacement.
const CLASSIC_OOF_PKG: &str = "version-58ed123c874e44c1-content-sounds.zip";

/// Where rustudio keeps the classic Oof it fetched from Roblox's CDN (its own
/// data folder: never inside a Studio install, never in the repo).
pub fn classic_oof() -> PathBuf {
    crate::paths::data().join("rustudio/classic-sounds/uuhhh.mp3")
}

/// Fetch the classic Oof in the background, once, unless it is already here.
pub fn fetch_classic_oof_once() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    // (RUSTUDIO_NO_STUDIO_FETCH or the rustudio option turns it off)
    if cfg!(test)
        || !crate::rustudio_settings::classic_death_sound_fetch()
        || classic_oof().is_file()
        || STARTED.swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let _ = std::thread::Builder::new().name("classic-oof".into()).spawn(|| {
        let dest = classic_oof();
        let got = fetch_one(&agent(), &format!("{CDN}/{CLASSIC_OOF_PKG}"), "uuhhh.mp3").and_then(|data| {
            let dir = dest.parent().ok_or("bad data dir")?;
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let tmp = dir.join(".uuhhh.mp3.part");
            std::fs::write(&tmp, &data).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())
        });
        if let Err(e) = got {
            post(true, format!("Couldn't fetch the classic death sound (Studio plays its replacement meanwhile): {e}"));
        }
    });
}

fn studio_version(version: &str) -> Option<&str> {
    version.strip_prefix("version-")
        .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(|_| version)
}

fn has_view_selector(root: &Path) -> bool {
    root.join("StudioContent/models/ViewSelector/Basic.mesh").is_file()
        && ["back", "bottom", "front", "left", "right", "top"].iter().all(|face| {
            ["", "_hover"].iter().all(|suffix| {
                root.join(format!("StudioContent/textures/ViewSelector/{face}{suffix}.png")).is_file()
            })
        })
}

fn install_view_selector(agent: &ureq::Agent, version: &str, root: &Path) -> Result<(), String> {
    // Publish the body last so the renderer can pick up the complete cube.
    for kind in ["textures", "models"] {
        let url = format!("{CDN}/{version}-studiocontent-{kind}.zip");
        let bytes = get(agent, &url, None)?;
        let zip = Zip::parse(&bytes).map_err(|e| format!("{url}: {e}"))?;
        for entry in &zip.entries {
            let Some((dir, file)) = entry.name.split_once(['\\', '/']) else { continue };
            if dir != "ViewSelector" || file.is_empty() || file.contains(['\\', '/']) {
                continue;
            }
            let data = zip.read(&bytes, entry).map_err(|e| format!("{url} {}: {e}", entry.name))?;
            write_file(&root.join("StudioContent").join(kind), &entry.name, &data)?;
        }
    }
    has_view_selector(root).then_some(()).ok_or_else(|| "Studio packages are missing View Selector files".into())
}

const LUA_PACKAGE_REQUIRED_FILES: &[&str] = &[
    "Packages/_Index/BuilderIcons/BuilderIcons/BuilderIcons.json",
    "Packages/_Index/BuilderIcons/BuilderIcons/Font/BuilderIcons-Regular.ttf",
    "Packages/_Index/BuilderIcons/BuilderIcons/Font/BuilderIcons-Filled.ttf",
    "Packages/_Index/FoundationImages/FoundationImages/SpriteSheets/img_set_1x_4.png",
];

fn is_lua_package_asset(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".ttf")
        || lower.ends_with(".otf")
        || lower.ends_with(".dds")
        || lower.ends_with(".webp")
        || lower.ends_with("buildericons.json")
}

fn has_lua_package_assets(root: &Path) -> bool {
    LUA_PACKAGE_REQUIRED_FILES
        .iter()
        .all(|rel| root.join("ExtraContent/LuaPackages").join(rel).is_file())
}

fn install_lua_package_assets(agent: &ureq::Agent, version: &str, root: &Path) -> Result<(), String> {
    let url = format!("{CDN}/{version}-extracontent-luapackages.zip");
    let bytes = get(agent, &url, None)?;
    let zip = Zip::parse(&bytes).map_err(|e| format!("{url}: {e}"))?;
    let base = root.join("ExtraContent/LuaPackages");
    for e in &zip.entries {
        if e.name.ends_with(['/', '\\']) || !is_lua_package_asset(&e.name) {
            continue;
        }
        let data = zip.read(&bytes, e).map_err(|err| format!("{url} {}: {err}", e.name))?;
        write_file(&base, &e.name, &data)?;
    }
    has_lua_package_assets(root)
        .then_some(())
        .ok_or_else(|| "Studio packages are missing LuaPackages UI assets".into())
}

fn has_studio_ui_textures(root: &Path) -> bool {
    root.join("content/studio_svg_textures/Shared/Utility/Dark/Standard/RadioOn.png").is_file()
        && root.join("StudioContent/textures/StudioToolbox/RoundedBackground.png").is_file()
}

fn install_studio_ui_textures(agent: &ureq::Agent, version: &str, root: &Path) -> Result<(), String> {
    for (pkg, sub) in [
        ("content-studio_svg_textures.zip", "content/studio_svg_textures"),
        ("studiocontent-textures.zip", "StudioContent/textures"),
    ] {
        let url = format!("{CDN}/{version}-{pkg}");
        let bytes = get(agent, &url, None)?;
        let zip = Zip::parse(&bytes).map_err(|e| format!("{url}: {e}"))?;
        let base = root.join(sub);
        for e in &zip.entries {
            if e.name.ends_with(['/', '\\']) {
                continue;
            }
            let data = zip.read(&bytes, e).map_err(|err| format!("{url} {}: {err}", e.name))?;
            write_file(&base, &e.name, &data)?;
        }
    }
    has_studio_ui_textures(root)
        .then_some(())
        .ok_or_else(|| "Studio packages are missing Studio UI textures".into())
}

pub fn install(progress: &dyn Fn(&str)) -> Result<String, String> {
    let agent = agent();
    let v: serde_json::Value = serde_json::from_slice(&get(&agent, VERSION_URL, None)?)
        .map_err(|e| format!("version answer: {e}"))?;
    let version = v["clientVersionUpload"]
        .as_str()
        .and_then(studio_version)
        .ok_or("version answer has no clientVersionUpload")?
        .to_string();
    progress(&format!("Studio {} ({version})", v["version"].as_str().unwrap_or("?")));

    let dest = install_dir();
    if std::fs::read_to_string(dest.join(MARKER)).is_ok_and(|s| s.trim() == format!("{version} {CONTENT_REVISION}")) {
        if !has_view_selector(&dest) {
            progress("downloading Studio View Selector");
            install_view_selector(&agent, &version, &dest)?;
        }
        if !has_lua_package_assets(&dest) {
            progress("downloading Studio LuaPackages UI assets");
            install_lua_package_assets(&agent, &version, &dest)?;
        }
        if !has_studio_ui_textures(&dest) {
            progress("downloading Studio UI textures");
            install_studio_ui_textures(&agent, &version, &dest)?;
        }
        progress("already installed");
        return Ok(version);
    }
    let parent = dest.parent().ok_or("bad install dir")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let partial = parent.join(format!(".studio-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);

    let result = (|| -> Result<(), String> {
        progress("Baixando os controles do modo Play");
        let scripts = get(&agent, &format!("{CDN}/{version}-{SCRIPTS_PACKAGE}"), None)?;
        publish_player_scripts(&dest, &version, &scripts)?;
        progress("Controles do modo Play prontos");
        unpack_package(&scripts, &partial.join(SCRIPTS_FOLDER))?;
        for (pkg, sub) in PACKAGES {
            progress(&format!("downloading {pkg}"));
            let url = format!("{CDN}/{version}-{pkg}");
            let bytes = get(&agent, &url, None)?;
            unpack_package(&bytes, &partial.join(sub)).map_err(|e| format!("{pkg}: {e}"))?;
        }
        // Studio resolves retired rbxasset paths through LegacyContentTable,
        // not another deployment zip. Materialize its missing textures too so
        // a downloaded content tree is usable by file-based consumers.
        for line in include_str!("../../../assets/legacy-content.tsv").lines() {
            let Some((path, id)) = line.split_once('\t') else { continue };
            if !path.starts_with("textures/") || partial.join("content").join(path).is_file() {
                continue;
            }
            let id: u64 = id.trim().parse().map_err(|e| format!("legacy texture {path}: {e}"))?;
            progress(&format!("fetching legacy {path}"));
            match crate::assets::remote_cached(id) {
                Ok(bytes) => write_file(&partial.join("content"), path, &bytes)?,
                // Some retired entries have also been removed from asset
                // delivery. They must not prevent installing current content.
                Err(e) => progress(&format!("legacy {path} unavailable: {e}")),
            }
        }
        progress("downloading Studio View Selector");
        if let Err(err) = install_view_selector(&agent, &version, &partial) {
            post(true, format!("Studio View Selector unavailable: {err}"));
        }
        progress("downloading Studio LuaPackages UI assets");
        install_lua_package_assets(&agent, &version, &partial)?;
        progress("downloading Studio UI textures");
        install_studio_ui_textures(&agent, &version, &partial)?;
        for (pkg, file) in SINGLE_FILES {
            progress(&format!("fetching {file} from {pkg}"));
            let url = format!("{CDN}/{version}-{pkg}");
            match fetch_one(&agent, &url, file) {
                Ok(data) => write_file(&partial, file, &data)?,
                Err(err) => post(true, format!("{file} unavailable: {err}")),
            }
        }
        std::fs::write(partial.join(MARKER), format!("{version} {CONTENT_REVISION}\n")).map_err(|e| e.to_string())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&partial, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    CONTENT_GENERATION.fetch_add(1, Ordering::Release);
    progress(&format!("installed in {}", dest.display()));
    Ok(version)
}

fn unpack_package(bytes: &[u8], root: &Path) -> Result<(), String> {
    let zip = Zip::parse(bytes)?;
    for entry in &zip.entries {
        if !entry.name.ends_with(['/', '\\']) {
            let data = zip.read(bytes, entry).map_err(|err| format!("{}: {err}", entry.name))?;
            write_file(root, &entry.name, &data)?;
        }
    }
    Ok(())
}

fn publish_player_scripts(dest: &Path, version: &str, bytes: &[u8]) -> Result<(), String> {
    if player_scripts_installed_at(dest).is_some() {
        return Ok(());
    }
    let controls = dest.with_extension("player-scripts");
    let partial = controls.with_extension(format!("player-scripts-partial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&partial);
    let result = (|| {
        unpack_package(bytes, &partial.join(SCRIPTS_FOLDER))?;
        if !crate::default_scripts::has_player_scripts(&partial) {
            return Err("The Studio scripts package is missing the Play controls".into());
        }
        std::fs::write(partial.join(MARKER), version).map_err(|err| err.to_string())?;
        if let Err(err) = std::fs::rename(&partial, &controls) {
            if player_scripts_installed_at(dest).is_none() {
                return Err(err.to_string());
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&partial);
    result
}

/// Write `name` (a zip path, `\` or `/` separated) under `root`, refusing
/// anything that would land outside it.
fn write_file(root: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let mut path = root.to_path_buf();
    for part in name.split(['\\', '/']) {
        match part {
            "" | "." => {}
            ".." => return Err(format!("unsafe path in package: {name}")),
            p if p.contains(':') => return Err(format!("unsafe path in package: {name}")),
            p => path.push(p),
        }
    }
    if path == root {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, data).map_err(|e| format!("{}: {e}", path.display()))
}

/// One file out of a remote zip: the end record, the central directory and
/// that one entry, by range requests.
fn fetch_one(agent: &ureq::Agent, url: &str, file: &str) -> Result<Vec<u8>, String> {
    let mut out = fetch_many(agent, url, &[file])?;
    Ok(out.remove(0).1)
}

/// Several files out of a remote zip, reading the central directory once.
fn fetch_many<'a>(agent: &ureq::Agent, url: &str, files: &[&'a str]) -> Result<Vec<(&'a str, Vec<u8>)>, String> {
    let size = content_length(agent, url)?;
    let tail_len = size.min(65_536 + 22);
    let tail = get(agent, url, Some((size - tail_len, size - 1)))?;
    let (cd_off, cd_len) = Zip::end_record(&tail).map_err(|e| format!("{url}: {e}"))?;
    let cd = get(agent, url, Some((cd_off, cd_off + cd_len - 1)))?;
    let entries = Zip::central(&cd).map_err(|e| format!("{url}: {e}"))?;
    let mut out = Vec::with_capacity(files.len());
    for &file in files {
        let want = file.trim_start_matches(['\\', '/']).replace('\\', "/");
        let e = entries
            .iter()
            .find(|e| {
                e.name
                    .trim_start_matches(['\\', '/'])
                    .replace('\\', "/")
                    .eq_ignore_ascii_case(&want)
            })
            .ok_or_else(|| format!("{file} not in {url}"))?;
        // Local header (30 bytes + name + extra) then the data; read generously.
        let span = 30 + 1024 + e.comp_size;
        let chunk = get(agent, url, Some((e.offset, (e.offset + span).min(size) - 1)))?;
        let mut local = e.clone();
        local.offset = 0;
        out.push((file, Zip::read_entry(&chunk, &local)?));
    }
    Ok(out)
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    method: u16,
    crc: u32,
    comp_size: u64,
    size: u64,
    offset: u64,
}

/// A minimal zip reader (stored + deflate, no zip64): enough for Roblox's
/// deployment packages.
struct Zip {
    entries: Vec<Entry>,
}

fn u16_at(b: &[u8], i: usize) -> Result<u16, String> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).ok_or_else(|| "truncated zip".into())
}
fn u32_at(b: &[u8], i: usize) -> Result<u32, String> {
    b.get(i..i + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "truncated zip".into())
}

impl Zip {
    fn parse(bytes: &[u8]) -> Result<Zip, String> {
        let (off, len) = Self::end_record(bytes)?;
        let cd = bytes
            .get(off as usize..(off + len) as usize)
            .ok_or("central directory out of range")?;
        Ok(Zip { entries: Self::central(cd)? })
    }

    /// (central directory offset, length) from the tail of a zip.
    fn end_record(tail: &[u8]) -> Result<(u64, u64), String> {
        let pos = tail
            .windows(4)
            .rposition(|w| w == b"PK\x05\x06")
            .ok_or("no zip end record")?;
        let len = u32_at(tail, pos + 12)? as u64;
        let off = u32_at(tail, pos + 16)? as u64;
        if len == 0xFFFF_FFFF || off == 0xFFFF_FFFF {
            return Err("zip64 not supported".into());
        }
        Ok((off, len))
    }

    fn central(cd: &[u8]) -> Result<Vec<Entry>, String> {
        let mut out = Vec::new();
        let mut p = 0;
        while p + 46 <= cd.len() && &cd[p..p + 4] == b"PK\x01\x02" {
            let method = u16_at(cd, p + 10)?;
            let crc = u32_at(cd, p + 16)?;
            let comp_size = u32_at(cd, p + 20)? as u64;
            let size = u32_at(cd, p + 24)? as u64;
            let nl = u16_at(cd, p + 28)? as usize;
            let el = u16_at(cd, p + 30)? as usize;
            let cl = u16_at(cd, p + 32)? as usize;
            let offset = u32_at(cd, p + 42)? as u64;
            let name = cd.get(p + 46..p + 46 + nl).ok_or("truncated zip")?;
            out.push(Entry {
                name: String::from_utf8_lossy(name).into_owned(),
                method,
                crc,
                comp_size,
                size,
                offset,
            });
            p += 46 + nl + el + cl;
        }
        Ok(out)
    }

    fn read(&self, bytes: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
        Self::read_entry(bytes, e)
    }

    /// The entry's data, given bytes where its local header starts at `e.offset`.
    fn read_entry(bytes: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
        let h = e.offset as usize;
        if bytes.get(h..h + 4) != Some(b"PK\x03\x04") {
            return Err("bad local header".into());
        }
        let nl = u16_at(bytes, h + 26)? as usize;
        let el = u16_at(bytes, h + 28)? as usize;
        let start = h + 30 + nl + el;
        let comp = bytes
            .get(start..start + e.comp_size as usize)
            .ok_or("entry data out of range")?;
        let data = match e.method {
            0 => comp.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(e.size as usize);
                flate2::read::DeflateDecoder::new(comp)
                    .read_to_end(&mut out)
                    .map_err(|err| format!("inflate: {err}"))?;
                out
            }
            m => return Err(format!("compression method {m} not supported")),
        };
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        if data.len() as u64 != e.size || crc.sum() != e.crc {
            return Err("checksum mismatch".into());
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_scripts_need_both_the_package_and_completion_marker() {
        let dest = std::env::temp_dir().join(format!("rs-sc-controls-test-{}", std::process::id()));
        let controls = dest.with_extension("player-scripts");
        write_file(&controls, "ExtraContent/scripts/PlayerScripts/PlayerScripts.rbxp", b"fixture").unwrap();
        assert!(player_scripts_installed_at(&dest).is_none());
        std::fs::write(controls.join(MARKER), "version-test").unwrap();
        assert_eq!(player_scripts_installed_at(&dest), Some(controls.clone()));
        let _ = std::fs::remove_dir_all(controls);
    }

    #[test]
    fn refuses_escaping_paths() {
        let d = std::env::temp_dir().join(format!("rs-sc-test-{}", std::process::id()));
        assert!(write_file(&d, "..\\evil.txt", b"x").is_err());
        assert!(write_file(&d, "a/../../evil.txt", b"x").is_err());
        assert!(write_file(&d, "C:\\evil.txt", b"x").is_err());
        assert!(write_file(&d, "\\sub\\ok.txt", b"x").is_ok());
        assert!(d.join("sub/ok.txt").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }
}
