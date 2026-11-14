//! Studio's viewport menu. Commands share the ribbon's availability and dispatch.
use crate::app::Editor;
use egui::{PointerButton, Response, Ui};

impl Editor {
    pub(crate) fn viewport_context_menu(&mut self, ui: &mut Ui, response: &Response) {
        if self.is_playing() { return; }
        if response.clicked_by(PointerButton::Secondary) {
            if let Some(p) = response.interact_pointer_pos() {
                let hit = self.pick(response.rect, p, ui.input(|i| i.modifiers.alt));
                match hit {
                    Some(r) if !self.doc.selection.contains(&r) => self.select(vec![r]),
                    None => self.select(Vec::new()),
                    _ => {}
                }
            }
        }
        let mut action = None;
        response.context_menu(|ui| {
            ui.set_min_width(254.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut item = |ui: &mut Ui, label: &str, shortcut: &str, command: &'static str| {
                let enabled = match command {
                    "parents" => self.doc.selection.iter().any(|r| self.doc.dom.get_by_ref(*r).is_some_and(|i| i.parent().is_some())),
                    "children" => self.doc.selection.iter().any(|r| self.doc.dom.get_by_ref(*r).is_some_and(|i| !i.children().is_empty())),
                    "focus" => self.selection_bounds().is_some(),
                    _ => self.action_state(command).0,
                };
                // Checkable items (Align Dragged Objects) show their tick.
                let checked = if command == "align_dragged" { self.action_state(command).1 } else { None };
                if crate::studio_actions::menu_item_at(ui, label, shortcut, enabled, checked, 182.0, 254.0).clicked() {
                    action = Some(command);
                    ui.close();
                }
            };
            item(ui,"Cut","Ctrl+X","cut");
            item(ui,"Copy","Ctrl+C","copy");
            ui.add_enabled_ui(false, |ui| { ui.menu_button("Copy As", |_| {}); });
            item(ui,"Paste","Ctrl+V","paste");
            item(ui,"Paste Into","Ctrl+Shift+V","paste_into");
            item(ui,"Paste at Original Location","","paste_original");
            item(ui,"Paste into at Original Location","","paste_into_original");
            item(ui,"Duplicate","Ctrl+D","duplicate");
            item(ui,"Delete","Del","delete");
            ui.separator();
            item(ui,"Group as Model","Ctrl+G","group");
            item(ui,"Group as Folder","Ctrl+Alt+G","group_folder");
            item(ui,"Ungroup","Ctrl+U","ungroup");
            item(ui,"Union","Ctrl+Shift+G","union");
            item(ui,"Intersect","Ctrl+Shift+I","intersect");
            item(ui,"Negate","Ctrl+Shift+N","negate");
            item(ui,"Separate","Ctrl+Shift+U","separate");
            ui.add_enabled_ui(false, |ui| { ui.menu_button("Generate", |_| {}); });
            item(ui,"Select Parent(s)","","parents");
            item(ui,"Select Children","","children");
            item(ui,"Zoom to","F","focus");
            item(ui,"Show Orientation Indicator","","orientation_indicator");
            item(ui,"Align Dragged Objects","","align_dragged");
            ui.separator();
            item(ui,"Select Connections","Alt+C","connections");
            item(ui,"Select Connection 0","Alt+1","connection_0");
            item(ui,"Select Connection 1","Alt+2","connection_1");
            item(ui,"Swap Attachments","","swap_attachments");
            ui.separator();
            item(ui,"Insert Part","","part");
            item(ui,"Insert Object…","Ctrl+I","insert_object");
            ui.separator();
            for label in ["Convert to Package", "Publish to Package", "Undo Changes to Package", "Get Latest Package", "View Script Changes…", "Package Details", "Update All", "Compare Package Versions"] {
                item(ui,label,"","unsupported_package");
            }
            ui.separator();
            item(ui,"Help","","documentation");
        });
        if let Some(action) = action {
            match action {
                "parents" | "children" => {
                    let mut refs = Vec::new();
                    for r in &self.doc.selection {
                        if let Some(i) = self.doc.dom.get_by_ref(*r) {
                            if action == "parents" {
                                if i.parent().is_some() && !refs.contains(&i.parent()) { refs.push(i.parent()); }
                            } else {
                                for r in i.children() { if !refs.contains(r) { refs.push(*r); } }
                            }
                        }
                    }
                    self.select(refs);
                }
                "focus" => self.focus_selection(),
                _ => self.ui_action(ui.ctx(), action),
            }
        }
    }
}
