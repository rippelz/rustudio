//! What each Studio menu and ribbon item does in rustudio, and whether it
//! is available (src/studio_ui.rs lists them). Anything rustudio can't do
//! yet is disabled rather than faked.


use egui::{Rect, RichText, Sense, Vec2};
use rbx_dom_weak::types::{Ref, Variant};

use crate::app::{Editor, PendingAction, Tab, Tool};
use rustudio_render::render::MeshKind;
use crate::studio_ui::{menu_rect_key, menu_shortcuts, RunType, MENUS, M};
use rustudio_engine::theme;

/// Studio's File menu is 233 px wide (checks/ui/studio/menu-File.png);
/// the account menu uses this width.
pub const MENU_W: f32 = 228.0;

/// One menu item as Studio draws it: a check column, the label, and the
/// shortcut left-aligned in a column after the menu's longest label.
pub fn menu_item(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool, checked: Option<bool>) -> egui::Response {
    menu_item_at(ui, label, shortcut, enabled, checked, MENU_W - 70.0, MENU_W)
}

pub(crate) fn menu_item_at(ui: &mut egui::Ui, label: &str, shortcut: &str, enabled: bool, checked: Option<bool>, col: f32, w: f32) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 22.0), sense);
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(r, 2.0, theme::HOVER_BG);
    }
    let c = if enabled { theme::TEXT } else { theme::TEXT_DIM };
    if checked == Some(true) {
        let cr = Rect::from_center_size(egui::pos2(r.min.x + 11.0, r.center().y), Vec2::splat(14.0));
        rustudio_engine::ti::paint(p, cr, "check", c);
    }
    let font = egui::FontId::proportional(13.0);
    p.text(egui::pos2(r.min.x + 24.0, r.center().y), egui::Align2::LEFT_CENTER, label, font.clone(), c);
    if !shortcut.is_empty() {
        let shortcut = crate::studio_ui::shortcut_label(shortcut);
        p.text(egui::pos2(r.min.x + col, r.center().y), egui::Align2::LEFT_CENTER, shortcut, font, c);
    }
    resp
}

/// A menu's shortcut column and width, from its longest label and shortcut.
fn menu_metrics(ui: &egui::Ui, items: &[M]) -> (f32, f32) {
    let font = egui::FontId::proportional(13.0);
    let width = |t: &str| ui.fonts_mut(|f| f.layout_no_wrap(t.to_string(), font.clone(), theme::TEXT).size().x);
    let (mut label, mut sc) = (0.0f32, 0.0f32);
    for m in items {
        match m {
            M::I(l, k, _) => {
                label = label.max(width(l));
                sc = sc.max(width(&crate::studio_ui::shortcut_label(k)));
            }
            M::Sub(l, _) => label = label.max(width(l) + 16.0),
            M::S => {}
        }
    }
    let col = 24.0 + label + 28.0;
    (col, (col + sc + 16.0).max(150.0))
}

/// A submenu title, indented like the items beside it (their check column).
fn sub_label(label: &str) -> egui::RichText {
    RichText::new(format!("\u{2007}\u{2007}\u{2007}{label}")).size(13.0)
}

impl Editor {
    /// (enabled, checked) of a menu / ribbon action.
    pub fn action_state(&self, a: &str) -> (bool, Option<bool>) {
        let sel = !self.game.doc.selection.is_empty();
        let playing = self.game.play.is_some();
        let edit = !playing;
        let open = |n: &str| Some(self.ui.panels_open.contains(n));
        match a {
            "new" | "open" | "open_roblox" | "open_folder" | "close_place" | "import_model" => (edit, None),
            "import_3d" => {
                if self.builtin_tools.plugins_enabled {
                    let (en, ck) = self.builtin_button_state("AssetImporter", Some("Mesh Importer"), Some("Import"));
                    (edit && en, ck)
                } else {
                    (false, None)
                }
            }
            "save" | "save_as" => (edit, None),
            "save_roblox" | "save_roblox_as" | "publish" | "publish_as" => {
                (edit && self.game.doc.kind == rustudio_core::doc::DocKind::Place, None)
            }
            "export_project" => (edit, None),
            "project_settings" => (edit && self.project.is_some(), None),
            "experience_settings" | "account" | "add_account" => (true, None),
            "live_collaborators" | "invite_collaborators" => (true,None),
            "avatar_settings" => (true, open("Avatar Settings")),
            "terrain_editor" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::TerrainEditor) {
                    self.promoted_button_state("TerrainEditor")
                } else {
                    (true, open("Terrain Editor"))
                }
            }
            "toolbox" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Toolbox) {
                    self.promoted_button_state("Toolbox")
                } else {
                    (true, open("Toolbox"))
                }
            }
            "asset_manager" => (true, open("Asset Manager")),
            "microprofiler" => (true, open("MicroProfiler")),
            a if a.starts_with("toolbox:") => (
                self.builtin_tools.use_native(rustudio_core::rustudio_settings::BuiltinTool::Toolbox),
                None,
            ),
            "character" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::RigBuilder) {
                    let (en, ck) = self.promoted_button_state("RigGenerator");
                    (edit && en, ck)
                } else {
                    (edit, Some(self.chars.open))
                }
            }
            a if a.starts_with("character:") => (
                edit && self.builtin_tools.use_native(rustudio_core::rustudio_settings::BuiltinTool::RigBuilder),
                None,
            ),
            "about" | "exit" | "studio_settings" | "logs_folder" => (true, None),
            "customize_shortcuts" => (true, Some(self.plugin_bar.shortcuts.open)),
            a if a.starts_with("studio_settings:") || a.starts_with("studio_settings_fill:") || a.starts_with("ribbon_tab:") => (true, None),
            "undo" => (edit && self.game.doc.can_undo().is_some(), None),
            "redo" => (edit && self.game.doc.can_redo().is_some(), None),
            "cut" | "copy" | "duplicate" | "delete" => (edit && sel, None),
            "paste" | "paste_original" => (edit && self.clipboard.is_some(), None),
            "paste_into" | "paste_into_original" => (edit && self.clipboard.is_some() && sel, None),
            "group" | "group_folder" => (edit && sel, None),
            "ungroup" => (
                edit && self.game
                    .doc
                    .selection
                    .iter()
                    .any(|r| matches!(self.game.doc.class_of(*r).as_deref(), Some("Model" | "Folder"))),
                None,
            ),
            "lock" | "anchor" => {
                let any = !self.selected_parts_shared().is_empty();
                let prop = if a == "lock" { "Locked" } else { "Anchored" };
                (edit && any, Some(any && self.selected_parts_all(prop)))
            }
            "unlock_all" | "select_all" => (edit, None),
            "zoom_in" | "zoom_out" => (true, None),
            "full_screen" => (true, Some(self.game.fullscreen)),
            "view_selector" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::ViewSelector) {
                    self.promoted_button_state("ViewSelector")
                } else {
                    (true, Some(self.ui.show_view_selector))
                }
            }
            "wind_direction" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("WindControl", Some("windControl"), Some("WindControl"))
                } else {
                    (false, None)
                }
            }
            "player_emulator" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("PlayerEmulator", Some("luaPlayerEmulatorToolbar"), Some("luaPlayerEmulatorButton"))
                } else {
                    (false, None)
                }
            }
            "align" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("AlignmentTool", Some("Alignment"), Some("AlignTool"))
                } else {
                    (false, None)
                }
            }
            "material_manager" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("MaterialManager", Some("Edit"), Some("MaterialManager"))
                } else {
                    (false, None)
                }
            }
            "clip_editor" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::AnimationEditor) {
                    self.builtin_button_state("AnimationClipEditor", Some("Animations"), Some("Animation Editor"))
                } else {
                    (true, open("Animation Editor"))
                }
            }
            "avatar_setup" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("AvatarCompatibilityPreviewer", Some("AvatarPreviewer"), Some("previewAvatar"))
                } else {
                    (false, None)
                }
            }
            "style_editor" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("StyleEditor", Some("StyleEditor"), Some("StyleEditorToolbarButton"))
                } else {
                    (false, None)
                }
            }
            "localization_tools" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("LocalizationTools", Some("Localization"), Some("Tools"))
                } else {
                    (false, None)
                }
            }
            "drafts" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("DraftsWidget", Some("draftsToolbar"), Some("draftsButton"))
                } else {
                    (false, None)
                }
            }
            "tag_editor" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("TagEditor", Some("tagEditorToolbar"), Some("TagEditor"))
                } else {
                    (false, None)
                }
            }
            "audio_discovery" => {
                if self.builtin_tools.plugins_enabled {
                    self.builtin_button_state("AudioDiscovery", Some("Audio Discovery"), Some("View Sounds"))
                } else {
                    (false, None)
                }
            }
            "pivot" => {
                if self.builtin_tools.plugins_enabled {
                    let (en, ck) = self.builtin_button_state("PivotEditor", Some("Pivot"), Some("EditPivot"));
                    (edit && en, ck)
                } else {
                    (false, None)
                }
            }
            "pivot_reset" => {
                if self.builtin_tools.plugins_enabled {
                    let (en, ck) = self.builtin_button_state("PivotEditor", Some("Pivot"), Some("ClearPivot"));
                    (edit && en, ck)
                } else {
                    (false, None)
                }
            }
            "constraint" => {
                if self.builtin_tools.plugins_enabled {
                    let any = crate::plugin_host::builtin_lazy::CONSTRAINT_MENU_ITEMS
                        .iter()
                        .flatten()
                        .any(|(tb, _)| self.builtin_button_state("ConstraintTool", Some(tb), Some("Place")).0);
                    let active = crate::plugin_host::builtin_lazy::CONSTRAINT_MENU_ITEMS
                        .iter()
                        .flatten()
                        .any(|(tb, _)| self.builtin_button_state("ConstraintTool", Some(tb), Some("Place")).1 == Some(true));
                    (edit && any, Some(active))
                } else {
                    (false, None)
                }
            }
            "constraint_menu" => (
                edit && self.builtin_tools.plugins_enabled
                    && self.builtin_plugin_button("ConstraintTool", None, None).is_some(),
                None,
            ),
            a if a.starts_with("constraint:") => {
                if self.builtin_tools.plugins_enabled {
                    let tb = &a["constraint:".len()..];
                    let (en, ck) = self.builtin_button_state("ConstraintTool", Some(tb), Some("Place"));
                    (edit && en, ck)
                } else {
                    (false, None)
                }
            }
            "show_grid" => (true, Some(self.show_grid)),
            "align_dragged" => (true, Some(self.align_dragged)),
            "dragger_menu" | "dragger_physical" | "dragger_geometric" => (true, None),
            "grid_2" => (self.show_grid, Some(self.ui.grid_size == 2.0)),
            "grid_4" => (self.show_grid, Some(self.ui.grid_size == 4.0)),
            "grid_16" => (self.show_grid, Some(self.ui.grid_size == 16.0)),
            "show_ui" => (true, Some(self.starter_gui_shown())),
            "show_core_ui" => (true, Some(self.ui.show_core_gui)),
            "screenshot" => (true, None),
            "mute" => (true, Some(rustudio_engine::audio::muted())),
            "audio_preview" => {
                let target = (edit && self.game.doc.selection.len() == 1)
                    .then(|| self.game.doc.selection[0])
                    .filter(|r| matches!(self.game.doc.class_of(*r).as_deref(), Some("Sound" | "AudioPlayer")));
                match target {
                    Some(r) => (true, Some(self.game.edit_audio.preview_state(r).0)),
                    None => (false, None),
                }
            }
            "test" | "test_here" | "run" => (!playing, None),
            "break_never" => (false, Some(true)),
            "toggle_client_server" => (
                self.game.play.as_ref().is_some_and(|p| p.mode == rustudio_engine::runtime::RunMode::Play),
                None,
            ),
            "stop" => (playing, None),
            "panel_properties" => (true, open("Properties")),
            "panel_explorer" => (true, open("Explorer")),
            "panel_output" => (true, open("Output")),
            "insert_object" => (edit, None),
            "reset_layout" => (true, None),
            "dock_all" => (!self.ui.popped.is_empty(), None),
            "documentation" | "tutorials" | "developer_forum" => (true, None),
            "manage_plugins" => (true, Some(self.plugin_store.ui.open)),
            a if a.starts_with("manage_plugins:") => (true, None),
            "plugins_folder" => (true, None),
            "save_local_plugin" | "publish_plugin" => (edit && sel, None),
            "command_bar" => (false, Some(false)),
            // Ribbon-only actions.
            "transform" => (true, None),
            "tool_select" | "tool_move" | "tool_scale" | "tool_rotate" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers) {
                    let stem = match a {
                        "tool_select" => "SelectDragger",
                        "tool_move" => "MoveDragger",
                        "tool_scale" => "ScaleDragger",
                        _ => "RotateDragger",
                    };
                    self.promoted_button_state(stem)
                } else {
                    (true, None)
                }
            }
            "part" | "part_block" | "part_sphere" | "part_wedge" | "part_corner" | "part_cylinder" => (edit, None),
            "gui" | "script" => (edit, None),
            "effect" | "color" => (edit && self.has_selected_parts(), None),
            "material" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::MaterialPicker) {
                    let (en, ck) = self.promoted_button_state("MaterialPicker");
                    (edit && en, ck)
                } else {
                    (edit && self.has_selected_parts(), None)
                }
            }
            "attachment" => {
                if self.builtin_tools.plugins_enabled
                    && self.builtin_plugin_button("ConstraintTool", Some("Attachment"), Some("Place")).is_some()
                {
                    let (en, ck) = self.builtin_button_state("ConstraintTool", Some("Attachment"), Some("Place"));
                    (edit && en, ck)
                } else {
                    (edit && self.has_selected_parts(), None)
                }
            }
            "weld" => {
                if self.builtin_tools.plugins_enabled
                    && self.builtin_plugin_button("ConstraintTool", Some("WeldConstraint"), Some("Place")).is_some()
                {
                    let (en, ck) = self.builtin_button_state("ConstraintTool", Some("WeldConstraint"), Some("Place"));
                    (edit && en, ck)
                } else {
                    (edit && self.selected_parts_shared().len() >= 2, None)
                }
            }
            "ui_frame" | "ui_label" | "ui_input" | "ui_appearance" | "ui_layout" | "ui_constraint" => {
                (edit && self.gui_target().is_some(), None)
            }
            _ => (false, None),
        }
    }

    pub fn all_parts(&self, prop: &str) -> bool {
        self.parts_have(&self.selected_parts(), prop)
    }

    fn parts_have(&self, parts: &[Ref], prop: &str) -> bool {
        parts.iter()
            .all(|r| matches!(self.game.doc.prop(*r, prop), Some(Variant::Bool(true))))
    }

    /// Where a UI-tab insert goes: the first selected `GuiBase2d`
    /// (`LayerCollector` or `GuiObject`), else — when nothing is selected —
    /// `StarterGui`'s first `ScreenGui` child (Studio's
    /// `InsertObjectUtils::setGuiObjectParent`).
    pub fn gui_target(&self) -> Option<Ref> {
        if self.game.doc.selection.is_empty() {
            let root = self.game.doc.dom.root_ref();
            let sg = self.game.doc.find_child(root, "StarterGui")?;
            return self
                .game
                .doc
                .dom
                .get_by_ref(sg)?
                .children()
                .iter()
                .copied()
                .find(|r| self.game.doc.class_of(*r).as_deref() == Some("ScreenGui"));
        }
        self.game.doc.selection.iter().copied().find(|r| {
            self.game
                .doc
                .class_of(*r)
                .is_some_and(|c| rustudio_core::doc::is_a(&c, "GuiBase2d"))
        })
    }

    /// Inserts go into the first selected object, else Workspace (Studio).
    pub fn insert_parent(&self) -> Ref {
        self.game.doc.selection.first().copied().unwrap_or(self.game.doc.default_parent())
    }

    fn service(&self, name: &str) -> Ref {
        let root = self.game.doc.dom.root_ref();
        self.game.doc.find_child(root, name).unwrap_or(self.game.doc.default_parent())
    }

    fn set_on_parts(&mut self, label: &str, prop: &str, v: bool) {
        let edits = self
            .selected_parts()
            .into_iter()
            .map(|r| (r, rbx_dom_weak::ustr(prop), Variant::Bool(v)))
            .collect();
        self.game.doc.set_props(label, edits, None);
    }

    /// Runs a menu or ribbon action (disabled ones do nothing).
    pub fn ui_action(&mut self, ctx: &egui::Context, a: &str) {
        if !a.starts_with("class:") && !self.action_state(a).0 {
            return;
        }
        match a {
            "live_collaborators" | "invite_collaborators" => self.cloud.live_collaborators_open=true,
            "new" => self.request(PendingAction::New),
            "close_place" => self.request(PendingAction::Close),
            "open" => self.request(PendingAction::Open(None)),
            "open_folder" => self.request(PendingAction::OpenFolder),
            "export_project" => self.open_export_dialog(),
            "project_settings" => self.open_settings_dialog(),
            "open_roblox" => {
                self.cloud.open = true;
                let client = self.game.account.shared_client();
                let tab = self.cloud.tab;
                self.cloud.refresh(client, tab);
            }
            "import_model" => self.insert_from_file(),
            "import_3d" => {
                self.trigger_builtin_plugin_button("AssetImporter", Some("Mesh Importer"), Some("Import"));
            }
            "character" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::RigBuilder) {
                    self.trigger_promoted_plugin_button("RigGenerator");
                } else {
                    self.toggle_character_builder();
                }
            }
            a if a.starts_with("character:") => self.character_action(&a["character:".len()..]),
            // "Save to File": a place from Roblox goes to a file too (Ctrl+S
            // saves it to Roblox).
            "save" if self.game.doc.cloud.is_some() || self.live_team_create_ids().is_some() => match self.game.doc.path.clone() {
                Some(p) => self.save_to(p),
                None => self.save_as(),
            },
            "save" => self.save(),
            "save_as" => self.save_as(),
            "save_roblox" | "save_roblox_as" | "publish" | "publish_as" => {
                use rustudio_account::VersionType;
                let vt = match a {
                    "publish" | "publish_as" => VersionType::Published,
                    _ => VersionType::Saved,
                };
                let linked = self.game.doc.cloud.is_some() || self.live_team_create_ids().is_some();
                if matches!(a, "save_roblox_as" | "publish_as") || !linked {
                    self.open_roblox_destination(vt);
                } else if self.game.doc.cloud.as_ref().is_some_and(|o| o.local_copy) {
                    self.cloud.confirm_copy_save = Some(vt);
                } else {
                    self.save_cloud(vt, false);
                }
            }
            "account" => self.game.account.open = true,
            "add_account" => {
                self.game.account.open = true;
                self.game.account.adding = true;
            }
            "experience_settings" => self.cloud.settings_open = true,
            "avatar_settings" => self.ui.panel_requests.push((Tab::AvatarSettings, None)),
            "terrain_editor" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::TerrainEditor) {
                    self.trigger_promoted_plugin_button("TerrainEditor");
                } else {
                    self.ui.panel_requests.push((Tab::TerrainEditor, None));
                }
            }
            "toolbox" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Toolbox) {
                    self.trigger_promoted_plugin_button("Toolbox");
                } else {
                    self.ui.panel_requests.push((Tab::Toolbox, None));
                }
            }
            "asset_manager" => self.ui.panel_requests.push((Tab::AssetManager, None)),
            "file_sync" => self.ui.panel_requests.push((Tab::FileSync, Some(true))),
            "microprofiler" => self.ui.panel_requests.push((Tab::Profiler, None)),
            a if a.starts_with("toolbox:") => self.toolbox_action(a),
            "about" => self.ui.about = true,
            "logs_folder" => {
                if let Err(error) = rustudio_core::diagnostics::open_folder() {
                    self.error(format!("Could not open the logs folder: {error}"));
                }
            }
            "studio_settings" => self.ui.studio_settings.open = true,
            "customize_shortcuts" => self.plugin_bar.shortcuts.open = true,
            // Harness: open on a page ("studio_settings:Physics"), or filling
            // the window like the Vinegar captures ("studio_settings_fill:…").
            a if a.starts_with("studio_settings:") => self.ui.studio_settings.open_at(&a["studio_settings:".len()..], false),
            a if a.starts_with("studio_settings_fill:") => self.ui.studio_settings.open_at(&a["studio_settings_fill:".len()..], true),
            a if a.starts_with("ribbon_tab:") => {
                let want = &a["ribbon_tab:".len()..];
                if let Some(tab) = crate::studio_ui::RibbonTab::ALL
                    .into_iter()
                    .find(|t| t.label().eq_ignore_ascii_case(want))
                {
                    self.ui.tab = tab;
                }
            }
            "restart" => self.request(PendingAction::Restart),
            "exit" => self.request(PendingAction::Quit),
            "undo" | "redo" | "cut" | "copy" | "paste" | "paste_into" | "duplicate" | "delete" | "group"
            | "ungroup" => {
                self.run_action(a);
                if matches!(a, "cut" | "copy") {
                    self.note_os_clipboard(ctx);
                }
            }
            "paste_original" => self.run_action("paste"),
            "paste_into_original" => self.run_action("paste_into"),
            "group_folder" => {
                let sel = self.game.doc.selection.clone();
                if let Some(f) = self.game.doc.group_as(&sel, "Folder") {
                    self.select(vec![f]);
                }
            }
            "lock" => {
                let on = !self.all_parts("Locked");
                self.set_on_parts("Lock", "Locked", on);
            }
            "anchor" => {
                let on = !self.all_parts("Anchored");
                self.set_on_parts("Anchor", "Anchored", on);
            }
            "unlock_all" => {
                let edits: Vec<_> = self.game
                    .doc
                    .dom
                    .descendants()
                    .filter(|i| rustudio_core::doc::is_a(i.class.as_str(), "BasePart"))
                    .map(|i| (i.referent(), rbx_dom_weak::ustr("Locked"), Variant::Bool(false)))
                    .collect();
                self.game.doc.set_props("Unlock All", edits, None);
            }
            "select_all" => {
                // Studio: everything directly in Workspace but the camera and terrain.
                if let Some(ws) = self.game.doc.workspace() {
                    let kids: Vec<Ref> = self.game
                        .doc
                        .dom
                        .get_by_ref(ws)
                        .map(|i| i.children().to_vec())
                        .unwrap_or_default();
                    let all = kids
                        .into_iter()
                        .filter(|r| !matches!(self.game.doc.class_of(*r).as_deref(), Some("Camera" | "Terrain")))
                        .collect();
                    self.select(all);
                }
            }
            "zoom_in" => self.ui.zoom = (self.ui.zoom + 0.1).min(2.0),
            "zoom_out" => self.ui.zoom = (self.ui.zoom - 0.1).max(0.6),
            "full_screen" => {
                self.game.fullscreen = !self.game.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.game.fullscreen));
            }
            "view_selector" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::ViewSelector) {
                    self.trigger_promoted_plugin_button("ViewSelector");
                } else {
                    self.ui.show_view_selector = !self.ui.show_view_selector;
                }
            }
            "wind_direction" => {
                self.trigger_builtin_plugin_button("WindControl", Some("windControl"), Some("WindControl"));
            }
            "player_emulator" => {
                self.trigger_builtin_plugin_button("PlayerEmulator", Some("luaPlayerEmulatorToolbar"), Some("luaPlayerEmulatorButton"));
            }
            "align" => {
                self.trigger_builtin_plugin_button("AlignmentTool", Some("Alignment"), Some("AlignTool"));
            }
            "material_manager" => {
                self.trigger_builtin_plugin_button("MaterialManager", Some("Edit"), Some("MaterialManager"));
            }
            "material" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::MaterialPicker) {
                    self.trigger_promoted_plugin_dropdown("MaterialPicker");
                }
            }
            "clip_editor" => {
                if self.builtin_tools.use_roblox(rustudio_core::rustudio_settings::BuiltinTool::AnimationEditor) {
                    self.trigger_builtin_plugin_button("AnimationClipEditor", Some("Animations"), Some("Animation Editor"));
                } else {
                    self.ui.panel_requests.push((Tab::AnimationEditor, None));
                }
            }
            "avatar_setup" => {
                self.trigger_builtin_plugin_button("AvatarCompatibilityPreviewer", Some("AvatarPreviewer"), Some("previewAvatar"));
            }
            "style_editor" => {
                self.trigger_builtin_plugin_button("StyleEditor", Some("StyleEditor"), Some("StyleEditorToolbarButton"));
            }
            "localization_tools" => {
                self.trigger_builtin_plugin_button("LocalizationTools", Some("Localization"), Some("Tools"));
            }
            "drafts" => {
                self.trigger_builtin_plugin_button("DraftsWidget", Some("draftsToolbar"), Some("draftsButton"));
            }
            "tag_editor" => {
                self.trigger_builtin_plugin_button("TagEditor", Some("tagEditorToolbar"), Some("TagEditor"));
            }
            "audio_discovery" => {
                self.trigger_builtin_plugin_button("AudioDiscovery", Some("Audio Discovery"), Some("View Sounds"));
            }
            "pivot" => {
                self.trigger_builtin_plugin_button("PivotEditor", Some("Pivot"), Some("EditPivot"));
            }
            "pivot_reset" => {
                self.trigger_builtin_plugin_button("PivotEditor", Some("Pivot"), Some("ClearPivot"));
            }
            "constraint_menu" => egui::Popup::open_id(ctx, egui::Id::new(crate::studio_ribbon::CONSTRAINT_MENU_ID)),
            a if a.starts_with("constraint:") => {
                let tb = &a["constraint:".len()..];
                self.trigger_builtin_plugin_button("ConstraintTool", Some(tb), Some("Place"));
            }
            "show_grid" => self.show_grid = !self.show_grid,
            "align_dragged" => self.align_dragged = !self.align_dragged,
            "dragger_menu" => egui::Popup::open_id(ctx, egui::Id::new(crate::studio_ribbon::DRAGGER_MENU_ID)),
            "dragger_physical" => self.physical_drag = true,
            "dragger_geometric" => self.physical_drag = false,
            "grid_2" => self.ui.grid_size = 2.0,
            "grid_4" => self.ui.grid_size = 4.0,
            "grid_16" => self.ui.grid_size = 16.0,
            "show_ui" => {
                let next = !self.starter_gui_shown();
                self.set_starter_gui_shown(next);
            }
            "show_core_ui" => {
                self.ui.show_core_gui = !self.ui.show_core_gui;
                self.ui.show_core_gui_saved = None;
            }
            "screenshot" => {
                let dir = rustudio_core::paths::pictures();
                let _ = std::fs::create_dir_all(&dir);
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                self.pending_user_shot = Some(dir.join(format!("rustudio-{secs}.png")));
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            "mute" => {
                let on = !rustudio_engine::audio::muted();
                rustudio_engine::audio::set_muted_global(on);
                self.game.edit_audio.set_muted(on);
                if let Some(s) = self.game.play.as_mut() {
                    s.game.set_muted(on);
                }
            }
            "audio_preview" => self.run_action("audio_preview"),
            "test" => self.start_run(RunType::Test),
            "test_here" => self.start_run(RunType::TestHere),
            "run" => self.start_run(RunType::Run),
            // Studio's Client / Server button: the other side of the running
            // Play in front (Explorer, viewport, the plugins' toolbars and
            // widgets); each Play still starts on the client.
            "toggle_client_server" => {
                let side = match self.game.play.as_ref().map(|p| p.showing) {
                    Some(rustudio_engine::runtime::Side::Client) => rustudio_engine::runtime::Side::Server,
                    _ => rustudio_engine::runtime::Side::Client,
                };
                self.show_play_side(side);
            }
            "stop" => self.stop_play(),
            "panel_properties" => self.ui.panel_requests.push((Tab::Properties, None)),
            "panel_explorer" => self.ui.panel_requests.push((Tab::Explorer, None)),
            "panel_output" => self.ui.panel_requests.push((Tab::Output, None)),
            "insert_object" => {
                let parent = self.insert_parent();
                let at = ctx.content_rect().center() - Vec2::new(120.0, 200.0);
                self.open_insert_popup(parent, at);
            }
            "dock_all" => self.ui.dock_moves.push(crate::panels::DockMove::DockAll),
            "reset_layout" => {
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("reset_dock"), true));
            }
            "documentation" => ctx.open_url(egui::OpenUrl::new_tab("https://create.roblox.com/docs")),
            "tutorials" => ctx.open_url(egui::OpenUrl::new_tab("https://create.roblox.com/docs/tutorials")),
            "developer_forum" => ctx.open_url(egui::OpenUrl::new_tab("https://devforum.roblox.com")),
            "manage_plugins" => self.plugin_store.ui.open = !self.plugin_store.ui.open,
            // Harness: Manage Plugins on a page ("manage_plugins:find:WORDS",
            // "manage_plugins:perms:ID").
            a if a.starts_with("manage_plugins:") => {
                self.plugin_store.ui.open = true;
                let rest = &a["manage_plugins:".len()..];
                if let Some(q) = rest.strip_prefix("find:") {
                    self.plugin_store.ui.find = true;
                    self.plugin_store.ui.query = q.to_string();
                    let client = self.game.account.shared_client();
                    self.plugin_store.search(client, q.to_string());
                } else if let Some(id) = rest.strip_prefix("perms:") {
                    self.plugin_store.ui.perms_page = Some(id.to_string());
                } else if let Some((on, id)) = rest.strip_prefix("enable:").map(|i| (true, i)).or_else(|| rest.strip_prefix("disable:").map(|i| (false, i))) {
                    // The Enabled switch on a plugin's card.
                    let client = self.game.account.shared_client();
                    self.plugin_store.set_enabled(client, id, on);
                }
            }
            "plugins_folder" => crate::plugin_store::ui::open_plugins_folder(&mut self.plugin_store),
            "save_local_plugin" => self.save_local_plugin(),
            "publish_plugin" => self.publish_as_plugin(),
            "transform" => self.tool = self.tool.toggle(Tool::Transform),
            "tool_select" => self.trigger_dragger_or_native(Tool::Select, "SelectDragger"),
            "tool_move" => self.trigger_dragger_or_native(Tool::Move, "MoveDragger"),
            "tool_scale" => self.trigger_dragger_or_native(Tool::Scale, "ScaleDragger"),
            "tool_rotate" => self.trigger_dragger_or_native(Tool::Rotate, "RotateDragger"),
            "part" | "part_block" => self.insert_part(MeshKind::Block),
            "part_sphere" => self.insert_part(MeshKind::Ball),
            "part_wedge" => self.insert_part(MeshKind::Wedge),
            "part_corner" => self.insert_part(MeshKind::CornerWedge),
            "part_cylinder" => self.insert_part(MeshKind::Cylinder),
            "gui" => {
                let sg = self.service("StarterGui");
                self.insert_class("ScreenGui", sg);
            }
            "script" => {
                // Into the selection, else ServerScriptService (Studio's Script button).
                let p = match self.game.doc.selection.first() {
                    Some(r) => *r,
                    None => self.service("ServerScriptService"),
                };
                self.insert_class("Script", p);
            }
            "attachment" => {
                if self.builtin_tools.plugins_enabled
                    && self.trigger_builtin_plugin_button("ConstraintTool", Some("Attachment"), Some("Place"))
                {
                } else {
                    for p in self.selected_parts() {
                        self.insert_class("Attachment", p);
                    }
                }
            }
            "weld" => {
                if self.builtin_tools.plugins_enabled
                    && self.trigger_builtin_plugin_button("ConstraintTool", Some("WeldConstraint"), Some("Place"))
                {
                } else {
                    let parts = self.selected_parts();
                    let (p0, p1) = (parts[0], parts[1]);
                    let builder = rbx_dom_weak::InstanceBuilder::new("WeldConstraint")
                        .with_name("WeldConstraint")
                        .with_property("Part0", Variant::Ref(p0))
                        .with_property("Part1", Variant::Ref(p1));
                    let w = self.game.doc.insert(p0, builder);
                    self.select(vec![w]);
                }
            }
            other => {
                if let Some(class) = other.strip_prefix("class:") {
                    let class = class.to_string();
                    let gui_class = rustudio_core::doc::is_a(&class, "UIComponent")
                        || rustudio_core::doc::is_a(&class, "GuiObject");
                    if gui_class {
                        if let Some(t) = self.gui_target() {
                            self.insert_class(&class, t);
                        }
                    } else {
                        for p in self.selected_parts() {
                            self.insert_class(&class, p);
                        }
                    }
                }
            }
        }
    }

    /// Plugins > Save as Local Plugin: the selection as a model file in the
    /// local Plugins folder, named after the first selected object, then
    /// loaded at once (Studio).
    pub fn save_local_plugin(&mut self) {
        if self.game.doc.selection.is_empty() {
            self.warn("Select the plugin's objects in the Explorer first");
            return;
        }
        let name = self.game.doc.dom.get_by_ref(self.game.doc.selection[0]).map(|i| i.name.to_string()).unwrap_or_else(|| "Plugin".into());
        let _ = self.plugin_store.store.ensure_plugins_dir();
        let default = self.plugin_store.store.local_save_path(&name, "rbxmx");
        if std::env::var_os("RUSTUDIO_NO_DIALOGS").is_some() {
            self.save_local_plugin_to(default);
            return;
        }
        use crate::file_dialog::{Kind, Purpose};
        let d = rfd::AsyncFileDialog::new()
            .add_filter("Roblox model", &["rbxmx", "rbxm"])
            .set_directory(&self.plugin_store.store.plugins_dir)
            .set_file_name(default.file_name().unwrap().to_string_lossy());
        self.dialogs.start(Purpose::SaveLocalPlugin(self.game.doc.selection.clone()), d, Kind::SaveFile);
    }

    /// Writes the selection to `path` as a local plugin and loads it.
    pub fn save_local_plugin_to(&mut self, path: std::path::PathBuf) {
        let sel = self.game.doc.selection.clone();
        let xml = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rbxmx"));
        let bytes = match self.game.doc.encode(&self.game.doc.top_level(&sel), xml) {
            Ok(b) => b,
            Err(e) => return self.error(format!("Save as Local Plugin: {e}")),
        };
        if let Err(e) = self.plugin_store.save_local(&path, &bytes) {
            self.error(format!("Save as Local Plugin: {e}"));
        }
    }

    /// Plugins > Publish as Plugin: the confirm dialog (name, description,
    /// creator; new plugin or overwrite one the account owns), then the
    /// upload through the session.
    pub fn publish_as_plugin(&mut self) {
        if self.game.doc.selection.is_empty() {
            self.warn("Select the plugin's objects in the Explorer first");
            return;
        }
        if !self.game.account.signed_in() {
            self.warn("Sign in to publish a plugin (File > Account)");
            return;
        }
        let sel = self.game.doc.top_level(&self.game.doc.selection.clone());
        let rbxm = match self.game.doc.encode(&sel, false) {
            Ok(b) => b,
            Err(e) => return self.error(format!("Publish as Plugin: {e}")),
        };
        let name = self.game.doc.dom.get_by_ref(sel[0]).map(|i| i.name.to_string()).unwrap_or_default();
        let creator = self.game.account.shared_client().and_then(|c| c.lock().ok().and_then(|g| g.as_ref().map(|c| c.who()))).unwrap_or_default();
        self.plugin_store.ui.publish = Some(crate::plugin_store::ui::PublishDialog { name, description: String::new(), creator, overwrite: String::new(), rbxm, error: None });
    }

    /// Starts the selected kind of test session (Studio's Play button).
    pub fn start_run(&mut self, t: RunType) {
        use rustudio_engine::runtime::RunMode;
        if self.game.play.is_some() || !t.available() {
            return;
        }
        self.ui.run_type = t;
        match t {
            RunType::Test => {
                self.game.play_here = None;
                self.start_play(RunMode::Play);
            }
            RunType::TestHere => {
                // The character starts where the camera is (Studio's Test Here).
                self.game.play_here = Some(self.game.cam.pos);
                self.start_play(RunMode::Play);
            }
            RunType::Run => {
                self.game.play_here = None;
                self.start_play(RunMode::Run);
            }
            RunType::ServerClients => {}
        }

        // switch window to main viewport when playtesting
        // (the dock lives on RustudioApp, which consumes this flag in ui())
        self.switch_to_viewport = true;
    }

    /// The menu bar: Studio's menus in Studio's order.
    pub fn studio_menu_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut chosen: Option<String> = None;
        egui::MenuBar::new().ui(ui, |ui| {
            for (title, items) in MENUS {
                let r = ui.menu_button(*title, |ui| {
                    self.menu_items(ui, items, &mut chosen);
                    if *title == "Plugins" {
                        // One submenu per plugin toolbar (src/plugin_host/ui_ribbon.rs).
                        self.plugins_menu_toolbars(ui);
                    }
                });
                self.ui_rects.insert(menu_rect_key(title), r.response.rect);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let dirty = if self.game.doc.dirty() { " •" } else { "" };
                // A place from Roblox: its experience and creator, and
                // whether Team Create is on (File > Experience Settings).
                let cloud = match &self.game.doc.cloud {
                    Some(o) => {
                        let tc = match self.cloud.status.as_ref().filter(|(u, _)| *u == o.universe_id) {
                            Some((_, s)) if s.enabled == Some(true) => " · Team Create on",
                            _ => "",
                        };
                        format!(" @ {} ({}) v{}{tc}", o.experience, o.creator_label(), o.version)
                    }
                    None => String::new(),
                };
                let r = ui.label(RichText::new(format!("{}{dirty}{cloud} - rustudio", self.game.doc.title)).color(theme::TEXT_DIM));
                if let Some((_, s)) = self.cloud.status.as_ref().filter(|_| self.game.doc.cloud.is_some()) {
                    r.on_hover_text(s.describe());
                }
            });
        });
        if let Some(a) = chosen {
            self.ui_action(&ctx, &a);
        }
    }

    fn menu_items(&mut self, ui: &mut egui::Ui, items: &'static [M], chosen: &mut Option<String>) {
        let (col, w) = menu_metrics(ui, items);
        ui.set_width(w);
        for m in items {
            match m {
                M::S => {
                    ui.separator();
                }
                M::I(label, sc, a) => {
                    let label = if *a == "group" && self.game.doc.should_group_as_frame(&self.game.doc.selection) {
                        "Group as Frame"
                    } else {
                        *label
                    };
                    let (en, ck) = self.action_state(a);
                    if menu_item_at(ui, label, sc, en, ck, col, w).clicked() {
                        *chosen = Some(a.to_string());
                        ui.close();
                    }
                }
                M::Sub("Recent", _) => {
                    let recent = self.ui.recent.clone();
                    ui.add_enabled_ui(!recent.is_empty(), |ui| {
                        ui.menu_button(sub_label("Recent"), |ui| {
                            for p in recent {
                                if ui.button(p.display().to_string()).clicked() {
                                    self.request(PendingAction::Open(Some(p)));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
                M::Sub(label, sub) => {
                    ui.menu_button(sub_label(label), |ui| {
                        self.menu_items(ui, sub, chosen);
                    });
                }
            }
        }
    }

    /// The menus' own accelerators (Ctrl+Shift+V, Alt+A, F11…), as actions.
    pub fn menu_shortcut_actions(&mut self, ctx: &egui::Context) -> Vec<&'static str> {
        let mut out = Vec::new();
        ctx.input_mut(|i| {
            // (only a key press can match a shortcut)
            if !i.events.iter().any(|e| matches!(e, egui::Event::Key { .. })) {
                return;
            }
            for &(k, a) in menu_shortcuts() {
                // Clipboard keys come as egui events; Del, F5, F8 and
                // Shift+F5 go through the editor's own paths.
                if matches!(a, "cut" | "copy" | "paste" | "test" | "run" | "stop" | "delete") {
                    continue;
                }
                if i.consume_shortcut(&k) {
                    out.push(a);
                }
            }
        });
        out
    }

    /// File > Studio Settings (Alt+S); keeps the Diagnostics counts current
    /// and writes changed settings.
    pub fn studio_settings_ui(&mut self, ctx: &egui::Context) {
        if self.ui.studio_settings.open {
            let instances = self.game.doc.dom.descendants_of(self.game.doc.dom.root_ref()).count() as i64;
            let (models, players) = match self.game.play.as_ref() {
                Some(p) if p.mode == rustudio_engine::runtime::RunMode::Play => (3, 1),
                Some(_) => (2, 0),
                None => (1, 0),
            };
            crate::studio_settings::set_live([instances, players, models, 0]);
        }
        self.ui.studio_settings.ui(ctx);
        crate::studio_settings::tick();
        rustudio_core::rustudio_settings::tick();
    }

    /// File > About rustudio.
    pub fn about_ui(&mut self, ctx: &egui::Context) {
        if !self.ui.about {
            return;
        }
        let mut open = true;
        egui::Window::new("About rustudio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                crate::icons::logo(ui, 32.0);
                ui.label(RichText::new(format!("rustudio {}", env!("CARGO_PKG_VERSION"))).strong());
                ui.label("A native editor for Roblox places. Not made by Roblox.");
                ui.label("Icons: Tabler Icons (MIT). See CREDITS.md.");
            });
        self.ui.about = open;
    }
}
