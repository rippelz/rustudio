//! The Terrain Editor panel's widgets, drawn as Studio's Terrain Editor
//! plugin draws them (its Foundation-style controls, read off the offline Studio
//! screenshots at 1:1): the Create / Edit tab strip, the tool tiles (four
//! across), collapsible "… Settings" sections, label + control rows with a
//! fixed label column, segmented controls (text or icon), slider + number
//! box, checkboxes, X Y Z number triples, the material grid (six across,
//! a lit ball per material) and the full-width blue button.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use rustudio_engine::theme;

/// Studio's colours for these controls (sampled in the offline Studio, dark theme).
pub const TAB_BAR: Color32 = Color32::from_rgb(32, 34, 39);
pub const FIELD: Color32 = Color32::from_rgb(39, 41, 48);
pub const BORDER: Color32 = Color32::from_rgb(53, 55, 65);
pub const SELECTED: Color32 = Color32::from_rgb(53, 55, 65);
pub const TILE_ON: Color32 = Color32::from_rgb(53, 55, 64);

/// Left edge of a row's label, the label column's width and row heights.
pub const INDENT: f32 = 29.0;
pub const LABEL_W: f32 = 105.0;
pub const ROW_H: f32 = 24.0;
pub const ROW_GAP: f32 = 10.0;

pub fn font() -> FontId {
    FontId::proportional(14.0)
}

fn bold() -> FontId {
    FontId::new(14.0, egui::FontFamily::Name("bold".into()))
}

/// Section headers: Builder Sans Bold (ti::install always sets "bold").
fn header_font(_ui: &Ui) -> FontId {
    bold()
}

/// The Create / Edit strip: the picked tab has a 2 px accent line on top
/// and the panel's colour; the strip is a shade lighter.
pub fn tabs(ui: &mut Ui, names: &[&str], current: &mut usize) -> bool {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 31.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 0.0, TAB_BAR);
    let mut x = r.min.x;
    let mut changed = false;
    for (i, n) in names.iter().enumerate() {
        let tw = text_w(ui, n, &font());
        let tab = Rect::from_min_size(pos2(x, r.min.y), vec2((tw + 42.0).max(84.0), r.height()));
        let resp = ui.interact(tab, ui.id().with(("te_tab", i)), Sense::click());
        if resp.clicked() && *current != i {
            *current = i;
            changed = true;
        }
        let p = ui.painter();
        if *current == i {
            p.rect_filled(tab, 0.0, theme::PANEL);
            p.rect_filled(Rect::from_min_size(tab.min, vec2(tab.width(), 2.0)), 0.0, theme::ACCENT);
            p.line_segment([tab.right_top(), tab.right_bottom()], Stroke::new(1.0, BORDER));
            if i > 0 {
                p.line_segment([tab.left_top(), tab.left_bottom()], Stroke::new(1.0, BORDER));
            }
        } else if resp.hovered() {
            p.rect_filled(tab, 0.0, theme::HOVER_BG);
        }
        p.text(tab.center() + vec2(0.0, 1.0), Align2::CENTER_CENTER, n, font(), theme::TEXT);
        x = tab.max.x;
    }
    changed
}

fn text_w(ui: &Ui, s: &str, f: &FontId) -> f32 {
    ui.fonts_mut(|fs| fs.layout_no_wrap(s.to_string(), f.clone(), theme::TEXT).size().x)
}

/// A tool tile: icon above its name, 86 × 60, four to a row.
pub struct TileIcon {
    pub icon: &'static str,
    /// A small coloured glyph at the icon's top right (Studio's tool icons
    /// are white with one coloured mark: Draw's green plus, Smooth's
    /// purple, Paint's green brush, Flatten's red line …).
    pub mark: Option<(&'static str, Color32)>,
}

pub fn tiles<T: Copy + PartialEq>(ui: &mut Ui, items: &[(T, &str, TileIcon)], current: Option<T>) -> Option<T> {
    let mut clicked = None;
    // Studio's Grid: 84 × 60 cells, 2 px apart, floor((W + 2) / 86) across.
    let cols = (((ui.available_width() + 2.0) / 86.0).floor() as usize).max(1);
    let tile = vec2(86.0, 60.0);
    let left = ui.min_rect().min.x + 3.0;
    for row in items.chunks(cols) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), tile.y + 2.0), Sense::hover());
        for (i, (t, name, icon)) in row.iter().enumerate() {
            let tr = Rect::from_min_size(pos2(left + i as f32 * tile.x, r.min.y + 1.0), tile);
            let resp = ui.interact(tr, ui.id().with(("te_tile", name)), Sense::click());
            let p = ui.painter();
            let on = current == Some(*t);
            if on {
                p.rect_filled(tr, 3.0, TILE_ON);
            } else if resp.hovered() {
                p.rect_filled(tr, 3.0, theme::HOVER_BG);
            }
            let ic = Rect::from_center_size(pos2(tr.center().x, tr.min.y + 21.0), Vec2::splat(26.0));
            rustudio_engine::ti::paint(p, ic, icon.icon, Color32::WHITE);
            if let Some((m, c)) = icon.mark {
                let mr = Rect::from_center_size(ic.right_top() + vec2(-1.0, 3.0), Vec2::splat(13.0));
                rustudio_engine::ti::paint(p, mr, m, c);
            }
            p.text(pos2(tr.center().x, tr.max.y - 13.0), Align2::CENTER_CENTER, *name, font(), theme::TEXT);
            if resp.clicked() {
                clicked = Some(*t);
            }
        }
    }
    clicked
}

/// A section header ("Brush Settings") with its chevron; click folds it.
/// Returns whether it is open.
pub fn section(ui: &mut Ui, title: &str) -> bool {
    let id = ui.id().with(("te_section", title));
    let mut open = ui.data_mut(|d| *d.get_persisted_mut_or_insert_with(id, || true));
    ui.add_space(4.0);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if resp.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_persisted(id, open));
    }
    let p = ui.painter();
    let chev = Rect::from_center_size(pos2(r.min.x + 17.0, r.center().y), Vec2::splat(14.0));
    rustudio_engine::ti::paint(p, chev, if open { "chevron-down" } else { "chevron-right" }, theme::TEXT);
    p.text(pos2(r.min.x + INDENT, r.center().y), Align2::LEFT_CENTER, title, header_font(ui), Color32::WHITE);
    open
}

/// One settings row: the label in its column (top aligned) and `add`'s
/// controls in the rest, `h` tall.
pub fn row<R>(ui: &mut Ui, label: &str, h: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    ui.painter().text(
        pos2(r.min.x + INDENT, r.min.y + ROW_H * 0.5 - 1.0),
        Align2::LEFT_CENTER,
        label,
        font(),
        theme::TEXT,
    );
    let ctl = Rect::from_min_max(pos2(r.min.x + INDENT + LABEL_W, r.min.y), pos2(r.max.x - 8.0, r.max.y));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ctl).layout(egui::Layout::left_to_right(egui::Align::Min)));
    let out = add(&mut child);
    ui.add_space(ROW_GAP);
    out
}

/// A label on its own line across the panel ("Source Material").
pub fn caption(ui: &mut Ui, label: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(pos2(r.min.x + INDENT, r.center().y), Align2::LEFT_CENTER, label, font(), theme::TEXT);
    ui.add_space(4.0);
}

/// What a segment shows.
pub enum Seg {
    Text(&'static str),
    Icon(&'static str),
}

/// Studio's segmented control: an outlined bar split evenly, the picked
/// segment filled. Text segments are 24 tall, icon ones 52.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, opts: &[(T, Seg)]) -> bool {
    let icons = opts.iter().any(|(_, s)| matches!(s, Seg::Icon(_)));
    let h = if icons { 52.0 } else { ROW_H };
    let w = ui.available_width();
    // Each bar's own (auto) id: segments keyed only by index clicked the
    // same segment of every bar in the panel at once.
    let (r, bar) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let n = opts.len().max(1) as f32;
    let sw = r.width() / n;
    let mut changed = false;
    for (i, (v, s)) in opts.iter().enumerate() {
        let sr = Rect::from_min_size(pos2(r.min.x + i as f32 * sw, r.min.y), vec2(sw, h));
        let resp = ui.interact(sr, bar.id.with(("te_seg", i)), Sense::click());
        if resp.clicked() && *value != *v {
            *value = *v;
            changed = true;
        }
        let p = ui.painter();
        if *value == *v {
            p.rect_filled(sr, 0.0, SELECTED);
        } else if resp.hovered() {
            p.rect_filled(sr, 0.0, theme::HOVER_BG);
        }
        if i > 0 {
            p.line_segment([sr.left_top(), sr.left_bottom()], Stroke::new(1.0, BORDER));
        }
        match s {
            Seg::Text(t) => {
                // Narrow panel: the label shrinks to fit its segment.
                let mut f = font();
                let w = text_w(ui, t, &f);
                if w > sw - 6.0 {
                    f.size = (f.size * (sw - 6.0) / w).max(9.0);
                }
                ui.painter().text(sr.center(), Align2::CENTER_CENTER, *t, f, theme::TEXT);
            }
            Seg::Icon(ic) => {
                rustudio_engine::ti::paint(p, Rect::from_center_size(sr.center(), Vec2::splat(30.0)), ic, theme::TEXT);
            }
        }
    }
    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    changed
}

/// A number box (Studio's dark field) for an f32, typed in; `decimals`
/// places shown.
pub fn number(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut f32, w: f32, range: (f32, f32), decimals: usize) -> bool {
    let id = ui.id().with(("te_num", id));
    let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    ui.painter().rect_filled(r, 3.0, FIELD);
    let fmt = |v: f32| {
        let s = format!("{v:.decimals$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    let mut editing: Option<String> = ui.data(|d| d.get_temp(id));
    if resp.clicked() && editing.is_none() {
        editing = Some(fmt(*value));
    }
    let mut changed = false;
    if let Some(mut text) = editing {
        let te = egui::TextEdit::singleline(&mut text)
            .frame(egui::Frame::NONE)
            .font(font())
            .margin(vec2(8.0, 3.0))
            .desired_width(w - 4.0);
        let out = ui.put(r, te);
        if resp.clicked() {
            out.request_focus();
        }
        let commit = out.lost_focus();
        if commit {
            if let Ok(v) = text.trim().parse::<f32>() {
                let v = v.clamp(range.0, range.1);
                changed = v != *value;
                *value = v;
            }
            ui.data_mut(|d| d.remove::<String>(id));
        } else {
            ui.data_mut(|d| d.insert_temp(id, text));
        }
    } else {
        ui.painter().text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, fmt(*value), font(), theme::TEXT);
    }
    changed
}

/// Number box then a slider across the rest (Brush Size, Strength,
/// Blending): white fill to the knob, a 16 px white knob.
pub fn slider(ui: &mut Ui, id: &str, value: &mut f32, range: (f32, f32), step: f32, decimals: usize) -> bool {
    let mut changed = number(ui, id, value, 50.0, range, decimals);
    ui.add_space(10.0);
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click_and_drag());
    let track = Rect::from_min_max(pos2(r.min.x + 8.0, r.center().y - 2.0), pos2(r.max.x - 8.0, r.center().y + 2.0));
    if let (true, Some(pp)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) {
        let t = ((pp.x - track.min.x) / track.width()).clamp(0.0, 1.0);
        let mut v = range.0 + t * (range.1 - range.0);
        if step > 0.0 {
            v = (v / step).round() * step;
        }
        let v = v.clamp(range.0, range.1);
        if v != *value {
            *value = v;
            changed = true;
        }
    }
    let t = ((*value - range.0) / (range.1 - range.0).max(1e-6)).clamp(0.0, 1.0);
    let kx = track.min.x + t * track.width();
    let p = ui.painter();
    p.rect_filled(track, 2.0, BORDER);
    p.rect_filled(Rect::from_min_max(track.min, pos2(kx, track.max.y)), 2.0, Color32::WHITE);
    p.circle_filled(pos2(kx, track.center().y), 8.0, Color32::WHITE);
    changed
}

/// Studio's checkbox: an outlined 18 px square, white with a dark tick when on.
pub fn checkbox(ui: &mut Ui, value: &mut bool, text: Option<&str>) -> bool {
    let tw = text.map(|t| text_w(ui, t, &font()) + 10.0).unwrap_or(0.0);
    let (r, resp) = ui.allocate_exact_size(vec2(22.0 + tw, ROW_H - 2.0), Sense::click());
    let b = Rect::from_center_size(pos2(r.min.x + 9.0, r.center().y), Vec2::splat(17.0));
    let p = ui.painter();
    if *value {
        p.rect_filled(b, 3.0, Color32::WHITE);
        rustudio_engine::ti::paint(p, b.shrink(2.0), "check", Color32::from_rgb(25, 26, 31));
    } else {
        p.rect_stroke(b, 3.0, Stroke::new(1.0, if resp.hovered() { theme::TEXT_DIM } else { BORDER }), StrokeKind::Inside);
    }
    if let Some(t) = text {
        p.text(pos2(b.max.x + 10.0, r.center().y), Align2::LEFT_CENTER, t, font(), theme::TEXT);
    }
    if resp.clicked() {
        *value = !*value;
        return true;
    }
    false
}

/// X [ ] Y [ ] Z [ ] number boxes (Selection Size / Position, Rotation).
pub fn vec3(ui: &mut Ui, id: &str, v: &mut [f32; 3], range: (f32, f32)) -> bool {
    let mut changed = false;
    let bw = ((ui.available_width() - 3.0 * 18.0) / 3.0).clamp(30.0, 54.0);
    for (k, axis) in ["X", "Y", "Z"].iter().enumerate() {
        let (lr, _) = ui.allocate_exact_size(vec2(16.0, ROW_H), Sense::hover());
        ui.painter().text(lr.center(), Align2::CENTER_CENTER, *axis, font(), theme::TEXT);
        changed |= number(ui, (id, k), &mut v[k], bw, range, 2);
        ui.add_space(2.0);
    }
    changed
}

/// A wide text-like field for a whole number (Biome Size, Seed).
pub fn int_field(ui: &mut Ui, id: &str, value: &mut f32, range: (f32, f32)) -> bool {
    let w = ui.available_width();
    number(ui, id, value, w, range, 0)
}

/// The Seed: a whole number typed in (any integer; Studio hashes its text).
pub fn seed_field(ui: &mut Ui, id: &str, value: &mut i64) -> bool {
    let w = ui.available_width();
    let id = ui.id().with(("te_seed", id));
    let (r, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click());
    ui.painter().rect_filled(r, 3.0, FIELD);
    let mut editing: Option<String> = ui.data(|d| d.get_temp(id));
    if resp.clicked() && editing.is_none() {
        editing = Some(value.to_string());
    }
    let mut changed = false;
    if let Some(mut text) = editing {
        let out = ui.put(r, egui::TextEdit::singleline(&mut text).frame(egui::Frame::NONE).font(font()).margin(vec2(8.0, 3.0)).desired_width(w - 4.0));
        if resp.clicked() {
            out.request_focus();
        }
        if out.lost_focus() {
            if let Ok(v) = text.trim().parse::<i64>() {
                changed = v != *value;
                *value = v;
            }
            ui.data_mut(|d| d.remove::<String>(id));
        } else {
            ui.data_mut(|d| d.insert_temp(id, text));
        }
    } else {
        ui.painter().text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, value.to_string(), font(), theme::TEXT);
    }
    changed
}

/// The full-width blue button (Generate, Fill, Create …), or the grey
/// disabled one (Clear All Terrain while there is none).
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    let w = ui.available_width() - INDENT - 8.0;
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), if enabled { Sense::click() } else { Sense::hover() });
        let p = ui.painter();
        let (bg, fg) = if enabled {
            (if resp.hovered() { Color32::from_rgb(78, 117, 255) } else { theme::ACCENT }, Color32::WHITE)
        } else {
            (SELECTED, theme::TEXT_DIM)
        };
        p.rect_filled(r, 3.0, bg);
        p.text(r.center(), Align2::CENTER_CENTER, text, font(), fg);
        enabled && resp.clicked()
    })
    .inner
}

/// A grey bordered box for an image (Import's Heightmap / Colormap):
/// shows the file name once one is picked; click to choose.
pub fn image_box(ui: &mut Ui, name: Option<&str>, preview: Option<&egui::TextureHandle>) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(100.0, 100.0), Sense::click());
    let p = ui.painter();
    p.rect_filled(r, 0.0, if resp.hovered() { theme::HOVER_BG } else { FIELD });
    if let Some(t) = preview {
        p.image(t.id(), r.shrink(2.0), Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
    } else if let Some(n) = name {
        p.text(r.center(), Align2::CENTER_CENTER, n, FontId::proportional(11.0), theme::TEXT_DIM);
    } else {
        rustudio_engine::ti::paint(p, Rect::from_center_size(r.center(), Vec2::splat(22.0)), "photo-plus", theme::TEXT_DIM);
    }
    resp
}

/// The material grid: 48 px cells, six across, each a lit ball in the
/// material's colour; the picked one outlined in the accent blue.
/// Returns the material clicked.
pub fn material_grid(ui: &mut Ui, id: &str, items: &[u8], current: u8) -> Option<u8> {
    let cell = 50.0;
    let cols = 6usize;
    let mut out = None;
    for (ri, row) in items.chunks(cols).enumerate() {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell), Sense::hover());
        for (ci, m) in row.iter().enumerate() {
            let cr = Rect::from_min_size(pos2(r.min.x + INDENT + ci as f32 * cell, r.min.y), Vec2::splat(cell)).shrink(1.0);
            let resp = ui.interact(cr, ui.id().with((id, ri, ci)), Sense::click());
            let p = ui.painter();
            p.rect_filled(cr, 4.0, if resp.hovered() { theme::HOVER_BG } else { FIELD });
            let r = cell * 0.42;
            if let Some(tex) = textured_ball(ui.ctx(), *m) {
                let br = Rect::from_center_size(cr.center(), Vec2::splat(r * 2.0));
                p.image(tex.id(), br, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                let col = rustudio_core::terrain::material_color(*m);
                ball(p, cr.center(), r, Color32::from_rgb(col[0], col[1], col[2]));
            }
            let stroke = if *m == current { Stroke::new(1.0, theme::ACCENT) } else { Stroke::new(1.0, BORDER) };
            p.rect_stroke(cr, 4.0, stroke, StrokeKind::Inside);
            if resp.on_hover_text(rustudio_core::terrain::material_name(*m)).clicked() {
                out = Some(*m);
            }
        }
    }
    out
}

/// A material ball wearing the material's own terrain texture (its top
/// face's pack, as Studio's MaterialFramework previews show it), lit like
/// [`ball`]. Rendered once per material on the CPU and cached; None while
/// the pack is still loading (the plain ball shows meanwhile) or for one
/// without a pack (Water).
fn textured_ball(ctx: &egui::Context, m: u8) -> Option<egui::TextureHandle> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<(u8, bool), egui::TextureHandle>>> = OnceLock::new();
    let use2022 = rustudio_core::terrain::use2022();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().ok()?.get(&(m, use2022)) {
        return Some(t.clone());
    }
    let mat = rustudio_core::terrain_mat::material(m, use2022)?;
    let pack = mat.faces[0].as_ref().map(|t| t.pack).filter(|p| *p != 0)?;
    let maps = match rustudio_render::matpack::terrain_pack_preview(pack, 0xff80_8080) {
        rustudio_render::matpack::Lookup::Ready(maps) => maps,
        rustudio_render::matpack::Lookup::Pending => {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return None;
        }
        rustudio_render::matpack::Lookup::Unavailable(_) => return None,
    };
    let image = ball_image(&maps.preview, BALL_PX);
    let tex = ctx.load_texture(format!("terrain-ball-{m}-{use2022}"), image, egui::TextureOptions::LINEAR);
    cache.lock().ok()?.insert((m, use2022), tex.clone());
    Some(tex)
}

/// Pixels a side of a textured material ball (drawn at ~42 px; 2× for HiDPI).
const BALL_PX: usize = 96;

/// A sphere `px` across with the pack's albedo (a mip chain from
/// matpack::mip_chain, LAYER_SIZE first) wrapped round it twice, lit from
/// the upper left; anti-aliased edge.
fn ball_image(albedo: &[u8], px: usize) -> egui::ColorImage {
    // The mip about 2 texels per screen pixel across one wrap.
    let levels = rustudio_render::matpack::preview_levels();
    let want = (px as u32).max(1);
    let (off, size) = levels.iter().copied().find(|(_, s)| *s <= want).unwrap_or(levels[0]);
    let size = size as usize;
    let texel = |u: f32, v: f32| -> [f32; 3] {
        let x = ((u.rem_euclid(1.0)) * size as f32) as usize % size;
        let y = ((v.rem_euclid(1.0)) * size as f32) as usize % size;
        let i = off + (y * size + x) * 4;
        match albedo.get(i..i + 3) {
            Some(c) => [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0],
            None => [0.5; 3],
        }
    };
    let light = glam::Vec3::new(-0.45, 0.6, 0.66).normalize();
    let half = (light + glam::Vec3::Z).normalize();
    let mut pixels = vec![Color32::TRANSPARENT; px * px];
    let ss = 3; // samples per axis
    for py in 0..px {
        for qx in 0..px {
            let mut acc = [0.0f32; 3];
            let mut hits = 0;
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = ((qx as f32 + (sx as f32 + 0.5) / ss as f32) / px as f32) * 2.0 - 1.0;
                    let y = ((py as f32 + (sy as f32 + 0.5) / ss as f32) / px as f32) * 2.0 - 1.0;
                    let rr = x * x + y * y;
                    if rr > 1.0 {
                        continue;
                    }
                    let n = glam::Vec3::new(x, -y, (1.0 - rr).sqrt());
                    let u = n.x.atan2(n.z) / std::f32::consts::TAU + 0.5;
                    let v = n.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
                    let c = texel(u * 2.0, v);
                    let diff = n.dot(light).max(0.0);
                    let spec = n.dot(half).max(0.0).powf(24.0) * 0.2;
                    let k = 0.3 + 0.85 * diff;
                    for (a, c) in acc.iter_mut().zip(c) {
                        *a += (c * k + spec).min(1.0);
                    }
                    hits += 1;
                }
            }
            if hits > 0 {
                let n = hits as f32;
                let cov = n / (ss * ss) as f32;
                let ch = |v: f32| (v / n * cov * 255.0).round() as u8;
                pixels[py * px + qx] =
                    Color32::from_rgba_premultiplied(ch(acc[0]), ch(acc[1]), ch(acc[2]), (cov * 255.0).round() as u8);
            }
        }
    }
    egui::ColorImage { size: [px, px], pixels, source_size: Vec2::splat(px as f32) }
}

/// A material ball: a sphere lit from the upper left (Lambert + a soft
/// specular spot), shaded per vertex on a fan of rings.
pub fn ball(p: &egui::Painter, c: Pos2, r: f32, base: Color32) {
    let mut mesh = egui::Mesh::default();
    let light = glam::Vec3::new(-0.45, 0.6, 0.66).normalize();
    let shade = |x: f32, y: f32| -> Color32 {
        let z = (1.0 - x * x - y * y).max(0.0).sqrt();
        let n = glam::Vec3::new(x, -y, z);
        let diff = n.dot(light).max(0.0);
        let h = (light + glam::Vec3::Z).normalize();
        let spec = n.dot(h).max(0.0).powf(24.0) * 0.35;
        let k = 0.18 + 0.95 * diff;
        let ch = |v: u8| ((v as f32 / 255.0 * k + spec).min(1.0) * 255.0) as u8;
        Color32::from_rgb(ch(base.r()), ch(base.g()), ch(base.b()))
    };
    let rings = 10;
    let segs = 40;
    mesh.colored_vertex(c, shade(0.0, 0.0));
    for i in 1..=rings {
        let rr = i as f32 / rings as f32;
        for j in 0..segs {
            let a = j as f32 / segs as f32 * std::f32::consts::TAU;
            let (x, y) = (rr * a.cos(), rr * a.sin());
            mesh.colored_vertex(c + vec2(x, y) * r, shade(x * 0.999, y * 0.999));
        }
    }
    let idx = |ring: u32, j: u32| -> u32 { if ring == 0 { 0 } else { 1 + (ring - 1) * segs + j % segs } };
    for j in 0..segs {
        mesh.add_triangle(0, idx(1, j), idx(1, j + 1));
    }
    for ring in 1..rings {
        for j in 0..segs {
            let (a, b, c2, d) = (idx(ring, j), idx(ring, j + 1), idx(ring + 1, j), idx(ring + 1, j + 1));
            mesh.add_triangle(a, c2, d);
            mesh.add_triangle(a, d, b);
        }
    }
    p.add(egui::Shape::mesh(mesh));
    let _ = CornerRadius::ZERO;
}

#[cfg(test)]
mod seg_tests {
    use super::*;

    /// Two bars in one panel: clicking a segment of the second changes only
    /// the second (they used to share ids, so every bar took the click).
    #[test]
    fn segmented_bars_take_their_own_clicks() {
        let ctx = egui::Context::default();
        let (mut a, mut b) = (0u8, 0u8);
        let opts = [(0u8, Seg::Text("Off")), (1, Seg::Text("Auto")), (2, Seg::Text("Manual"))];
        let mut second = Rect::NOTHING;
        let mut frame = |events: Vec<egui::Event>, a: &mut u8, b: &mut u8, second: &mut Rect| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                segmented(ui, a, &opts);
                let top = ui.cursor().min.y;
                segmented(ui, b, &opts);
                *second = Rect::from_min_max(pos2(ui.min_rect().min.x, top), pos2(ui.min_rect().max.x, top + ROW_H));
            });
            // No renderer here: the font atlas upload is dropped on purpose.
            out.textures_delta.clear();
        };
        frame(vec![], &mut a, &mut b, &mut second);
        let w = second.width() / 3.0;
        let at = pos2(second.min.x + w * 1.5, second.center().y);
        let press = |down: bool| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: down,
            modifiers: Default::default(),
        };
        frame(vec![egui::Event::PointerMoved(at), press(true)], &mut a, &mut b, &mut second);
        frame(vec![press(false)], &mut a, &mut b, &mut second);
        frame(vec![], &mut a, &mut b, &mut second);
        assert_eq!((a, b), (0, 1), "only the clicked bar changes");
    }
}
