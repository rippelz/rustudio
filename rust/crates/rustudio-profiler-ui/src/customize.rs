//! The Customize popup: categories and their colours, lanes (drag to
//! reorder), tabs, display settings and presets. Everything persists with
//! the rustudio settings.

use egui::{Align, Align2, Color32, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
use rustudio_core::profiler::{analysis, Capture, Cat};

use crate::prefs::{Labels, Preset, TabId, TooltipDetail};
use crate::theme::*;
use crate::{Act, ProfilerWindow};

fn rgb(c: Color32) -> u32 {
    ((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32
}

/// A labelled dropdown row of the Display column.
fn combo_row(ui: &mut Ui, label: &str, id: &str, current: String, options: &[String]) -> Option<usize> {
    let mut picked = None;
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    ui.painter().text(Pos2::new(r.left(), r.center().y), Align2::LEFT_CENTER, label, body(), MUTED);
    let text = format!("{current}  ▾");
    let w = text_width(ui, &text, &mono()) + 18.0;
    let br = Rect::from_min_size(Pos2::new(r.right() - w.max(70.0), r.center().y - 11.0), Vec2::new(w.max(70.0), 22.0));
    let resp = ui.interact(br, ui.id().with(("combo", id)), Sense::click());
    ui.painter().rect(br, 3.0, DEEP, Stroke::new(1.0, if resp.hovered() { PRIMARY_LINE } else { LINE }), StrokeKind::Inside);
    ui.painter().text(Pos2::new(br.right() - 8.0, br.center().y), Align2::RIGHT_CENTER, text, mono(), TEXT);
    let popup_id = ui.id().with(("combo-pop", id));
    let open_key = egui::Id::new("profiler-open-combo");
    let open_now: Option<egui::Id> = ui.data(|d| d.get_temp(open_key));
    if resp.clicked() {
        let next = if open_now == Some(popup_id) { None } else { Some(popup_id) };
        ui.data_mut(|d| match next {
            Some(v) => {
                d.insert_temp(open_key, v);
            }
            None => {
                d.remove::<egui::Id>(open_key);
            }
        });
    }
    let open_now: Option<egui::Id> = ui.data(|d| d.get_temp(open_key));
    if open_now == Some(popup_id) {
        let area = egui::Area::new(popup_id).fade_in(false).order(egui::Order::Tooltip).fixed_pos(br.left_bottom() + Vec2::new(0.0, 2.0)).show(ui.ctx(), |ui| {
            egui::Frame::new().fill(POP_BG).stroke(Stroke::new(1.0, POP_LINE)).corner_radius(4).inner_margin(4).show(ui, |ui| {
                ui.set_min_width(br.width());
                for (i, o) in options.iter().enumerate() {
                    let (rr, r2) = ui.allocate_exact_size(Vec2::new(br.width().max(140.0), 24.0), Sense::click());
                    if r2.hovered() || *o == current {
                        ui.painter().rect_filled(rr, 3.0, if *o == current { SEG_ON } else { PANEL2 });
                    }
                    ui.painter().text(Pos2::new(rr.left() + 8.0, rr.center().y), Align2::LEFT_CENTER, o, mono(), TEXT);
                    if r2.clicked() {
                        picked = Some(i);
                    }
                }
            });
        });
        if picked.is_some() || (ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !resp.contains_pointer()) {
            ui.data_mut(|d| d.remove::<egui::Id>(open_key));
        }
    }
    picked
}

pub(crate) fn show(w: &mut ProfilerWindow, ui: &mut Ui, top_right: Pos2, cap: &Capture, acts: &mut Vec<Act>) {
    let width = 760.0;
    let at = Pos2::new(top_right.x - width - 10.0, top_right.y + 4.0);
    let mut close = false;
    let area = egui::Area::new(ui.id().with("customize")).fade_in(false).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
        egui::Frame::new()
            .fill(POP_BG)
            .stroke(Stroke::new(1.0, POP_LINE))
            .corner_radius(5)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(128) })
            .show(ui, |ui| {
                ui.set_width(width - 32.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 22.0;
                    let col_w = (width - 32.0 - 44.0) / 3.3;
                    ui.allocate_ui_with_layout(Vec2::new(col_w, 10.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(col_w);
                        categories(w, ui);
                    });
                    ui.allocate_ui_with_layout(Vec2::new(col_w, 10.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(col_w);
                        threads(w, ui, cap);
                        section(ui, "Tabs", true);
                        for t in TabId::ALL {
                            let on = !w.prefs.hidden_tabs.contains(&t);
                            if checkbox(ui, on, t.title(), None) {
                                if on {
                                    if w.prefs.hidden_tabs.len() < 4 {
                                        w.prefs.hidden_tabs.push(t);
                                    }
                                } else {
                                    w.prefs.hidden_tabs.retain(|x| *x != t);
                                }
                            }
                        }
                    });
                    ui.allocate_ui_with_layout(Vec2::new(col_w * 1.3, 10.0), Layout::top_down(Align::Min), |ui| {
                        ui.set_width(col_w * 1.3);
                        close = display(w, ui, acts);
                    });
                });
            });
    });
    // a click outside (but not on the Customize button itself) closes it
    let outside = ui.input(|i| i.pointer.any_pressed())
        && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && p.y > top_right.y + 2.0)
        && ui.data(|d| d.get_temp::<egui::Id>(egui::Id::new("profiler-open-combo"))).is_none();
    if close || outside {
        w.customize_open = false;
    }
}

fn categories(w: &mut ProfilerWindow, ui: &mut Ui) {
    section(ui, "Categories & colours", false);
    for cat in crate::legend_cats() {
        ui.horizontal(|ui| {
            let on = w.prefs.cat_visible(cat);
            let mut c = w.prefs.style.cat_color(cat);
            if checkbox(ui, on, cat.name(), Some(c)) {
                if on {
                    w.prefs.hidden_cats.push(cat);
                } else {
                    w.prefs.hidden_cats.retain(|x| *x != cat);
                }
                w.breakdown.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let before = c;
                ui.spacing_mut().interact_size = Vec2::new(28.0, 14.0);
                ui.color_edit_button_srgba(&mut c);
                if c != before {
                    if w.prefs.style.cat_rgb.len() < Cat::ALL.len() {
                        w.prefs.style.cat_rgb = Cat::ALL.iter().map(|c| c.default_rgb()).collect();
                    }
                    w.prefs.style.cat_rgb[cat as usize] = rgb(c);
                }
            });
        });
    }
}

fn threads(w: &mut ProfilerWindow, ui: &mut Ui, cap: &Capture) {
    section(ui, "Threads (drag to reorder)", false);
    // every lane in the capture, in the window's order (hidden ones too)
    let mut names: Vec<String> = Vec::new();
    {
        let hidden = std::mem::take(&mut w.prefs.hidden_lanes);
        for t in crate::timeline::lanes(w, cap) {
            names.push(t.name.clone());
        }
        w.prefs.hidden_lanes = hidden;
    }
    for n in &w.prefs.hidden_lanes {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let drag_id = ui.id().with("lane-drag");
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let mut drop_at: Option<usize> = None;
    for (i, n) in names.iter().enumerate() {
        let (title, _, _) = analysis::lane_info(n);
        let row = ui.horizontal(|ui| {
            let on = !w.prefs.hidden_lanes.contains(n);
            if checkbox(ui, on, &title, None) {
                if on {
                    w.prefs.hidden_lanes.push(n.clone());
                } else {
                    w.prefs.hidden_lanes.retain(|x| x != n);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (hr, hresp) = ui.allocate_exact_size(Vec2::new(16.0, 20.0), Sense::drag());
                // Drag grip: two columns of three dots (the bundled fonts have no ⋮).
                for (dx, dy) in [(-2.5, -4.0), (-2.5, 0.0), (-2.5, 4.0), (2.5, -4.0), (2.5, 0.0), (2.5, 4.0)] {
                    ui.painter().circle_filled(hr.center() + Vec2::new(dx, dy), 1.1, FAINT);
                }
                if hresp.drag_started() {
                    dragging = Some(i);
                }
                if hresp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
            });
        });
        if dragging.is_some() && row.response.rect.contains(ui.input(|i| i.pointer.hover_pos()).unwrap_or(Pos2::ZERO)) {
            let r = row.response.rect;
            ui.painter().hline(r.x_range(), r.top(), Stroke::new(2.0, ACCENT));
            drop_at = Some(i);
        }
    }
    if let Some(from) = dragging {
        if ui.input(|i| i.pointer.any_released()) {
            if let Some(to) = drop_at {
                let mut order = names.clone();
                let n = order.remove(from);
                order.insert(to.min(order.len()), n);
                w.prefs.lane_order = order;
            }
            dragging = None;
        }
    }
    ui.data_mut(|d| match dragging {
        Some(i) => {
            d.insert_temp(drag_id, i);
        }
        None => {
            d.remove::<usize>(drag_id);
        }
    });
}

/// The Display column; true when Done was clicked.
fn display(w: &mut ProfilerWindow, ui: &mut Ui, acts: &mut Vec<Act>) -> bool {
    section(ui, "Display", false);
    let mut cfg = w.cfg.clone();
    let budgets = [8.3f32, 11.1, 16.7, 33.3];
    if let Some(i) = combo_row(ui, "Frame budget", "budget", format!("{:.1} ms", cfg.budget_ms), &budgets.iter().map(|b| format!("{b:.1} ms")).collect::<Vec<_>>()) {
        cfg.budget_ms = budgets[i];
    }
    let spikes = [16.7f32, 25.0, 33.0, 50.0, 100.0];
    if let Some(i) = combo_row(ui, "Spike threshold", "spike", format!("{:.1} ms", cfg.spike_ms), &spikes.iter().map(|b| format!("{b:.1} ms")).collect::<Vec<_>>()) {
        cfg.spike_ms = spikes[i];
    }
    let hist = [5.0f32, 10.0, 20.0, 30.0, 60.0];
    let fps = 60.0;
    if let Some(i) = combo_row(ui, "History", "history", format!("{:.0} s · {:.0} frames", cfg.history_secs, cfg.history_secs * fps), &hist.iter().map(|h| format!("{h:.0} s · {:.0} frames", h * fps)).collect::<Vec<_>>()) {
        cfg.history_secs = hist[i];
    }
    let lens = [60u32, 120, 300, 600, 1200];
    if let Some(i) = combo_row(ui, "Capture length", "caplen", format!("{} frames", w.prefs.capture_frames), &lens.iter().map(|l| format!("{l} frames")).collect::<Vec<_>>()) {
        w.prefs.capture_frames = lens[i];
    }
    if let Some(i) = combo_row(ui, "Units", "units", if cfg.instr_mode { "instr".into() } else { "ms".into() }, &["ms".into(), "instr".into()]) {
        acts.push(Act::SetInstr(i == 1));
        cfg.instr_mode = i == 1;
    }
    if let Some(i) = combo_row(ui, "Density", "density", if w.prefs.style.compact { "Compact".into() } else { "Comfortable".into() }, &["Comfortable".into(), "Compact".into()]) {
        w.prefs.style.compact = i == 1;
    }
    if let Some(i) = combo_row(ui, "Timeline labels", "labels", w.prefs.labels.title().into(), &Labels::ALL.iter().map(|l| l.title().to_string()).collect::<Vec<_>>()) {
        w.prefs.labels = Labels::ALL[i];
    }
    if let Some(i) = combo_row(ui, "Tooltip detail", "tooltip", w.prefs.tooltip.title().into(), &TooltipDetail::ALL.iter().map(|l| l.title().to_string()).collect::<Vec<_>>()) {
        w.prefs.tooltip = TooltipDetail::ALL[i];
    }
    if let Some(i) = combo_row(ui, "GPU timing", "gputiming", if cfg.gpu_timing { "On".into() } else { "Off".into() }, &["On".into(), "Off".into()]) {
        cfg.gpu_timing = i == 0;
    }
    if let Some(i) = combo_row(ui, "Allocations", "alloc", if cfg.track_alloc { "Counted".into() } else { "Off".into() }, &["Counted".into(), "Off".into()]) {
        cfg.track_alloc = i == 0;
    }
    if cfg != w.cfg && !matches!(acts.last(), Some(Act::SetInstr(_))) {
        acts.push(Act::Config(cfg));
    }

    section(ui, "Presets", true);
    let current: Vec<Preset> = w.prefs.all_presets();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for p in &current {
            let on = p.hidden_cats == w.prefs.hidden_cats && p.hidden_lanes == w.prefs.hidden_lanes;
            let resp = button_ex(ui, &p.name, if on { BtnKind::Primary } else { BtnKind::Normal }, None, None);
            if resp.clicked() {
                w.prefs.hidden_cats = p.hidden_cats.clone();
                w.prefs.hidden_lanes = p.hidden_lanes.clone();
                w.breakdown.clear();
            }
            if resp.secondary_clicked() {
                w.prefs.presets.retain(|x| x.name != p.name);
            }
        }
        if button(ui, "+ Save current").on_hover_text("Saves the visible categories and lanes as a preset (right-click a preset to delete it)").clicked() {
            let n = w.prefs.presets.len() + 1;
            w.prefs.presets.push(Preset { name: format!("Preset {n}"), hidden_cats: w.prefs.hidden_cats.clone(), hidden_lanes: w.prefs.hidden_lanes.clone() });
        }
    });
    ui.add_space(14.0);
    let mut done = false;
    ui.horizontal(|ui| {
        if button(ui, "Reset to defaults").clicked() {
            let keep_tab = w.prefs.tab;
            w.prefs = crate::prefs::Prefs { tab: keep_tab, ..Default::default() };
            acts.push(Act::Config(rustudio_core::profiler::Config::default()));
            w.breakdown.clear();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button_ex(ui, "Done", BtnKind::Primary, None, None).clicked() {
                done = true;
            }
        });
    });
    let _ = Rect::NOTHING;
    done
}
