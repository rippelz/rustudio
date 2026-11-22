//! The legacy Message and Hint (deprecated, still used by old places).
//!
//! Studio 0.747 draws them as 2D adornments, not GuiObjects:
//! `Message::render2d` draws nothing while Text is empty, else
//! `renderFullScreen`: a (0.5, 0.5, 0.5, 0.5) box over the whole view and
//! the text in Font Legacy 14, white with a black stroke, centred on the
//! view. `Hint::render2d` (Hint is a Message) draws an opaque black bar 20 px
//! tall across the top of the view, even with empty Text, and its text
//! centred in it, white with no stroke. They show while in the Workspace
//! (Workspace collects its adornable descendants) or in the LocalPlayer's
//! PlayerGui; anywhere else they are inert. Both go under the game's GUIs
//! (a Roblox client draws its top bar and hotbar over a Message).

use egui::{Color32, Pos2, Rect};
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::assets::AssetStore;
use rustudio_core::doc::{self, Document};
use rustudio_core::mech;

use super::Gui;

/// The Messages / Hints that can show, kept from the document's journal (a
/// scan of a big place's Workspace every frame would cost milliseconds).
#[derive(Default)]
pub struct LegacyMessages {
    /// Candidates in the order they were found; `draw_messages` checks
    /// each one is still a shown Message / Hint.
    list: Vec<Ref>,
    doc: u64,
    player_gui: Option<Ref>,
}

fn is_message(d: &Document, r: Ref) -> bool {
    d.class_of(r).is_some_and(|c| c == "Message" || c == "Hint")
}

/// Under the Workspace or the LocalPlayer's PlayerGui.
fn shown_under(d: &Document, r: Ref, ws: Option<Ref>, pg: Option<Ref>) -> bool {
    ws.is_some_and(|w| d.is_ancestor(w, r)) || pg.is_some_and(|p| d.is_ancestor(p, r))
}

impl LegacyMessages {
    fn sync(&mut self, d: &mut Document) {
        let marks = d.marks_for(doc::Reader::LegacyMessages);
        let pg = super::input::local_player_gui(d);
        let ws = d.workspace();
        let rebuild = !matches!(marks, doc::Marks::Some(_)) || d.id != self.doc || pg != self.player_gui;
        if rebuild {
            self.doc = d.id;
            self.player_gui = pg;
            self.list = [ws, pg]
                .into_iter()
                .flatten()
                .flat_map(|top| d.dom.descendants_of(top).filter(|i| i.class == "Message" || i.class == "Hint").map(|i| i.referent()).collect::<Vec<_>>())
                .collect();
            return;
        }
        let doc::Marks::Some(marks) = marks else { return };
        for m in marks {
            let doc::Mark::Tree(t) = m else { continue };
            if !d.exists(t) {
                continue;
            }
            for r in d.subtree(t) {
                if is_message(d, r) && !self.list.contains(&r) {
                    self.list.push(r);
                }
            }
        }
        self.list.retain(|r| d.exists(*r) && is_message(d, *r) && shown_under(d, *r, ws, pg));
    }
}

impl Gui {
    /// Draws the shown Messages and Hints over `rect` (the 3D view), before
    /// the game's GUIs. Call once a frame while the view's GUIs show.
    pub fn draw_messages(&mut self, ctx: &egui::Context, painter: &egui::Painter, store: &mut AssetStore, d: &mut Document, rect: Rect) {
        let _p = rustudio_core::bench::scope("gui_messages");
        let mut state = std::mem::take(&mut self.messages);
        state.sync(d);
        let list = state.list.clone();
        self.messages = state;
        if list.is_empty() {
            return;
        }
        let font = self.core_font(ctx, store, "Legacy", mech::GUI_MESSAGE_TEXT_SIZE * mech::GUI_LEGACY_TEXT_SCALE);
        for r in list {
            let text = match d.prop(r, "Text") {
                Some(Variant::String(s)) => s,
                _ => String::new(),
            };
            let hint = d.class_of(r).is_some_and(|c| c == "Hint");
            let (area, stroke) = if hint {
                let bar = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, rect.min.y + mech::GUI_HINT_HEIGHT));
                painter.rect_filled(bar, 0.0, Color32::BLACK);
                (bar, false)
            } else {
                if text.is_empty() {
                    continue;
                }
                let [r, g, b, a] = mech::GUI_MESSAGE_FILL.map(|v| (v * 255.0).round() as u8);
                painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(r, g, b, a));
                (rect, true)
            };
            if text.is_empty() {
                continue;
            }
            let galley = ctx.fonts_mut(|f| f.layout_no_wrap(text, font.clone(), Color32::WHITE));
            let at = area.center() - galley.size() / 2.0;
            if stroke {
                for k in 0..8 {
                    let a = std::f32::consts::TAU * k as f32 / 8.0;
                    painter.add(egui::epaint::TextShape {
                        override_text_color: Some(Color32::BLACK),
                        ..egui::epaint::TextShape::new(at + egui::vec2(a.cos(), a.sin()), galley.clone(), Color32::BLACK)
                    });
                }
            }
            painter.galley(at, galley, Color32::WHITE);
        }
    }
}
