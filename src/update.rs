//! Update notice: on every start, rustudio asks GitHub for the latest
//! release. When it is newer than this build, a small notice at the top
//! right offers it. Update downloads this platform's file, checks it against
//! the release's SHA256SUMS, puts it in place of the running copy and asks
//! for a restart. A build from source (running out of a cargo `target`
//! folder) gets `git pull` instructions instead. Dismissing the notice hides
//! it for 12 hours. `RUSTUDIO_NO_UPDATE_CHECK=1` turns the check off;
//! `RUSTUDIO_UPDATE_URL` points it at another release feed (for testing).

use crate::theme;
use egui::{Align2, RichText};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FEED: &str = "https://api.github.com/repos/rippelz/rustudio/releases/latest";
const SNOOZE: Duration = Duration::from_secs(12 * 3600);

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// This platform's download: (file name, URL).
    asset: Option<(String, String)>,
    sums: Option<String>,
}

#[derive(Clone)]
enum Stage {
    Checking,
    Current,
    Available(Release),
    Downloading(Release),
    Installed(Release),
    Failed(Release, String),
}

pub struct Updater {
    stage: Arc<Mutex<Stage>>,
    hidden: bool,
    progress: Arc<Progress>,
}

/// Bytes of the download so far, and its size once the server says.
#[derive(Default)]
struct Progress {
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
}

impl Progress {
    fn fraction(&self) -> Option<f32> {
        use std::sync::atomic::Ordering::Relaxed;
        let t = self.total.load(Relaxed);
        (t > 0).then(|| (self.done.load(Relaxed) as f32 / t as f32).min(1.0))
    }
}

fn progress_text(p: &Progress) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let mb = |b: u64| b as f64 / 1_048_576.0;
    match (p.done.load(Relaxed), p.total.load(Relaxed)) {
        (d, 0) => format!("{:.1} MB", mb(d)),
        (d, t) => format!("{:.1} of {:.1} MB", mb(d), mb(t)),
    }
}

/// Where the newly installed executable is, once an update has been put in
/// place: a restart must start that one, not the renamed old file.
static INSTALLED_EXE: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn installed_exe() -> Option<PathBuf> {
    INSTALLED_EXE.lock().ok()?.clone()
}

pub enum Action {
    None,
    Restart,
}

impl Updater {
    pub fn start() -> Self {
        let stage = Arc::new(Mutex::new(Stage::Checking));
        let off = std::env::var_os("RUSTUDIO_NO_UPDATE_CHECK").is_some()
            || std::env::var_os("RUSTUDIO_UI_SHOTS").is_some();
        if off {
            *stage.lock().unwrap() = Stage::Current;
        } else {
            let s = stage.clone();
            std::thread::Builder::new()
                .name("update check".into())
                .spawn(move || {
                    remove_leftovers();
                    let next = match check() {
                        Ok(Some(r)) => Stage::Available(r),
                        Ok(None) => Stage::Current,
                        Err(e) => {
                            eprintln!("update check: {e}");
                            Stage::Current
                        }
                    };
                    *s.lock().unwrap() = next;
                })
                .ok();
        }
        Updater { stage, hidden: snoozed(), progress: Arc::default() }
    }

    /// The notice, if there is something to show. Call once per frame.
    pub fn show(&mut self, ctx: &egui::Context) -> Action {
        let stage = self.stage.lock().unwrap().clone();
        match &stage {
            Stage::Checking | Stage::Current => return Action::None,
            Stage::Available(_) if self.hidden => return Action::None,
            Stage::Downloading(_) => ctx.request_repaint_after(Duration::from_millis(100)),
            _ => {}
        }
        let mut action = Action::None;
        let mut close = false;
        let ours = env!("CARGO_PKG_VERSION");
        let width = 320.0;
        egui::Area::new(egui::Id::new("update notice"))
            .anchor(Align2::RIGHT_TOP, [-12.0, 66.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let stripe = match &stage {
                    Stage::Failed(..) => theme::ERROR,
                    _ => theme::ACCENT,
                };
                let frame = egui::Frame::new()
                    .fill(theme::PANEL_DARK)
                    .stroke(egui::Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(6)
                    .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: egui::Color32::from_black_alpha(110) })
                    .inner_margin(egui::Margin { left: 16, right: 12, top: 12, bottom: 12 })
                    .show(ui, |ui| {
                        ui.set_width(width - 28.0);
                        ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                        // Header: logo, title and subtitle, close.
                        let (title, subtitle) = match &stage {
                            Stage::Available(r) => ("Update available".to_string(), format!("{ours}  →  {}", r.version)),
                            Stage::Downloading(r) => (format!("Downloading {}", r.version), progress_text(&self.progress)),
                            Stage::Installed(r) => ("Update ready".to_string(), format!("{} starts when rustudio restarts.", r.version)),
                            Stage::Failed(..) => ("Update failed".to_string(), "Nothing was changed.".to_string()),
                            _ => unreachable!(),
                        };
                        ui.horizontal(|ui| {
                            crate::icons::logo(ui, 30.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                let color = if matches!(stage, Stage::Failed(..)) { theme::ERROR } else { theme::TEXT };
                                ui.label(RichText::new(title).size(14.0).strong().color(color));
                                ui.label(RichText::new(subtitle).size(12.0).color(theme::TEXT_DIM));
                            });
                            if !matches!(stage, Stage::Downloading(_)) {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                    let x = ui.add(egui::Button::new(RichText::new("✕").size(12.0).color(theme::TEXT_DIM)).frame(false));
                                    let tip = if matches!(stage, Stage::Available(_)) { "Hide for 12 hours" } else { "Close" };
                                    if x.on_hover_text(tip).clicked() {
                                        close = true;
                                    }
                                });
                            }
                        });
                        // Body.
                        match &stage {
                            Stage::Downloading(_) => {
                                ui.add_space(6.0);
                                let f = self.progress.fraction();
                                let bar = egui::ProgressBar::new(f.unwrap_or(0.0)).desired_height(4.0).fill(theme::ACCENT).corner_radius(2);
                                ui.add(if f.is_none() { bar.animate(true) } else { bar });
                            }
                            Stage::Failed(_, e) => {
                                ui.add_space(4.0);
                                ui.label(RichText::new(e).size(12.0).color(theme::TEXT));
                            }
                            Stage::Available(_) if source_build() => {
                                ui.add_space(4.0);
                                ui.label(RichText::new("This copy was built from source. Update it with:").size(12.0).color(theme::TEXT_DIM));
                                egui::Frame::new().fill(theme::BG).corner_radius(3).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new("git pull && cargo build --release").monospace().size(12.0).color(theme::TEXT));
                                });
                            }
                            Stage::Available(r) if r.asset.is_none() => {
                                ui.add_space(4.0);
                                ui.label(RichText::new("There's no download for this platform yet.").size(12.0).color(theme::TEXT_DIM));
                            }
                            _ => {}
                        }
                        // Buttons, right-aligned.
                        if !matches!(stage, Stage::Downloading(_)) {
                            ui.add_space(8.0);
                            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 26.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let primary = |text: &str| egui::Button::new(RichText::new(text).color(egui::Color32::WHITE)).fill(theme::ACCENT).min_size(egui::vec2(84.0, 26.0));
                                let secondary = |text: &str| egui::Button::new(RichText::new(text).color(theme::TEXT)).fill(egui::Color32::TRANSPARENT).min_size(egui::vec2(0.0, 26.0));
                                match &stage {
                                    Stage::Available(r) => {
                                        if !source_build() && r.asset.is_some() && ui.add(primary("Update")).clicked() {
                                            self.install(r.clone());
                                        }
                                        if ui.add(secondary("What's new")).clicked() {
                                            ctx.open_url(egui::OpenUrl::new_tab(&r.page));
                                        }
                                    }
                                    Stage::Installed(_) => {
                                        if ui.add(primary("Restart now")).clicked() {
                                            action = Action::Restart;
                                        }
                                        if ui.add(secondary("Later")).clicked() {
                                            close = true;
                                        }
                                    }
                                    Stage::Failed(r, _) => {
                                        if ui.add(primary("Download page")).clicked() {
                                            ctx.open_url(egui::OpenUrl::new_tab(&r.page));
                                        }
                                    }
                                    _ => {}
                                }
                            });
                        }
                    });
                // Accent stripe down the left edge.
                let r = frame.response.rect;
                let stripe_rect = egui::Rect::from_min_max(r.min + egui::vec2(1.0, 1.0), egui::pos2(r.min.x + 4.0, r.max.y - 1.0));
                ui.painter().rect_filled(stripe_rect, egui::CornerRadius { nw: 5, sw: 5, ne: 0, se: 0 }, stripe);
            });
        if close {
            match stage {
                Stage::Available(_) => {
                    self.hidden = true;
                    snooze();
                }
                _ => *self.stage.lock().unwrap() = Stage::Current,
            }
        }
        action
    }

    fn install(&self, r: Release) {
        *self.stage.lock().unwrap() = Stage::Downloading(r.clone());
        let s = self.stage.clone();
        let progress = self.progress.clone();
        std::thread::Builder::new()
            .name("update install".into())
            .spawn(move || {
                let next = match install(&r, &progress) {
                    Ok(exe) => {
                        *INSTALLED_EXE.lock().unwrap() = Some(exe);
                        Stage::Installed(r)
                    }
                    Err(e) => Stage::Failed(r, e),
                };
                *s.lock().unwrap() = next;
            })
            .ok();
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(concat!("rustudio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    resp.body_mut().with_config().limit(1 << 30).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

/// `get`, counting bytes into `progress` as they arrive.
fn download(url: &str, progress: &Progress) -> Result<Vec<u8>, String> {
    use std::io::Read;
    use std::sync::atomic::Ordering::Relaxed;
    let mut resp = agent().get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok()).unwrap_or(0);
    progress.total.store(total, Relaxed);
    progress.done.store(0, Relaxed);
    let mut reader = resp.body_mut().with_config().limit(1 << 30).reader();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        progress.done.store(out.len() as u64, Relaxed);
    }
    Ok(out)
}

fn version_of(tag: &str) -> Option<[u64; 3]> {
    let mut it = tag.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<u64>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

fn asset_suffix() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("macos-universal.zip")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64.tar.gz")
    } else {
        None
    }
}

fn check() -> Result<Option<Release>, String> {
    let url = std::env::var("RUSTUDIO_UPDATE_URL").unwrap_or_else(|_| FEED.into());
    let feed: serde_json::Value = serde_json::from_slice(&get(&url)?).map_err(|e| format!("{url}: {e}"))?;
    let tag = feed["tag_name"].as_str().ok_or("release without tag_name")?;
    let (Some(latest), Some(ours)) = (version_of(tag), version_of(env!("CARGO_PKG_VERSION"))) else {
        return Err(format!("can't compare versions {tag} and {}", env!("CARGO_PKG_VERSION")));
    };
    if latest <= ours {
        return Ok(None);
    }
    let assets: Vec<(String, String)> = feed["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x["name"].as_str()?.to_string(), x["browser_download_url"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let asset = asset_suffix().and_then(|suf| assets.iter().find(|(n, _)| n.ends_with(suf)).cloned());
    let sums = assets.iter().find(|(n, _)| n == "SHA256SUMS").map(|(_, u)| u.clone());
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        page: feed["html_url"].as_str().unwrap_or("https://github.com/rippelz/rustudio/releases").to_string(),
        asset,
        sums,
    }))
}

/// Running out of a cargo `target/{debug,release}` folder: built from source.
fn source_build() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let parts: Vec<_> = exe.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
    parts.windows(2).any(|w| w[0] == "target" && (w[1] == "release" || w[1] == "debug"))
}

fn snooze_file() -> PathBuf {
    crate::paths::config().join("rustudio").join("update-dismissed")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn snoozed() -> bool {
    std::fs::read_to_string(snooze_file())
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|t| now() < t + SNOOZE.as_secs())
}

fn snooze() {
    let f = snooze_file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, now().to_string());
}

/// The folder whose contents an update replaces: the `.app` bundle on macOS,
/// else the executable's folder.
fn install_root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| format!("can't find rustudio's own file: {e}"))?;
    if cfg!(target_os = "macos") {
        exe.ancestors()
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .map(Path::to_path_buf)
            .ok_or_else(|| "rustudio isn't running from an .app bundle".into())
    } else {
        exe.parent().map(Path::to_path_buf).ok_or_else(|| "rustudio's folder is unknown".into())
    }
}

/// Old copies an earlier update renamed out of the way.
fn remove_leftovers() {
    let Ok(root) = install_root() else { return };
    if cfg!(target_os = "macos") {
        let _ = std::fs::remove_dir_all(root.with_extension("app.old"));
    } else if let Ok(dir) = std::fs::read_dir(&root) {
        for e in dir.flatten() {
            if e.file_name().to_string_lossy().ends_with(".update-old") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn install(r: &Release, progress: &Progress) -> Result<PathBuf, String> {
    let (name, url) = r.asset.clone().ok_or("no download for this platform")?;
    let root = install_root()?;
    let data = download(&url, progress)?;
    let sums_url = r.sums.clone().ok_or("the release has no SHA256SUMS")?;
    let sums = String::from_utf8_lossy(&get(&sums_url)?).to_string();
    let want = sums
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let (h, f) = (it.next()?, it.next()?);
            (f.trim_start_matches('*') == name).then(|| h.to_ascii_lowercase())
        })
        .ok_or_else(|| format!("{name} isn't in SHA256SUMS"))?;
    let got: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if got != want {
        return Err(format!("{name}: checksum mismatch, nothing was changed"));
    }
    // Unpack next to the install so the final renames stay on one disk.
    let stage = root.parent().ok_or("no folder above the install")?.join(format!(".rustudio-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("can't write next to {}: {e}", root.display()))?;
    let result = unpack_and_swap(&data, &name, &stage, &root);
    let _ = std::fs::remove_dir_all(&stage);
    result
}

fn unpack_and_swap(data: &[u8], name: &str, stage: &Path, root: &Path) -> Result<PathBuf, String> {
    let archive = stage.join(name);
    std::fs::write(&archive, data).map_err(|e| e.to_string())?;
    let out = stage.join("out");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("ditto").arg("-x").arg("-k").arg(&archive).arg(&out).status()
    } else {
        // Windows 10+ ships bsdtar as tar.exe, which reads zip too.
        std::process::Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out).status()
    };
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => return Err(format!("unpacking {name} failed ({s})")),
        Err(e) => return Err(format!("can't run the unpacker: {e}")),
    }
    if cfg!(target_os = "macos") {
        let new_app = out.join("rustudio.app");
        if !new_app.is_dir() {
            return Err(format!("{name} has no rustudio.app"));
        }
        let old = root.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(root, &old).map_err(|e| format!("can't move {}: {e}", root.display()))?;
        if let Err(e) = std::fs::rename(&new_app, root) {
            let _ = std::fs::rename(&old, root);
            return Err(format!("can't put the new app in place: {e}"));
        }
        return Ok(root.join("Contents/MacOS/rustudio"));
    }
    // Linux / Windows: one folder holding the executable and its data files.
    let folder = std::fs::read_dir(&out)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .ok_or_else(|| format!("{name} has no folder inside"))?;
    let exe_name = if cfg!(windows) { "rustudio.exe" } else { "rustudio" };
    if !folder.join(exe_name).is_file() {
        return Err(format!("{name} has no {exe_name}"));
    }
    for file in [exe_name, "api-dump.json"] {
        let src = folder.join(file);
        let dst = root.join(file);
        if !src.is_file() {
            continue;
        }
        let tmp = root.join(format!("{file}.update-new"));
        std::fs::copy(&src, &tmp).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
        if dst.exists() {
            // A running executable can be renamed (Windows too), not overwritten.
            std::fs::rename(&dst, root.join(format!("{file}.update-old"))).map_err(|e| format!("can't move {}: {e}", dst.display()))?;
        }
        std::fs::rename(&tmp, &dst).map_err(|e| format!("can't put {} in place: {e}", dst.display()))?;
    }
    Ok(root.join(exe_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert_eq!(version_of("v0.1.0"), Some([0, 1, 0]));
        assert_eq!(version_of("0.10.2"), Some([0, 10, 2]));
        assert_eq!(version_of("v1.2.3-rc1"), Some([1, 2, 3]));
        assert!(version_of("v0.2.0") > version_of("v0.1.9"));
        assert!(version_of("v0.10.0") > version_of("v0.9.0"));
        assert_eq!(version_of("latest"), None);
    }
}
