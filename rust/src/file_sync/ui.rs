//! File Sync's UI: the top-bar button with its popover, and the File Sync
//! dock panel (a tab like Explorer / Properties / Output, Window > File
//! Sync; it opens tabbed with Properties).
//!
//! The popover covers the usual case: the state, the project, one action
//! (Attach / Apply / Detach) and Open panel.
//!
//! The panel is one flat surface drawn the way the editor's own panels are:
//! - a status row: dot, state ("4 pending", "Synced"…), the project's name
//!   (its path, place and options in the tooltip), the actions (Apply and
//!   Skip side by side) and a ⋯ menu;
//! - Changes: Explorer rows (A / M / D / R letter, class icon, instance
//!   name; the path and file in the tooltip); a script's row unfolds its
//!   diff. Not attached: the attach form as Properties rows;
//! - Mapping: an Explorer tree, service → mapped instance, with its folder
//!   and a dot (in the place / missing);
//! - Settings: Properties rows under folding bands, the description in the
//!   tooltip;
//! - Log: Output's look and filter bar.

use egui::{Align, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use rustudio_engine::theme;

use super::diff::{self, Row};
use super::plan::{Line, MapRow};
use super::settings::{AskDeletes, Initial, LogLevel, Prompts};
use super::{Reason, Status};
use crate::app::Editor;
use crate::properties::{BAND, CAT_H, DIVIDER, FONT, INDENT as P_INDENT, ROW_H as P_ROW_H, TEXT as P_TEXT};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    /// Changes (the attach form while nothing is attached).
    #[default]
    Sync,
    Mapping,
    Settings,
    Log,
}

/// Explorer's row height and indent.
const ROW_H: f32 = 20.0;
const INDENT: f32 = 14.0;
const GREEN: Color32 = Color32::from_rgb(72, 199, 116);
const ADD: Color32 = Color32::from_rgb(98, 196, 122);
const MOD: Color32 = Color32::from_rgb(110, 168, 255);
const DEL: Color32 = Color32::from_rgb(236, 104, 98);
const ADD_BG: Color32 = Color32::from_rgb(30, 50, 38);
const DEL_BG: Color32 = Color32::from_rgb(58, 32, 34);
const LIVE: Color32 = Color32::from_rgb(196, 140, 30);

fn ago(t: std::time::Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..=4 => "just now".into(),
        5..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Off => theme::TEXT_DIM,
        Status::Synced => GREEN,
        Status::Pending => theme::WARN,
        Status::Error => theme::ERROR,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The letter and colour of a change.
fn letter(kind: &str) -> (&'static str, Color32) {
    match kind {
        "create" => ("A", ADD),
        "set" => ("M", MOD),
        "replace" => ("R", theme::WARN),
        _ => ("D", DEL),
    }
}

/// A small filled button (the one action that matters).
fn accent_button(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(fill))
}

/// The instance's own name from a dotted path.
fn leaf(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

/// What the header / popover shows: which action.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    /// Attach the picked project (or open the panel when none is picked).
    Attach,
    Apply,
    Retry,
    Detach,
    /// The place is a project folder opened as one: its own sync runs, so
    /// File Sync only offers the folder.
    ShowFolder,
}

fn open_in_file_manager(dir: &std::path::Path) {
    let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(dir).spawn();
}

impl Editor {
    /// One line for the button's tooltip, MCP and the Output.
    pub fn file_sync_status_text(&self) -> (Color32, String) {
        let fs = &self.file_sync;
        let col = status_color(self.file_sync_ui_status());
        if let (None, Some(p)) = (&fs.att, &self.project) {
            return (col, format!("{} · opened as a project folder: its files sync with the place", p.name));
        }
        let Some(a) = &fs.att else {
            return (col, match &fs.error {
                Some(e) => format!("Not attached · {e}"),
                None => "Off · no project attached to this place".into(),
            });
        };
        let mut t = format!("{} → {}", a.st.name, a.place_label);
        if let Some(p) = &fs.pending {
            t += &format!(" · {} change(s) wait for your OK", p.lines.len());
        } else if a.deferred.is_some() {
            t += " · files changed during Play (applied at Stop)";
        } else if !fs.commits.is_empty() {
            let conflicts = fs.commits.iter().filter(|c| c.conflict).count();
            t += &if conflicts > 0 { format!(" · {conflicts} script conflict(s) with the server") } else { format!(" · {} script commit(s) in flight", fs.commits.len()) };
        } else if let Some(e) = &fs.error {
            t += &format!(" · {e}");
        } else {
            t += &match &a.last_sync {
                Some((at, what)) => format!(" · synced ({what}, {})", ago(*at)),
                None => " · in sync".into(),
            };
        }
        (col, t)
    }

    /// The state in a word or two.
    fn file_sync_word(&self) -> String {
        let fs = &self.file_sync;
        if fs.att.is_none() && self.project.is_some() {
            return "Synced as project".into();
        }
        let Some(a) = &fs.att else {
            return if fs.error.is_some() { "Error".into() } else { "Not attached".into() };
        };
        let conflicts = fs.commits.iter().filter(|c| c.conflict).count();
        if conflicts > 0 {
            return format!("{conflicts} conflict{}", plural(conflicts));
        }
        if let Some(p) = &fs.pending {
            return format!("{} pending", p.lines.len());
        }
        if let Some(q) = &fs.queue {
            return format!("Sending {}/{}", q.sent, q.total);
        }
        if a.deferred.is_some() {
            return "Waiting for Stop".into();
        }
        if !fs.commits.is_empty() {
            return format!("Committing {}", fs.commits.len());
        }
        if fs.error.is_some() {
            return "Error".into();
        }
        "Synced".into()
    }

    /// The status the File Sync UI shows: a project folder opened as one
    /// syncs through its own project sync, which counts as synced here.
    fn file_sync_ui_status(&self) -> Status {
        if self.file_sync.att.is_none() && self.project.is_some() {
            return Status::Synced;
        }
        self.file_sync.status()
    }

    fn file_sync_action(&self) -> Action {
        match (&self.file_sync.att, &self.file_sync.pending) {
            (None, _) if self.project.is_some() => Action::ShowFolder,
            (None, _) => Action::Attach,
            (Some(_), Some(_)) => Action::Apply,
            (Some(_), None) if self.file_sync.error.is_some() => Action::Retry,
            _ => Action::Detach,
        }
    }

    /// Everything about the attachment that isn't needed to act: the
    /// project name's tooltip.
    fn file_sync_details(&self) -> String {
        let Some(a) = &self.file_sync.att else { return String::new() };
        let mut t = format!("{}\n→ {}{}", a.st.file.display(), a.place_label, if a.live { " (live Team Create)" } else { "" });
        t += &format!("\n{} instances from files", a.st.map.len());
        if let Some((at, what)) = &a.last_sync {
            t += &format!("\nLast sync {}: {what}", ago(*at));
        }
        t += if self.file_sync.settings.two_way { "\nTwo-way: edits here are written to the files" } else { "\nFiles → place (two-way off)" };
        if a.place_key.as_ref().is_some_and(|k| self.file_sync.settings.remembered_for(k).is_some()) {
            t += "\nAttached again when this place opens";
        }
        t
    }

    /// The picked project, when it names one.
    fn file_sync_pick(&self) -> Option<std::path::PathBuf> {
        let p = self.file_sync.pick.trim();
        (!p.is_empty()).then(|| crate::mcp::project_apply::project_file(std::path::Path::new(p)).ok()).flatten()
    }

    /// Projects attached before (newest first), other than the pick: the
    /// popover offers them as suggestions, never as this place's own.
    fn file_sync_recent(&self, max: usize) -> Vec<std::path::PathBuf> {
        let pick = self.file_sync_pick();
        let mut v: Vec<std::path::PathBuf> = Vec::new();
        for r in self.file_sync.settings.remembered.iter().rev() {
            if v.len() >= max {
                break;
            }
            if Some(&r.project) != pick.as_ref() && !v.contains(&r.project) && r.project.exists() {
                v.push(r.project.clone());
            }
        }
        v
    }

    fn file_sync_do(&mut self, action: Action) {
        match action {
            Action::Attach => match self.file_sync_pick() {
                Some(p) => {
                    let initial = self.file_sync.settings.initial;
                    if let Err(e) = self.file_sync_attach(&p, initial) {
                        self.file_sync.error = Some(e.clone());
                        self.file_sync_log(LogLevel::Error, e, None);
                    }
                }
                None => {
                    self.file_sync.tab = Tab::Sync;
                    self.file_sync.open_panel = true;
                }
            },
            Action::Apply => self.file_sync_answer(true),
            Action::Retry => {
                self.file_sync.error = None;
                self.file_sync_run(super::plan::Scope::All, false, None);
            }
            Action::Detach => self.file_sync_detach(false),
            Action::ShowFolder => {
                if let Some(p) = &self.project {
                    open_in_file_manager(&p.root);
                }
            }
        }
    }

    /// The action's button; true when clicked.
    fn file_sync_action_button(&self, ui: &mut egui::Ui, action: Action) -> bool {
        let live = self.file_sync.att.as_ref().is_some_and(|a| a.live);
        match action {
            Action::Attach => {
                let blocked = self.file_sync_blocked();
                let label = match self.file_sync_pick() {
                    Some(p) => format!("Attach {}", project_label(&p)),
                    None => "Attach…".into(),
                };
                let r = ui.add_enabled(blocked.is_none(), egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(theme::ACCENT));
                r.on_disabled_hover_text(blocked.map(|b| format!("Can't attach now: {b}")).unwrap_or_default()).clicked()
            }
            Action::Apply => {
                let r = accent_button(ui, "Apply", if live { LIVE } else { theme::ACCENT });
                let r = if live { r.on_hover_text("Applies to the live Team Create place: every collaborator gets it") } else { r };
                r.clicked()
            }
            Action::Retry => ui.button("Retry").on_hover_text("Compare every file with the place again").clicked(),
            Action::Detach => ui.button("Detach").on_hover_text("Stop syncing; the place keeps what it has").clicked(),
            Action::ShowFolder => ui
                .button("Open folder")
                .on_hover_text(self.project.as_ref().map(|p| p.root.display().to_string()).unwrap_or_default())
                .clicked(),
        }
    }

    // ------------------------------------------------------------ top bar

    /// The top bar's File Sync button (drawn right to left: it lands left
    /// of the MCP button). A click opens the popover.
    pub fn file_sync_top_button(&mut self, ui: &mut egui::Ui) {
        let (col, text) = self.file_sync_status_text();
        let waiting = self.file_sync.pending.as_ref().map(|p| p.lines.len()).unwrap_or(0);
        let label = if waiting > 0 { format!("File Sync {waiting}") } else { "File Sync".into() };
        let galley = ui.painter().layout_no_wrap(label, FontId::proportional(13.0), theme::TEXT);
        let size = Vec2::new(galley.size().x + 26.0, 24.0);
        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
        let bg = if self.file_sync.popover { theme::ACTIVE_TAB } else if resp.hovered() { theme::HOVER_BG } else { Color32::TRANSPARENT };
        ui.painter().rect_filled(r, 4.0, bg);
        ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, theme::DIVIDER), egui::StrokeKind::Inside);
        ui.painter().circle_filled(egui::pos2(r.min.x + 10.0, r.center().y), 4.0, col);
        ui.painter().galley(egui::pos2(r.min.x + 18.0, r.center().y - galley.size().y / 2.0), galley, theme::TEXT);
        self.ui_rects.insert("filesync.button", r);
        let resp = if self.file_sync.popover { resp } else { resp.on_hover_text(format!("File Sync: {text}")) };
        if resp.clicked() {
            self.file_sync.popover = !self.file_sync.popover;
        }
    }

    /// The popover under the top-bar button.
    pub fn file_sync_popover_ui(&mut self, ctx: &egui::Context) {
        self.file_sync_never_confirm_ui(ctx);
        if !self.file_sync.popover {
            return;
        }
        let Some(button) = self.ui_rects.get("filesync.button").copied() else { return };
        let status = self.file_sync_ui_status();
        let word = self.file_sync_word();
        let action = self.file_sync_action();
        let mut run = None;
        let mut skip = false;
        let mut open = false;
        let mut attach_recent: Option<std::path::PathBuf> = None;
        let mut new_project = false;
        let recent = if action == Action::Attach && self.file_sync_pick().is_none() { self.file_sync_recent(3) } else { Vec::new() };
        let blocked = self.file_sync_blocked();
        let area = egui::Area::new(egui::Id::new("filesync_popover"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(Pos2::new(button.max.x, button.max.y + 4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(270.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, status_color(status));
                        ui.label(RichText::new(&word).strong());
                        if let Some(a) = &self.file_sync.att {
                            ui.label(RichText::new(&a.st.name).color(theme::TEXT_DIM)).on_hover_text(self.file_sync_details());
                        } else if let Some(p) = &self.project {
                            ui.label(RichText::new(&p.name).color(theme::TEXT_DIM)).on_hover_text(p.file.display().to_string());
                        }
                    });
                    if action == Action::ShowFolder {
                        ui.add(
                            egui::Label::new(
                                RichText::new("This place was opened from its project folder, so the files are the truth: edits sync both ways and Ctrl+S writes them. File Sync isn't needed.")
                                    .size(12.0)
                                    .color(theme::TEXT_DIM),
                            )
                            .wrap(),
                        );
                    }
                    if let (None, Some(e)) = (&self.file_sync.att, &self.file_sync.error) {
                        ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme::ERROR)).wrap());
                    }
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if self.file_sync_action_button(ui, action) {
                            run = Some(action);
                        }
                        if action == Action::Apply && ui.button("Skip").clicked() {
                            skip = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Open panel").clicked() {
                                open = true;
                            }
                        });
                    });
                    if action == Action::Attach {
                        if !recent.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new("Recent projects").size(11.5).color(theme::TEXT_DIM));
                            for p in &recent {
                                let name = project_label(p);
                                let r = ui.add_enabled(
                                    blocked.is_none(),
                                    egui::Button::new(RichText::new(format!("{name}  ·  recent")).size(12.5)).frame(false),
                                );
                                if r.on_hover_text(format!("Attach {} to this place", p.display())).clicked() {
                                    attach_recent = Some(p.clone());
                                }
                            }
                        }
                        ui.add_space(4.0);
                        let r = ui.add_enabled(blocked.is_none(), egui::Button::new("New project…"));
                        if r.on_hover_text("Write this place out as a new project folder and attach it").clicked() {
                            new_project = true;
                        }
                    }
                });
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !button.contains(p)));
        let closed = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(a) = run {
            self.file_sync_do(a);
            self.file_sync.popover = false;
        }
        if let Some(p) = attach_recent {
            self.file_sync.pick = p.display().to_string();
            self.file_sync_do(Action::Attach);
            self.file_sync.popover = false;
        }
        if new_project {
            self.open_export_dialog_attach();
            self.file_sync.popover = false;
        }
        if skip {
            self.file_sync_answer(false);
            self.file_sync.popover = false;
        }
        if open {
            self.file_sync.open_panel = true;
            self.file_sync.popover = false;
        }
        if clicked_outside || closed {
            self.file_sync.popover = false;
        }
    }

    /// The one confirmation for Display Prompts = Never in a live place.
    fn file_sync_never_confirm_ui(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.file_sync.never_confirm else { return };
        let previous = c.previous;
        let deletes_ask = self.file_sync.settings.ask_deletes.asks(true);
        let mut answer = None;
        egui::Modal::new(egui::Id::new("filesync_never_confirm")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Live place: changes will be sent to the server without asking. Continue?").strong());
            if deletes_ask {
                ui.add_space(2.0);
                ui.label(RichText::new("Deletes still ask first (Safety › Ask Before Deletes).").size(12.0).color(theme::TEXT_DIM));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                    if accent_button(ui, "Continue", theme::ACCENT).clicked() {
                        answer = Some(true);
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
        let Some(yes) = answer else { return };
        self.file_sync.never_confirm = None;
        if yes {
            if let Some(file) = self.file_sync.att.as_ref().map(|a| a.st.file.clone()) {
                if !self.file_sync.settings.live_never_ok.contains(&file) {
                    self.file_sync.settings.live_never_ok.push(file);
                }
            }
            self.file_sync.settings.prompts = Prompts::Never;
            self.file_sync.save_settings();
            self.file_sync_log(LogLevel::Info, "Display Prompts is Never for this project in live places: changes go to the server without asking", None);
            // A batch that waited only for this goes now.
            if previous.is_none() && self.file_sync.pending.as_ref().is_some_and(|p| p.reason == Reason::LiveNever) {
                self.file_sync_answer(true);
            }
        }
        // Cancel: a setting change goes back to `previous` (it was never
        // stored); a batch that asked stays waiting in the panel.
    }

    // ------------------------------------------------------------ the panel

    pub fn file_sync_panel_ui(&mut self, ui: &mut egui::Ui) {
        self.ui_rects.insert("filesync.panel", ui.max_rect());
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        self.file_sync_status_row(ui);
        self.file_sync_tab_strip(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("filesync_scroll").stick_to_bottom(self.file_sync.tab == Tab::Log).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            match self.file_sync.tab {
                Tab::Sync => self.file_sync_changes_ui(ui),
                Tab::Mapping => self.file_sync_mapping_ui(ui),
                Tab::Settings => self.file_sync_settings_ui(ui),
                Tab::Log => self.file_sync_log_ui(ui),
            }
        });
    }

    fn file_sync_status_row(&mut self, ui: &mut egui::Ui) {
        let status = self.file_sync_ui_status();
        let word = self.file_sync_word();
        let action = self.file_sync_action();
        let w = ui.available_width();
        let mut run = None;
        let mut skip = false;
        let mut menu = None;
        // The actions first (right to left), so the name truncates into
        // whatever room is left.
        ui.allocate_ui_with_layout(Vec2::new(w, 28.0), Layout::right_to_left(Align::Center), |ui| {
            ui.set_min_size(Vec2::new(w, 28.0));
            {
                ui.add_space(4.0);
                if self.file_sync.att.is_some() {
                    let more = ui.add(egui::Button::new("⋯").min_size(Vec2::new(24.0, 20.0)));
                    egui::Popup::menu(&more).show(|ui| {
                        if ui.button("Check for differences").on_hover_text("Compare every file with the place; differences wait here").clicked() {
                            menu = Some(0);
                        }
                        if ui.button("Open project folder").clicked() {
                            menu = Some(1);
                        }
                        ui.separator();
                        if ui.button("Detach").clicked() {
                            menu = Some(2);
                        }
                        if ui.button("Detach and forget").on_hover_text("Detach, and don't attach this project again when the place opens").clicked() {
                            menu = Some(3);
                        }
                    });
                }
                if action == Action::Apply && ui.button("Skip").on_hover_text("Keep the place's version; these files sync again when they next change").clicked() {
                    skip = true;
                }
                if action != Action::Detach && self.file_sync_action_button(ui, action) {
                    run = Some(action);
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, status_color(status));
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(&word).size(13.0).color(if status == Status::Off { theme::TEXT } else { status_color(status) })).truncate());
                if let Some(a) = &self.file_sync.att {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(&a.st.name).size(13.0).color(theme::TEXT_DIM)).truncate()).on_hover_text(self.file_sync_details());
                } else if let Some(p) = &self.project {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(RichText::new(&p.name).size(13.0).color(theme::TEXT_DIM)).truncate()).on_hover_text(p.file.display().to_string());
                }
            });
        });
        // A failed attach: the reason, once, in the panel's own text.
        if let (None, Some(e)) = (&self.file_sync.att, &self.file_sync.error) {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme::ERROR)).wrap());
            });
            ui.add_space(4.0);
        }
        if let Some(a) = run {
            self.file_sync_do(a);
        }
        if skip {
            self.file_sync_answer(false);
        }
        match menu {
            Some(0) => {
                self.file_sync.pending = None;
                self.file_sync_run(super::plan::Scope::All, false, Some(Reason::Always));
                if self.file_sync.pending.is_none() {
                    self.file_sync_log(LogLevel::Info, "the place matches the files", None);
                }
                self.file_sync.tab = Tab::Sync;
            }
            Some(1) => {
                if let Some(dir) = self.file_sync.att.as_ref().map(|a| a.st.root.clone()) {
                    open_in_file_manager(&dir);
                }
            }
            Some(2) => self.file_sync_detach(false),
            Some(3) => self.file_sync_detach(true),
            _ => {}
        }
    }

    fn file_sync_tab_strip(&mut self, ui: &mut egui::Ui) {
        let fs = &self.file_sync;
        let tabs = [
            (Tab::Sync, if fs.att.is_none() { "Attach".to_string() } else { "Changes".to_string() }),
            (Tab::Mapping, "Mapping".to_string()),
            (Tab::Settings, "Settings".to_string()),
            (Tab::Log, "Log".to_string()),
        ];
        let mut tab = fs.tab;
        let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
        // The labels fit the panel: the gaps shrink first, then the type.
        let width = |px: f32| tabs.iter().map(|(_, l)| ui.painter().layout_no_wrap(l.clone(), FontId::proportional(px), theme::TEXT).size().x + 8.0).sum::<f32>();
        let (mut px, mut gap) = (12.5, 12.0);
        let room = row.width() - 8.0;
        if width(px) + gap * 3.0 > room {
            gap = ((room - width(px)) / 3.0).max(2.0);
            if width(px) + gap * 3.0 > room {
                px = 11.0;
            }
        }
        let mut x = row.min.x + 8.0;
        for (t, label) in tabs {
            let on = tab == t;
            let galley = ui.painter().layout_no_wrap(label, FontId::proportional(px), theme::TEXT);
            let r = Rect::from_min_size(Pos2::new(x - 4.0, row.min.y), Vec2::new(galley.size().x + 8.0, row.height()));
            let resp = ui.interact(r, egui::Id::new(("filesync_tab", t as u8)), Sense::click());
            let c = if on || resp.hovered() { theme::TEXT } else { theme::TEXT_DIM };
            ui.painter().galley(Pos2::new(x, row.center().y - galley.size().y / 2.0), galley, c);
            if on {
                ui.painter().line_segment([Pos2::new(r.min.x, row.max.y - 1.0), Pos2::new(r.max.x, row.max.y - 1.0)], Stroke::new(2.0, theme::SELECT));
            }
            if resp.clicked() {
                tab = t;
            }
            x = r.max.x + gap;
        }
        ui.painter().hline(row.x_range(), row.max.y, Stroke::new(1.0, BAND));
        self.file_sync.tab = tab;
    }

    // ------------------------------------------------------------ changes

    fn file_sync_changes_ui(&mut self, ui: &mut egui::Ui) {
        if self.file_sync.att.is_none() {
            if self.project.is_some() {
                ui.add(
                    egui::Label::new(
                        RichText::new("This place was opened from its project folder: the files are the truth, edits sync both ways and Ctrl+S writes them. Nothing to attach.")
                            .size(12.5)
                            .color(theme::TEXT_DIM),
                    )
                    .wrap(),
                );
                return;
            }
            self.file_sync_attach_form(ui);
            return;
        }
        self.file_sync_conflict_rows(ui);
        let Some(p) = &self.file_sync.pending else { return };
        let lines: Vec<Line> = p.lines.iter().map(|l| Line { kind: l.kind, path: l.path.clone(), class: l.class.clone(), prop: l.prop.clone(), file: l.file.clone(), old: l.old.clone(), new: l.new.clone(), instances: l.instances }).collect();
        let mut toggle = None;
        let mut select = None;
        let mut budget = self.file_sync.settings.diff_lines;
        for (i, l) in lines.iter().enumerate() {
            let script = l.prop.as_deref() == Some("Source");
            let open = script && (self.file_sync.unfolded.contains(&l.path) || lines.len() == 1);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
            let painter = ui.painter_at(rect);
            if resp.hovered() {
                painter.rect_filled(rect, 0.0, theme::HOVER_BG);
            }
            let x0 = rect.min.x + 4.0;
            if script {
                let tri = Rect::from_center_size(Pos2::new(x0 + 6.0, rect.center().y), Vec2::splat(12.0));
                rustudio_engine::ti::paint(&painter, tri, if open { "caret-down-filled" } else { "caret-right-filled" }, theme::TEXT_DIM);
            }
            let (ch, col) = letter(l.kind);
            painter.text(Pos2::new(x0 + 19.0, rect.center().y), egui::Align2::CENTER_CENTER, ch, FontId::new(11.5, egui::FontFamily::Name("mono-bold".into())), col);
            crate::icons::paint_class_icon(&painter, Rect::from_center_size(Pos2::new(x0 + 36.0, rect.center().y), Vec2::splat(16.0)), &l.class);
            let mut name = leaf(&l.path).to_string();
            if let Some(prop) = l.prop.as_ref().filter(|p| *p != "Source") {
                name += &format!(".{prop}");
            }
            let name_end = painter.text(Pos2::new(x0 + 48.0, rect.center().y), egui::Align2::LEFT_CENTER, &name, FontId::proportional(13.0), if l.kind == "delete unknown" || l.kind == "delete" { theme::TEXT_DIM } else { theme::TEXT }).max.x;
            let mut right = rect.max.x - 6.0;
            if let (true, Some(old), Some(new)) = (script, &l.old, &l.new) {
                let (plus, minus) = diff::stat(&diff::diff(old, new, 0));
                let r = painter.text(Pos2::new(right, rect.center().y), egui::Align2::RIGHT_CENTER, format!("−{minus}"), FontId::proportional(11.5), DEL);
                right = r.min.x - 4.0;
                painter.text(Pos2::new(right, rect.center().y), egui::Align2::RIGHT_CENTER, format!("+{plus}"), FontId::proportional(11.5), ADD);
            } else if l.instances > 1 {
                painter.text(Pos2::new(name_end + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, format!("{} inside", l.instances - 1), FontId::proportional(11.5), theme::TEXT_DIM);
            }
            let file = l.file.as_ref().and_then(|f| self.file_sync.att.as_ref().map(|a| a.rel(f)));
            let what = match l.kind {
                "create" => "added",
                "set" => "changed",
                "replace" => "replaced",
                "delete unknown" => "deleted: no file describes it",
                _ => "deleted: its file is gone",
            };
            let tip = format!("{} ({}) {what}{}", l.path, l.class, file.map(|f| format!("\n{f}")).unwrap_or_default());
            let resp = resp.on_hover_text(tip);
            if resp.clicked() {
                if script {
                    toggle = Some(l.path.clone());
                } else {
                    select = Some(l.path.clone());
                }
            }
            if open {
                let (Some(old), Some(new)) = (&l.old, &l.new) else { continue };
                for r in diff::diff(old, new, 3) {
                    if budget == 0 {
                        break;
                    }
                    budget -= 1;
                    let (t, fg, bg) = match r {
                        Row::Same(t) => (format!("  {t}"), theme::TEXT_DIM, Color32::TRANSPARENT),
                        Row::Add(t) => (format!("+ {t}"), ADD, ADD_BG),
                        Row::Del(t) => (format!("- {t}"), DEL, DEL_BG),
                        Row::Fold(k) => (format!("  ⋯ {k} unchanged"), theme::TEXT_DIM, Color32::TRANSPARENT),
                    };
                    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), Sense::hover());
                    let painter = ui.painter_at(r);
                    painter.rect_filled(Rect::from_min_max(Pos2::new(x0 + 40.0, r.min.y), r.max), 0.0, bg);
                    painter.text(Pos2::new(x0 + 44.0, r.center().y), egui::Align2::LEFT_CENTER, t.replace('\t', "    "), FontId::monospace(11.5), fg);
                }
            }
            let _ = i;
        }
        if let Some(p) = toggle {
            if !self.file_sync.unfolded.remove(&p) {
                self.file_sync.unfolded.insert(p);
            }
        }
        if let Some(p) = select {
            if let Ok(r) = crate::mcp::tree::resolve(&self.game.doc, &p) {
                self.select(vec![r]);
            }
        }
    }

    fn file_sync_conflict_rows(&mut self, ui: &mut egui::Ui) {
        let conflicts: Vec<(rbx_dom_weak::types::Ref, String)> = self.file_sync.commits.iter().filter(|c| c.conflict).map(|c| (c.r, c.path.clone())).collect();
        let mut action = None;
        for (r, path) in conflicts {
            let w = ui.available_width();
            ui.allocate_ui_with_layout(Vec2::new(w, ROW_H), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(Vec2::new(w, ROW_H));
                ui.add_space(14.0);
                ui.label(RichText::new("!").font(FontId::new(11.5, egui::FontFamily::Name("mono-bold".into()))).color(theme::ERROR));
                ui.add_space(6.0);
                let class = self.game.doc.class_of(r).map(|c| c.to_string()).unwrap_or_default();
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                crate::icons::paint_class_icon(ui.painter(), ir, &class);
                ui.add_space(4.0);
                ui.label(RichText::new(leaf(&path)).size(13.0)).on_hover_text(format!("{path}\nChanged on the server since you had it: the file's version was not committed"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(4.0);
                    if ui.small_button("Keep server's").clicked() {
                        action = Some((r, false));
                    }
                    if ui.small_button("Use file's").clicked() {
                        action = Some((r, true));
                    }
                });
            });
        }
        match action {
            Some((r, true)) => self.file_sync_replace_conflict(r),
            Some((r, false)) => self.file_sync_keep_server(r),
            None => {}
        }
    }

    /// Not attached: the project, how it attaches, whether it comes back.
    fn file_sync_attach_form(&mut self, ui: &mut egui::Ui) {
        let mut browse = false;
        let mut picked = None;
        let recent: Vec<String> = {
            let mut v: Vec<String> = Vec::new();
            for r in self.file_sync.settings.remembered.iter().rev() {
                let p = r.project.display().to_string();
                if !v.contains(&p) {
                    v.push(p);
                }
            }
            v
        };
        let ok = self.file_sync.pick.trim().is_empty() || self.file_sync_pick().is_some();
        prop_row(ui, "Project", "A *.project.json, or a folder with default.project.json", |ui, v| {
            let field_w = v.width() - 26.0;
            let mut field = egui::TextEdit::singleline(&mut self.file_sync.pick).hint_text("default.project.json").desired_width(field_w).font(FontId::proportional(FONT));
            if !ok {
                field = field.text_color(theme::WARN);
            }
            ui.add(field);
            let more = ui.add(egui::Button::new("⋯").min_size(Vec2::new(22.0, 18.0)));
            egui::Popup::menu(&more).show(|ui| {
                if ui.button("Browse…").clicked() {
                    browse = true;
                }
                if !recent.is_empty() {
                    ui.separator();
                }
                for p in &recent {
                    if ui.button(p).clicked() {
                        picked = Some(p.clone());
                    }
                }
            });
        });
        let mut initial = self.file_sync.settings.initial;
        prop_row(ui, "On Attach", initial.hint(), |ui, v| {
            egui::ComboBox::from_id_salt("fs_attach_initial").width(v.width() - 4.0).selected_text(initial.label()).show_ui(ui, |ui| {
                for i in Initial::ALL {
                    ui.selectable_value(&mut initial, i, i.label()).on_hover_text(i.hint());
                }
            });
        });
        let has_key = self.file_sync_place_key().is_some();
        let mut remember = self.file_sync.remember;
        prop_row(ui, "Re-attach", "Attach this project again when this place opens", |ui, _| {
            ui.add_enabled(has_key, egui::Checkbox::without_text(&mut remember));
        });
        self.file_sync.remember = remember;
        if initial != self.file_sync.settings.initial {
            self.file_sync.settings.initial = initial;
            self.file_sync.save_settings();
        }
        if let Some(p) = picked {
            self.file_sync.pick = p;
        }
        if browse {
            use crate::file_dialog::{Kind, Purpose};
            let mut d = crate::file_dialog::AsyncFileDialog::new().add_filter("Rojo / Argon project", &["json"]);
            if let Some(dir) = std::path::Path::new(&self.file_sync.pick).parent().filter(|d| d.is_dir()) {
                d = d.set_directory(dir);
            }
            self.dialogs.start(Purpose::FileSyncProject, d, Kind::PickFile);
        }
    }

    // ------------------------------------------------------------ mapping

    fn file_sync_mapping_ui(&mut self, ui: &mut egui::Ui) {
        let Some(a) = self.file_sync.att.as_ref() else { return };
        let mut groups: Vec<(String, Vec<&MapRow>)> = Vec::new();
        for m in &a.mappings {
            let top = m.place.split('.').next().unwrap_or("").to_string();
            match groups.iter_mut().find(|(g, _)| *g == top) {
                Some((_, v)) => v.push(m),
                None => groups.push((top, vec![m])),
            }
        }
        for (service, rows) in groups {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
            let painter = ui.painter_at(rect);
            let x0 = rect.min.x + 4.0;
            rustudio_engine::ti::paint(&painter, Rect::from_center_size(Pos2::new(x0 + 6.0, rect.center().y), Vec2::splat(12.0)), "caret-down-filled", theme::TEXT_DIM);
            crate::icons::paint_class_icon(&painter, Rect::from_center_size(Pos2::new(x0 + 22.0, rect.center().y), Vec2::splat(16.0)), &service);
            painter.text(Pos2::new(x0 + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, &service, FontId::proportional(13.0), theme::TEXT);
            for m in rows {
                let rest = m.place.strip_prefix(&service).unwrap_or(&m.place).trim_start_matches('.');
                let depth = 1 + rest.matches('.').count();
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
                let painter = ui.painter_at(rect);
                if resp.hovered() {
                    painter.rect_filled(rect, 0.0, theme::HOVER_BG);
                }
                let x0 = rect.min.x + 4.0 + depth as f32 * INDENT;
                crate::icons::paint_class_icon(&painter, Rect::from_center_size(Pos2::new(x0 + 22.0, rect.center().y), Vec2::splat(16.0)), &m.class);
                let name_end = painter.text(Pos2::new(x0 + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, leaf(rest), FontId::proportional(13.0), if m.has { theme::TEXT } else { theme::TEXT_DIM }).max.x;
                let dot = Pos2::new(rect.max.x - 10.0, rect.center().y);
                painter.circle_filled(dot, 3.5, if m.has { GREEN } else { theme::WARN });
                let disk = painter.layout_no_wrap(m.disk.clone(), FontId::proportional(12.0), theme::TEXT_DIM);
                let room = dot.x - 10.0 - (name_end + 12.0);
                if room > 30.0 {
                    let clip = Rect::from_min_max(Pos2::new(name_end + 12.0, rect.min.y), Pos2::new(dot.x - 10.0, rect.max.y));
                    let x = (clip.max.x - disk.size().x).max(clip.min.x);
                    ui.painter_at(clip).galley(Pos2::new(x, rect.center().y - disk.size().y / 2.0), disk, theme::TEXT_DIM);
                }
                resp.on_hover_text(format!(
                    "{} ← {}\n{} file{}{}",
                    m.place,
                    m.disk,
                    m.files,
                    plural(m.files),
                    if m.has { "" } else { "\nNot in the place yet: a sync creates it" }
                ));
            }
        }
    }

    // ------------------------------------------------------------ settings

    fn file_sync_settings_ui(&mut self, ui: &mut egui::Ui) {
        let mut s = self.file_sync.settings.clone();
        let mut folded = std::mem::take(&mut self.file_sync.folded);
        let mut group = |ui: &mut egui::Ui, key: &'static str, name: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
            let f = folded.contains(key);
            if band(ui, name, f) && !folded.remove(key) {
                folded.insert(key);
            }
            if !f {
                body(ui);
            }
        };
        group(ui, "attach", "On Attach", &mut |ui| {
            prop_row(ui, "Initial Sync", s.initial.hint(), |ui, v| combo(ui, "fs_set_initial", v, &mut s.initial, &Initial::ALL.map(|i| (i, i.label()))));
            prop_row(ui, "Live Hydrate", "Create instances for mapped files the place doesn't have", |ui, _| check(ui, &mut s.hydrate));
        });
        group(ui, "automation", "Automation", &mut |ui| {
            prop_row(ui, "Auto Attach", "Attach the remembered project when its place opens or is joined", |ui, _| check(ui, &mut s.auto_attach));
            // Instances no file describes: kept where (one row for both flags).
            let mut keep = (s.keep_unknowns_live, s.keep_unknowns);
            prop_row(ui, "Keep Unknowns", "Instances inside mapped folders that no file describes are kept instead of deleted. An instance whose file is removed is still deleted", |ui, v| {
                combo(ui, "fs_set_unknowns", v, &mut keep, &[((true, false), "Live places"), ((true, true), "Always"), ((false, false), "Never"), ((false, true), "Local places")])
            });
            (s.keep_unknowns_live, s.keep_unknowns) = keep;
        });
        group(ui, "twoway", "Two-Way Sync", &mut |ui| {
            prop_row(ui, "Two-Way Sync", "Write edits made here back to their files", |ui, _| check(ui, &mut s.two_way));
            let on = s.two_way;
            ui.add_enabled_ui(on, |ui| {
                prop_row(ui, "Syncback Properties", "Write every property, not just script sources and names", |ui, _| check(ui, &mut s.syncback_props));
                prop_row(ui, "Only Code", "Write back only scripts and instances that hold scripts", |ui, _| check(ui, &mut s.code_only));
            });
            prop_row(ui, "Open In Editor", "Open the files' scripts in the OS default editor", |ui, _| check(ui, &mut s.open_in_editor));
        });
        group(ui, "safety", "Safety", &mut |ui| {
            prop_row(
                ui,
                "Display Prompts",
                "Whether changes from the files wait for your OK before they change the place.\nAlways: every batch.\nOver threshold: batches with more changes than Changes Threshold.\nNever: nothing waits, live places too (a live place asks you to confirm that once per project).\nDeletes follow Ask Before Deletes.",
                |ui, v| combo(ui, "fs_set_prompts", v, &mut s.prompts, &Prompts::ALL.map(|p| (p, p.label()))),
            );
            prop_row(ui, "Changes Threshold", "With Display Prompts set to Over threshold: a batch with more changes than this waits for your OK", |ui, _| {
                ui.add(egui::DragValue::new(&mut s.threshold).range(0..=10_000));
            });
            prop_row(
                ui,
                "Ask Before Deletes",
                "A batch that deletes instances waits for your OK, whatever Display Prompts says.\nAlways / only in live Team Create places / never.",
                |ui, v| combo(ui, "fs_set_deletes", v, &mut s.ask_deletes, &AskDeletes::ALL.map(|a| (a, a.label()))),
            );
            prop_row(
                ui,
                "Live Batch Size",
                "Live Team Create places: changes go to the server this many instances at a time, a quarter second apart (0: all at once). Large single sends have dropped the session",
                |ui, _| {
                    ui.add(egui::DragValue::new(&mut s.live_batch).range(0..=10_000));
                },
            );
            prop_row(ui, "Diff Lines Limit", "Most diff lines the change list draws", |ui, _| {
                ui.add(egui::DragValue::new(&mut s.diff_lines).range(10..=100_000));
            });
        });
        group(ui, "log", "Log", &mut |ui| {
            prop_row(ui, "Log Level", "What the Log and the Output record", |ui, v| combo(ui, "fs_set_log", v, &mut s.log_level, &LogLevel::ALL.map(|l| (l, l.label()))));
        });
        if !s.remembered.is_empty() {
            let mut forget = None;
            let list = s.remembered.clone();
            group(ui, "remembered", "Remembered Places", &mut |ui| {
                for r in &list {
                    let name = if r.label.is_empty() { r.place.clone() } else { r.label.clone() };
                    let project = r.project.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    prop_row(ui, &name, &format!("{}\n{}", r.place, r.project.display()), |ui, v| {
                        ui.add_sized(Vec2::new(v.width() - 26.0, 18.0), egui::Label::new(RichText::new(&project).size(FONT).color(P_TEXT)).truncate());
                        if ui.small_button("✖").on_hover_text("Forget").clicked() {
                            forget = Some(r.place.clone());
                        }
                    });
                }
            });
            if let Some(p) = forget {
                s.remember(&p, None);
            }
        }
        self.file_sync.folded = folded;
        // Never picked while attached to a live place: confirmed once per project.
        let old = self.file_sync.settings.prompts;
        if s.prompts == Prompts::Never && old != Prompts::Never && self.file_sync.att.as_ref().is_some_and(|a| a.live) && !self.file_sync_never_ok() {
            s.prompts = old;
            self.file_sync.never_confirm = Some(super::NeverConfirm { previous: Some(old) });
        }
        if s != self.file_sync.settings {
            self.file_sync.settings = s;
            self.file_sync.save_settings();
        }
    }

    // ------------------------------------------------------------ log

    fn file_sync_log_ui(&mut self, ui: &mut egui::Ui) {
        #[derive(Clone, Copy, PartialEq)]
        enum Show {
            All,
            Changes,
            Warnings,
            Errors,
        }
        const SHOW: [(Show, &str); 4] = [(Show::All, "All Messages"), (Show::Changes, "Changes"), (Show::Warnings, "Warnings"), (Show::Errors, "Errors")];
        let fid = egui::Id::new("filesync_log_show");
        let tid = egui::Id::new("filesync_log_text");
        let mut show = SHOW[ui.ctx().data(|d| d.get_temp::<usize>(fid)).unwrap_or(0) % 4].0;
        let mut text: String = ui.ctx().data(|d| d.get_temp(tid)).unwrap_or_default();
        let mut clear = false;
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            egui::ComboBox::from_id_salt("fs_log_show").width(96.0).selected_text(SHOW.iter().find(|(v, _)| *v == show).map(|(_, n)| *n).unwrap_or("")).show_ui(ui, |ui| {
                for (v, n) in SHOW {
                    ui.selectable_value(&mut show, v, n);
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (r, c) = ui.allocate_exact_size(Vec2::new(24.0, 20.0), Sense::click());
                rustudio_engine::ti::paint(ui.painter(), r.shrink(2.0), "clear-all", theme::TEXT);
                clear = c.on_hover_text("Clear the log").clicked();
                ui.add(egui::TextEdit::singleline(&mut text).hint_text("Filter...").desired_width((ui.available_width() - 4.0).clamp(60.0, 240.0)));
            });
        });
        ui.add_space(3.0);
        let i = SHOW.iter().position(|(v, _)| *v == show).unwrap_or(0);
        ui.ctx().data_mut(|d| {
            d.insert_temp(fid, i);
            d.insert_temp(tid, text.clone());
        });
        if clear {
            self.file_sync.log.clear();
        }
        let level = self.file_sync.settings.log_level;
        let needle = text.to_lowercase();
        let font = FontId::monospace(12.0);
        for e in self.file_sync.log.iter().filter(|e| e.level <= level) {
            let keep = match show {
                Show::All => true,
                Show::Changes => e.file.is_some() && e.level == LogLevel::Info,
                Show::Warnings => e.level <= LogLevel::Warn,
                Show::Errors => e.level == LogLevel::Error || e.ok == Some(false),
            };
            if !keep || !(needle.is_empty() || e.text.to_lowercase().contains(&needle) || e.file.as_ref().is_some_and(|f| f.to_lowercase().contains(&needle))) {
                continue;
            }
            let col = match (e.level, e.ok) {
                (LogLevel::Error, _) | (_, Some(false)) => crate::output_panel::ERROR,
                (LogLevel::Warn, _) => crate::output_panel::WARN,
                (LogLevel::Verbose, _) => theme::TEXT_DIM,
                _ if e.file.is_some() => crate::output_panel::PRINT,
                _ => crate::output_panel::INFO,
            };
            let mut job = egui::text::LayoutJob::default();
            let fmt = |c: Color32| egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
            job.append(&format!("{}  ", e.at), 0.0, fmt(theme::TEXT_DIM));
            job.append(&e.text, 0.0, fmt(col));
            if let Some(f) = &e.file {
                job.append(&format!("  {f}"), 0.0, fmt(theme::TEXT_DIM));
            }
            if let Some(s) = &e.status {
                job.append(&format!("  [{s}]"), 0.0, fmt(if e.ok == Some(false) { crate::output_panel::ERROR } else { theme::TEXT_DIM }));
            }
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.add(egui::Label::new(job).wrap());
            });
        }
    }
}

// ---------------------------------------------------------------- Properties-style rows

/// A folding band, as Properties draws a category; true when clicked.
fn band(ui: &mut egui::Ui, name: &str, folded: bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), CAT_H), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(r, 0.0, BAND);
    let tri = Rect::from_center_size(Pos2::new(r.min.x + 10.0, r.center().y), Vec2::splat(12.0));
    rustudio_engine::ti::paint(painter, tri, if folded { "caret-right-filled" } else { "caret-down-filled" }, P_TEXT);
    painter.text(Pos2::new(r.min.x + P_INDENT, r.center().y), egui::Align2::LEFT_CENTER, name, FontId::new(FONT, egui::FontFamily::Name("bold".into())), P_TEXT);
    resp.clicked()
}

/// A Properties row: the name left of the divider, the control right of
/// it, the description in the name's tooltip.
fn prop_row(ui: &mut egui::Ui, name: &str, hint: &str, control: impl FnOnce(&mut egui::Ui, Rect)) {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), P_ROW_H + 1.0), Sense::hover());
    let painter = ui.painter();
    painter.line_segment([Pos2::new(r.min.x, r.max.y - 0.5), Pos2::new(r.max.x, r.max.y - 0.5)], Stroke::new(1.0, BAND));
    let div = r.min.x + (r.width() * DIVIDER).round();
    painter.line_segment([Pos2::new(div, r.min.y), Pos2::new(div, r.max.y)], Stroke::new(1.0, BAND));
    let enabled = ui.is_enabled();
    let clip = Rect::from_min_max(r.min, Pos2::new(div - 2.0, r.max.y));
    ui.painter_at(clip).text(
        Pos2::new(r.min.x + P_INDENT, r.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        FontId::proportional(FONT),
        if enabled { P_TEXT } else { crate::properties::READ_ONLY },
    );
    if !hint.is_empty() {
        let name_rect = Rect::from_min_max(r.min, Pos2::new(div, r.max.y));
        if resp.hovered() && ui.rect_contains_pointer(name_rect) {
            resp.on_hover_text(hint);
        }
    }
    let value = Rect::from_min_max(Pos2::new(div + 5.0, r.min.y + 1.0), Pos2::new(r.max.x - 4.0, r.max.y - 1.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(value).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        ui.spacing_mut().interact_size.y = 18.0;
        control(ui, value);
    });
}

fn check(ui: &mut egui::Ui, v: &mut bool) {
    ui.add(egui::Checkbox::without_text(v));
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, id: &str, v: Rect, value: &mut T, options: &[(T, &str)]) {
    let label = options.iter().find(|(o, _)| o == value).map(|(_, l)| *l).unwrap_or("");
    egui::ComboBox::from_id_salt(id).width(v.width() - 4.0).selected_text(RichText::new(label).size(FONT)).show_ui(ui, |ui| {
        for (o, l) in options {
            ui.selectable_value(value, *o, *l);
        }
    });
}

/// A project's name for buttons: its folder's name.
fn project_label(p: &std::path::Path) -> String {
    p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}
