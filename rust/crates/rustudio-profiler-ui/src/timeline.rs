//! The Timeline tab: one lane per thread (and the GPU) with nested flame
//! bars over the selected frame or range; wheel zooms around the pointer,
//! Shift+wheel scrolls the lanes, drag pans, click pins a scope to the
//! sidebar, double-click zooms to it. The search box dims what doesn't match.

use std::time::Instant;

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use rustudio_core::profiler::{analysis, Capture, Cat, SpanRec, Track};

use crate::theme::*;
use crate::{display_name, ms, ProfilerWindow, Sel};

/// A scope on a lane, as the sidebar remembers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpanKey {
    pub track: u32,
    pub start: u64,
    pub end: u64,
    pub name: u32,
    pub depth: u8,
}

impl SpanKey {
    fn of(t: &Track, s: &SpanRec) -> SpanKey {
        SpanKey { track: t.id, start: s.start, end: s.end, name: s.name, depth: s.depth }
    }
}

/// Per-frame totals of one scope name on one lane (the sidebar's "across
/// frames" block), refreshed a few times a second.
struct Across {
    key: (u32, u32),
    at: Instant,
    frames: usize,
    /// (frame index, ns)
    per_frame: Vec<(usize, u64)>,
}

#[derive(Default)]
pub(crate) struct State {
    pub strip_drag: Option<usize>,
    /// The strip's scale source: (when, frames, p99 ms).
    pub strip_p99: Option<(Instant, usize, f64)>,
    pub pinned: Option<SpanKey>,
    /// The visible time range (ns) and the selection it was fitted to.
    view: Option<(f64, f64)>,
    view_for: Option<(u64, u64)>,
    v_off: f32,
    pub search: String,
    across: Option<Across>,
    /// The sidebar shown (Some) or hidden (Some(false)) by the user under
    /// 800 px; None follows the width.
    pub side_open: Option<bool>,
}

/// The lanes to draw, in order.
pub(crate) fn lanes<'a>(w: &ProfilerWindow, cap: &'a Capture) -> Vec<&'a Track> {
    let mut v: Vec<&Track> = cap
        .tracks
        .iter()
        .filter(|t| !t.spans.is_empty() && !w.prefs.hidden_lanes.contains(&t.name))
        .collect();
    let key = |t: &Track| -> (usize, u32, u32) {
        let user = w.prefs.lane_order.iter().position(|n| *n == t.name).unwrap_or(usize::MAX);
        let default = if Some(t.id) == cap.render_track { 0 } else { analysis::lane_info(&t.name).2 + 1 };
        (user, if user == usize::MAX { default } else { 0 }, t.id)
    };
    v.sort_by_key(|t| key(t));
    v
}

fn bar_color(w: &ProfilerWindow, cat: Cat) -> (Color32, Color32) {
    match cat {
        Cat::Frame | Cat::Wait => (NEUTRAL_BAR, TEXT),
        c => (w.prefs.style.cat_color(c), BAR_TEXT),
    }
}

/// Nice tick step (ns) for about `target` ticks over `span` ns.
fn tick_step(span: f64, target: f64) -> f64 {
    let raw = span / target;
    let mag = 10f64.powf(raw.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if m * mag >= raw {
            return m * mag;
        }
    }
    10.0 * mag
}

fn fmt_tick(ns: f64, step: f64) -> String {
    if step >= 1e9 {
        format!("{:.1} s", ns / 1e9)
    } else if step >= 1e6 {
        format!("{:.1} ms", ns / 1e6)
    } else if step >= 1e5 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.0} µs", ns / 1e3)
    }
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, live: bool) {
    let r = ui.max_rect();
    // The sidebar: about 40 % of the width (at most the preference, at
    // least 220 px); under 800 px it can be hidden from the title bar, and
    // starts hidden under 600 px.
    let shown = r.width() >= 800.0 || w.tl.side_open.unwrap_or(r.width() >= 600.0);
    let side_w = if shown { (r.width() * 0.4).min(w.prefs.side_width).max(220.0).min(r.width() - 160.0) } else { 0.0 };
    let side = Rect::from_min_max(Pos2::new(r.right() - side_w, r.top()), r.right_bottom());
    let area = Rect::from_min_max(r.left_top(), Pos2::new(side.left(), r.bottom()));
    fill(ui, area, BG);

    // the view follows the selection until the user zooms or pans
    let (s0, s1) = w.sel_range(cap);
    if w.tl.view_for != Some((s0, s1)) || w.tl.view.is_none() {
        let pad = (s1 - s0) as f64 * 0.04;
        w.tl.view = Some((s0 as f64, s1 as f64 + pad));
        w.tl.view_for = Some((s0, s1));
    }
    let (v0, v1) = w.tl.view.unwrap_or((s0 as f64, s1 as f64));
    let plot = Rect::from_min_max(Pos2::new(area.left() + LANE_LABEL_W, area.top() + RULER_H), area.right_bottom());
    let ns_per_px = (v1 - v0) / plot.width().max(1.0) as f64;
    let x_of = |t: f64| plot.left() + ((t - v0) / ns_per_px) as f32;
    let origin = s0 as f64;

    // ruler
    let ruler = Rect::from_min_max(Pos2::new(plot.left(), area.top()), Pos2::new(area.right(), plot.top()));
    fill(ui, ruler, PANEL);
    hline(ui, ruler.left(), ruler.right(), ruler.bottom() - 1.0, LINE2);
    let step = tick_step(v1 - v0, (plot.width() / 80.0).max(2.0) as f64);
    let mut t = ((v0 - origin) / step).floor() * step + origin;
    let rp = ui.painter_at(ruler);
    while t <= v1 {
        let x = x_of(t);
        if x >= plot.left() - 1.0 {
            rp.vline(x + 0.5, (ruler.bottom() - 8.0)..=ruler.bottom(), Stroke::new(1.0, LINE));
            rp.text(Pos2::new(x + 4.0, ruler.top() + 5.0), Align2::LEFT_TOP, fmt_tick(t - origin, step), FontIdExt::mono_small(), MUTED);
        }
        t += step;
    }
    // the search box sits in the corner above the lane labels
    let corner = Rect::from_min_max(area.left_top(), Pos2::new(plot.left(), plot.top()));
    fill(ui, corner, PANEL);
    hline(ui, corner.left(), corner.right(), corner.bottom() - 1.0, LINE2);
    vline(ui, corner.right() - 1.0, corner.top(), corner.bottom(), LINE);
    {
        let mut c = crate::region(ui, corner.shrink2(Vec2::new(4.0, 1.0)), "tl-search");
        let mut s = std::mem::take(&mut w.tl.search);
        let sr = Rect::from_min_size(c.max_rect().left_top(), Vec2::new(corner.width() - 8.0, 20.0));
        c.scope_builder(egui::UiBuilder::new().max_rect(sr), |ui| {
            search(ui, &mut s, "Search…", sr.width());
        });
        w.tl.search = s;
    }
    let needle = w.tl.search.to_lowercase();

    // lanes
    let lanes = lanes(w, cap);
    let mut heights = Vec::with_capacity(lanes.len());
    for t in &lanes {
        let depth = t.spans_in(v0 as u64, v1 as u64).map(|s| s.depth).max().map_or(1, |d| d as usize + 1).min(24);
        heights.push((depth as f32 * BAR_ROW + 12.0).max(40.0));
    }
    let total: f32 = heights.iter().sum();
    let max_off = (total - plot.height()).max(0.0);
    w.tl.v_off = w.tl.v_off.clamp(0.0, max_off);
    let clip_all = Rect::from_min_max(Pos2::new(area.left(), plot.top()), area.right_bottom());
    let p = ui.painter_at(Rect::from_min_max(plot.left_top(), plot.right_bottom()));
    let lp = ui.painter_at(clip_all);
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| plot.contains(*p));
    let mut hovered: Option<(usize, SpanRec, Rect)> = None;
    let mut y = plot.top() - w.tl.v_off;
    let labels = w.prefs.labels;
    for (li, (t, h)) in lanes.iter().zip(&heights).enumerate() {
        let top = y;
        y += h;
        if y < plot.top() || top > plot.bottom() {
            continue;
        }
        // label column
        let lr = Rect::from_min_max(Pos2::new(area.left(), top), Pos2::new(plot.left(), top + h));
        lp.rect_filled(lr, 0.0, PANEL);
        lp.vline(lr.right() - 0.5, lr.y_range(), Stroke::new(1.0, LINE));
        lp.hline(area.left()..=area.right(), top + h - 0.5, Stroke::new(1.0, LINE2));
        let (title, sub, _) = analysis::lane_info(&t.name);
        lp.text(Pos2::new(lr.left() + 10.0, top + 8.0), Align2::LEFT_TOP, title, small(), TEXT);
        let sub = if t.dropped > 0 { format!("{sub} · {} dropped", t.dropped) } else { sub.to_string() };
        lp.text(Pos2::new(lr.left() + 10.0, top + 24.0), Align2::LEFT_TOP, sub, tiny(), FAINT);

        // bars, deepest last so parents never cover children
        let mut last_px = [f32::MIN; 32];
        for s in t.spans_in(v0 as u64, v1 as u64) {
            if !w.prefs.cat_visible(s.cat) {
                continue;
            }
            let d = (s.depth as usize).min(31);
            let mut x0 = x_of(s.start as f64).max(plot.left() - 2.0);
            let mut x1 = x_of(s.end as f64).min(plot.right() + 2.0);
            if x1 - x0 < 1.0 {
                // (sub-pixel scopes: one 1 px tick per pixel column and depth)
                let px = x0.floor();
                if px <= last_px[d] {
                    continue;
                }
                last_px[d] = px;
                x0 = px;
                x1 = px + 1.0;
            } else {
                last_px[d] = x1.floor();
            }
            let by = top + 5.0 + d as f32 * BAR_ROW;
            let br = Rect::from_min_max(Pos2::new(x0, by), Pos2::new((x1 - 1.0).max(x0 + 1.0), by + BAR_H));
            let name = cap.name(s.name);
            let matches = needle.is_empty() || name.to_lowercase().contains(&needle);
            let (mut bg, mut fg) = bar_color(w, s.cat);
            if !matches {
                bg = bg.gamma_multiply(0.22);
                fg = fg.gamma_multiply(0.3);
            }
            p.rect_filled(br, CornerRadius::same(2), bg);
            if !needle.is_empty() && matches {
                p.rect_stroke(br, CornerRadius::same(2), Stroke::new(1.0, HILITE), StrokeKind::Inside);
            }
            let wide = br.width() > 34.0;
            if labels != crate::prefs::Labels::Never && (wide || labels == crate::prefs::Labels::Always) && br.width() > 8.0 {
                let tp = p.with_clip_rect(br.shrink2(Vec2::new(1.0, 0.0)).intersect(plot));
                tp.text(Pos2::new(br.left() + 5.0, br.center().y), Align2::LEFT_CENTER, display_name(name), bar_font(), fg);
            }
            if pointer.is_some_and(|ptr| br.contains(ptr)) {
                hovered = Some((li, *s, br));
            }
            if w.tl.pinned == Some(SpanKey::of(t, s)) {
                p.rect_stroke(br, CornerRadius::same(2), Stroke::new(2.0, HILITE), StrokeKind::Inside);
            }
        }
        for m in t.marks.iter().filter(|m| (m.t as f64) >= v0 && (m.t as f64) <= v1) {
            let x = x_of(m.t as f64);
            p.vline(x, (top + 1.0)..=(top + 5.0), Stroke::new(1.0, w.prefs.style.cat_color(m.cat)));
        }
    }
    // frame boundaries when several frames show, and the budget line
    let frames = cap.render_frames();
    let i0 = frames.partition_point(|f| (f.end as f64) < v0);
    let visible: Vec<_> = frames.range(i0..).take_while(|f| (f.start as f64) <= v1).collect();
    if visible.len() > 1 && visible.len() < 200 {
        for f in &visible {
            let x = x_of(f.start as f64);
            if x > plot.left() {
                p.vline(x + 0.5, plot.y_range(), Stroke::new(1.0, LINE));
            }
        }
    }
    if visible.len() <= 2 {
        let bx = x_of(origin + w.cfg.budget_ms as f64 * 1e6);
        if bx > plot.left() && bx < plot.right() {
            dashed_v(&p, bx, plot.top(), plot.bottom(), MUTED.gamma_multiply(0.5));
            p.text(Pos2::new(bx + 4.0, plot.bottom() - 8.0), Align2::LEFT_BOTTOM, format!("{:.1} ms budget", w.cfg.budget_ms), FontIdExt::mono_small(), FAINT);
        }
    }
    if lanes.is_empty() {
        p.text(plot.center(), Align2::CENTER_CENTER, if live { "Recording… nothing in this range yet" } else { "Nothing recorded in this range" }, body(), FAINT);
    }

    // input: wheel zoom / shift-wheel scroll, drag pan, click pin
    let resp = ui.interact(plot, ui.id().with("tl-plot"), Sense::click_and_drag());
    let mut moved = false;
    if resp.hovered() {
        let (dy, dx, shift) = ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x, i.modifiers.shift));
        if shift && dy != 0.0 {
            w.tl.v_off = (w.tl.v_off - dy).clamp(0.0, max_off);
        } else if dy != 0.0 {
            let k = (-dy as f64 * 0.003).exp();
            let px = pointer.map_or(plot.center().x, |p| p.x);
            let at = v0 + (px - plot.left()) as f64 * ns_per_px;
            let nv0 = at - (at - v0) * k;
            let nv1 = at + (v1 - at) * k;
            if nv1 - nv0 > 1_000.0 {
                w.tl.view = Some((nv0, nv1));
                moved = true;
            }
        }
        if dx != 0.0 {
            let d = -dx as f64 * ns_per_px;
            w.tl.view = Some((v0 + d, v1 + d));
            moved = true;
        }
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        let dt = -d.x as f64 * ns_per_px;
        let (a, b) = w.tl.view.unwrap_or((v0, v1));
        w.tl.view = Some((a + dt, b + dt));
        w.tl.v_off = (w.tl.v_off - d.y).clamp(0.0, max_off);
        moved |= d.x != 0.0;
    }
    if moved && w.sel == Sel::Latest {
        // (stop following the newest frame once the view is the user's)
        if let Some((_, f)) = w.sel_frame(cap) {
            w.sel = Sel::Frame(f.start);
            w.tl.view_for = Some((f.start, f.end));
        }
    }
    if resp.clicked() {
        w.tl.pinned = hovered.map(|(li, s, _)| SpanKey::of(lanes[li], &s));
    }
    if resp.double_clicked() {
        if let Some((_, s, _)) = hovered {
            let pad = s.dur() as f64 * 0.05;
            w.tl.view = Some((s.start as f64 - pad, s.end as f64 + pad));
        }
    }
    if let Some((li, s, br)) = hovered {
        p.rect_stroke(br, CornerRadius::same(2), Stroke::new(1.0, WHITE), StrokeKind::Inside);
        if !resp.dragged() && w.prefs.tooltip != crate::prefs::TooltipDetail::Off {
            tooltip(w, ui, cap, lanes[li], &s, pointer.unwrap_or(br.center()));
        }
    }

    if shown {
        sidebar(w, &mut crate::region(ui, side, "tl-side"), cap, &lanes);
    }
}

/// Monospace small font for the ruler.
struct FontIdExt;
impl FontIdExt {
    fn mono_small() -> egui::FontId {
        egui::FontId::monospace(10.5)
    }
}

/// (self ns, parent name and duration) of one span on its lane.
fn self_and_parent<'a>(cap: &'a Capture, t: &'a Track, s: &SpanRec) -> (u64, Option<(&'a str, u64)>) {
    let mut child = 0u64;
    let mut parent = None;
    for x in t.spans_in(s.start, s.end) {
        if x.depth == s.depth + 1 && x.start >= s.start && x.end <= s.end {
            child += x.dur();
        }
    }
    if s.depth > 0 {
        parent = t
            .spans_in(s.start, s.end)
            .find(|x| x.depth + 1 == s.depth && x.start <= s.start && x.end >= s.end)
            .map(|x| (cap.name(x.name), x.dur()));
    }
    (s.dur().saturating_sub(child), parent)
}

fn tooltip(w: &ProfilerWindow, ui: &Ui, cap: &Capture, t: &Track, s: &SpanRec, at: Pos2) {
    let (own, parent) = self_and_parent(cap, t, s);
    let full = w.prefs.tooltip == crate::prefs::TooltipDetail::Full;
    let screen = ui.ctx().content_rect();
    let width = 268.0;
    let mut pos = at + Vec2::new(16.0, 16.0);
    if pos.x + width > screen.right() - 8.0 {
        pos.x = at.x - width - 16.0;
    }
    egui::Area::new(ui.id().with("tl-tip")).fade_in(false)
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new().fill(POP_BG).stroke(Stroke::new(1.0, POP_LINE)).corner_radius(5).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(egui::RichText::new(display_name(cap.name(s.name))).font(strong(12.5, ui)).color(WHITE));
                let (lane, _, _) = analysis::lane_info(&t.name);
                ui.label(egui::RichText::new(format!("{lane} · {}", s.cat.name())).font(egui::FontId::proportional(11.0)).color(MUTED));
                ui.add_space(6.0);
                if let Some(src) = cap.location(s.loc).filter(|_| full) {
                    ui.label(egui::RichText::new(src).font(egui::FontId::monospace(10.5)).color(FAINT));
                    ui.add_space(4.0);
                }
                kv(ui, "Duration", &ms(s.dur()), TEXT);
                kv(ui, "Self", &ms(own), TEXT);
                if full {
                    if let Some((pn, pd)) = parent {
                        kv(ui, &format!("Of {}", display_name(pn)), &format!("{:.1} %", s.dur() as f64 * 100.0 / pd.max(1) as f64), TEXT);
                    }
                    if s.instr > 0 {
                        kv(ui, "Instructions", &analysis::fmt_instr(s.instr), TEXT);
                    }
                    if s.alloc > 0 {
                        kv(ui, "Allocated", &rustudio_core::profiler::Unit::Bytes.format(s.alloc as f64), TEXT);
                    }
                }
            });
        });
}

fn sidebar(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, lanes: &[&Track]) {
    let r = ui.max_rect();
    fill(ui, r, PANEL);
    vline(ui, r.left(), r.top(), r.bottom(), LINE);
    let inner = r.shrink2(Vec2::new(14.0, 12.0));
    let mut c = crate::region(ui, inner, "side-inner");
    c.spacing_mut().item_spacing.y = 0.0;
    let pinned = w.tl.pinned.and_then(|k| {
        let t = cap.tracks.iter().find(|t| t.id == k.track)?;
        let s = t.spans_in(k.start, k.end).find(|s| s.start == k.start && s.end == k.end && s.depth == k.depth && s.name == k.name)?;
        Some((t, *s))
    });
    match pinned {
        Some((t, s)) => span_side(w, &mut c, cap, t, &s),
        None => frame_side(w, &mut c, cap, lanes),
    }
}

fn header(ui: &mut Ui, title: &str, badge_cat: Option<(Cat, Color32)>, sub: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(title).font(strong(14.0, ui)).color(WHITE)).truncate());
        if let Some((cat, c)) = badge_cat {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                badge(ui, cat.name(), tint(c, 0.32), lift(c));
            });
        }
    });
    ui.add_space(2.0);
    ui.label(egui::RichText::new(sub).font(egui::FontId::proportional(11.5)).color(MUTED));
    ui.add_space(8.0);
}

fn span_side(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, t: &Track, s: &SpanRec) {
    let name = display_name(cap.name(s.name)).to_string();
    let frames = cap.render_frames();
    let fi = frames.partition_point(|f| f.start <= s.start).saturating_sub(1);
    let frame = frames.get(fi).copied();
    let (lane, _, _) = analysis::lane_info(&t.name);
    let fno = cap.frames_trimmed + fi as u64;
    header(ui, &name, Some((s.cat, w.prefs.style.cat_color(s.cat))), &format!("{lane} · depth {} · frame #{}", s.depth, crate::spaced(fno)));
    if let Some(src) = cap.location(s.loc) {
        ui.add(egui::Label::new(egui::RichText::new(src).font(egui::FontId::monospace(11.0)).color(FAINT)).truncate())
            .on_hover_text("Where in rustudio's source this scope is opened");
        ui.add_space(6.0);
    }
    let (own, _) = self_and_parent(cap, t, s);
    kv(ui, "This call", &ms(s.dur()), TEXT);
    kv(ui, "Self time", &ms(own), TEXT);
    if let Some(f) = frame {
        kv(ui, "% of frame", &format!("{:.1} %", s.dur() as f64 * 100.0 / f.dur().max(1) as f64), TEXT);
        let calls = t.spans_in(f.start, f.end).filter(|x| x.name == s.name && x.start >= f.start).count();
        kv(ui, "Calls this frame", &calls.to_string(), TEXT);
    }
    if s.instr > 0 {
        kv(ui, "Instructions", &analysis::fmt_instr(s.instr), TEXT);
    }
    if cap.meta.alloc_tracked {
        kv(ui, "Allocated", &rustudio_core::profiler::Unit::Bytes.format(s.alloc as f64), TEXT);
    }

    // across frames (refreshed 4 times a second)
    let key = (t.id, s.name);
    let stale = w.tl.across.as_ref().is_none_or(|a| a.key != key || a.frames != frames.len() || a.at.elapsed().as_millis() > 250);
    if stale {
        let fr: Vec<_> = frames.iter().copied().collect();
        let mut per: Vec<(usize, u64)> = Vec::new();
        for x in t.spans.iter().filter(|x| x.name == s.name) {
            let i = fr.partition_point(|f| f.start <= x.start);
            if i == 0 || x.start >= fr[i - 1].end {
                continue;
            }
            match per.iter_mut().find(|e| e.0 == i - 1) {
                Some(e) => e.1 += x.dur(),
                None => per.push((i - 1, x.dur())),
            }
        }
        per.sort_unstable_by_key(|e| e.0);
        w.tl.across = Some(Across { key, at: Instant::now(), frames: frames.len(), per_frame: per });
    }
    let a = w.tl.across.as_ref().expect("filled above");
    section(ui, &format!("Across {} frames", crate::spaced(frames.len() as u64)), true);
    let mut v: Vec<u64> = a.per_frame.iter().map(|e| e.1).collect();
    v.sort_unstable();
    let avg = if v.is_empty() { 0 } else { v.iter().sum::<u64>() / v.len() as u64 };
    let max = v.last().copied().unwrap_or(0);
    kv(ui, "Average", &ms(avg), TEXT);
    kv(ui, "p95", &ms(analysis::quantile_sorted(&v, 0.95)), TEXT);
    kv(ui, "Max", &ms(max), if max as f64 > w.cfg.budget_ms as f64 * 1e6 { BAD } else { TEXT });
    kv(ui, "Frames present", &format!("{} / {}", v.len(), frames.len()), TEXT);
    // the last 30 frames' totals, this one in yellow
    ui.add_space(6.0);
    let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::hover());
    let last = frames.len();
    let first = last.saturating_sub(30);
    let hmax = a.per_frame.iter().filter(|e| e.0 >= first).map(|e| e.1).max().unwrap_or(1).max(1);
    let cat_c = w.prefs.style.cat_color(s.cat);
    for i in first..last {
        let val = a.per_frame.binary_search_by_key(&i, |e| e.0).map(|j| a.per_frame[j].1).unwrap_or(0);
        let h = (val as f32 / hmax as f32 * 44.0).max(if val > 0 { 2.0 } else { 0.0 });
        let x = hr.left() + (i - first) as f32 * 9.0;
        let c = if i == fi { HILITE } else { cat_c.gamma_multiply(0.9) };
        ui.painter().rect_filled(Rect::from_min_max(Pos2::new(x, hr.bottom() - h), Pos2::new(x + 7.0, hr.bottom())), 0.0, c);
    }

    // children of this call
    let mut kids: Vec<(u32, Cat, u64)> = Vec::new();
    for x in t.spans_in(s.start, s.end) {
        if x.depth == s.depth + 1 && x.start >= s.start && x.end <= s.end {
            match kids.iter_mut().find(|k| k.0 == x.name) {
                Some(k) => k.2 += x.dur(),
                None => kids.push((x.name, x.cat, x.dur())),
            }
        }
    }
    if !kids.is_empty() {
        section(ui, "Children", true);
        kids.sort_by_key(|k| std::cmp::Reverse(k.2));
        for (n, c, d) in kids.iter().take(8) {
            kv_swatch(ui, bar_color(w, *c).0, display_name(cap.name(*n)), &ms(*d));
        }
        let own: u64 = s.dur().saturating_sub(kids.iter().map(|k| k.2).sum());
        kv_swatch(ui, cat_c, "(self)", &ms(own));
    }
    ui.add_space(10.0);
    let script = if s.cat == Cat::Script && w.can_open_scripts { crate::script_location(cap.name(s.name)) } else { None };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        if let Some((path, line)) = &script {
            let label = match line {
                Some(l) => format!("Open script at line {l}"),
                None => "Open script".to_string(),
            };
            if button_ex(ui, &label, BtnKind::Primary, None, None).on_hover_text(path.as_str()).clicked() {
                w.open_script = Some((path.clone(), *line));
            }
        }
        let kind = if script.is_some() { BtnKind::Normal } else { BtnKind::Primary };
        if button_ex(ui, "Show in Table", kind, None, None).clicked() {
            w.table.filter = name.clone();
            w.table.invalidate();
            w.prefs.tab = crate::prefs::TabId::Table;
        }
        if button(ui, "Zoom to it").clicked() {
            let pad = s.dur() as f64 * 0.05;
            w.tl.view = Some((s.start as f64 - pad, s.end as f64 + pad));
        }
    });
}

/// Nothing pinned: the selected frame (or range) at a glance.
fn frame_side(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture, lanes: &[&Track]) {
    let (t0, t1) = w.sel_range(cap);
    let frames = analysis::frames_in(cap, rustudio_core::profiler::FrameKind::Render, t0, t1).count().max(1);
    let title = match (w.sel, w.sel_frame(cap)) {
        (Sel::Range(..), _) => format!("{} frames", crate::spaced(frames as u64)),
        (_, Some((i, _))) => format!("Frame #{}", crate::spaced(cap.frames_trimmed + i as u64)),
        _ => "Capture".into(),
    };
    header(ui, &title, None, &format!("{} · click a bar to pin it here", ms(t1 - t0)));
    let main = analysis::main_track(cap);
    if let Some(m) = main {
        let by = analysis::category_breakdown(m, t0, t1);
        let busy: u64 = by.iter().sum::<u64>() - by[Cat::Wait as usize];
        kv(ui, "Main thread busy", &format!("{} ({:.0} %)", ms(busy / frames as u64), busy as f64 * 100.0 / (t1 - t0).max(1) as f64), TEXT);
    }
    if let Some((cpu, gpu)) = analysis::cpu_gpu_balance(cap, t0, t1) {
        if gpu > 0.0 {
            kv(ui, "GPU per frame", &format!("{gpu:.2} ms"), TEXT);
        }
        let _ = cpu;
    }
    kv(ui, "Lanes", &lanes.len().to_string(), TEXT);
    if let Some(m) = main {
        section(ui, "Where the main thread's time went", true);
        let by = analysis::category_breakdown(m, t0, t1);
        let mut v: Vec<(Cat, u64)> = Cat::ALL.iter().map(|c| (*c, by[*c as usize])).filter(|x| x.1 > 0).collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        let total = (t1 - t0).max(1) as f32;
        for (c, ns) in v.into_iter().take(9) {
            let (rr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 23.0), Sense::hover());
            let col = bar_color(w, c).0;
            swatch(ui, Pos2::new(rr.left(), rr.center().y - 5.0), col);
            let vr = ui.painter().text(Pos2::new(rr.right(), rr.center().y), Align2::RIGHT_CENTER, ms(ns / frames as u64), mono(), TEXT);
            // (the share bar only where name, bar and value all fit)
            let bar_x = rr.right() - 150.0;
            let name_room = text_width(ui, c.name(), &body()) + 15.0 + 10.0;
            let with_bar = rr.width() >= 280.0 && rr.left() + name_room <= bar_x;
            if with_bar {
                let bw = 70.0 * ns as f32 / total;
                ui.painter().rect_filled(Rect::from_min_size(Pos2::new(bar_x, rr.center().y - 3.0), Vec2::new(bw.max(1.0), 6.0)), 3.0, col);
            }
            let name_end = if with_bar { bar_x - 8.0 } else { vr.left() - 10.0 };
            let name = elide(ui, c.name(), &body(), name_end - rr.left() - 15.0);
            ui.painter().text(Pos2::new(rr.left() + 15.0, rr.center().y), Align2::LEFT_CENTER, name, body(), MUTED);
        }
        section(ui, "Slowest scopes (self time)", true);
        let hot = analysis::hot_list(cap, t0, t1, |t| Some(t.id) == cap.render_track || lanes.iter().any(|l| l.id == t.id));
        for e in hot.into_iter().filter(|e| w.prefs.cat_visible(e.cat)).take(8) {
            kv_swatch(ui, bar_color(w, e.cat).0, display_name(cap.name(e.name)), &ms(e.stats.self_ns / frames as u64));
        }
    }
}
