//! File > Customize Shortcuts for plugins: Studio's "Keyboard and Mouse"
//! window lists every action, plugin PluginActions and toolbar buttons
//! included, and lets a shortcut be bound to them; a bound shortcut fires
//! the action's Triggered / the button's Click.
//!
//! Measured in Studio 0.747 (a hidden Studio): the window (700x500,
//! title "Keyboard and Mouse") has a "Search Actions" field and a table
//! Name | Shortcut | Description sorted by Name, descending, case-sensitive
//! ("iii…" above "Zoom to" above "Zeta B"). A PluginAction is listed by its
//! Text with its StatusTip as description; one made with
//! allowBinding=false or with PluginMenu:AddNewAction is not listed; a
//! toolbar button by its label with its tooltip. Double-click the shortcut
//! cell, press the keys, OK: the binding is saved at once in
//! `HKCU\Software\Roblox\RobloxStudio\Roblox Studio Key Mapping` under
//! `<plugin Name>_<actionId>` (= the action's ActionId) or
//! `<plugin Name>_<toolbar>_<button id>` (every plugin entry is written,
//! "" when unbound), and pressing it fires Triggered. Defaults: no
//! shortcut. A PluginMenu shows a bound action's shortcut on the right.
//!
//! rustudio keeps the bindings in `<config>/rustudio/plugin-shortcuts.json`
//! under the same keys. rustudio's own menu commands are listed with their
//! fixed shortcuts (not rebindable here).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use egui::{Key, KeyboardShortcut, Modifiers};
use rbx_dom_weak::types::Ref;

use crate::app::Editor;
use rustudio_engine::theme;

#[derive(Default)]
pub struct Shortcuts {
    /// Studio key → shortcut text ("Ctrl+Alt+J"); loaded on first use.
    bindings: Option<BTreeMap<String, String>>,
    pub open: bool,
    search: String,
    /// The row whose shortcut cell waits for keys.
    editing: Option<String>,
    /// Edits not yet saved with OK.
    draft: Option<BTreeMap<String, String>>,
}

pub fn file() -> PathBuf {
    if let Some(p) = std::env::var_os("RUSTUDIO_PLUGIN_SHORTCUTS") {
        return PathBuf::from(p);
    }
    rustudio_core::paths::config().join("rustudio").join("plugin-shortcuts.json")
}

impl Shortcuts {
    fn map(&mut self) -> &mut BTreeMap<String, String> {
        self.bindings.get_or_insert_with(|| {
            std::fs::read_to_string(file())
                .ok()
                .and_then(|t| serde_json::from_str::<BTreeMap<String, String>>(&t).ok())
                .unwrap_or_default()
        })
    }

    /// Bound keys → shortcut text (for menus).
    pub fn labels(&mut self) -> HashMap<String, String> {
        self.map().iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// Binds (or with "" unbinds) and saves.
    pub fn set(&mut self, key: &str, shortcut: &str) -> Result<(), String> {
        self.map().insert(key.to_string(), shortcut.to_string());
        self.save()
    }

    fn save(&mut self) -> Result<(), String> {
        let map = self.map().clone();
        let path = file();
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(&map).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }
}

/// A key press as Studio writes it ("Ctrl+Alt+J", "Shift+F3").
pub fn shortcut_text(m: Modifiers, k: Key) -> String {
    let mut s = String::new();
    if m.command || m.ctrl {
        s.push_str("Ctrl+");
    }
    if m.alt {
        s.push_str("Alt+");
    }
    if m.shift {
        s.push_str("Shift+");
    }
    s.push_str(k.name());
    s
}

/// "Ctrl+Alt+J" → egui's shortcut (Studio's order; any order accepted).
pub fn parse(s: &str) -> Option<KeyboardShortcut> {
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split('+').collect();
    let mut m = Modifiers::NONE;
    for p in &parts[..parts.len() - 1] {
        match *p {
            "Ctrl" => m = m | Modifiers::COMMAND,
            "Alt" => m = m | Modifiers::ALT,
            "Shift" => m = m | Modifiers::SHIFT,
            _ => return None,
        }
    }
    Some(KeyboardShortcut::new(m, Key::from_name(parts.last()?)?))
}

/// One row of the window.
struct Row {
    key: Option<String>,
    name: String,
    shortcut: String,
    description: String,
}

impl Editor {
    /// Bound plugin shortcuts pressed this frame fire their action /
    /// button (not while a text field has the keyboard).
    pub fn plugin_shortcut_keys(&mut self, ctx: &egui::Context) {
        if self.plugin_bar.shortcuts.open || ctx.egui_wants_keyboard_input() {
            return;
        }
        let labels = self.plugin_bar.shortcuts.labels();
        if labels.is_empty() {
            return;
        }
        let mut targets: Vec<(Ref, &'static str, String)> = Vec::new();
        for a in &self.plugin_bar.model.actions {
            if let Some(s) = labels.get(&a.key) {
                targets.push((a.r, "Triggered", s.clone()));
            }
        }
        for g in &self.plugin_bar.model.groups {
            if self.is_hidden_from_plugins_tab(g) {
                continue;
            }
            for b in &g.buttons {
                if let Some(s) = labels.get(&b.key) {
                    targets.push((b.r, "Click", s.clone()));
                }
            }
        }
        for (r, ev, s) in targets {
            if let Some(ks) = parse(&s) {
                if ctx.input_mut(|i| i.consume_shortcut(&ks)) {
                    self.plugin_fire(r, ev);
                }
            }
        }
    }

    fn shortcut_rows(&mut self, draft: &BTreeMap<String, String>) -> Vec<Row> {
        let mut rows = Vec::new();
        fn walk(items: &'static [crate::studio_ui::M], rows: &mut Vec<Row>) {
            for m in items {
                match m {
                    crate::studio_ui::M::I(label, sc, _) => rows.push(Row {
                        key: None,
                        name: label.to_string(),
                        shortcut: sc.to_string(),
                        description: label.to_string(),
                    }),
                    crate::studio_ui::M::Sub(_, items) => walk(items, rows),
                    crate::studio_ui::M::S => {}
                }
            }
        }
        for (_, items) in crate::studio_ui::MENUS {
            walk(items, &mut rows);
        }
        for a in &self.plugin_bar.model.actions {
            rows.push(Row {
                key: Some(a.key.clone()),
                name: a.text.clone(),
                shortcut: draft.get(&a.key).cloned().unwrap_or_default(),
                description: a.status_tip.clone(),
            });
        }
        for g in &self.plugin_bar.model.groups {
            if self.is_hidden_from_plugins_tab(g) {
                continue;
            }
            for b in &g.buttons {
                rows.push(Row {
                    key: Some(b.key.clone()),
                    name: b.label.clone(),
                    shortcut: draft.get(&b.key).cloned().unwrap_or_default(),
                    description: b.tooltip.clone(),
                });
            }
        }
        rows.sort_by(|a, b| b.name.cmp(&a.name));
        rows
    }

    /// The "Keyboard and Mouse" window (File > Customize Shortcuts).
    pub fn customize_shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.plugin_bar.shortcuts.open {
            return;
        }
        if self.plugin_bar.shortcuts.draft.is_none() {
            let m = self.plugin_bar.shortcuts.map().clone();
            self.plugin_bar.shortcuts.draft = Some(m);
        }
        let mut draft = self.plugin_bar.shortcuts.draft.clone().unwrap_or_default();
        // Keys for the row being edited.
        if let Some(k) = self.plugin_bar.shortcuts.editing.clone() {
            let pressed = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, m)) = pressed {
                match key {
                    Key::Escape => {}
                    Key::Backspace | Key::Delete if !m.any() => {
                        draft.insert(k.clone(), String::new());
                    }
                    _ => {
                        draft.insert(k.clone(), shortcut_text(m, key));
                    }
                }
                self.plugin_bar.shortcuts.editing = None;
            }
        }
        let rows = self.shortcut_rows(&draft);
        let mut open = true;
        let mut ok = false;
        let mut cancel = false;
        let mut restore = false;
        egui::Window::new("Keyboard and Mouse")
            .id(egui::Id::new("customize_shortcuts"))
            .open(&mut open)
            .default_size([700.0, 500.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Keyboard").color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut self.plugin_bar.shortcuts.search).hint_text("Search Actions").desired_width(f32::INFINITY));
                let q = self.plugin_bar.shortcuts.search.to_lowercase();
                egui::ScrollArea::vertical().max_height(330.0).show(ui, |ui| {
                    egui::Grid::new("shortcut_rows").num_columns(3).striped(true).min_col_width(180.0).show(ui, |ui| {
                        ui.strong("Name ▼");
                        ui.strong("Shortcut");
                        ui.strong("Description");
                        ui.end_row();
                        for r in rows.iter().filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q)) {
                            ui.label(&r.name);
                            match &r.key {
                                Some(k) => {
                                    let editing = self.plugin_bar.shortcuts.editing.as_deref() == Some(k.as_str());
                                    let text = if editing { "Press keys…".to_string() } else { r.shortcut.clone() };
                                    let resp = ui.add(egui::Button::new(text).min_size(egui::vec2(170.0, 0.0)).selected(editing));
                                    if resp.double_clicked() || resp.clicked() {
                                        self.plugin_bar.shortcuts.editing = Some(k.clone());
                                    }
                                }
                                None => {
                                    ui.label(egui::RichText::new(&r.shortcut).color(theme::TEXT_DIM));
                                }
                            }
                            ui.label(&r.description);
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Restore Defaults").clicked() {
                        restore = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("OK").clicked() {
                            ok = true;
                        }
                    });
                });
            });
        if restore {
            for v in draft.values_mut() {
                v.clear();
            }
        }
        self.plugin_bar.shortcuts.draft = Some(draft.clone());
        if ok {
            *self.plugin_bar.shortcuts.map() = draft;
            if let Err(e) = self.plugin_bar.shortcuts.save() {
                self.warn(format!("Shortcuts could not be saved: {e}"));
            }
        }
        if ok || cancel || !open {
            self.plugin_bar.shortcuts.open = false;
            self.plugin_bar.shortcuts.draft = None;
            self.plugin_bar.shortcuts.editing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_round_trip() {
        let m = Modifiers { ctrl: true, alt: true, ..Modifiers::NONE };
        let t = shortcut_text(m, Key::J);
        assert_eq!(t, "Ctrl+Alt+J");
        let k = parse(&t).unwrap();
        assert_eq!(k.logical_key, Key::J);
        assert!(k.modifiers.alt && k.modifiers.command);
        assert!(parse("").is_none());
    }
}
