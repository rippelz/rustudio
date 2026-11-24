//! The Counters tab: every counter as a card with its current value, its
//! range and a sparkline, grouped (scene, rendering, memory, physics,
//! network & assets); pin a card to show it in the stats bar, drag cards to
//! reorder them. List mode shows the same as a table.

use egui::{Align, Align2, Color32, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use rustudio_core::profiler::{Capture, Cat, CounterTrack, Unit};

use crate::theme::*;
use crate::{ProfilerWindow, Style};

/// A card: one counter, or the sum of several ("Luau heap" = server +
/// client), optionally "of" a total ("812 / 5,165").
pub(crate) struct CardDef {
    pub key: &'static str,
    pub title: &'static str,
    pub group: &'static str,
    color: Tint,
    sum: &'static [&'static str],
    of: &'static [&'static str],
}

#[derive(Clone, Copy)]
enum Tint {
    Cat(Cat),
    Rgb(u32),
}

const GROUPS: [&str; 6] = ["Scene", "Rendering", "Memory", "Physics", "Network & assets", "Other"];

macro_rules! card {
    ($key:expr, $title:expr, $group:expr, $tint:expr, [$($s:expr),*]) => {
        CardDef { key: $key, title: $title, group: $group, color: $tint, sum: &[$($s),*], of: &[] }
    };
    ($key:expr, $title:expr, $group:expr, $tint:expr, [$($s:expr),*], of [$($o:expr),*]) => {
        CardDef { key: $key, title: $title, group: $group, color: $tint, sum: &[$($s),*], of: &[$($o),*] }
    };
}

static CARDS: &[CardDef] = &[
    card!("instances", "Instances", "Scene", Tint::Rgb(0x8fa3b5), ["instances"]),
    card!("parts", "Parts", "Scene", Tint::Rgb(0x8fa3b5), ["parts"]),
    card!("lights", "Lights", "Scene", Tint::Rgb(0x8fa3b5), ["lights"]),
    card!("gui", "GUI elements", "Scene", Tint::Cat(Cat::Ui), ["GUI items drawn"]),
    card!("gui layers", "GUI layers", "Scene", Tint::Cat(Cat::Ui), ["GUI layers"]),
    card!("draw calls (all)", "Draw calls", "Rendering", Tint::Cat(Cat::Render), ["draw calls (all)"]),
    card!("triangles", "Triangles", "Rendering", Tint::Cat(Cat::Render), ["triangles (all)"]),
    card!("textures", "Textures", "Rendering", Tint::Cat(Cat::Render), ["textures loaded"]),
    card!("shadow draws", "Shadow draw calls", "Rendering", Tint::Cat(Cat::Render), ["shadow draw calls"]),
    card!("instances drawn", "Instances drawn", "Rendering", Tint::Cat(Cat::Render), ["render instances drawn"]),
    card!("render encode", "Render encode (CPU)", "Rendering", Tint::Cat(Cat::Render), ["render CPU encode"]),
    card!("gpu frame", "GPU frame", "Rendering", Tint::Cat(Cat::Gpu), ["GPU frame"]),
    card!("batches culled", "Batches culled", "Rendering", Tint::Cat(Cat::Render), ["render batches culled"], of ["render batches"]),
    card!("process", "Process", "Memory", Tint::Rgb(0xa0a4ae), ["memory (resident)"]),
    card!("Luau heap", "Luau heap", "Memory", Tint::Cat(Cat::Script), ["Luau heap (server)", "Luau heap (client)"]),
    card!("luau server", "Luau heap (server)", "Memory", Tint::Cat(Cat::Script), ["Luau heap (server)"]),
    card!("luau client", "Luau heap (client)", "Memory", Tint::Cat(Cat::Script), ["Luau heap (client)"]),
    card!("meshes", "Meshes", "Memory", Tint::Cat(Cat::Assets), ["meshes loaded"]),
    card!("awake", "Awake bodies", "Physics", Tint::Cat(Cat::Physics), ["physics awake bodies"], of ["physics bodies"]),
    card!("physics contact pairs", "Contacts", "Physics", Tint::Cat(Cat::Physics), ["physics contact pairs"]),
    card!("islands", "Islands", "Physics", Tint::Cat(Cat::Physics), ["physics islands"]),
    card!("sim rate", "Sim rate", "Physics", Tint::Cat(Cat::Physics), ["physics sim rate"]),
    card!("contact points", "Contact points", "Physics", Tint::Cat(Cat::Physics), ["physics contact points"]),
    card!("largest island", "Largest island", "Physics", Tint::Cat(Cat::Physics), ["physics largest island"]),
    card!("solver rows", "Solver rows", "Physics", Tint::Cat(Cat::Physics), ["physics solver rows"]),
    card!("rep in", "Replication in", "Network & assets", Tint::Cat(Cat::Network), ["replication to client"]),
    card!("rep out", "Replication out", "Network & assets", Tint::Cat(Cat::Network), ["replication to server"]),
    card!("phys in", "Physics in", "Network & assets", Tint::Cat(Cat::Network), ["physics to client"]),
    card!("phys out", "Physics out", "Network & assets", Tint::Cat(Cat::Network), ["physics to server"]),
    card!("tc in", "Team Create in", "Network & assets", Tint::Cat(Cat::Network), ["Team Create received"]),
    card!("tc out", "Team Create out", "Network & assets", Tint::Cat(Cat::Network), ["Team Create sent"]),
    card!("asset queue", "Asset queue", "Network & assets", Tint::Cat(Cat::Assets), ["asset downloads pending"]),
    card!("decoding", "Textures decoding", "Network & assets", Tint::Cat(Cat::Assets), ["textures decoding"]),
    card!("voices", "Audio voices", "Network & assets", Tint::Cat(Cat::Audio), ["audio voices"]),
];

pub(crate) fn card(key: &str) -> Option<&'static CardDef> {
    CARDS.iter().find(|c| c.key == key)
}

/// A card for a counter no definition names (shown under Other).
struct Adhoc<'a> {
    name: &'a str,
    cat: Cat,
}

#[derive(Default)]
pub(crate) struct State {
    filter: String,
    dragging: Option<String>,
}

/// One card's numbers over a time window.
struct Series {
    unit: Unit,
    values: Vec<f64>,
    current: f64,
    of: Option<f64>,
    lo: f64,
    hi: f64,
    mean: f64,
}

fn track<'a>(cap: &'a Capture, name: &str) -> Option<&'a CounterTrack> {
    cap.counters.iter().find(|c| c.name == name)
}

/// The summed series of `names` over [t0, t1], at most `points` points.
fn series(cap: &Capture, names: &[&str], of: &[&str], t0: u64, t1: u64, points: usize) -> Option<Series> {
    let tracks: Vec<&CounterTrack> = names.iter().filter_map(|n| track(cap, n)).collect();
    let first = tracks.first()?;
    let times: Vec<u64> = first.samples.iter().map(|s| s.0).filter(|t| *t >= t0 && *t <= t1).collect();
    let at = |t: u64| tracks.iter().map(|tr| tr.at(t).unwrap_or(0.0)).sum::<f64>();
    let raw: Vec<f64> = if times.is_empty() { vec![at(t1)] } else { times.iter().map(|&t| at(t)).collect() };
    let (lo, hi, sum) = raw.iter().fold((f64::MAX, f64::MIN, 0.0), |a, &v| (a.0.min(v), a.1.max(v), a.2 + v));
    let mean = sum / raw.len() as f64;
    // downsample: the mean of each bucket
    let values = if raw.len() > points {
        let k = raw.len() as f64 / points as f64;
        (0..points)
            .map(|i| {
                let a = (i as f64 * k) as usize;
                let b = (((i + 1) as f64 * k) as usize).max(a + 1).min(raw.len());
                raw[a..b].iter().sum::<f64>() / (b - a) as f64
            })
            .collect()
    } else {
        raw.clone()
    };
    let current = *raw.last().unwrap_or(&0.0);
    let of = (!of.is_empty()).then(|| of.iter().filter_map(|n| track(cap, n)).map(|t| t.at(t1).unwrap_or(0.0)).sum());
    Some(Series { unit: first.unit, values, current, of, lo, hi, mean })
}

fn fmt_value(s: &Series) -> String {
    let v = s.unit.format(s.current);
    match s.of {
        Some(total) => format!("{v} / {}", s.unit.format(total)),
        None => v,
    }
}

fn tint_color(t: Tint, style: &Style) -> Color32 {
    match t {
        Tint::Cat(c) => style.cat_color(c),
        Tint::Rgb(v) => Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8),
    }
}

/// The window the counters cover (Last 10 s / Last 60 s / Capture).
fn window(w: &ProfilerWindow, cap: &Capture) -> (u64, u64) {
    let end = cap.counters.iter().filter_map(|c| c.samples.back().map(|s| s.0)).max().unwrap_or(0);
    let start = cap.counters.iter().filter_map(|c| c.samples.front().map(|s| s.0)).min().unwrap_or(0);
    match w.prefs.counters_window {
        0 => (end.saturating_sub(10_000_000_000), end),
        1 => (end.saturating_sub(60_000_000_000), end),
        _ => (start, end),
    }
}

/// A pinned counter in the stats bar.
pub(crate) fn chip(ui: &mut Ui, cap: &Capture, def: &CardDef, style: &Style) {
    let end = cap.counters.iter().filter_map(|c| c.samples.back().map(|s| s.0)).max().unwrap_or(0);
    let Some(s) = series(cap, def.sum, def.of, end.saturating_sub(10_000_000_000), end, 24) else { return };
    let value = fmt_value(&s);
    let tw = text_width(ui, &value, &mono()).max(text_width(ui, def.title, &egui::FontId::proportional(11.0)));
    let (r, _) = ui.allocate_exact_size(Vec2::new(tw + 56.0 + 24.0, 34.0), Sense::hover());
    let p = ui.painter();
    p.rect(r, 3.0, PANEL2, Stroke::new(1.0, LINE2), StrokeKind::Inside);
    p.text(Pos2::new(r.left() + 8.0, r.top() + 3.0), Align2::LEFT_TOP, def.title, egui::FontId::proportional(11.0), MUTED);
    p.text(Pos2::new(r.left() + 8.0, r.top() + 17.0), Align2::LEFT_TOP, value, mono(), TEXT);
    sparkline(p, Rect::from_min_size(Pos2::new(r.right() - 64.0, r.top() + 7.0), Vec2::new(56.0, 20.0)), &s.values, tint_color(def.color, style));
}

/// The card order: the user's, then the catalogue's, then counters no card
/// names (by category).
fn ordered<'a>(w: &ProfilerWindow, cap: &'a Capture) -> Vec<(String, Option<&'static CardDef>, Option<Adhoc<'a>>)> {
    let mut v: Vec<(String, Option<&'static CardDef>, Option<Adhoc<'a>>)> = Vec::new();
    for c in CARDS {
        if c.sum.iter().any(|n| track(cap, n).is_some()) {
            v.push((c.key.to_string(), Some(c), None));
        }
    }
    for t in &cap.counters {
        if !CARDS.iter().any(|c| c.sum.contains(&t.name.as_str()) || c.of.contains(&t.name.as_str())) {
            v.push((t.name.clone(), None, Some(Adhoc { name: &t.name, cat: t.cat })));
        }
    }
    let pos = |k: &str| w.prefs.counter_order.iter().position(|x| x == k).unwrap_or(usize::MAX);
    v.sort_by_key(|x| pos(&x.0));
    v
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture) {
    let r = ui.max_rect();
    let bar = Rect::from_min_size(r.left_top(), Vec2::new(r.width(), SUBBAR_H));
    hline(ui, bar.left(), bar.right(), bar.bottom() - 1.0, LINE2);
    {
        let mut c = crate::region(ui, bar.shrink2(Vec2::new(10.0, 0.0)), "ctr-bar");
        c.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            search(ui, &mut w.counters.filter, "Filter counters…", 220.0);
            if let Some(i) = seg(ui, &["Cards", "List"], w.prefs.counters_list as usize) {
                w.prefs.counters_list = i == 1;
            }
            if let Some(i) = seg(ui, &["Last 10 s", "Last 60 s", "Capture"], w.prefs.counters_window as usize) {
                w.prefs.counters_window = i as u8;
            }
            let hint = "Pinned counters show in the top bar · drag cards to reorder";
            if ui.available_width() > text_width(ui, hint, &body()) + 16.0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(hint).font(body()).color(MUTED));
                });
            }
        });
    }
    let body_r = Rect::from_min_max(Pos2::new(r.left(), bar.bottom()), r.right_bottom());
    let (t0, t1) = window(w, cap);
    let needle = w.counters.filter.to_lowercase();
    let cards: Vec<_> = ordered(w, cap)
        .into_iter()
        .filter(|(_, d, a)| {
            let title = d.map(|d| d.title).or(a.as_ref().map(|a| a.name)).unwrap_or("");
            needle.is_empty() || title.to_lowercase().contains(&needle)
        })
        .collect();
    let mut inner = crate::region(ui, body_r.shrink2(Vec2::new(10.0, 12.0)), "ctr-body");
    if cards.is_empty() {
        inner.label(egui::RichText::new("No counters recorded yet (they are sampled while the profiler records).").font(body()).color(FAINT));
        return;
    }
    let mut drop_on: Option<String> = None;
    let mut toggle_pin: Option<String> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut inner, |ui| {
        if w.prefs.counters_list {
            list(w, ui, cap, &cards, t0, t1, &mut toggle_pin);
            return;
        }
        for g in GROUPS {
            let in_group: Vec<_> = cards
                .iter()
                .filter(|(_, d, a)| d.map(|d| d.group).unwrap_or_else(|| if a.is_some() { "Other" } else { "" }) == g)
                .collect();
            if in_group.is_empty() {
                continue;
            }
            ui.label(egui::RichText::new(g.to_uppercase()).font(egui::FontId::proportional(11.0)).color(FAINT).extra_letter_spacing(0.6));
            ui.add_space(7.0);
            // (cards at least ~330 px wide: fewer per row in a narrow window)
            let cols = ((ui.available_width() + 10.0) / 340.0).floor().clamp(1.0, 4.0) as usize;
            let gap = 10.0;
            let cw = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
            for chunk in in_group.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (key, def, adhoc) in chunk {
                        let (title, tint, names, of): (&str, Tint, Vec<&str>, Vec<&str>) = match (def, adhoc) {
                            (Some(d), _) => (d.title, d.color, d.sum.to_vec(), d.of.to_vec()),
                            (None, Some(a)) => (a.name, Tint::Cat(a.cat), vec![a.name], vec![]),
                            _ => continue,
                        };
                        let Some(s) = series(cap, &names, &of, t0, t1, 40) else { continue };
                        let (cr, resp) = ui.allocate_exact_size(Vec2::new(cw, 80.0), Sense::click_and_drag());
                        let pinned = w.prefs.pinned_counters.iter().any(|k| k == key);
                        let p = ui.painter();
                        let dragging = w.counters.dragging.as_deref() == Some(key.as_str());
                        let border = if !dragging && w.counters.dragging.is_some() && resp.contains_pointer() { ACCENT } else { LINE2 };
                        p.rect(cr, 4.0, if dragging { PANEL2 } else { PANEL }, Stroke::new(1.0, border), StrokeKind::Inside);
                        p.text(cr.left_top() + Vec2::new(12.0, 11.0), Align2::LEFT_TOP, title, small(), MUTED);
                        let pin_text = if pinned { "pinned" } else { "pin" };
                        let pin_w = text_width(ui, pin_text, &small()) + if pinned { 16.0 } else { 0.0 };
                        let pr = Rect::from_min_size(Pos2::new(cr.right() - 12.0 - pin_w, cr.top() + 8.0), Vec2::new(pin_w, 18.0));
                        if pinned {
                            rustudio_engine::ti::paint(p, Rect::from_center_size(Pos2::new(pr.left() + 6.0, pr.center().y), Vec2::splat(12.0)), "pin", ACCENT);
                        }
                        p.text(Pos2::new(pr.right(), pr.center().y), Align2::RIGHT_CENTER, pin_text, small(), if pinned { ACCENT } else { FAINT });
                        p.text(cr.left_top() + Vec2::new(12.0, 30.0), Align2::LEFT_TOP, fmt_value(&s), egui::FontId::monospace(19.0), WHITE);
                        let mm = if s.mean.abs() > 1e-9 {
                            format!("min {:.2}× · max {:.2}×", s.lo / s.mean, s.hi / s.mean)
                        } else {
                            format!("min {} · max {}", s.unit.format(s.lo), s.unit.format(s.hi))
                        };
                        p.text(Pos2::new(cr.left() + 12.0, cr.bottom() - 10.0), Align2::LEFT_BOTTOM, mm, egui::FontId::monospace(11.0), FAINT);
                        sparkline(p, Rect::from_min_size(Pos2::new(cr.right() - 162.0, cr.bottom() - 48.0), Vec2::new(150.0, 38.0)), &s.values, tint_color(tint, &w.prefs.style));
                        if resp.clicked() && ui.input(|i| i.pointer.interact_pos()).is_some_and(|q| pr.expand(4.0).contains(q)) {
                            toggle_pin = Some(key.clone());
                        }
                        if resp.drag_started() {
                            w.counters.dragging = Some(key.clone());
                        }
                        if w.counters.dragging.is_some() && resp.contains_pointer() && ui.input(|i| i.pointer.any_released()) {
                            drop_on = Some(key.clone());
                        }
                        let tip = format!("{title}: now {} · min {} · mean {} · max {}", s.unit.format(s.current), s.unit.format(s.lo), s.unit.format(s.mean), s.unit.format(s.hi));
                        resp.on_hover_text(tip);
                    }
                });
                ui.add_space(gap);
            }
            ui.add_space(4.0);
        }
    });
    if let Some(k) = toggle_pin {
        let pins = &mut w.prefs.pinned_counters;
        if let Some(i) = pins.iter().position(|x| *x == k) {
            pins.remove(i);
        } else {
            pins.push(k);
        }
    }
    if ui.input(|i| i.pointer.any_released()) {
        if let (Some(from), Some(to)) = (w.counters.dragging.take(), drop_on) {
            if from != to {
                let mut order: Vec<String> = ordered(w, cap).into_iter().map(|x| x.0).collect();
                order.retain(|k| *k != from);
                let at = order.iter().position(|k| *k == to).unwrap_or(order.len());
                order.insert(at, from);
                w.prefs.counter_order = order;
            }
        }
        w.counters.dragging = None;
    }
}

#[allow(clippy::type_complexity)]
fn list(w: &ProfilerWindow, ui: &mut Ui, cap: &Capture, cards: &[(String, Option<&'static CardDef>, Option<Adhoc<'_>>)], t0: u64, t1: u64, toggle_pin: &mut Option<String>) {
    let cols = [("Counter", 260.0), ("Group", 150.0), ("Now", 130.0), ("Min", 130.0), ("Mean", 130.0), ("Max", 130.0), ("", 70.0), ("History", 0.0)];
    let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    ui.painter().rect_filled(hr, 0.0, PANEL);
    let mut x = hr.left();
    for (i, (t, wd)) in cols.iter().enumerate() {
        let right = (2..=5).contains(&i);
        let pos = if right { Pos2::new(x + wd - 10.0, hr.center().y) } else { Pos2::new(x + 10.0, hr.center().y) };
        ui.painter().text(pos, if right { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER }, t, egui::FontId::proportional(11.5), MUTED);
        x += wd;
    }
    for (key, def, adhoc) in cards {
        let (title, group, tint, names, of): (&str, &str, Tint, Vec<&str>, Vec<&str>) = match (def, adhoc) {
            (Some(d), _) => (d.title, d.group, d.color, d.sum.to_vec(), d.of.to_vec()),
            (None, Some(a)) => (a.name, "Other", Tint::Cat(a.cat), vec![a.name], vec![]),
            _ => continue,
        };
        let Some(s) = series(cap, &names, &of, t0, t1, 40) else { continue };
        let (rr, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), w.prefs.style.row_height()), Sense::click());
        hline(ui, rr.left(), rr.right(), rr.bottom() - 1.0, LINE2);
        let p = ui.painter();
        let cy = rr.center().y;
        let vals = [title.to_string(), group.to_string(), fmt_value(&s), s.unit.format(s.lo), s.unit.format(s.mean), s.unit.format(s.hi)];
        let mut x = rr.left();
        for (i, v) in vals.iter().enumerate() {
            let wd = cols[i].1;
            if (2..=5).contains(&i) {
                p.text(Pos2::new(x + wd - 10.0, cy), Align2::RIGHT_CENTER, v, mono(), TEXT);
            } else {
                p.text(Pos2::new(x + 10.0, cy), Align2::LEFT_CENTER, v, body(), if i == 0 { TEXT } else { MUTED });
            }
            x += wd;
        }
        let pinned = w.prefs.pinned_counters.iter().any(|k| k == key);
        let pr = Rect::from_min_size(Pos2::new(x, rr.top()), Vec2::new(70.0, rr.height()));
        p.text(Pos2::new(pr.left() + 10.0, cy), Align2::LEFT_CENTER, if pinned { "pinned" } else { "pin" }, small(), if pinned { ACCENT } else { FAINT });
        x += 70.0;
        sparkline(p, Rect::from_min_size(Pos2::new(x + 10.0, rr.top() + 5.0), Vec2::new(120.0, rr.height() - 10.0)), &s.values, tint_color(tint, &w.prefs.style));
        if resp.clicked() && ui.input(|i| i.pointer.interact_pos()).is_some_and(|q| pr.contains(q)) {
            *toggle_pin = Some(key.clone());
        }
    }
}
