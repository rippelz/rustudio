//! Group icons (thumbnails `v1/groups/icons`, 150x150 PNG) for the Start
//! Page, the publish destination menu and Open from Roblox, as Studio's
//! Start Page shows them: one process-wide set. Kept on disk
//! (`<cache>/rustudio/group-icons/<id>.png`, asked again after a week, the
//! old one shown meanwhile) so later launches draw them at once. Disk reads,
//! the batched thumbnail asks and decoding run on a worker thread; `get`
//! only uploads what has arrived.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use egui::{CornerRadius, Rect};
use rustudio_engine::theme;

/// Pixels kept per icon: rows are ~28 px, twice that on HiDPI.
const DECODE: u32 = 64;
const DISK_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const BATCH: usize = 50;
const DOWNLOADS: usize = 6;

#[derive(Default)]
struct State {
    asked: HashSet<u64>,
    decoded: Vec<(u64, egui::ColorImage)>,
    textures: HashMap<u64, egui::TextureHandle>,
    placeholder: Option<egui::TextureHandle>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn dir() -> PathBuf {
    rustudio_core::paths::cache().join("rustudio").join("group-icons")
}

/// Starts loading these groups' icons (each once per run); `thumbnails`
/// is the thumbnails host (`CloudEndpoints::thumbnails`).
pub fn request(ctx: &egui::Context, thumbnails: &str, ids: impl IntoIterator<Item = u64>) {
    let ids: Vec<u64> = {
        let mut s = STATE.lock().unwrap();
        ids.into_iter().filter(|i| *i > 0 && s.asked.insert(*i)).collect()
    };
    if ids.is_empty() {
        return;
    }
    let ctx = ctx.clone();
    let host = thumbnails.trim_end_matches('/').to_string();
    std::thread::spawn(move || load(&ctx, &host, &dir(), &ids));
}

fn load(ctx: &egui::Context, host: &str, dir: &Path, ids: &[u64]) {
    let push = |id: u64, bytes: &[u8]| {
        let Ok(img) = image::load_from_memory(bytes) else { return false };
        let img = img.thumbnail(DECODE, DECODE).to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        STATE.lock().unwrap().decoded.push((id, egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())));
        ctx.request_repaint();
        true
    };
    let mut fetch = Vec::new();
    for &id in ids {
        let path = dir.join(format!("{id}.png"));
        let fresh = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < DISK_TTL);
        let shown = std::fs::read(&path).is_ok_and(|b| push(id, &b));
        if !(shown && fresh) {
            fetch.push((id, shown));
        }
    }
    let mut todo = Vec::new();
    for chunk in fetch.chunks(BATCH) {
        let list: Vec<String> = chunk.iter().map(|(i, _)| i.to_string()).collect();
        let url = format!("{host}/v1/groups/icons?groupIds={}&size=150x150&format=Png&isCircular=false", list.join(","));
        let Some(v) = get(&url).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else { continue };
        todo.extend(completed(&v));
    }
    // The PNGs from the CDN, a few at a time.
    let todo = Mutex::new(todo.into_iter());
    std::thread::scope(|sc| {
        for _ in 0..DOWNLOADS {
            sc.spawn(|| loop {
                let Some((id, url)) = todo.lock().unwrap().next() else { break };
                let Some(bytes) = get(&url) else { continue };
                let had = fetch.iter().any(|(i, shown)| *i == id && *shown);
                if !had && !push(id, &bytes) {
                    continue;
                }
                let _ = std::fs::create_dir_all(dir);
                let tmp = dir.join(format!("{id}.png.tmp"));
                if std::fs::write(&tmp, &bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join(format!("{id}.png")));
                }
            });
        }
    });
}

fn get(url: &str) -> Option<Vec<u8>> {
    match crate::toolbox::api::anon_get(url) {
        Ok((200, b)) => Some(b),
        _ => None,
    }
}

/// `(groupId, imageUrl)` for each finished thumbnail.
fn completed(v: &serde_json::Value) -> Vec<(u64, String)> {
    v.get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter(|t| t.get("state").and_then(|s| s.as_str()) == Some("Completed"))
        .filter_map(|t| Some((t.get("targetId")?.as_u64()?, t.get("imageUrl")?.as_str()?.to_string())))
        .collect()
}

/// The group's icon, once loaded.
pub fn get_texture(ctx: &egui::Context, id: u64) -> Option<egui::TextureId> {
    let mut s = STATE.lock().unwrap();
    for (gid, img) in std::mem::take(&mut s.decoded) {
        let t = ctx.load_texture(format!("group_icon_{gid}"), img, egui::TextureOptions::LINEAR);
        s.textures.insert(gid, t);
    }
    s.textures.get(&id).map(|t| t.id())
}

/// The icon in `r`, a plain square until it loads.
pub fn paint(ui: &egui::Ui, r: Rect, id: u64) {
    match get_texture(ui.ctx(), id) {
        Some(t) => {
            egui::Image::new((t, r.size())).corner_radius(CornerRadius::same(2)).paint_at(ui, r);
        }
        None => {
            ui.painter().rect_filled(r, CornerRadius::same(2), theme::PANEL_ALT);
        }
    }
}

/// The icon as an `Image` of `side` points (for buttons and labels), a
/// plain square until it loads.
pub fn image(ctx: &egui::Context, id: u64, side: f32) -> egui::Image<'static> {
    let t = get_texture(ctx, id).unwrap_or_else(|| {
        let mut s = STATE.lock().unwrap();
        s.placeholder
            .get_or_insert_with(|| ctx.load_texture("group_icon_placeholder", egui::ColorImage::new([1, 1], vec![theme::PANEL_ALT]), egui::TextureOptions::NEAREST))
            .id()
    });
    egui::Image::new((t, egui::vec2(side, side))).corner_radius(CornerRadius::same(2))
}

/// `paint` as a widget of `side` points.
pub fn show(ui: &mut egui::Ui, id: u64, side: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    paint(ui, r, id);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_completed_thumbnails() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"data":[{"targetId":7,"state":"Completed","imageUrl":"https://t/a.png"},
                       {"targetId":8,"state":"Pending","imageUrl":""},
                       {"targetId":9,"state":"Blocked","imageUrl":"https://t/b.png"}]}"#,
        )
        .unwrap();
        assert_eq!(completed(&v), vec![(7, "https://t/a.png".to_string())]);
    }
}
