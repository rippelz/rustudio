//! Studio's Home / Model ribbon quick pickers: the Material drop-down (a
//! grid of material tiles) and the Color drop-down (the BrickColor
//! honeycomb from crate::color_editors). Both apply to the selected parts.

use egui::{Color32, FontId, Rect, Sense, Stroke, Vec2};
use rbx_dom_weak::types::{Color3uint8, Enum, Variant};
use rbx_dom_weak::ustr;

use crate::app::Editor;
use crate::studio_ribbon::ribbon_button;
use rustudio_engine::theme;

const TILE: f32 = 56.0;
const LABEL_H: f32 = 16.0;
const COLS: usize = 5;

/// Part materials, in Studio's enum order (no terrain-only ones, no Air /
/// Water).
fn part_materials() -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = rustudio_core::doc::db()
        .enums
        .get("Material")
        .map(|e| e.items.iter().map(|(k, v)| (k.to_string(), *v)).collect())
        .unwrap_or_default();
    v.retain(|(k, n)| rustudio_render::matpack::entry(*n, false).is_some() && !matches!(k.as_str(), "Air" | "Water"));
    v.sort_by_key(|(_, n)| *n);
    v
}

/// A material's tile image: its pack's colour map scaled down (cached in
/// egui's memory), or None while the pack loads / when it has none.
fn thumb(ctx: &egui::Context, material: u32, use2022: bool) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("mat_thumb", material, use2022, rustudio_render::matpack::generation()));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(t);
    }
    let e = rustudio_render::matpack::entry(material, use2022)?;
    // (the preview only: the full maps are not kept for a tile image)
    let rustudio_render::matpack::Lookup::Ready(m) = rustudio_render::matpack::pack_preview(e.pack, e.color) else {
        return None;
    };
    let src = rustudio_render::matpack::PREVIEW_SIZE as usize;
    if m.preview.len() < src * src * 4 {
        return None;
    }
    // Box-filter the preview level down to 64×64, tinted by nothing (the
    // map's own colours, as Studio's tiles show them).
    const N: usize = 64;
    let step = src / N;
    let mut px = Vec::with_capacity(N * N * 4);
    for y in 0..N {
        for x in 0..N {
            let mut acc = [0u32; 3];
            let mut cnt = 0u32;
            for sy in 0..step {
                for sx in 0..step {
                    let i = ((y * step + sy) * src + x * step + sx) * 4;
                    acc[0] += m.preview[i] as u32;
                    acc[1] += m.preview[i + 1] as u32;
                    acc[2] += m.preview[i + 2] as u32;
                    cnt += 1;
                }
            }
            px.extend_from_slice(&[(acc[0] / cnt) as u8, (acc[1] / cnt) as u8, (acc[2] / cnt) as u8, 255]);
        }
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([N, N], &px);
    let t = ctx.load_texture(format!("mat_thumb_{material}_{use2022}"), img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(key, t.clone()));
    Some(t)
}

impl Editor {
    /// The selection's material, when every selected part shares one.
    fn selection_material(&self) -> Option<u32> {
        self.selected_parts_material()
    }

    /// Ribbon "Material": Roblox's built-in MaterialPicker plugin when chosen,
    /// otherwise rustudio's native drop-down grid of part materials.
    pub fn material_quick_button(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::MaterialPicker) {
            let active = self.action_state("material").1 == Some(true);
            let r = self.rb(ui, "texture", "Material", "material", active, act);
            self.ui_rects.insert("home.material", r);
            return;
        }
        let (enabled, _) = self.action_state("material");
        let resp = ribbon_button(ui, "texture", "Material", false, enabled);
        self.ui_rects.insert("home.material", resp.rect);
        if !enabled {
            resp.on_hover_text("Material: select parts first");
            return;
        }
        let ctx = ui.ctx().clone();
        let use2022 = rustudio_render::scene::use_2022_materials(&self.game.doc);
        let cur = self.selection_material();
        let mats = part_materials();
        let mut picked = None;
        egui::Popup::menu(&resp)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(4.0, 4.0);
                let mut loading = false;
                egui::Grid::new("mat_quick_grid").spacing(Vec2::new(4.0, 4.0)).show(ui, |ui| {
                    for (i, (name, n)) in mats.iter().enumerate() {
                        let (r, t) = ui.allocate_exact_size(Vec2::new(TILE, TILE + LABEL_H), Sense::click());
                        let img_r = Rect::from_min_size(r.min, Vec2::splat(TILE));
                        let p = ui.painter();
                        if t.hovered() {
                            p.rect_filled(r.expand(2.0), 2.0, theme::HOVER_BG);
                        }
                        match thumb(&ctx, *n, use2022) {
                            Some(tex) => {
                                p.image(tex.id(), img_r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            None => {
                                if rustudio_render::matpack::entry(*n, use2022).is_some_and(|e| e.pack != 0) && rustudio_render::matpack::enabled() {
                                    loading = true;
                                }
                                let c = rustudio_render::matpack::entry(*n, use2022).map(|e| rustudio_render::matpack::rgb(e.color)).unwrap_or([200; 3]);
                                // Flat materials: the default part grey times the table colour.
                                let f = |c: u8| ((c as f32 / 255.0) * 163.0) as u8;
                                p.rect_filled(img_r, 0.0, Color32::from_rgb(f(c[0]), f(c[1]), f(c[2])));
                            }
                        }
                        if cur == Some(*n) {
                            p.rect_stroke(img_r, 0.0, Stroke::new(2.0, Color32::from_rgb(0, 162, 255)), egui::StrokeKind::Inside);
                        }
                        let label = ui.painter().layout(name.clone(), FontId::proportional(10.5), theme::TEXT, TILE + 6.0);
                        let lp = egui::pos2(r.center().x - label.size().x / 2.0, img_r.max.y + 2.0);
                        ui.painter().with_clip_rect(r.expand2(Vec2::new(2.0, 0.0))).galley(lp, label, theme::TEXT);
                        if t.clicked() {
                            picked = Some(*n);
                        }
                        if i % COLS == COLS - 1 {
                            ui.end_row();
                        }
                    }
                });
                if loading {
                    // Packs download in the background; redraw until they land.
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                }
            });
        if let Some(n) = picked {
            let edits = self
                .selected_parts()
                .into_iter()
                .map(|r| (r, ustr("Material"), Variant::Enum(Enum::from_u32(n))))
                .collect();
            self.game.doc.set_props("Set Material", edits, None);
        }
    }

    /// Ribbon "Color": Studio's BrickColor honeycomb under the button.
    pub fn color_quick_button(&mut self, ui: &mut egui::Ui) {
        let (enabled, _) = self.action_state("color");
        let resp = ribbon_button(ui, "palette", "Color", self.props.ribbon_brick.is_some(), enabled);
        self.ui_rects.insert("home.color", resp.rect);
        if !enabled {
            resp.on_hover_text("Color: select parts first");
            return;
        }
        if resp.clicked() && self.props.ribbon_brick.is_none() {
            self.props.ribbon_brick = Some(crate::color_editors::BrickPopup {
                prop: "BrickColor".into(),
                origin: resp.rect.left_bottom() + Vec2::new(0.0, 2.0),
                opened_frame: self.game.frame_no,
                hovered: None,
            });
        }
    }

    /// Draws the ribbon's BrickColor popup when open; applies the pick.
    pub fn ribbon_color_popup(&mut self, ctx: &egui::Context) {
        use crate::color_editors::{brick_popup_ui, BrickPick};
        let Some(mut pop) = self.props.ribbon_brick.take() else {
            return;
        };
        let parts = self.selected_parts();
        if parts.is_empty() {
            return;
        }
        let current = match self.game.doc.prop(parts[0], "Color") {
            Some(Variant::Color3uint8(c)) => Some(crate::properties::nearest_brick([c.r, c.g, c.b])),
            _ => None,
        };
        match brick_popup_ui(ctx, &mut pop, current, self.game.frame_no) {
            BrickPick::None => self.props.ribbon_brick = Some(pop),
            BrickPick::Close => {}
            BrickPick::Picked(b) => {
                let rgb = rustudio_render::scene::brick_rgb(b);
                let edits = parts
                    .into_iter()
                    .map(|r| (r, ustr("Color"), Variant::Color3uint8(Color3uint8::new(rgb[0], rgb[1], rgb[2]))))
                    .collect();
                self.game.doc.set_props("Set Color", edits, None);
            }
            BrickPick::MoreColors => {
                let rgb = current.map(rustudio_render::scene::brick_rgb).unwrap_or([163, 162, 165]);
                self.open_color_dialog("Color", rgb);
            }
        }
    }
}
