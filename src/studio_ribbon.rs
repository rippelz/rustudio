//! Studio's top row (run type, Play / Pause / Stop, the ribbon tabs, the
//! account) and the ribbon tabs' buttons, as in an offline Studio
//! (checks/ui/studio/ribbon-*.png).

use egui::{Color32, Rect, Sense, Vec2};

use crate::app::{Editor, Tool};
use crate::studio_ui::{run_type_key, tab_rect_key, RibbonTab, RunType};
use rustudio_engine::theme;

/// The dragger settings drop-down's popup id (the `dragger_menu` action opens it).
pub(crate) const DRAGGER_MENU_ID: &str = "dragger_menu";
/// Model > Constraint drop-down's popup id (the `constraint_menu` action opens it).
pub(crate) const CONSTRAINT_MENU_ID: &str = "constraint_menu";

/// Model > Constraint's native menu, in Studio's order: two columns,
/// `(class, label)`. The left column takes the extra entry so the one empty
/// cell is at the bottom right, where a left-to-right reading ends (Studio's
/// capture leaves it under Line Force instead). Weld is its own ribbon
/// button and No Collision is not in Studio's menu.
/// `rustudio_core::constraint` joins the two selected parts or attachments
/// with the chosen class.
const CONSTRAINT_MENU_LEFT: &[(&str, &str)] = &[
    ("AlignPosition", "Align Position"),
    ("AlignOrientation", "Align Orientation"),
    ("AngularVelocity", "Angular Velocity"),
    ("AnimationConstraint", "Animation Constraint"),
    ("BallSocketConstraint", "Ball in Socket"),
    ("CylindricalConstraint", "Cylindrical"),
    ("HingeConstraint", "Hinge"),
    ("LinearVelocity", "Linear Velocity"),
    ("LineForce", "Line Force"),
    ("VectorForce", "Vector Force"),
];
const CONSTRAINT_MENU_RIGHT: &[(&str, &str)] = &[
    ("PlaneConstraint", "Plane"),
    ("PrismaticConstraint", "Prismatic"),
    ("RigidConstraint", "Rigid Constraint"),
    ("RodConstraint", "Rod"),
    ("RopeConstraint", "Rope"),
    ("SpringConstraint", "Spring"),
    ("Torque", "Torque"),
    ("TorsionSpringConstraint", "Torsion Spring"),
    ("UniversalConstraint", "Universal"),
];

fn divider(ui: &mut egui::Ui) {
    ui.add_space(4.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, 46.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, theme::DIVIDER);
    ui.add_space(4.0);
}

/// Studio's ribbon button: a 24-px icon over an 11-px label.
pub fn ribbon_button(ui: &mut egui::Ui, icon: &str, label: &str, active: bool, enabled: bool) -> egui::Response {
    let font = egui::FontId::proportional(11.5);
    let tw = ui.fonts_mut(|f| f.layout_no_wrap(label.into(), font.clone(), theme::TEXT).size().x);
    let w = (tw + 10.0).max(48.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 50.0), sense);
    let p = ui.painter();
    let icon_r = Rect::from_center_size(r.center() - Vec2::new(0.0, 8.0), Vec2::splat(24.0));
    if active {
        p.rect_filled(icon_r.expand(5.0), 4.0, theme::ACTIVE_TAB.gamma_multiply(1.5));
    } else if enabled && resp.hovered() {
        p.rect_filled(r, 4.0, theme::HOVER_BG);
    }
    let col = if !enabled {
        theme::TEXT_DIM.gamma_multiply(0.8)
    } else if active {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    let icon_color = ribbon_icon_color(icon);
    let icon_color = if enabled { icon_color } else { icon_color.gamma_multiply(0.55) };
    rustudio_engine::ti::paint(p, icon_r, icon, icon_color);
    p.text(egui::pos2(r.center().x, r.max.y - 7.0), egui::Align2::CENTER_CENTER, label, font, col);
    resp
}

/// Palette audited against ../checks/ui/studio/ribbon-{Home,Model,Avatar,UI,Script,Plugins}.png.
/// Neutral tools retain Studio's cool white; disabled icons retain their hue at reduced brightness.
fn ribbon_icon_color(icon: &str) -> Color32 {
    let rgb = match icon {
        "cube" | "circle-dot" | "link" | "link-plus" | "sparkles" => [183, 135, 242],
        "mountain" => [89, 210, 190],
        "user-plus" | "user-cog" | "accessible" | "hanger" | "shirt" | "checklist" |
        "movie" | "chart-line" => [255, 160, 35],
        "app-window" | "square" | "typography" | "forms" | "border-corner-rounded" |
        "layout-list" | "aspect-ratio" => [229, 126, 174],
        "file-code" | "indent-increase" | "search" | "list-numbers" | "terminal-2" |
        "terminal" | "point" | "stack-2" | "chart-bar" | "activity" => [190, 218, 86],
        _ => [225, 229, 241],
    };
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// One row of Model > Constraint's native menu: the class's glyph and its
/// Studio label.
fn constraint_menu_entry(ui: &mut egui::Ui, entry: Option<(&str, &str)>, act: &mut Option<String>) {
    let Some((class, label)) = entry else {
        ui.label("");
        return;
    };
    ui.horizontal(|ui| {
        crate::icons::class_icon(ui, class, 16.0);
        if ui.add(egui::Button::selectable(false, label)).clicked() {
            *act = Some(format!("constraint:{class}"));
            ui.close();
        }
    });
}

/// A top-row icon button (Play, Pause, Stop, Notifications…).
fn top_icon(ui: &mut egui::Ui, icon: &str, color: Color32, enabled: bool) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(28.0, 24.0), sense);
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r, 3.0, theme::HOVER_BG);
    }
    let col = if enabled { color } else { color.gamma_multiply(0.35) };
    rustudio_engine::ti::paint(ui.painter(), r.shrink2(Vec2::new(6.0, 4.0)), icon, col);
    resp
}

fn tool_key(t: Tool) -> &'static str {
    match t {
        Tool::None => "tool.none",
        Tool::Transform => "tool.transform",
        Tool::Select => "tool.select",
        Tool::Move => "tool.move",
        Tool::Scale => "tool.scale",
        Tool::Rotate => "tool.rotate",
    }
}

impl Editor {
    /// Under the menus: run type, Play / Pause / Stop, the ribbon tabs in
    /// the middle, the account at the right (Studio's layout).
    pub fn studio_top_row(&mut self, ui: &mut egui::Ui) {
        let mode = self.game.play.as_ref().map(|p| p.mode);
        let full = ui.max_rect();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);
            let mut picked = None;
            let resp = egui::ComboBox::from_id_salt("run_type")
                .width(80.0)
                .selected_text(self.ui.run_type.label())
                .show_ui(ui, |ui| {
                    for t in RunType::ALL {
                        let b = egui::Button::selectable(self.ui.run_type == t, t.label());
                        let r = ui.add_enabled(t.available(), b);
                        self.ui_rects.insert(run_type_key(t), r.rect);
                        if r.clicked() {
                            picked = Some(t);
                        }
                    }
                })
                .response;
            self.ui_rects.insert("test.type", resp.rect);
            if let Some(t) = picked {
                self.ui.run_type = t;
            }
            let key = match self.ui.run_type {
                RunType::Test => " (F5)",
                RunType::Run => " (F8)",
                _ => "",
            };
            let tip = format!("{}{key}", self.ui.run_type.label());
            let resp = top_icon(ui, "player-play-filled", theme::ACCENT, mode.is_none()).on_hover_text(tip);
            self.ui_rects.insert("test.play", resp.rect);
            if mode.is_none() && resp.clicked() {
                self.start_run(self.ui.run_type);
            }
            let resp = top_icon(ui, "player-pause-filled", theme::TEXT, false).on_hover_text("Pause: not in rustudio yet");
            self.ui_rects.insert("test.pause", resp.rect);
            let resp = top_icon(ui, "player-stop-filled", Color32::from_rgb(200, 60, 60), mode.is_some())
                .on_hover_text("Stop (Shift+F5)");
            self.ui_rects.insert("test.stop", resp.rect);
            if mode.is_some() && resp.clicked() {
                self.stop_play();
            }
            top_icon(ui, "device-mobile", theme::TEXT, false).on_hover_text("Device Simulator: not in rustudio yet");

            // The tabs, centred like Studio's.
            let tab_w = 80.0;
            let x0 = full.center().x - tab_w * 7.0 / 2.0;
            let y = ui.min_rect().min.y;
            for (i, t) in RibbonTab::ALL.into_iter().enumerate() {
                let r = Rect::from_min_size(egui::pos2(x0 + i as f32 * tab_w, y), egui::vec2(tab_w - 4.0, 24.0));
                let resp = ui.interact(r, egui::Id::new(("ribbon_tab", i)), Sense::click());
                let active = self.ui.tab == t;
                let bg = if active {
                    theme::ACTIVE_TAB
                } else if resp.hovered() {
                    theme::HOVER_BG
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(r, 4.0, bg);
                let col = if active { theme::TEXT } else { theme::TEXT.gamma_multiply(0.75) };
                let font = egui::FontId::proportional(13.0);
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, t.label(), font, col);
                self.ui_rects.insert(tab_rect_key(t), r);
                if resp.clicked() {
                    self.ui.tab = t;
                }
            }
            let plus = Rect::from_min_size(egui::pos2(x0 + 6.0 * tab_w + 26.0, y), egui::vec2(24.0, 24.0));
            let font = egui::FontId::proportional(15.0);
            ui.painter().text(plus.center(), egui::Align2::CENTER_CENTER, "+", font, theme::TEXT_DIM);
            ui.interact(plus, egui::Id::new("ribbon_tab_add"), Sense::hover())
                .on_hover_text("Custom tabs: not in rustudio yet");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                // The account: a round button and its menu (Add Account, Log Out).
                let (r, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                let signed = self.game.account.signed_in();
                let fill = if signed { theme::ACCENT.gamma_multiply(0.8) } else { theme::PANEL_ALT };
                ui.painter().circle_filled(r.center(), 12.0, fill);
                rustudio_engine::ti::paint(ui.painter(), r.shrink(5.0), "user", theme::TEXT);
                self.ui_rects.insert("account", r);
                let resp = resp.on_hover_text(self.game.account.status_short());
                if resp.clicked() {
                    self.game.account.open = !self.game.account.open;
                    self.game.account.adding = false;
                }
                top_icon(ui, "bell", theme::TEXT, false).on_hover_text("Notifications: not in rustudio yet");
                let tc = match (&self.game.doc.cloud, &self.cloud.status) {
                    (Some(o), Some((u, s))) if *u == o.universe_id => s.describe(),
                    _ => "Open a Team Create place to see its live collaborators".into(),
                };
                if top_icon(ui, "users-group", theme::TEXT, false).on_hover_text(tc).clicked() {self.cloud.live_collaborators_open=true;}
                top_icon(ui, "share", theme::TEXT, false).on_hover_text("Share: not in rustudio yet");
                if ui.button("Collaborate").clicked() {self.cloud.live_collaborators_open=true;}
                // (right to left: the MCP button sits left of Collaborate)
                self.mcp_top_button(ui);
                // (and File Sync left of MCP)
                self.file_sync_top_button(ui);
            });
        });
    }

    fn ribbon_tools(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        let roblox_draggers = self
            .builtin_tools
            .use_roblox(rustudio_core::rustudio_settings::BuiltinTool::Draggers);
        for (icon, label, a, tool) in [
            ("pointer", "Select", "tool_select", Tool::Select),
            ("arrows-move", "Move", "tool_move", Tool::Move),
            ("resize", "Scale", "tool_scale", Tool::Scale),
            ("rotate-clockwise-2", "Rotate", "tool_rotate", Tool::Rotate),
        ] {
            let on = if roblox_draggers {
                self.action_state(a).1 == Some(true)
            } else {
                self.tool == tool
            };
            let r = self.rb(ui, icon, label, a, on, act);
            self.ui_rects.insert(tool_key(tool), r);
            self.hud.toolbar_buttons += 1;
        }
        self.rb(ui, "transform", "Transform", "transform", self.tool == Tool::Transform, act);
        divider(ui);
        self.movement_mode_button(ui);
        self.snap_widget(ui);
        divider(ui);
    }

    /// The selected ribbon tab's buttons, grouped with dividers like Studio's.
    pub fn studio_ribbon(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut act: Option<String> = None;
        if self.ui.tab != RibbonTab::Plugins {
            self.plugins_tab_hidden();
        }
        let a = &mut act;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
            match self.ui.tab {
                RibbonTab::Home => {
                    self.ribbon_tools(ui, a);
                    self.part_button(ui, a);
                    let terrain_on = self.action_state("terrain_editor").1 == Some(true);
                    self.rb(ui, "mountain", "Terrain", "terrain_editor", terrain_on, a);
                    let char_on = self.action_state("character").1 == Some(true);
                    let r = self.rb(ui, "user-plus", "Character", "character", char_on, a);
                    self.ui_rects.insert("home.character", r);
                    self.rb(ui, "app-window", "GUI", "gui", false, a);
                    self.rb(ui, "file-code", "Script", "script", false, a);
                    self.import_button(ui, a);
                    divider(ui);
                    self.material_quick_button(ui, a);
                    self.color_quick_button(ui);
                    self.rb(ui, "box-multiple", "Group", "group", false, a);
                    let lock = self.action_state("lock").1 == Some(true);
                    self.rb(ui, "lock", "Lock", "lock", lock, a);
                    let anchored = self.action_state("anchor").1 == Some(true);
                    let r = self.rb(ui, "anchor", "Anchor", "anchor", anchored, a);
                    self.ui_rects.insert("home.anchor", r);
                    divider(ui);
                    let e = self.ui.panels_open.contains("Explorer");
                    self.rb(ui, "list-tree", "Explorer", "panel_explorer", e, a);
                    let p = self.ui.panels_open.contains("Properties");
                    self.rb(ui, "list-details", "Properties", "panel_properties", p, a);
                    let tb_on = self.action_state("toolbox").1 == Some(true);
                    self.rb(ui, "tools", "Toolbox", "toolbox", tb_on, a);
                    self.rb(ui, "folders", "Assets", "asset_manager", false, a);
                }
                RibbonTab::Model => {
                    self.ribbon_tools(ui, a);
                    let pivot_on = self.action_state("pivot").1 == Some(true);
                    self.rb(ui, "focus-centered", "Pivot", "pivot", pivot_on, a);
                    self.rb(ui, "restore", "Reset", "pivot_reset", false, a);
                    divider(ui);
                    self.part_button(ui, a);
                    let fx = ["ParticleEmitter", "Beam", "Trail", "Fire", "Smoke", "Sparkles", "Highlight"];
                    self.class_menu_button(ui, "sparkles", "Effect", "effect", &fx, a);
                    divider(ui);
                    self.attachment_button(ui, a);
                    self.constraint_button(ui, a);
                    let weld_on = self.action_state("weld").1 == Some(true);
                    self.rb(ui, "link-plus", "Weld", "weld", weld_on, a);
                    divider(ui);
                    self.material_quick_button(ui, a);
                    self.color_quick_button(ui);
                    self.rb(ui, "photo", "Texture", "texture_tool", false, a);
                    self.rb(ui, "box-multiple", "Group", "group", false, a);
                    let lock = self.action_state("lock").1 == Some(true);
                    self.rb(ui, "lock", "Lock", "lock", lock, a);
                    let anchored = self.action_state("anchor").1 == Some(true);
                    self.rb(ui, "anchor", "Anchor", "anchor", anchored, a);
                    divider(ui);
                    let align_on = self.action_state("align").1 == Some(true);
                    self.rb(ui, "layout-align-center", "Align", "align", align_on, a);
                }
                RibbonTab::Avatar => {
                    self.ribbon_tools(ui, a);
                    let open = self.ui.panels_open.contains("Avatar Settings");
                    let r = self.rb(ui, "user-cog", "Settings", "avatar_settings", open, a);
                    self.ui_rects.insert("avatar.settings", r);
                    divider(ui);
                    let char_on = self.action_state("character").1 == Some(true);
                    let r = self.rb(ui, "user-plus", "Character", "character", char_on, a);
                    self.ui_rects.insert("avatar.character", r);
                    let setup_on = self.action_state("avatar_setup").1 == Some(true);
                    self.rb(ui, "accessible", "Setup", "avatar_setup", setup_on, a);
                    self.rb(ui, "hanger", "Accessory", "accessory_editor", false, a);
                    self.rb(ui, "shirt", "Adaptive…", "adaptive", false, a);
                    self.rb(ui, "checklist", "UGC Validati…", "ugc_validation", false, a);
                    divider(ui);
                    let clip_on = self.action_state("clip_editor").1 == Some(true);
                    self.rb(ui, "movie", "Clip Editor", "clip_editor", clip_on, a);
                    self.rb(ui, "chart-line", "Graph Editor", "graph_editor", false, a);
                }
                RibbonTab::Ui => {
                    self.rb(ui, "app-window", "GUI", "gui", false, a);
                    self.rb_class(ui, "square", "Frame", "ui_frame", "Frame", a);
                    self.rb_class(ui, "typography", "Label", "ui_label", "TextLabel", a);
                    self.rb_class(ui, "forms", "Input", "ui_input", "TextBox", a);
                    let ap = ["UICorner", "UIStroke", "UIGradient", "UIPadding", "UIScale"];
                    self.class_menu_button(ui, "border-corner-rounded", "Appearance", "ui_appearance", &ap, a);
                    let la = ["UIListLayout", "UIGridLayout", "UIPageLayout", "UITableLayout", "UIFlexItem"];
                    self.class_menu_button(ui, "layout-list", "Layout", "ui_layout", &la, a);
                    let co = ["UIAspectRatioConstraint", "UISizeConstraint", "UITextSizeConstraint"];
                    self.class_menu_button(ui, "aspect-ratio", "Constraint", "ui_constraint", &co, a);
                    divider(ui);
                    let style_on = self.action_state("style_editor").1 == Some(true);
                    self.rb(ui, "brush", "Style Editor", "style_editor", style_on, a);
                }
                RibbonTab::Script => {
                    self.rb(ui, "chevron-left", "Back", "script_back", false, a);
                    self.rb(ui, "chevron-right", "Fwd", "script_fwd", false, a);
                    divider(ui);
                    self.rb(ui, "file-code", "Script", "script", false, a);
                    divider(ui);
                    self.rb(ui, "indent-increase", "Format", "format_document", false, a);
                    self.rb(ui, "search", "Find", "find_in_place", false, a);
                    self.rb(ui, "list-numbers", "Go to Line", "goto_line", false, a);
                    divider(ui);
                    self.rb(ui, "terminal-2", "Command", "command_bar", false, a);
                    let o = self.ui.panels_open.contains("Output");
                    self.rb(ui, "terminal", "Output", "panel_output", o, a);
                    divider(ui);
                    self.rb(ui, "point", "Breakpoints", "breakpoints", false, a);
                    self.rb(ui, "stack-2", "Call Stack", "call_stack", false, a);
                    self.rb(ui, "eye", "Watch", "watch", false, a);
                    divider(ui);
                    self.rb(ui, "chart-bar", "Analysis", "script_analysis", false, a);
                    self.rb(ui, "activity", "Script Activity", "script_activity", false, a);
                }
                // Only plugin toolbars, as Studio's (Manage Plugins and
                // Plugins Folder are in the Plugins menu): src/plugin_host/ui_ribbon.rs.
                RibbonTab::Plugins => self.plugins_ribbon(ui),
            }
        });
        self.ribbon_color_popup(&ctx);
        if let Some(a) = act {
            self.ui_action(&ctx, &a);
        }
    }

    /// Studio PNG icon URI when `action` is currently backed by a Roblox
    /// built-in plugin (so Home/Model/Avatar/UI draw Studio's real PNG icon
    /// with the Tabler icon as fallback, never a diamond placeholder).
    fn action_plugin_icon(&self, action: &str) -> Option<String> {
        use rustudio_core::rustudio_settings::BuiltinTool;
        if !self.builtin_tools.plugins_enabled {
            return None;
        }
        let btn = match action {
            "toolbox" if self.builtin_tools.use_roblox(BuiltinTool::Toolbox) => {
                self.builtin_plugin_button("Toolbox", None, None)
            }
            "terrain_editor" if self.builtin_tools.use_roblox(BuiltinTool::TerrainEditor) => {
                self.builtin_plugin_button("TerrainEditor", None, None)
            }
            "character" if self.builtin_tools.use_roblox(BuiltinTool::RigBuilder) => {
                self.builtin_plugin_button("RigGenerator", None, None)
            }
            "material" if self.builtin_tools.use_roblox(BuiltinTool::MaterialPicker) => {
                self.builtin_plugin_button("MaterialPicker", None, None)
            }
            "tool_select" if self.builtin_tools.use_roblox(BuiltinTool::Draggers) => {
                self.builtin_plugin_button("SelectDragger", None, None)
            }
            "tool_move" if self.builtin_tools.use_roblox(BuiltinTool::Draggers) => {
                self.builtin_plugin_button("MoveDragger", None, None)
            }
            "tool_scale" if self.builtin_tools.use_roblox(BuiltinTool::Draggers) => {
                self.builtin_plugin_button("ScaleDragger", None, None)
            }
            "tool_rotate" if self.builtin_tools.use_roblox(BuiltinTool::Draggers) => {
                self.builtin_plugin_button("RotateDragger", None, None)
            }
            "import_3d" => self.builtin_plugin_button("AssetImporter", Some("Mesh Importer"), Some("Import")),
            "pivot" => self.builtin_plugin_button("PivotEditor", Some("Pivot"), Some("EditPivot")),
            "pivot_reset" => self.builtin_plugin_button("PivotEditor", Some("Pivot"), Some("ClearPivot")),
            "attachment" => self.builtin_plugin_button("ConstraintTool", Some("Attachment"), Some("Place")),
            "weld" => self.builtin_plugin_button("ConstraintTool", Some("WeldConstraint"), Some("Place")),
            "align" => self.builtin_plugin_button("AlignmentTool", Some("Alignment"), Some("AlignTool")),
            "avatar_setup" => self.builtin_plugin_button("AvatarCompatibilityPreviewer", Some("AvatarPreviewer"), Some("previewAvatar")),
            "clip_editor" if self.builtin_tools.use_roblox(BuiltinTool::AnimationEditor) => {
                self.builtin_plugin_button("AnimationClipEditor", Some("Animations"), Some("Animation Editor"))
            }
            "style_editor" => self.builtin_plugin_button("StyleEditor", Some("StyleEditor"), Some("StyleEditorToolbarButton")),
            _ => None,
        }?;
        (!btn.icon.trim().is_empty()).then(|| btn.icon.clone())
    }

    /// Ribbon button that paints a Studio PNG icon (when available) over the
    /// standard ribbon cell, falling back to the Tabler icon while loading or
    /// when Studio textures aren't present.
    fn rb_icon(
        &mut self,
        ui: &mut egui::Ui,
        fallback_icon: &str,
        plugin_icon: Option<&str>,
        label: &str,
        active: bool,
        enabled: bool,
    ) -> egui::Response {
        let font = egui::FontId::proportional(11.5);
        let tw = ui.fonts_mut(|f| f.layout_no_wrap(label.into(), font.clone(), theme::TEXT).size().x);
        let w = (tw + 10.0).max(48.0);
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 50.0), sense);
        let ctx = ui.ctx().clone();
        let p = ui.painter().clone();
        let icon_r = Rect::from_center_size(r.center() - Vec2::new(0.0, 8.0), Vec2::splat(24.0));
        if active {
            p.rect_filled(icon_r.expand(5.0), 4.0, theme::ACTIVE_TAB.gamma_multiply(1.5));
        } else if enabled && resp.hovered() {
            p.rect_filled(r, 4.0, theme::HOVER_BG);
        }
        let col = if !enabled {
            theme::TEXT_DIM.gamma_multiply(0.8)
        } else if active {
            Color32::WHITE
        } else {
            theme::TEXT
        };
        let tint = if enabled { Color32::WHITE } else { Color32::WHITE.gamma_multiply(0.45) };
        let painted = plugin_icon.is_some_and(|uri| self.paint_builtin_icon(&ctx, &p, icon_r, uri, tint));
        if !painted {
            let icon_color = ribbon_icon_color(fallback_icon);
            let icon_color = if enabled { icon_color } else { icon_color.gamma_multiply(0.55) };
            rustudio_engine::ti::paint(&p, icon_r, fallback_icon, icon_color);
        }
        p.text(egui::pos2(r.center().x, r.max.y - 7.0), egui::Align2::CENTER_CENTER, label, font, col);
        resp
    }

    /// A ribbon button for an action; disabled ones say so. Returns its rect.
    pub(crate) fn rb(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, action: &str, active: bool, act: &mut Option<String>) -> Rect {
        let (enabled, _) = self.action_state(action);
        let plugin_icon = self.action_plugin_icon(action);
        let resp = self.rb_icon(ui, icon, plugin_icon.as_deref(), label, active, enabled);
        let rect = resp.rect;
        if enabled {
            if resp.clicked() {
                *act = Some(action.to_string());
            }
        } else {
            let tip = match action {
                "toolbox" | "terrain_editor" | "character" | "material" | "tool_select" | "tool_move" | "tool_scale" | "tool_rotate"
                    if self.builtin_tools.plugins_enabled =>
                {
                    format!("{label}: Roblox's built-in plugin is unavailable")
                }
                _ => format!("{label}: not in rustudio yet"),
            };
            resp.on_hover_text(tip);
        }
        rect
    }

    /// Home > Import: native Import Roblox Model when built-in plugins are OFF;
    /// split button for Studio's 3D Asset Importer + Import Roblox Model when ON.
    fn import_button(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        if !self.builtin_tools.plugins_enabled || !self.action_state("import_3d").0 {
            self.rb(ui, "file-import", "Import", "import_model", false, act);
            return;
        }
        let active = self.action_state("import_3d").1 == Some(true);
        let icon_uri = self.action_plugin_icon("import_3d");
        let resp = self.rb_icon(ui, "file-import", icon_uri.as_deref(), "Import", active, true);
        self.ui_rects.insert("home.import", resp.rect);
        let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
        rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
        let ar = ui.interact(arrow, egui::Id::new("import_arrow"), Sense::click());
        if resp.clicked() && !ar.hovered() {
            *act = Some("import_3d".into());
        }
        egui::Popup::menu(&ar).show(|ui| {
            if ui.button("Import 3D… (Ctrl+M)").clicked() {
                *act = Some("import_3d".into());
                ui.close();
            }
            if ui.button("Import Roblox Model…").clicked() {
                *act = Some("import_model".into());
                ui.close();
            }
        });
    }

    /// Model > Attachment: native Attachment inserter when built-in plugins are
    /// OFF; Studio's ConstraintTool Attachment / Bone placer when ON.
    fn attachment_button(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        if !self.builtin_tools.plugins_enabled
            || self.builtin_plugin_button("ConstraintTool", Some("Attachment"), Some("Place")).is_none()
        {
            self.rb(ui, "circle-dot", "Attachment", "attachment", false, act);
            return;
        }
        let (enabled, active) = self.action_state("attachment");
        let bone_active = self.action_state("constraint:Bone").1 == Some(true);
        let active = active == Some(true) || bone_active;
        let icon_uri = if bone_active {
            self.builtin_plugin_button("ConstraintTool", Some("Bone"), Some("Place"))
                .map(|b| b.icon.clone())
        } else {
            self.action_plugin_icon("attachment")
        };
        let resp = self.rb_icon(ui, "circle-dot", icon_uri.as_deref(), "Attachment", active, enabled);
        self.ui_rects.insert("model.attachment", resp.rect);
        let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
        rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
        let ar = ui.interact(arrow, egui::Id::new("attachment_arrow"), Sense::click());
        if enabled && resp.clicked() && !ar.hovered() {
            *act = Some("attachment".into());
        }
        let ctx = ui.ctx().clone();
        egui::Popup::menu(&ar).show(|ui| {
            for &(tb, label) in crate::plugin_host::builtin_lazy::ATTACHMENT_MENU_ITEMS {
                let icon = self
                    .builtin_plugin_button("ConstraintTool", Some(tb), Some("Place"))
                    .map(|b| b.icon.clone())
                    .unwrap_or_default();
                let item_on = self.action_state(&format!("constraint:{tb}")).1 == Some(true);
                ui.horizontal(|ui| {
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                    let p = ui.painter().clone();
                    if !self.paint_builtin_icon(&ctx, &p, ir, &icon, Color32::WHITE) {
                        rustudio_engine::ti::paint(&p, ir, "circle-dot", ribbon_icon_color("circle-dot"));
                    }
                    if ui.selectable_label(item_on, label).clicked() {
                        *act = Some(format!("constraint:{tb}"));
                        ui.close();
                    }
                });
            }
        });
    }

    /// Model > Constraint: Studio's ConstraintTool dropdown. Without Roblox's
    /// built-in plugins it lists the same classes natively (one insertion
    /// between the selected parts); with them, their own Studio PNG icons.
    fn constraint_button(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        let (enabled, active) = self.action_state("constraint");
        if !self.builtin_tools.plugins_enabled {
            let resp = self.rb_icon(ui, "link", None, "Constraint", active == Some(true), enabled);
            self.ui_rects.insert("model.constraint", resp.rect);
            let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
            rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
            egui::Popup::menu(&resp)
                .id(egui::Id::new(CONSTRAINT_MENU_ID))
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| {
                    egui::Grid::new("constraint_menu_grid")
                        .num_columns(4)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            let rows = CONSTRAINT_MENU_LEFT.len().max(CONSTRAINT_MENU_RIGHT.len());
                            for row in 0..rows {
                                constraint_menu_entry(ui, CONSTRAINT_MENU_LEFT.get(row).copied(), act);
                                constraint_menu_entry(ui, CONSTRAINT_MENU_RIGHT.get(row).copied(), act);
                                ui.end_row();
                            }
                        });
                });
            return;
        }
        if !enabled {
            let r = self.rb(ui, "link", "Constraint", "constraint", false, act);
            self.ui_rects.insert("model.constraint", r);
            return;
        }
        let active_icon = crate::plugin_host::builtin_lazy::CONSTRAINT_MENU_ITEMS
            .iter()
            .flatten()
            .find_map(|(tb, _)| {
                let b = self.builtin_plugin_button("ConstraintTool", Some(tb), Some("Place"))?;
                b.active.then(|| b.icon.clone())
            })
            .or_else(|| {
                self.builtin_plugin_button("ConstraintTool", Some("HingeConstraint"), Some("Place"))
                    .map(|b| b.icon.clone())
            });
        let resp = self.rb_icon(ui, "link", active_icon.as_deref(), "Constraint", active == Some(true), true);
        self.ui_rects.insert("model.constraint", resp.rect);
        let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
        rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
        let ctx = ui.ctx().clone();
        egui::Popup::menu(&resp)
            .id(egui::Id::new(CONSTRAINT_MENU_ID))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(168.0);
                for entry in crate::plugin_host::builtin_lazy::CONSTRAINT_MENU_ITEMS {
                    let Some((tb, label)) = *entry else {
                        ui.separator();
                        continue;
                    };
                    let Some(btn) = self.builtin_plugin_button("ConstraintTool", Some(tb), Some("Place")).cloned() else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        let (ir, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                        let p = ui.painter().clone();
                        if !self.paint_builtin_icon(&ctx, &p, ir, &btn.icon, Color32::WHITE) {
                            rustudio_engine::ti::paint(&p, ir, "link", ribbon_icon_color("link"));
                        }
                        if ui.selectable_label(btn.active, label).clicked() {
                            *act = Some(format!("constraint:{tb}"));
                            ui.close();
                        }
                    });
                }
            });
    }

    fn rb_class(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, action: &str, class: &str, act: &mut Option<String>) {
        let (enabled, _) = self.action_state(action);
        let resp = ribbon_button(ui, icon, label, false, enabled);
        if enabled {
            if resp.clicked() {
                *act = Some(format!("class:{class}"));
            }
        } else {
            resp.on_hover_text(format!("Select a GUI object to add a {class} to it"));
        }
    }

    /// A ribbon button whose drop-down inserts one of `classes` into the selection.
    fn class_menu_button(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, action: &str, classes: &[&str], act: &mut Option<String>) {
        let (enabled, _) = self.action_state(action);
        let resp = ribbon_button(ui, icon, label, false, enabled);
        if !enabled {
            resp.on_hover_text(format!("{label}: select where it goes first"));
            return;
        }
        egui::Popup::menu(&resp).show(|ui| {
            for c in classes {
                ui.horizontal(|ui| {
                    crate::icons::class_icon(ui, c, 16.0);
                    if ui.selectable_label(false, *c).clicked() {
                        *act = Some(format!("class:{c}"));
                        ui.close();
                    }
                });
            }
        });
    }

    /// Part: inserts a block; the arrow beside it lists the shapes (Studio).
    fn part_button(&mut self, ui: &mut egui::Ui, act: &mut Option<String>) {
        let enabled = self.action_state("part").0;
        let resp = ribbon_button(ui, "cube", "Part", false, enabled);
        self.ui_rects.insert("insert.part", resp.rect);
        self.hud.toolbar_buttons += 1;
        let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
        rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
        let ar = ui.interact(arrow, egui::Id::new("part_arrow"), Sense::click());
        if enabled && resp.clicked() && !ar.hovered() {
            *act = Some("part".into());
        }
        egui::Popup::menu(&ar).show(|ui| {
            for (label, a) in [
                ("Block", "part_block"),
                ("Sphere", "part_sphere"),
                ("Wedge", "part_wedge"),
                ("Corner Wedge", "part_corner"),
                ("Cylinder", "part_cylinder"),
            ] {
                if ui.button(label).clicked() {
                    *act = Some(a.into());
                    ui.close();
                }
            }
        });
    }

    /// The dragger settings drop-down, labelled with the movement mode
    /// (Studio's "Geometric ▾"): Dragger modes Geometric | Physical
    /// (DraggerService.DraggerMovementMode), Collisions (plugin.CollisionEnabled),
    /// Join Surfaces (plugin:GetJoinMode()), then Snap: Use Bounding Boxes
    /// (DraggerService.UseBoundingBoxes) and Snap to Parts (PartSnapEnabled).
    fn movement_mode_button(&mut self, ui: &mut egui::Ui) {
        let mode = if self.physical_drag { "Physical" } else { "Geometric" };
        let resp = ribbon_button(ui, if self.physical_drag { "atom-2" } else { "geometry" }, mode, false, true);
        self.ui_rects.insert("model.geometric", resp.rect);
        let arrow = Rect::from_min_size(resp.rect.right_top() + Vec2::new(-13.0, 18.0), Vec2::splat(12.0));
        rustudio_engine::ti::paint(ui.painter(), arrow, "chevron-down", theme::TEXT_DIM);
        let resp = resp.on_hover_text("Dragger settings: movement mode, collisions, joins and snapping");
        egui::Popup::menu(&resp)
            .id(egui::Id::new(DRAGGER_MENU_ID))
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| self.dragger_menu(ui));
    }

    fn dragger_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_width(186.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        let head = |ui: &mut egui::Ui, t: &str| {
            ui.label(egui::RichText::new(t).size(12.5).color(theme::TEXT));
        };
        ui.add_space(2.0);
        head(ui, "Dragger modes");
        // Segmented Geometric | Physical toggle.
        let (r, _) = ui.allocate_exact_size(Vec2::new(186.0, 26.0), Sense::hover());
        ui.painter().rect_filled(r, 4.0, Color32::from_rgb(30, 31, 35));
        ui.painter().rect_stroke(r, 4.0, egui::Stroke::new(1.0, Color32::from_rgb(70, 72, 78)), egui::StrokeKind::Inside);
        for (i, (label, physical)) in [("Geometric", false), ("Physical", true)].into_iter().enumerate() {
            let half = Rect::from_min_size(r.min + Vec2::new(i as f32 * r.width() / 2.0, 0.0), Vec2::new(r.width() / 2.0, r.height()));
            let resp = ui.interact(half, egui::Id::new(("dragger_mode", i)), Sense::click());
            self.ui_rects.insert(if physical { "dragger.physical" } else { "dragger.geometric" }, half);
            let on = self.physical_drag == physical;
            if on {
                ui.painter().rect_filled(half.shrink(2.0), 3.0, Color32::from_rgb(64, 66, 72));
            } else if resp.hovered() {
                ui.painter().rect_filled(half.shrink(2.0), 3.0, theme::HOVER_BG);
            }
            ui.painter().text(half.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(13.0),
                if on { Color32::WHITE } else { theme::TEXT });
            if resp.clicked() {
                self.physical_drag = physical;
            }
        }
        ui.add_space(2.0);
        let r = ui.checkbox(&mut self.collisions, "Collisions")
            .on_hover_text("Physical drags and handle moves / scales stop where parts would overlap");
        self.ui_rects.insert("dragger.collisions", r.rect);
        let r = ui.checkbox(&mut self.join_surfaces, "Join Surfaces")
            .on_hover_text("Dropped parts are welded to the parts they touch");
        self.ui_rects.insert("dragger.join", r.rect);
        ui.add_space(2.0);
        head(ui, "Snap");
        let r = ui.checkbox(&mut self.use_bounding_boxes, "Use Bounding Boxes")
            .on_hover_text("Drop onto the bounding box of the part under the mouse instead of its shape");
        self.ui_rects.insert("dragger.bbox", r.rect);
        let r = ui.checkbox(&mut self.part_snap, "Snap to Parts")
            .on_hover_text("Snap the dragged selection's edges and centre to the target's (Shift+C)");
        self.ui_rects.insert("dragger.part_snap", r.rect);
        ui.add_space(2.0);
    }

    /// Studio's snap controls: a check box and an increment for moves, then
    /// for rotations. Studio turns a snap on when its increment is changed to
    /// more than zero.
    fn snap_widget(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.game.snap_move, "").on_hover_text("Snap moves to the grid");
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                rustudio_engine::ti::paint(ui.painter(), r, "arrows-move", theme::TEXT_DIM);
                let dv = egui::DragValue::new(&mut self.game.move_inc).range(0.01..=512.0).speed(0.1).suffix(" studs");
                if ui.add(dv).changed() && self.game.move_inc > 0.0 {
                    self.game.snap_move = true;
                }
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.game.snap_rotate, "").on_hover_text("Snap rotation");
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                rustudio_engine::ti::paint(ui.painter(), r, "rotate", theme::TEXT_DIM);
                let dv = egui::DragValue::new(&mut self.game.rotate_inc).range(0.1..=180.0).speed(0.5).suffix("°");
                if ui.add(dv).changed() && self.game.rotate_inc > 0.0 {
                    self.game.snap_rotate = true;
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    /// Every icon the top row and the ribbon name exists in the bundled set.
    #[test]
    fn icons_exist() {
        let all = include_str!("studio_ribbon.rs");
        // (the code above this test module, not the test's own strings)
        let src = &all[..all.find("#[cfg(test)]").unwrap_or(all.len())];
        let quoted: Vec<&str> = src.split('"').collect();
        let mut missing = Vec::new();
        for w in quoted.windows(2) {
            let calls = ["rb(ui, ", "top_icon(ui, ", "class_menu_button(ui, ", "rb_class(ui, ", "ribbon_button(ui, "];
            if calls.iter().any(|p| w[0].ends_with(p)) && !rustudio_engine::ti::has(w[1]) {
                missing.push(w[1]);
            }
        }
        for i in ["pointer", "arrows-move", "resize", "rotate-clockwise-2", "user", "chevron-down", "rotate", "check"] {
            if !rustudio_engine::ti::has(i) {
                missing.push(i);
            }
        }
        assert!(missing.is_empty(), "icons not in Tabler: {missing:?}");
    }
}
