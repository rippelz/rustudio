//! The emote wheel in Play ("." opens it, as Roblox's Emotes menu): the
//! avatar's equipped emotes around a ring with their thumbnails (Roblox's
//! built-in ones when none are equipped). Click one or press its number;
//! ".", Esc or a click outside closes it. Picking sends "/e name" through
//! the chat path (players_chat.luau), which plays it with
//! Humanoid:PlayEmote.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use egui::{Color32, Pos2, Rect, Vec2};

use crate::app::Editor;
use crate::theme;

/// Animate's own emotes, for avatars with none equipped.
const BUILT_IN: &[&str] = &["wave", "point", "cheer", "laugh", "dance", "dance2", "dance3"];

struct Wheel {
    open: bool,
    thumbs: HashMap<u64, egui::TextureHandle>,
    asked: HashSet<u64>,
    tx: Sender<(u64, egui::ColorImage)>,
    rx: Receiver<(u64, egui::ColorImage)>,
}

fn wheel() -> &'static Mutex<Wheel> {
    static W: OnceLock<Mutex<Wheel>> = OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = channel();
        Mutex::new(Wheel { open: false, thumbs: HashMap::new(), asked: HashSet::new(), tx, rx })
    })
}

/// "." pressed in Play.
pub fn toggle() {
    if let Ok(mut w) = wheel().lock() {
        w.open = !w.open;
    }
}

pub fn is_open() -> bool {
    wheel().lock().map(|w| w.open).unwrap_or(false)
}

impl Editor {
    /// Draws the wheel over the game view; number keys pick from it while
    /// it's open (they're taken out of `slots`, the hotbar's).
    pub fn emote_wheel(&mut self, ui: &mut egui::Ui, rect: Rect, slots: &mut Vec<usize>) {
        let Ok(mut w) = wheel().lock() else { return };
        if self.play.is_none() {
            w.open = false;
            return;
        }
        if !w.open {
            return;
        }
        let ctx = ui.ctx().clone();
        while let Ok((id, img)) = w.rx.try_recv() {
            let t = ctx.load_texture(format!("emote-{id}"), img, egui::TextureOptions::LINEAR);
            w.thumbs.insert(id, t);
        }
        // (slot 1-8, label, emote name for /e, asset id)
        let mut items: Vec<(u32, String, String, Option<u64>)> = Vec::new();
        if let Some(look) = self.account.avatar() {
            let mut eq: Vec<_> = look.emotes.iter().filter(|e| e.2 > 0).cloned().collect();
            eq.sort_by_key(|e| e.2);
            for (name, id, slot) in eq.into_iter().take(8) {
                items.push((slot, name.clone(), name, Some(id)));
            }
        }
        if items.is_empty() {
            for (n, name) in BUILT_IN.iter().enumerate() {
                let mut label = name.to_string();
                label[..1].make_ascii_uppercase();
                items.push((n as u32 + 1, label, name.to_string(), None));
            }
        }
        let want: Vec<u64> = items.iter().filter_map(|i| i.3).filter(|id| !w.asked.contains(id)).collect();
        if !want.is_empty() {
            w.asked.extend(want.iter().copied());
            let tx = w.tx.clone();
            std::thread::spawn(move || {
                let got = rustudio_account::characters::thumbnails(rustudio_account::characters::Thumb::Asset, &want)
                    .unwrap_or_default();
                for (id, bytes) in got {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let img = img.thumbnail(150, 150).to_rgba8();
                        let size = [img.width() as usize, img.height() as usize];
                        let _ = tx.send((id, egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())));
                    }
                }
            });
        }

        let center = rect.center();
        let radius = (rect.height().min(rect.width()) * 0.32).clamp(120.0, 220.0);
        let cell = (radius * 0.52).min(110.0);
        let painter = ui.painter_at(rect).with_layer_id(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("emote_wheel")));
        // A ring (the character stays visible in the middle) with a slot
        // plate for every position, filled or not.
        painter.circle_stroke(center, radius, egui::Stroke::new(cell * 1.3, Color32::from_black_alpha(120)));
        painter.circle_filled(center, 74.0, Color32::from_black_alpha(140));
        for k in 0..8usize {
            if items.iter().any(|i| (i.0 as usize).saturating_sub(1) % 8 == k) {
                continue;
            }
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / 8.0;
            let c = center + Vec2::new(a.cos(), a.sin()) * radius;
            let r = Rect::from_center_size(c, Vec2::splat(cell * 0.8));
            painter.rect_stroke(r, cell * 0.2, egui::Stroke::new(1.0, Color32::from_white_alpha(40)), egui::StrokeKind::Inside);
            painter.text(r.center(), egui::Align2::CENTER_CENTER, (k + 1).to_string(), egui::FontId::proportional(13.0), Color32::from_white_alpha(70));
        }
        let mut picked: Option<String> = None;
        let mut hovered_label: Option<String> = None;
        let n = 8usize;
        for (slot, label, name, id) in &items {
            let k = (*slot as usize).saturating_sub(1) % n;
            // Slot 1 at the top, clockwise.
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / n as f32;
            let c = center + Vec2::new(a.cos(), a.sin()) * radius;
            let r = Rect::from_center_size(c, Vec2::splat(cell));
            let resp = ui.interact(r, ui.id().with(("emote", *slot)), egui::Sense::click());
            let hot = resp.hovered();
            painter.rect_filled(r, cell * 0.22, if hot { theme::ACCENT.gamma_multiply(0.85) } else { Color32::from_rgba_unmultiplied(40, 42, 50, 230) });
            match id.and_then(|id| w.thumbs.get(&id)) {
                Some(t) => {
                    painter.image(t.id(), r.shrink(cell * 0.1), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    crate::ti::paint(&painter, Rect::from_center_size(r.center(), Vec2::splat(cell * 0.42)), "mood-happy", Color32::WHITE);
                }
            }
            painter.text(r.min + Vec2::new(8.0, 6.0), egui::Align2::LEFT_TOP, slot.to_string(), egui::FontId::proportional(13.0), Color32::from_white_alpha(200));
            if hot {
                hovered_label = Some(label.clone());
            }
            if resp.clicked() {
                picked = Some(name.clone());
            }
        }
        // The name in the middle (hovered, else a hint).
        let (title, sub) = match &hovered_label {
            Some(l) => (l.clone(), "Click to play".to_string()),
            None => ("Emotes".to_string(), "1–8 or click · Esc closes".to_string()),
        };
        painter.text(center - Vec2::new(0.0, 10.0), egui::Align2::CENTER_CENTER, title, egui::FontId::proportional(20.0), Color32::WHITE);
        painter.text(center + Vec2::new(0.0, 14.0), egui::Align2::CENTER_CENTER, sub, egui::FontId::proportional(12.0), Color32::from_white_alpha(170));
        // Number keys pick from the wheel instead of the hotbar.
        for s in std::mem::take(slots) {
            if let Some(it) = items.iter().find(|i| i.0 as usize == s + 1) {
                picked = Some(it.2.clone());
            }
        }
        let outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| p.distance(center) > radius + cell * 0.7 && rect.contains(p))
        });
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if picked.is_some() || outside || esc {
            w.open = false;
        }
        drop(w);
        if let (Some(name), Some(s)) = (picked, self.play.as_mut()) {
            s.game.input.chat.push(format!("/e {name}"));
        }
    }
}
