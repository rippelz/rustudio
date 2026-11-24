use super::{Editor, RustudioApp, Tab, Viewer};
use crate::studio_ui::RibbonTab;
use egui::scroll_area::{DragScroll, ScrollBarVisibility, ScrollSource};
use egui::{Context, Id, RichText, Ui};
use egui_dock::TabViewer;
use rbx_dom_weak::types::Ref;
use rustudio_core::mech::MOBILE_SCRIPT_SHEET_FRACTION;
use rustudio_engine::{theme, ti};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Sheet {
    #[default]
    Script,
    Panel(Tab),
}

pub(super) struct Workspace {
    sheet: Sheet,
    visible: bool,
    expanded: bool,
    ribbon_collapsed: bool,
    docked: Vec<Tab>,
    script: Option<Ref>,
    scripts: Vec<Ref>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dock {
    Left,
    Right,
    Bottom,
}

impl Dock {
    fn of(tab: Tab) -> Option<Self> {
        match tab {
            Tab::Toolbox | Tab::TerrainEditor => Some(Self::Left),
            Tab::Explorer | Tab::Properties | Tab::FileSync => Some(Self::Right),
            Tab::Output | Tab::Profiler | Tab::AvatarSettings => Some(Self::Bottom),
            _ => None,
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            sheet: Sheet::default(),
            visible: false,
            expanded: false,
            ribbon_collapsed: false,
            docked: vec![Tab::Explorer],
            script: None,
            scripts: Vec::new(),
        }
    }
}

const PANELS: [Tab; 8] = [
    Tab::Explorer,
    Tab::Properties,
    Tab::Output,
    Tab::Toolbox,
    Tab::TerrainEditor,
    Tab::FileSync,
    Tab::AvatarSettings,
    Tab::Profiler,
];

impl Workspace {
    pub(super) fn reset_scripts(&mut self, ctx: &Context) {
        self.close(ctx);
        self.scripts.clear();
        self.script = None;
    }

    fn release_script_focus(&self, ctx: &Context) {
        if let Some(script) = self.script {
            ctx.memory_mut(|m| m.surrender_focus(Id::new(("script_text", script))));
        }
    }

    fn show(&mut self, ctx: &Context, sheet: Sheet) {
        if let Sheet::Panel(tab) = sheet {
            if let Some(dock) = Dock::of(tab) {
                if dock == Dock::Bottom || self.expanded || self.editing_script(ctx) {
                    self.close(ctx);
                }
                self.activate_panel(tab);
                return;
            }
        }
        if sheet != self.sheet {
            self.release_script_focus(ctx);
            self.expanded = false;
        }
        self.sheet = sheet;
        self.visible = true;
    }

    fn close(&mut self, ctx: &Context) {
        self.release_script_focus(ctx);
        self.visible = false;
        self.expanded = false;
    }

    fn sync_scripts(&mut self, ed: &mut Editor, ctx: &Context) {
        ed.file_sync_open_external();
        for script in std::mem::take(&mut ed.close_script) {
            self.scripts.retain(|&r| r != script);
            if self.script == Some(script) {
                self.release_script_focus(ctx);
                self.script = self.scripts.last().copied();
            }
        }
        for script in std::mem::take(&mut ed.open_script) {
            if !self.scripts.contains(&script) {
                self.scripts.push(script);
            }
            if self.script != Some(script) {
                self.release_script_focus(ctx);
            }
            self.script = Some(script);
            self.show(ctx, Sheet::Script);
        }
        crate::plugin_host::script_docs::sync_tabs(&self.scripts);
        crate::plugin_host::script_docs::set_active(
            self.script
                .filter(|_| self.visible && self.sheet == Sheet::Script),
        );
    }

    fn editing_script(&self, ctx: &Context) -> bool {
        self.visible
            && self.sheet == Sheet::Script
            && self
                .script
                .is_some_and(|r| ctx.memory(|m| m.has_focus(Id::new(("script_text", r)))))
    }

    fn header(&mut self, ed: &mut Editor, ui: &mut Ui, ctx: &Context) {
        let playing = ed.game.is_playing();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.menu_button(ti::text("menu-2", 18.0), |ui| ed.mobile_menu(ui))
                .response
                .on_hover_text("Menu");
            if ui
                .add(egui::Button::selectable(
                    !self.visible,
                    labeled("device-desktop", "Viewport"),
                ))
                .clicked()
            {
                self.close(ctx);
            }
            if ui
                .add(egui::Button::selectable(
                    self.visible && self.sheet == Sheet::Script,
                    labeled("file-code", "Editor"),
                ))
                .clicked()
            {
                self.show(ctx, Sheet::Script);
            }
            let (play_icon, play_label) = if playing {
                ("player-stop-filled", "Stop")
            } else {
                ("player-play-filled", "Play")
            };
            let play =
                ui.add(egui::Button::new(labeled(play_icon, play_label)).fill(theme::ACTIVE_TAB));
            ed.ui_rects
                .insert(if playing { "test.stop" } else { "test.play" }, play.rect);
            if play.clicked() {
                self.close(ctx);
                if playing {
                    ed.stop_play();
                } else {
                    ed.start_run(ed.ui.run_type);
                }
            }
            ui.menu_button(ti::text("layout-sidebar", 18.0), |ui| {
                for panel in PANELS {
                    if ui.button(panel.title()).clicked() {
                        self.show(ctx, Sheet::Panel(panel));
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Panels");
            if !playing {
                ui.menu_button(
                    ti::text("camera", 18.0),
                    crate::mobile::edit_camera_controls,
                )
                .response
                .on_hover_text("Camera");
            }
            if self.ribbon_visible(ctx) {
                ui.separator();
                for (tab, icon) in [
                    (RibbonTab::Home, "home"),
                    (RibbonTab::Model, "box"),
                    (RibbonTab::Avatar, "user"),
                    (RibbonTab::Ui, "app-window"),
                    (RibbonTab::Script, "file-code"),
                    (RibbonTab::Plugins, "puzzle"),
                ] {
                    if ui
                        .add(egui::Button::selectable(
                            ed.ui.tab == tab,
                            labeled(icon, tab.label()),
                        ))
                        .clicked()
                    {
                        ed.ui.tab = tab;
                    }
                }
            }
            ui.label(RichText::new(&ed.game.doc.title).color(theme::TEXT_DIM));
        });
    }

    fn ribbon_visible(&self, ctx: &Context) -> bool {
        !self.ribbon_collapsed && !self.editing_script(ctx)
    }

    fn ribbon(&mut self, ed: &mut Editor, ui: &mut Ui, ctx: &Context) {
        let row_height = ui.spacing().interact_size.y;
        let spacing = ui.spacing().item_spacing.y;
        // The snap controls stack two touch targets with padding between them.
        let tools_height = row_height * 2.0 + spacing * 3.0;
        let frame = bar_frame();
        let show_tools = self.ribbon_visible(ctx);
        let panel_height = row_height
            + frame.total_margin().sum().y
            + if show_tools {
                tools_height + spacing * 3.0
            } else {
                0.0
            };
        egui::Panel::top("mobile_ribbon")
            .resizable(false)
            .exact_size(panel_height)
            .frame(frame)
            .show(ui, |ui| {
                // A centered horizontal layout needs a finite height; using
                // the parent height can put the navigation in the viewport.
                let row_size = egui::vec2(ui.available_width(), row_height);
                ui.allocate_ui_with_layout(
                    row_size,
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let editing = self.editing_script(ctx);
                        let (icon, label) = if self.ribbon_visible(ctx) {
                            ("chevron-up", "Minimize ribbon")
                        } else {
                            ("chevron-down", "Show ribbon")
                        };
                        let toggle = ui
                            .add_enabled(!editing, egui::Button::new(ti::text(icon, 18.0)))
                            .on_hover_text(label);
                        toggle.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, !editing, label)
                        });
                        if toggle.clicked() {
                            self.ribbon_collapsed = !self.ribbon_collapsed;
                            ctx.request_repaint();
                        }
                        ribbon_scroll()
                            .id_salt("mobile_ribbon_navigation")
                            .max_height(row_height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        self.header(ed, ui, ctx);
                                    },
                                );
                            });
                    },
                );
                if show_tools {
                    ui.separator();
                    let tool = ed.tool;
                    ribbon_scroll()
                        .id_salt(("mobile_ribbon_tools", ed.ui.tab.label()))
                        .max_height(tools_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| ed.studio_ribbon(ui));
                    if ed.tool != tool {
                        crate::mobile::select_edit_mode();
                        self.close(ctx);
                    }
                }
            });
    }

    fn activate_panel(&mut self, tab: Tab) {
        self.docked.retain(|&open| open != tab);
        self.docked.push(tab);
    }

    fn panel_open(&self, tab: Tab) -> bool {
        self.docked.contains(&tab) || self.visible && self.sheet == Sheet::Panel(tab)
    }

    fn dock(&mut self, ed: &mut Editor, ui: &mut Ui, side: Dock) {
        let mut tabs: Vec<_> = self
            .docked
            .iter()
            .copied()
            .filter(|&t| Dock::of(t) == Some(side))
            .collect();
        let Some(mut active) = tabs.last().copied() else {
            return;
        };
        let space = ui.available_size();
        let row = ui.spacing().interact_size;
        if space.y < row.y * 3.0 || space.x < row.x * 5.0 {
            return;
        }

        let panel = match side {
            Dock::Right => {
                let preferred = space.x * crate::panels::placement("explorer").size;
                let width = preferred.max(row.x * 5.0).min(space.x / 3.0);
                egui::Panel::right("mobile_right_dock").exact_size(width)
            }
            Dock::Left => {
                let preferred = space.x * crate::panels::placement("toolbox").size;
                let width = preferred.max(row.x * 6.0).min(space.x / 3.0);
                egui::Panel::left("mobile_left_dock").exact_size(width)
            }
            // The reference sketch splits the central column in half.
            Dock::Bottom => egui::Panel::bottom("mobile_bottom_dock").exact_size(space.y / 2.0),
        };
        let frame = egui::Frame::new()
            .fill(theme::PANEL)
            .inner_margin(egui::Margin::symmetric(6, 2));
        panel.resizable(false).frame(frame).show(ui, |ui| {
            tabs.sort_by_key(|tab| PANELS.iter().position(|t| t == tab));
            let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
            let header_height = (row.y * 0.8).min(rect.height());
            let header_rect =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), header_height));
            let mut header = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("dock_header")
                    .max_rect(header_rect)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            header.set_clip_rect(header_rect);
            let spacing = header.spacing_mut();
            spacing.interact_size = egui::Vec2::splat(header_height);
            spacing.button_padding = egui::vec2(5.0, 2.0);
            spacing.item_spacing = egui::vec2(4.0, 2.0);

            if header
                .button(ti::text("x", 13.0))
                .on_hover_text("Close panel")
                .clicked()
            {
                self.docked.retain(|&open| open != active);
            }
            ribbon_scroll()
                .id_salt("panel_tabs")
                .max_height(header_height)
                .auto_shrink([false, true])
                .show(&mut header, |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        for tab in tabs {
                            let title = RichText::new(tab.title()).small();
                            if ui.selectable_label(tab == active, title).clicked() {
                                self.activate_panel(tab);
                                active = tab;
                            }
                        }
                    });
                });

            let mut body_rect = rect;
            body_rect.min.y = (header_rect.bottom() + 4.0).min(rect.bottom());
            ui.painter().hline(
                rect.x_range(),
                header_rect.bottom() + 1.0,
                ui.visuals().widgets.noninteractive.bg_stroke,
            );
            // Let long panel contents scroll or clip within their own rect.
            // Their minimum size must not move the console/viewport boundary.
            let mut body = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("dock_body", active.key()))
                    .max_rect(body_rect),
            );
            body.set_clip_rect(body_rect);
            if body_rect.is_positive() {
                Viewer {
                    ed,
                    window: None,
                    windows: &[],
                }
                .ui(&mut body, &mut active);
            }
            ui.advance_cursor_after_rect(rect);
        });
    }

    fn sheet_content(&mut self, ed: &mut Editor, ui: &mut Ui, ctx: &Context) {
        let touch_height = ui.spacing().interact_size.y;
        let (grip, drag) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), touch_height * 0.45),
            egui::Sense::drag(),
        );
        ui.painter().hline(
            grip.center().x - 14.0..=grip.center().x + 14.0,
            grip.center().y,
            egui::Stroke::new(2.0, theme::TEXT_DIM),
        );
        if let Some(delta) = drag.total_drag_delta() {
            if delta.y > touch_height {
                self.close(ctx);
            } else if delta.y < -touch_height {
                self.expanded = true;
            }
        }
        let script = self.script.filter(|_| self.sheet == Sheet::Script);
        let title = match self.sheet {
            Sheet::Script => "Script",
            Sheet::Panel(panel) => panel.title(),
        };
        ui.horizontal(|ui| {
            if ui
                .button(ti::text("x", 17.0))
                .on_hover_text("Close panel")
                .clicked()
            {
                self.close(ctx);
            }
            if ui
                .button(if self.expanded {
                    labeled("arrows-minimize", "Restore")
                } else {
                    labeled("arrows-maximize", "Expand")
                })
                .clicked()
            {
                self.expanded = !self.expanded;
            }
            if let Some(script) = script {
                self.script_controls(ed, ui, ctx, script);
            } else {
                ui.label(RichText::new(title).strong());
            }
        });
        ui.separator();
        match self.sheet {
            Sheet::Script => {
                if let Some(script) = self.script {
                    ed.script_tab_ui(ui, script);
                } else if ui.button("Browse scripts in Explorer").clicked() {
                    self.show(ctx, Sheet::Panel(Tab::Explorer));
                }
            }
            Sheet::Panel(mut panel) => Viewer {
                ed,
                window: None,
                windows: &[],
            }
            .ui(ui, &mut panel),
        }
    }

    fn script_controls(&mut self, ed: &mut Editor, ui: &mut Ui, ctx: &Context, script: Ref) {
        let id = Id::new(("script_text", script));
        let editing = ctx.memory(|m| m.has_focus(id));
        if ui
            .add(egui::Button::selectable(
                editing,
                labeled("keyboard", "Keyboard"),
            ))
            .clicked()
        {
            ctx.memory_mut(|m| {
                if editing {
                    m.surrender_focus(id);
                } else {
                    m.request_focus(id);
                }
            });
            ctx.request_repaint();
        }
        if ui
            .button(ti::text("x", 15.0))
            .on_hover_text("Close script")
            .clicked()
        {
            ed.close_script.push(script);
            ctx.request_repaint();
        }
        if self.scripts.len() == 1 {
            ui.add(egui::Label::new(ed.script_title(script)).truncate());
            return;
        }
        egui::ComboBox::from_id_salt("mobile_scripts")
            .selected_text(ed.script_title(script))
            .truncate()
            .show_ui(ui, |ui| {
                for candidate in &self.scripts {
                    if ui
                        .selectable_label(script == *candidate, ed.script_title(*candidate))
                        .clicked()
                    {
                        self.release_script_focus(ctx);
                        self.script = Some(*candidate);
                    }
                }
            });
    }
}

fn labeled<'a>(icon: &str, label: &'a str) -> (RichText, &'a str) {
    (ti::text(icon, 16.0), label)
}

fn ribbon_scroll() -> egui::ScrollArea {
    egui::ScrollArea::horizontal()
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .scroll_source(ScrollSource {
            scroll_bar: false,
            drag: DragScroll::Always,
            mouse_wheel: true,
        })
}

fn bar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PANEL)
        .inner_margin(egui::Margin::symmetric(8, 3))
}

impl RustudioApp {
    pub(super) fn mobile_workspace_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let ed = &mut self.ed;
        let mobile = &mut self.mobile_workspace;
        mobile.sync_scripts(ed, &ctx);
        if std::mem::take(&mut ed.switch_to_viewport) {
            mobile.close(&ctx);
        }

        mobile.ribbon(ed, ui, &ctx);

        let workspace_rect = ui.available_rect_before_wrap();
        let editing = mobile.editing_script(&ctx);
        let full_editor = mobile.visible && (mobile.expanded || editing);
        if !full_editor {
            // Side panels span the space below the ribbon. The console only
            // takes height from the central column, as in the layout sketch.
            mobile.dock(ed, ui, Dock::Right);
            mobile.dock(ed, ui, Dock::Left);
            if !mobile.visible {
                mobile.dock(ed, ui, Dock::Bottom);
            }
        }
        let height = ui.available_height();
        let fraction = if full_editor {
            1.0
        } else {
            MOBILE_SCRIPT_SHEET_FRACTION
        };
        let open = mobile.visible;
        let mut animated_open = open;
        if height > 1.0 {
            egui::Panel::bottom("mobile_workspace_sheet")
                .resizable(false)
                .exact_size(height * fraction)
                .frame(bar_frame())
                .show_collapsible(ui, &mut animated_open, |ui| {
                    mobile.sheet_content(ed, ui, &ctx)
                });
        }

        ed.game.gui.mobile_play_controls = true;
        let mut viewport_visible = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                if ui.available_height() >= ui.spacing().interact_size.y
                    && ui.available_width() >= 1.0
                {
                    viewport_visible = true;
                    ed.viewport_ui(ui);
                }
            });
        if !viewport_visible {
            crate::mobile::note_view_rect(egui::Rect::NOTHING);
        }
        crate::mobile::hud(&ctx, &mut ed.game);
        for (tab, want) in std::mem::take(&mut ed.ui.panel_requests) {
            if let Tab::Script(script) = tab {
                if want == Some(false) {
                    ed.close_script.push(script);
                } else {
                    ed.open_script.push(script);
                }
                continue;
            }
            let current = mobile.panel_open(tab);
            if want.unwrap_or(!current) {
                mobile.show(&ctx, Sheet::Panel(tab));
            } else if Dock::of(tab).is_some() {
                mobile.docked.retain(|&open| open != tab);
            } else if current {
                mobile.close(&ctx);
            }
        }
        ed.ui.panels_open = PANELS
            .into_iter()
            .filter(|&tab| mobile.panel_open(tab))
            .map(Tab::title)
            .collect();
        if !ed.ui.panels_open.contains("MicroProfiler") {
            ed.profiler = None;
        }
        ed.ui.viewport_hidden = !viewport_visible;
        ed.ui.viewport_group.clear();
        ed.ui.viewport_group.push(Tab::Viewport);
        ed.ui
            .viewport_group
            .extend(mobile.scripts.iter().copied().map(Tab::Script));
        ed.ui.popped.clear();
        crate::plugin_host::widgets_ui::set_dock_area(workspace_rect);
        ed.plugin_ui_note_open(|tab| mobile.visible && mobile.sheet == Sheet::Panel(*tab));
        ed.plugin_widget_shots(&ctx);
    }
}
