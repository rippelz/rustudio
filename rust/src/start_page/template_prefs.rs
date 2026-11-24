//! The person's own take on "Open a Template": Roblox templates they hid,
//! templates they added from local place files, and the order of them all.
//!
//! Kept in `~/.config/rustudio/templates.json`. An added template's place
//! file (and its thumbnail, if one was set) is copied into
//! `~/.local/share/rustudio/templates/`, so moving or editing the original
//! afterwards doesn't change the template.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::templates::TemplateInfo;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemplatePrefs {
    /// Roblox templates hidden from the list, by root place.
    #[serde(default)]
    pub hidden: Vec<u64>,
    /// Templates added from place files, oldest first.
    #[serde(default)]
    pub custom: Vec<Custom>,
    /// Card order by key ([`Entry::key`]); keys not in it keep their
    /// natural place after the ordered ones.
    #[serde(default)]
    pub order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Custom {
    pub id: String,
    pub name: String,
    /// The copy the template opens.
    pub file: PathBuf,
    /// Where it was added from.
    pub source: PathBuf,
    /// A picture the person chose (wins over the preview).
    #[serde(default)]
    pub thumb: Option<PathBuf>,
    /// The 3D view the last time the template was opened ([`save_preview`]).
    #[serde(default)]
    pub preview: Option<PathBuf>,
    #[serde(default)]
    pub hidden: bool,
}

/// One card: a Roblox template or one of the person's.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Roblox(TemplateInfo),
    Custom(Custom),
}

impl Entry {
    pub fn key(&self) -> String {
        match self {
            Entry::Roblox(t) => format!("r:{}", t.place_id),
            Entry::Custom(c) => format!("c:{}", c.id),
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Entry::Roblox(t) => &t.name,
            Entry::Custom(c) => &c.name,
        }
    }
}

pub fn prefs_file() -> PathBuf {
    match std::env::var_os("RUSTUDIO_TEMPLATE_PREFS") {
        Some(p) => PathBuf::from(p),
        None => rustudio_core::paths::config().join("rustudio").join("templates.json"),
    }
}

fn store_dir() -> PathBuf {
    match std::env::var_os("RUSTUDIO_TEMPLATE_PREFS") {
        Some(p) => PathBuf::from(p).with_extension("d"),
        None => rustudio_core::paths::data().join("rustudio").join("templates"),
    }
}

impl TemplatePrefs {
    pub fn load() -> TemplatePrefs {
        std::fs::read_to_string(prefs_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let f = prefs_file();
        if let Some(d) = f.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let tmp = f.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &f).map_err(|e| format!("{}: {e}", f.display()))
    }

    pub fn is_hidden(&self, e: &Entry) -> bool {
        match e {
            Entry::Roblox(t) => self.hidden.contains(&t.place_id),
            Entry::Custom(c) => c.hidden,
        }
    }

    pub fn set_hidden(&mut self, key: &str, hide: bool) {
        if let Some(id) = key.strip_prefix("r:").and_then(|s| s.parse::<u64>().ok()) {
            self.hidden.retain(|h| *h != id);
            if hide {
                self.hidden.push(id);
            }
        } else if let Some(c) = self.custom_mut(key) {
            c.hidden = hide;
        }
    }

    pub fn custom_mut(&mut self, key: &str) -> Option<&mut Custom> {
        let id = key.strip_prefix("c:")?;
        self.custom.iter_mut().find(|c| c.id == id)
    }

    /// Every card in its order: the person's (newest first) then Roblox's, then
    /// sorted by `order` (stable, so new ones keep their place at the end).
    pub fn entries(&self, roblox: &[TemplateInfo]) -> Vec<Entry> {
        let mut v: Vec<Entry> = self.custom.iter().rev().cloned().map(Entry::Custom).chain(roblox.iter().cloned().map(Entry::Roblox)).collect();
        if !self.order.is_empty() {
            v.sort_by_key(|e| self.order.iter().position(|k| *k == e.key()).unwrap_or(usize::MAX));
        }
        v
    }

    /// Moves `from` next to `to` (before it, or after with `after`); the
    /// order is written out in full from the current list.
    pub fn move_entry(&mut self, roblox: &[TemplateInfo], from: &str, to: &str, after: bool) {
        if from == to {
            return;
        }
        let mut keys: Vec<String> = self.entries(roblox).iter().map(Entry::key).collect();
        let Some(i) = keys.iter().position(|k| k == from) else { return };
        let k = keys.remove(i);
        let Some(j) = keys.iter().position(|x| x == to) else { return };
        keys.insert(if after { j + 1 } else { j }, k);
        self.order = keys;
    }

    /// Adds a place file as a template: checked, then copied into the
    /// store. Returns its key.
    pub fn add(&mut self, source: &Path) -> Result<String, String> {
        check_place(source)?;
        let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("rbxl").to_lowercase();
        let id = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        let dir = store_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let file = dir.join(format!("{id}.{ext}"));
        std::fs::copy(source, &file).map_err(|e| format!("copying {}: {e}", source.display()))?;
        let name = source.file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_else(|| "My template".into());
        let c = Custom { id, name, file, source: source.to_path_buf(), thumb: None, preview: None, hidden: false };
        let key = format!("c:{}", c.id);
        if !self.order.is_empty() {
            self.order.insert(0, key.clone());
        }
        self.custom.push(c);
        Ok(key)
    }

    /// Replaces a template's place file with another one.
    pub fn replace_file(&mut self, key: &str, source: &Path) -> Result<(), String> {
        check_place(source)?;
        let c = self.custom_mut(key).ok_or("that template is gone")?;
        let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("rbxl").to_lowercase();
        let file = c.file.with_extension(ext);
        std::fs::copy(source, &file).map_err(|e| format!("copying {}: {e}", source.display()))?;
        if file != c.file {
            let _ = std::fs::remove_file(&c.file);
        }
        c.file = file;
        c.source = source.to_path_buf();
        if let Some(p) = c.preview.take() {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }

    /// Sets a template's thumbnail from an image (cropped square, 256 px).
    pub fn set_thumb(&mut self, key: &str, image: &Path) -> Result<PathBuf, String> {
        let img = image::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
        let side = img.width().min(img.height());
        let img = img.crop_imm((img.width() - side) / 2, (img.height() - side) / 2, side, side).thumbnail(256, 256);
        let c = self.custom_mut(key).ok_or("that template is gone")?;
        // A new name each time: the page keys its texture by path.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let out = store_dir().join(format!("{}-thumb-{stamp:x}.png", c.id));
        img.save(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        if let Some(old) = c.thumb.replace(out.clone()) {
            let _ = std::fs::remove_file(old);
        }
        Ok(out)
    }

    /// A new preview replaces the old one (file and all).
    pub fn set_preview(&mut self, key: &str, preview: PathBuf) {
        match self.custom_mut(key) {
            Some(c) => {
                if let Some(old) = c.preview.replace(preview) {
                    let _ = std::fs::remove_file(old);
                }
            }
            None => {
                let _ = std::fs::remove_file(preview);
            }
        }
    }

    /// Removes one of the person's templates and its stored files.
    pub fn remove(&mut self, key: &str) {
        let Some(id) = key.strip_prefix("c:") else { return };
        if let Some(i) = self.custom.iter().position(|c| c.id == id) {
            let c = self.custom.remove(i);
            let _ = std::fs::remove_file(&c.file);
            for f in [c.thumb, c.preview].into_iter().flatten() {
                let _ = std::fs::remove_file(f);
            }
        }
        self.order.retain(|k| k != key);
    }

    /// Roblox's templates all shown again, in Roblox's order (the person's
    /// own stay, first).
    pub fn restore_defaults(&mut self) {
        self.hidden.clear();
        self.order.clear();
    }
}

/// A template's preview from the 3D view as it was drawn (RGBA8, top row
/// first): the middle cut to the card's shape, 528x336, saved beside its
/// place file. None when it shows next to nothing (sky, fog, a void).
pub fn save_preview(id: &str, w: u32, h: u32, rgba: Vec<u8>) -> Result<Option<PathBuf>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("the frame has the wrong size")?;
    let (tw, th) = (528u32, 336u32);
    // The largest centred crop with the card's aspect.
    let (cw, ch) = if w * th > h * tw { (h * tw / th, h) } else { (w, w * th / tw) };
    let crop = image::imageops::crop_imm(&img, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image();
    let small = image::imageops::resize(&crop, tw, th, image::imageops::FilterType::Triangle);
    if blank(&image::DynamicImage::ImageRgba8(small.clone()).to_luma8()) {
        return Ok(None);
    }
    let dir = store_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let out = dir.join(format!("{id}-preview-{stamp:x}.png"));
    image::DynamicImage::ImageRgba8(small).to_rgb8().save(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(Some(out))
}

/// Next to no detail: sky, fog or an empty void.
fn blank(img: &image::GrayImage) -> bool {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return true;
    }
    // Edges: how many pixels differ clearly from their right / lower neighbour.
    let mut edges = 0u32;
    let mut n = 0u32;
    for y in (0..h - 1).step_by(2) {
        for x in (0..w - 1).step_by(2) {
            let v = img.get_pixel(x, y).0[0] as i32;
            let dx = (v - img.get_pixel(x + 1, y).0[0] as i32).abs();
            let dy = (v - img.get_pixel(x, y + 1).0[0] as i32).abs();
            if dx.max(dy) > 12 {
                edges += 1;
            }
            n += 1;
        }
    }
    edges * 100 < n
}

/// A Roblox place file, binary or XML (by its header, not its name).
fn check_place(p: &Path) -> Result<(), String> {
    use std::io::Read;
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext != "rbxl" && ext != "rbxlx" {
        return Err(format!("{} isn't a place file (.rbxl or .rbxlx)", p.display()));
    }
    let mut head = [0u8; 64];
    let n = std::fs::File::open(p).and_then(|mut f| f.read(&mut head)).map_err(|e| format!("{}: {e}", p.display()))?;
    let text = String::from_utf8_lossy(&head[..n]);
    if text.starts_with("<roblox!") || text.trim_start().starts_with("<roblox") {
        Ok(())
    } else {
        Err(format!("{} doesn't look like a Roblox place file", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start_page::templates::measured;

    #[test]
    fn order_hide_add_remove() {
        let dir = std::env::temp_dir().join(format!("rustudio-tpl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: only this test reads the variable.
        unsafe { std::env::set_var("RUSTUDIO_TEMPLATE_PREFS", dir.join("templates.json")) };
        let roblox = measured();
        let mut p = TemplatePrefs::default();
        assert_eq!(p.entries(&roblox)[0].key(), "r:95206881");

        // Move Village before Baseplate.
        p.move_entry(&roblox, "r:520390648", "r:95206881", false);
        assert_eq!(p.entries(&roblox)[0].name(), "Village");
        assert_eq!(p.entries(&roblox)[1].name(), "Baseplate");

        p.set_hidden("r:95206881", true);
        assert!(p.is_hidden(&p.entries(&roblox)[1]));
        assert!(!p.is_hidden(&p.entries(&roblox)[0]));

        let src = dir.join("My_Game.rbxl");
        std::fs::write(&src, b"<roblox!\x89\xff\r\n\x1a\n rest").unwrap();
        let key = p.add(&src).unwrap();
        let last = p.entries(&roblox)[0].clone();
        assert_eq!(last.key(), key);
        assert_eq!(last.name(), "My Game");
        let Entry::Custom(c) = last else { panic!() };
        assert!(c.file.exists() && c.file != src);

        std::fs::write(dir.join("notes.rbxl"), b"hello").unwrap();
        assert!(p.add(&dir.join("notes.rbxl")).is_err());

        p.remove(&key);
        assert!(!c.file.exists());
        assert!(p.entries(&roblox).iter().all(|e| e.key() != key));

        let sky = image::GrayImage::from_fn(64, 40, |_, y| image::Luma([120 + (y as u8) / 2]));
        assert!(blank(&sky));
        let busy = image::GrayImage::from_fn(64, 40, |x, y| image::Luma([if (x / 3 + y / 3) % 2 == 0 { 40 } else { 200 }]));
        assert!(!blank(&busy));

        // A preview: the middle of a wide frame at the card's shape; a blank
        // frame gives none.
        let (w, h) = (1600u32, 900u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| { let v = if (i % w / 7 + i / w / 7) % 2 == 0 { 30 } else { 220 }; [v, v, v, 255] }).collect();
        let out = save_preview("pv", w, h, rgba).unwrap().unwrap();
        assert_eq!(image::image_dimensions(&out).unwrap(), (528, 336));
        assert!(save_preview("pv", w, h, vec![128; (w * h * 4) as usize]).unwrap().is_none());

        p.restore_defaults();
        assert_eq!(p.entries(&roblox)[0].name(), "Baseplate");
        assert!(p.hidden.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
