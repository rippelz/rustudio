//! The profiler window's look: the approved mockup's palette, sizes and the
//! few widgets every tab shares (buttons, segmented toggles, search box,
//! sparklines, checkboxes). Drawn by hand so they match the mockup exactly
//! instead of following egui's default visuals.

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const BG: Color32 = hex(0x202227);
pub const PANEL: Color32 = hex(0x272930);
pub const PANEL2: Color32 = hex(0x2c2e36);
pub const DEEP: Color32 = hex(0x191a1f);
pub const LINE: Color32 = hex(0x353741);
pub const LINE2: Color32 = hex(0x2f3139);
pub const TEXT: Color32 = hex(0xd6d8de);
pub const MUTED: Color32 = hex(0x8b8f9a);
pub const FAINT: Color32 = hex(0x5e626c);
pub const WHITE: Color32 = hex(0xffffff);
pub const ACCENT: Color32 = hex(0x1999ff);
pub const WARN: Color32 = hex(0xe6a23c);
pub const BAD: Color32 = hex(0xe5636e);
pub const GOOD: Color32 = hex(0x5cc489);
pub const SEL_ROW: Color32 = hex(0x243246);
pub const SEG_ON: Color32 = hex(0x33465e);
pub const PRIMARY_BG: Color32 = hex(0x1a3c5e);
pub const PRIMARY_LINE: Color32 = hex(0x1f5f99);
pub const PRIMARY_TEXT: Color32 = hex(0xcfe6ff);
pub const POP_BG: Color32 = hex(0x2b2d35);
pub const POP_LINE: Color32 = hex(0x444754);
/// Frame / wait bars on the timeline (grey, light text).
pub const NEUTRAL_BAR: Color32 = hex(0x454956);
pub const BAR_TEXT: Color32 = hex(0x0d0e11);
pub const HILITE: Color32 = hex(0xffd34d);
pub const CHECK_LINE: Color32 = hex(0x5a5e6a);

pub const TITLEBAR_H: f32 = 34.0;
pub const TOOLBAR_H: f32 = 40.0;
pub const STRIP_H: f32 = 118.0;
pub const LEGEND_H: f32 = 30.0;
pub const SUBBAR_H: f32 = 40.0;
pub const SIDE_W: f32 = 320.0;
pub const LANE_LABEL_W: f32 = 150.0;
pub const RULER_H: f32 = 22.0;
pub const BAR_H: f32 = 17.0;
pub const BAR_ROW: f32 = 19.0;

pub fn body() -> FontId {
    FontId::proportional(13.0)
}
pub fn small() -> FontId {
    FontId::proportional(12.0)
}
pub fn tiny() -> FontId {
    FontId::proportional(10.5)
}
pub fn bar_font() -> FontId {
    FontId::proportional(11.0)
}
pub fn mono() -> FontId {
    FontId::monospace(12.0)
}
pub fn mono_big() -> FontId {
    FontId::monospace(15.0)
}

/// The editor's semibold face when it is installed (ti::install), else the
/// regular one.
pub fn strong(size: f32, ui: &Ui) -> FontId {
    let medium = FontFamily::Name("medium".into());
    let has = ui.ctx().fonts(|f| f.definitions().families.contains_key(&medium));
    FontId::new(size, if has { medium } else { FontFamily::Proportional })
}

pub fn text_width(ui: &Ui, text: &str, font: &FontId) -> f32 {
    ui.ctx().fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), TEXT).size().x)
}

/// `text` cut to fit `max_w` with "…" at the end (unchanged if it fits).
pub fn elide(ui: &Ui, text: &str, font: &FontId, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if text_width(ui, text, font) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let t: String = chars[..mid].iter().collect::<String>() + "…";
        if text_width(ui, &t, font) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 { "…".into() } else { chars[..lo].iter().collect::<String>() + "…" }
}

pub fn fill(ui: &Ui, rect: Rect, color: Color32) {
    ui.painter().rect_filled(rect, 0.0, color);
}

pub fn hline(ui: &Ui, x0: f32, x1: f32, y: f32, color: Color32) {
    ui.painter().hline(x0..=x1, y + 0.5, Stroke::new(1.0, color));
}

pub fn vline(ui: &Ui, x: f32, y0: f32, y1: f32, color: Color32) {
    ui.painter().vline(x + 0.5, y0..=y1, Stroke::new(1.0, color));
}

/// A dashed horizontal line.
pub fn dashed_h(p: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let mut x = x0;
    while x < x1 {
        p.hline(x..=(x + 4.0).min(x1), y + 0.5, Stroke::new(1.0, color));
        x += 8.0;
    }
}

pub fn dashed_v(p: &egui::Painter, x: f32, y0: f32, y1: f32, color: Color32) {
    let mut y = y0;
    while y < y1 {
        p.vline(x + 0.5, y..=(y + 4.0).min(y1), Stroke::new(1.0, color));
        y += 8.0;
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum BtnKind {
    Normal,
    Primary,
    /// Toggled on (armed spike capture, an open menu).
    On,
}

/// The mockup's 24 px button: optional dot (Live) or icon, then text.
pub fn button_ex(ui: &mut Ui, text: &str, kind: BtnKind, dot: Option<Color32>, icon: Option<&str>) -> Response {
    let font = small();
    let tw = text_width(ui, text, &font);
    let lead = if dot.is_some() { 14.0 } else if icon.is_some() { 18.0 } else { 0.0 };
    let size = Vec2::new(tw + lead + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let (bg, line, fg) = match kind {
        BtnKind::Normal => (PANEL2, LINE, TEXT),
        BtnKind::Primary => (PRIMARY_BG, PRIMARY_LINE, PRIMARY_TEXT),
        BtnKind::On => (SEG_ON, PRIMARY_LINE, WHITE),
    };
    let bg = if resp.hovered() { bg.gamma_multiply(1.18) } else { bg };
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(3), bg, Stroke::new(1.0, line), StrokeKind::Inside);
    let mut x = rect.left() + 10.0;
    if let Some(c) = dot {
        p.circle_filled(Pos2::new(x + 4.0, rect.center().y), 4.0, c);
        x += lead;
    } else if let Some(name) = icon {
        let r = Rect::from_center_size(Pos2::new(x + 6.0, rect.center().y), Vec2::splat(14.0));
        rustudio_engine::ti::paint(p, r, name, fg);
        x += lead;
    }
    p.text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, text, font, fg);
    resp
}

pub fn button(ui: &mut Ui, text: &str) -> Response {
    button_ex(ui, text, BtnKind::Normal, None, None)
}

/// A segmented toggle: returns the clicked option, if any.
pub fn seg(ui: &mut Ui, options: &[&str], on: usize) -> Option<usize> {
    let font = small();
    let widths: Vec<f32> = options.iter().map(|o| text_width(ui, o, &font) + 18.0).collect();
    let size = Vec2::new(widths.iter().sum::<f32>() + 2.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut clicked = None;
    let mut x = rect.left() + 1.0;
    for (i, (o, w)) in options.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, rect.top() + 1.0), Vec2::new(*w, 22.0));
        let resp = ui.interact(r, ui.id().with(("seg", o, i, rect.left() as i32)), Sense::click());
        let bg = if i == on { SEG_ON } else if resp.hovered() { PANEL2.gamma_multiply(1.2) } else { PANEL2 };
        ui.painter().rect_filled(r, 0.0, bg);
        ui.painter().text(r.center(), Align2::CENTER_CENTER, o, font.clone(), if i == on { WHITE } else { MUTED });
        if resp.clicked() {
            clicked = Some(i);
        }
        x += w;
    }
    ui.painter().rect_stroke(rect, CornerRadius::same(3), Stroke::new(1.0, LINE), StrokeKind::Inside);
    clicked
}

/// The mockup's search box (deep fill, magnifier glyph, placeholder).
pub fn search(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    ui.painter().rect(rect, CornerRadius::same(3), DEEP, Stroke::new(1.0, LINE), StrokeKind::Inside);
    rustudio_engine::ti::paint(ui.painter(), Rect::from_center_size(Pos2::new(rect.left() + 13.0, rect.center().y), Vec2::splat(13.0)), "search", MUTED);
    let inner = Rect::from_min_max(Pos2::new(rect.left() + 24.0, rect.top() + 2.0), Pos2::new(rect.right() - 6.0, rect.bottom() - 2.0));
    let edit = egui::TextEdit::singleline(text)
        .frame(egui::Frame::NONE)
        .font(small())
        .text_color(TEXT)
        .hint_text(egui::RichText::new(hint).color(MUTED).font(small()))
        .vertical_align(egui::Align::Center);
    ui.put(inner, edit)
}

/// A small checkbox (13 px, accent fill when on). Returns true when clicked.
pub fn checkbox(ui: &mut Ui, on: bool, label: &str, swatch: Option<Color32>) -> bool {
    let font = body();
    let tw = text_width(ui, label, &font);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(20.0 + if swatch.is_some() { 15.0 } else { 0.0 } + tw, 22.0), Sense::click());
    let b = Rect::from_min_size(Pos2::new(rect.left(), rect.center().y - 6.5), Vec2::splat(13.0));
    if on {
        ui.painter().rect_filled(b, 2.0, ACCENT);
    } else {
        ui.painter().rect_stroke(b, 2.0, Stroke::new(1.0, CHECK_LINE), StrokeKind::Inside);
    }
    let mut x = rect.left() + 20.0;
    if let Some(c) = swatch {
        ui.painter().rect_filled(Rect::from_min_size(Pos2::new(x, rect.center().y - 5.0), Vec2::splat(10.0)), 2.0, c);
        x += 15.0;
    }
    ui.painter().text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, label, font, TEXT);
    resp.clicked()
}

/// A category colour square.
pub fn swatch(ui: &Ui, at: Pos2, color: Color32) {
    ui.painter().rect_filled(Rect::from_min_size(at, Vec2::splat(10.0)), 2.0, color);
}

/// A line sparkline over `values` (scaled to their own range).
pub fn sparkline(p: &egui::Painter, rect: Rect, values: &[f64], color: Color32) {
    if values.len() < 2 {
        return;
    }
    let (lo, hi) = values.iter().fold((f64::MAX, f64::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let n = values.len() as f32 - 1.0;
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let t = if hi > lo { ((v - lo) / span) as f32 * 0.9 + 0.05 } else { 0.5 };
            Pos2::new(rect.left() + i as f32 / n * rect.width(), rect.bottom() - t * rect.height())
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.4, color)));
}

/// The upper-case section heading with a rule above (sidebar, popups).
pub fn section(ui: &mut Ui, text: &str, rule: bool) {
    if rule {
        ui.add_space(8.0);
        let r = ui.available_rect_before_wrap();
        hline(ui, r.left(), r.right(), r.top(), LINE2);
        ui.add_space(10.0);
    }
    ui.label(egui::RichText::new(text.to_uppercase()).font(tiny()).color(FAINT).extra_letter_spacing(0.6));
    ui.add_space(4.0);
}

/// One key / value row (muted key, monospace value right-aligned).
pub fn kv(ui: &mut Ui, key: &str, value: &str, value_color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 23.0), Sense::hover());
    let vr = ui.painter().text(Pos2::new(rect.right(), rect.center().y), Align2::RIGHT_CENTER, value, mono(), value_color);
    let key = elide(ui, key, &body(), vr.left() - 10.0 - rect.left());
    ui.painter().text(Pos2::new(rect.left(), rect.center().y), Align2::LEFT_CENTER, key, body(), MUTED);
}

/// A key / value row with a category swatch before the key.
pub fn kv_swatch(ui: &mut Ui, color: Color32, key: &str, value: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 23.0), Sense::hover());
    swatch(ui, Pos2::new(rect.left(), rect.center().y - 5.0), color);
    let vr = ui.painter().text(Pos2::new(rect.right(), rect.center().y), Align2::RIGHT_CENTER, value, mono(), TEXT);
    let key = elide(ui, key, &body(), vr.left() - 10.0 - rect.left() - 15.0);
    ui.painter().text(Pos2::new(rect.left() + 15.0, rect.center().y), Align2::LEFT_CENTER, key, body(), MUTED);
}

/// A small coloured badge ("Script", "native").
pub fn badge(ui: &mut Ui, text: &str, bg: Color32, fg: Color32) {
    let font = FontId::proportional(10.0);
    let w = text_width(ui, text, &font) + 10.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 16.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font, fg);
}

/// A colour darkened toward the panel (badge backgrounds).
pub fn tint(c: Color32, k: f32) -> Color32 {
    let m = |a: u8, b: u8| (a as f32 * k + b as f32 * (1.0 - k)) as u8;
    Color32::from_rgb(m(c.r(), PANEL.r()), m(c.g(), PANEL.g()), m(c.b(), PANEL.b()))
}

/// Light text for badges: the colour lifted toward white.
pub fn lift(c: Color32) -> Color32 {
    let m = |a: u8| (a as f32 + (255.0 - a as f32) * 0.45) as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}
