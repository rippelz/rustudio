//! Drawing the Asset Manager panel 1:1 with Roblox Studio's modern Asset
//! Manager, in rustudio's dark Studio theme (`theme.rs`) and Tabler icons.

use egui::{Align2, Color32, FontFamily, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use rbx_dom_weak::types::Ref;

use super::{
    Action, AssetEntry, AssetKind, AssetManager, Folder, FolderDialog, ImportStatus, OwnershipFilter, Scope, SortBy,
    SortDir, ViewMode,
};
use crate::toolbox::thumbs::Thumb;
use crate::toolbox::ui::ToolboxDrag;
use rustudio_engine::theme;

/// The Asset Manager search box's narrowest width.
const SEARCH_MIN_W: f32 = 110.0;

const TILE: Color32 = Color32::from_rgb(44, 45, 51);
const TILE_HOVER: Color32 = Color32::from_rgb(56, 58, 66);
const ROW_ALT: Color32 = Color32::from_rgb(29, 30, 36);
const CHIP_BG: Color32 = Color32::from_rgb(48, 50, 58);
const LINK: Color32 = Color32::from_rgb(82, 156, 255);
const GOOD: Color32 = Color32::from_rgb(57, 196, 104);
const FOLDER_GOLD: Color32 = Color32::from_rgb(228, 179, 76);

/// Drag payload for moving an asset into a folder inside Asset Manager.
#[derive(Debug, Clone, Copy)]
pub struct FolderAssetDrag(pub u64);

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}

fn icon(p: &egui::Painter, r: Rect, name: &str, c: Color32) {
    rustudio_engine::ti::paint(p, r, name, c);
}

fn kind_color(k: AssetKind) -> Color32 {
    match k {
        AssetKind::Model => Color32::from_rgb(100, 176, 255),
        AssetKind::MeshPart => Color32::from_rgb(120, 210, 190),
        AssetKind::Image => Color32::from_rgb(245, 178, 72),
        AssetKind::Audio => Color32::from_rgb(176, 132, 255),
        AssetKind::Animation => Color32::from_rgb(95, 208, 128),
        AssetKind::Package => Color32::from_rgb(235, 130, 95),
        AssetKind::Video => Color32::from_rgb(240, 110, 150),
        AssetKind::Font => Color32::from_rgb(200, 200, 215),
        AssetKind::Plugin => Color32::from_rgb(115, 190, 255),
        AssetKind::Place => Color32::from_rgb(82, 156, 255),
    }
}

fn icon_btn(ui: &mut egui::Ui, name: &str, size: f32, active: bool, enabled: bool, tip: &str) -> egui::Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), sense);
    let bg = if active {
        theme::SELECT_BG
    } else if enabled && resp.hovered() {
        TILE_HOVER
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(r, 4.0, bg);
    }
    let fg = if !enabled {
        theme::TEXT_DIM.gamma_multiply(0.55)
    } else if active {
        Color32::WHITE
    } else {
        theme::TEXT
    };
    icon(ui.painter(), r.shrink(size * 0.2), name, fg);
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

fn dark_popup(ui: &egui::Ui, id: &str, pos: Pos2, add: impl FnOnce(&mut egui::Ui)) {
    let frame = egui::Frame::new()
        .fill(theme::PANEL_DARK)
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .corner_radius(6.0)
        .inner_margin(8.0);
    let _ = egui::Popup::new(ui.id().with(id), ui.ctx().clone(), pos, ui.layer_id())
        .kind(egui::PopupKind::Menu)
        .align_alternatives(&[])
        .frame(frame)
        .show(|ui| {
            ui.set_style(ui.ctx().style_of(egui::Theme::Dark));
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
            add(ui);
        });
}

fn format_short_date(iso: &str) -> &str {
    if iso.len() >= 10 { &iso[..10] } else { iso }
}

fn format_bytes(b: u64) -> String {
    if b >= 1_048_576 {
        format!("{:.1} MB", b as f64 / 1_048_576.0)
    } else if b >= 1024 {
        format!("{:.0} KB", b as f64 / 1024.0)
    } else {
        format!("{b} B")
    }
}

/// Context passed from `Editor` so `ui.rs` stays decoupled from `Editor` internals.
pub struct DrawCtx<'a> {
    pub project_name: &'a str,
    pub universe_id: Option<u64>,
    pub place_id: Option<u64>,
    pub instance_name: &'a dyn Fn(Ref) -> String,
}

impl AssetManager {
    pub fn show(&mut self, ui: &mut egui::Ui, dctx: &DrawCtx<'_>) {
        // The experience's places load whatever the scope (one request): the
        // sidebar's Places count read 0 until Places was opened.
        if let Some(u) = dctx.universe_id {
            self.ensure_places(u, false);
        }
        // Ensure cloud data is requested for the active scope when signed in.
        match &self.persist.scope {
            Scope::Places => {}
            Scope::MyInventory { .. } => self.ensure_inventory("user", None, false),
            Scope::Group { group_id, .. } => {
                let gid = *group_id;
                let key = format!("group:{gid}");
                self.ensure_inventory(&key, Some(gid), false);
            }
            Scope::Experience | Scope::RecentUploads => {}
        }

        // If user typed a numeric ID or rbxassetid:// URI, trigger a lookup.
        if let Some(id) = super::parse_asset_id(&self.query) {
            self.lookup_asset_id(id);
        }

        let total_rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(total_rect, 0.0, theme::PANEL);

        let visible = self.visible_entries();

        // Top toolbar (mezzanine).
        let toolbar_h = 34.0;
        let toolbar_rect = Rect::from_min_size(total_rect.min, Vec2::new(total_rect.width(), toolbar_h));
        self.draw_toolbar(ui, toolbar_rect, dctx);

        let mut content_top = toolbar_rect.bottom();

        // Active filter chips strip (only when filters are active).
        if self.active_filter_count() > 0 && !matches!(self.persist.scope, Scope::Places) {
            let chips_h = 28.0;
            let chips_rect = Rect::from_min_size(Pos2::new(total_rect.min.x, content_top), Vec2::new(total_rect.width(), chips_h));
            self.draw_filter_chips(ui, chips_rect);
            content_top = chips_rect.bottom();
        }

        // Bottom status bar.
        let status_h = 24.0;
        let status_rect = Rect::from_min_size(
            Pos2::new(total_rect.min.x, (total_rect.max.y - status_h).max(content_top)),
            Vec2::new(total_rect.width(), status_h),
        );

        let body_rect = Rect::from_min_max(Pos2::new(total_rect.min.x, content_top), Pos2::new(total_rect.max.x, status_rect.min.y));

        // Adaptive sidebar & details drawer layout.
        let show_sidebar = self.persist.sidebar_open && body_rect.width() >= 420.0;
        let sidebar_w = if show_sidebar {
            self.persist.sidebar_width.clamp(140.0, (body_rect.width() * 0.42).max(140.0))
        } else {
            0.0
        };
        let show_details = self.persist.details_open && body_rect.width() - sidebar_w >= 460.0;
        let details_w = if show_details { 244.0 } else { 0.0 };

        if show_sidebar {
            let side_rect = Rect::from_min_size(body_rect.min, Vec2::new(sidebar_w, body_rect.height()));
            self.draw_sidebar(ui, side_rect, dctx);

            // Draggable splitter between sidebar and content.
            let split_rect = Rect::from_min_size(Pos2::new(side_rect.max.x - 2.0, body_rect.min.y), Vec2::new(5.0, body_rect.height()));
            let split_resp = ui.interact(split_rect, ui.id().with("am_side_split"), Sense::drag());
            if split_resp.hovered() || split_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if split_resp.dragged() {
                self.persist.sidebar_width = (self.persist.sidebar_width + split_resp.drag_delta().x).clamp(140.0, 340.0);
                self.mark_dirty();
            }
            ui.painter().line_segment(
                [Pos2::new(side_rect.max.x, body_rect.min.y), Pos2::new(side_rect.max.x, body_rect.max.y)],
                Stroke::new(1.0, if split_resp.hovered() || split_resp.dragged() { theme::ACCENT } else { theme::DIVIDER }),
            );
        }

        let main_rect = Rect::from_min_max(
            Pos2::new(body_rect.min.x + sidebar_w + if show_sidebar { 1.0 } else { 0.0 }, body_rect.min.y),
            Pos2::new(body_rect.max.x - details_w, body_rect.max.y),
        );
        self.draw_main(ui, main_rect, &visible, dctx);

        if show_details {
            let det_rect = Rect::from_min_max(Pos2::new(main_rect.max.x, body_rect.min.y), body_rect.max);
            ui.painter().line_segment(
                [Pos2::new(det_rect.min.x, det_rect.min.y), Pos2::new(det_rect.min.x, det_rect.max.y)],
                Stroke::new(1.0, theme::DIVIDER),
            );
            self.draw_details(ui, det_rect, &visible, dctx);
        }

        self.draw_status_bar(ui, status_rect, visible.len());

        // Allocate the whole rect so parent scroll/dock stays clean.
        ui.allocate_rect(total_rect, Sense::hover());

        // Modals (Folder Create/Rename & Bulk Import).
        self.draw_folder_dialog(ui.ctx());
        self.draw_import_modal(ui.ctx());
    }

    // ------------------------------------------------------------------ toolbar

    fn draw_toolbar(&mut self, ui: &mut egui::Ui, rect: Rect, dctx: &DrawCtx<'_>) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_DARK);
        ui.painter().line_segment(
            [Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)],
            Stroke::new(1.0, theme::DIVIDER),
        );

        let inner = rect.shrink2(Vec2::new(6.0, 4.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
        child.spacing_mut().item_spacing.x = 4.0;

        // Sidebar toggle.
        let side_icon = if self.persist.sidebar_open { "layout-sidebar-left-collapse" } else { "layout-sidebar-left-expand" };
        if icon_btn(&mut child, side_icon, 24.0, false, true, "Toggle Sidebar").clicked() {
            self.persist.sidebar_open = !self.persist.sidebar_open;
            self.mark_dirty();
        }

        // Back / Forward.
        if icon_btn(&mut child, "chevron-left", 22.0, false, !self.back.is_empty(), "Back").clicked() {
            self.go_back();
        }
        if icon_btn(&mut child, "chevron-right", 22.0, false, !self.forward.is_empty(), "Forward").clicked() {
            self.go_forward();
        }

        // Breadcrumbs get what the right-hand controls leave (six 24 px
        // buttons, Import, the search box at its narrowest), clipped there,
        // and none when that's too little to read.
        let compact = child.available_width() < 520.0;
        let import_w = if compact { 26.0 } else { 72.0 };
        let right_w = 6.0 * 24.0 + import_w + 8.0 * 4.0 + SEARCH_MIN_W;
        let room = child.available_width() - right_w - 6.0;
        if room >= 48.0 {
            let h = child.available_height();
            child.allocate_ui_with_layout(Vec2::new(room, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_max_width(room);
                let clip = ui.max_rect().intersect(ui.clip_rect());
                ui.set_clip_rect(clip);
                ui.spacing_mut().item_spacing.x = 4.0;
                self.draw_breadcrumbs(ui, dctx);
            });
        }

        // Right-aligned controls + flexible search bar.
        child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            // Refresh.
            if icon_btn(ui, "refresh", 24.0, false, true, "Refresh").clicked() {
                self.refresh_current(dctx.universe_id);
            }

            // Details drawer toggle.
            if icon_btn(ui, "info-circle", 24.0, self.persist.details_open, true, "Toggle Asset Details").clicked() {
                self.persist.details_open = !self.persist.details_open;
                self.mark_dirty();
            }

            // Grid / List segmented toggle.
            let is_list = self.persist.view == ViewMode::List;
            if icon_btn(ui, "list", 24.0, is_list, true, "List View").clicked() {
                self.persist.view = ViewMode::List;
                self.mark_dirty();
            }
            if icon_btn(ui, "layout-grid", 24.0, !is_list, true, "Grid View").clicked() {
                self.persist.view = ViewMode::Grid;
                self.mark_dirty();
            }

            // Sort button + popup.
            let sort_resp = icon_btn(ui, "arrows-sort", 24.0, self.sort_popup, true, "Sort");
            if sort_resp.clicked() {
                self.sort_popup = !self.sort_popup;
                self.filter_popup = false;
            }
            if self.sort_popup {
                let pos = Pos2::new((sort_resp.rect.right() - 170.0).max(4.0), sort_resp.rect.bottom() + 4.0);
                let mut close = false;
                dark_popup(ui, "am_sort_pop", pos, |ui| {
                    ui.set_min_width(160.0);
                    ui.label(RichText::new("Sort By").font(bold(11.5)).color(theme::TEXT_DIM));
                    ui.add_space(4.0);
                    for s in SortBy::ALL {
                        let sel = self.persist.sort_by == s;
                        let r = ui.selectable_label(sel, s.label());
                        if r.clicked() {
                            if sel {
                                self.persist.sort_dir = self.persist.sort_dir.flip();
                            } else {
                                self.persist.sort_by = s;
                            }
                            self.mark_dirty();
                            close = true;
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.persist.sort_dir == SortDir::Asc, "↑ Ascending").clicked() {
                            self.persist.sort_dir = SortDir::Asc;
                            self.mark_dirty();
                            close = true;
                        }
                        if ui.selectable_label(self.persist.sort_dir == SortDir::Desc, "↓ Descending").clicked() {
                            self.persist.sort_dir = SortDir::Desc;
                            self.mark_dirty();
                            close = true;
                        }
                    });
                });
                if close || (ui.input(|i| i.pointer.any_click()) && !sort_resp.hovered() && !ui.ctx().is_pointer_over_egui()) {
                    self.sort_popup = false;
                }
            }

            // Filter button + active blue dot + popup.
            let active_n = self.active_filter_count();
            let filter_resp = icon_btn(ui, "filter", 24.0, self.filter_popup || active_n > 0, true, "Filter Assets");
            if active_n > 0 {
                ui.painter().circle_filled(
                    Pos2::new(filter_resp.rect.right() - 4.0, filter_resp.rect.top() + 4.0),
                    3.5,
                    theme::SELECT,
                );
            }
            if filter_resp.clicked() {
                self.filter_popup = !self.filter_popup;
                self.sort_popup = false;
            }
            if self.filter_popup {
                let pos = Pos2::new((filter_resp.rect.right() - 200.0).max(4.0), filter_resp.rect.bottom() + 4.0);
                dark_popup(ui, "am_filter_pop", pos, |ui| {
                    ui.set_min_width(190.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Filters").font(bold(12.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if active_n > 0 && ui.link(RichText::new("Clear all").size(11.5).color(LINK)).clicked() {
                                self.persist.type_filters.clear();
                                self.persist.ownership = OwnershipFilter::All;
                                self.mark_dirty();
                            }
                        });
                    });
                    if matches!(self.persist.scope, Scope::MyInventory { .. } | Scope::Group { .. }) {
                        ui.separator();
                        ui.label(RichText::new("Source").size(11.0).color(theme::TEXT_DIM));
                        for own in [OwnershipFilter::All, OwnershipFilter::Creations, OwnershipFilter::Inventory] {
                            if ui.radio(self.persist.ownership == own, own.label()).clicked() {
                                self.persist.ownership = own;
                                self.mark_dirty();
                            }
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("Asset Type").size(11.0).color(theme::TEXT_DIM));
                    for kind in AssetKind::FILTERABLE {
                        let mut on = self.persist.type_filters.contains(&kind);
                        if ui.checkbox(&mut on, kind.plural()).changed() {
                            if on {
                                self.persist.type_filters.push(kind);
                            } else {
                                self.persist.type_filters.retain(|k| *k != kind);
                            }
                            self.mark_dirty();
                        }
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.filter_popup = false;
                }
            }

            // Import button (Studio primary accent pill).
            let import_size = Vec2::new(import_w, 24.0);
            let (imp_r, imp_resp) = ui.allocate_exact_size(import_size, Sense::click());
            let imp_bg = if imp_resp.hovered() {
                Color32::from_rgb(70, 114, 255)
            } else {
                theme::ACCENT
            };
            ui.painter().rect_filled(imp_r, 4.0, imp_bg);
            if compact {
                icon(ui.painter(), Rect::from_center_size(imp_r.center(), Vec2::splat(14.0)), "upload", Color32::WHITE);
            } else {
                icon(ui.painter(), Rect::from_min_size(imp_r.min + Vec2::new(7.0, 5.0), Vec2::splat(14.0)), "upload", Color32::WHITE);
                ui.painter().text(
                    Pos2::new(imp_r.min.x + 25.0, imp_r.center().y),
                    Align2::LEFT_CENTER,
                    "Import",
                    medium(12.0),
                    Color32::WHITE,
                );
            }
            if imp_resp.on_hover_text("Bulk import images, audio, models, or meshes").clicked() {
                self.actions.push(Action::OpenImportDialog);
            }

            // Search box (takes remaining width up to 240px).
            let search_w = ui.available_width().clamp(SEARCH_MIN_W, 240.0);
            let (sr, _) = ui.allocate_exact_size(Vec2::new(search_w, 24.0), Sense::hover());
            ui.painter().rect_filled(sr, 4.0, theme::PANEL_ALT);
            ui.painter().rect_stroke(sr, 4.0, Stroke::new(1.0, theme::DIVIDER), StrokeKind::Inside);
            icon(
                ui.painter(),
                Rect::from_min_size(sr.min + Vec2::new(6.0, 5.0), Vec2::splat(14.0)),
                "search",
                theme::TEXT_DIM,
            );
            let clear_w = if self.query.is_empty() { 4.0 } else { 20.0 };
            let edit_r = Rect::from_min_max(sr.min + Vec2::new(23.0, 2.0), Pos2::new(sr.max.x - clear_w, sr.max.y - 2.0));
            ui.put(
                edit_r,
                egui::TextEdit::singleline(&mut self.query)
                    .frame(egui::Frame::NONE)
                    .hint_text(RichText::new("Search or paste Asset ID...").color(theme::TEXT_DIM).size(12.0))
                    .font(FontId::proportional(12.0)),
            );
            if !self.query.is_empty() {
                let cr = Rect::from_min_size(Pos2::new(sr.max.x - 19.0, sr.min.y + 4.0), Vec2::splat(16.0));
                let cresp = ui.interact(cr, ui.id().with("am_clear_q"), Sense::click());
                icon(ui.painter(), cr.shrink(2.0), "x", if cresp.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                if cresp.clicked() {
                    self.query.clear();
                }
            }
        });
    }

    fn draw_breadcrumbs(&mut self, ui: &mut egui::Ui, dctx: &DrawCtx<'_>) {
        let mut crumbs: Vec<(String, Scope)> = Vec::new();
        match &self.persist.scope {
            Scope::Places => {
                crumbs.push((dctx.project_name.to_string(), Scope::Experience));
                crumbs.push(("Places".into(), Scope::Places));
            }
            Scope::Experience => {
                crumbs.push((dctx.project_name.to_string(), Scope::Experience));
                crumbs.push(("In Experience".into(), Scope::Experience));
            }
            Scope::RecentUploads => {
                crumbs.push(("Recent".into(), Scope::RecentUploads));
                crumbs.push(("Uploads".into(), Scope::RecentUploads));
            }
            Scope::MyInventory { folder } => {
                crumbs.push(("My Inventory".into(), Scope::MyInventory { folder: None }));
                if let Some(fid) = folder {
                    for (id, name) in self.folder_breadcrumbs(fid) {
                        crumbs.push((name, Scope::MyInventory { folder: Some(id) }));
                    }
                }
            }
            Scope::Group { group_id, folder } => {
                let gname = self
                    .group_name(*group_id)
                    .unwrap_or_else(|| format!("Group {group_id}"));
                crumbs.push((gname, Scope::Group { group_id: *group_id, folder: None }));
                if let Some(fid) = folder {
                    for (id, name) in self.folder_breadcrumbs(fid) {
                        crumbs.push((name, Scope::Group { group_id: *group_id, folder: Some(id) }));
                    }
                }
            }
        }

        let last_idx = crumbs.len().saturating_sub(1);
        for (i, (label, target)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").color(theme::TEXT_DIM).size(12.5));
            }
            let is_last = i == last_idx;
            let text = if is_last {
                RichText::new(&label).font(medium(12.5)).color(theme::TEXT)
            } else {
                RichText::new(&label).size(12.5).color(theme::TEXT_DIM)
            };
            let resp = ui.add(egui::Label::new(text).truncate().sense(Sense::click()));
            if resp.clicked() && !is_last {
                self.navigate(target);
            }
        }
    }

    fn draw_filter_chips(&mut self, ui: &mut egui::Ui, rect: Rect) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL);
        ui.painter().line_segment(
            [Pos2::new(rect.min.x, rect.max.y), Pos2::new(rect.max.x, rect.max.y)],
            Stroke::new(1.0, theme::DIVIDER),
        );
        let inner = rect.shrink2(Vec2::new(8.0, 3.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
        child.spacing_mut().item_spacing.x = 6.0;

        if self.persist.ownership != OwnershipFilter::All {
            if draw_chip(&mut child, self.persist.ownership.label()).clicked() {
                self.persist.ownership = OwnershipFilter::All;
                self.mark_dirty();
            }
        }
        let kinds = self.persist.type_filters.clone();
        for k in kinds {
            if draw_chip(&mut child, k.plural()).clicked() {
                self.persist.type_filters.retain(|x| *x != k);
                self.mark_dirty();
            }
        }
        if child.link(RichText::new("Clear all").size(11.5).color(LINK)).clicked() {
            self.persist.type_filters.clear();
            self.persist.ownership = OwnershipFilter::All;
            self.mark_dirty();
        }
    }

    // ------------------------------------------------------------------ sidebar

    fn draw_sidebar(&mut self, ui: &mut egui::Ui, rect: Rect, dctx: &DrawCtx<'_>) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_DARK);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(4.0, 6.0))));
        egui::ScrollArea::vertical().id_salt("am_sidebar").show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;

            // --- PROJECT ---
            if sidebar_section(ui, dctx.project_name, &mut self.project_expanded) {
                let places_count = if dctx.universe_id.is_some() { Some(self.places.places.len()) } else { None };
                if sidebar_item(ui, "map-pin", "Places", matches!(self.persist.scope, Scope::Places), places_count, 10.0).clicked() {
                    self.navigate(Scope::Places);
                }
                if sidebar_item(
                    ui,
                    "box",
                    "In Experience",
                    matches!(self.persist.scope, Scope::Experience),
                    Some(self.experience_assets.len()),
                    10.0,
                )
                .clicked()
                {
                    self.navigate(Scope::Experience);
                }
            }

            ui.add_space(6.0);

            // --- INVENTORIES ---
            if sidebar_section(ui, "INVENTORIES", &mut self.inventories_expanded) {
                let user_count = self.inventories.get("user").filter(|c| c.loaded).map(|c| c.items.len());
                let my_resp = sidebar_item(
                    ui,
                    "user",
                    "My Inventory",
                    matches!(self.persist.scope, Scope::MyInventory { folder: None }),
                    user_count,
                    10.0,
                );
                if my_resp.clicked() {
                    self.navigate(Scope::MyInventory { folder: None });
                }
                my_resp.context_menu(|ui| {
                    if ui.button("New Folder...").clicked() {
                        self.folder_dialog = Some(FolderDialog::Create {
                            root: "user".into(),
                            parent: None,
                            name: "New Folder".into(),
                        });
                        ui.close();
                    }
                    if ui.button("Refresh").clicked() {
                        self.ensure_inventory("user", None, true);
                        ui.close();
                    }
                });
                self.draw_sidebar_folders(ui, "user", None, None, 22.0);

                // Groups sub-section.
                if sidebar_section(ui, "Groups", &mut self.groups_expanded) {
                    if !self.signed_in {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(RichText::new("Sign in to view groups").size(11.5).color(theme::TEXT_DIM));
                        });
                    } else if self.groups_loading {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.spinner();
                            ui.label(RichText::new("Loading groups...").size(11.5).color(theme::TEXT_DIM));
                        });
                    } else if let Some(Ok(groups)) = self.groups.clone() {
                        let visible_groups: Vec<(u64, String)> = groups
                            .into_iter()
                            .filter(|(id, _)| !self.persist.hidden_groups.contains(id))
                            .collect();
                        if visible_groups.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                ui.label(RichText::new("No groups").size(11.5).color(theme::TEXT_DIM));
                            });
                        }
                        for (gid, gname) in visible_groups {
                            let root = format!("group:{gid}");
                            let gcount = self.inventories.get(&root).filter(|c| c.loaded).map(|c| c.items.len());
                            let active = matches!(self.persist.scope, Scope::Group { group_id, folder: None } if group_id == gid);
                            let resp = sidebar_item(ui, "users", &gname, active, gcount, 16.0);
                            if resp.clicked() {
                                self.navigate(Scope::Group { group_id: gid, folder: None });
                            }
                            resp.context_menu(|ui| {
                                if ui.button("New Folder...").clicked() {
                                    self.folder_dialog = Some(FolderDialog::Create {
                                        root: root.clone(),
                                        parent: None,
                                        name: "New Folder".into(),
                                    });
                                    ui.close();
                                }
                                if ui.button("Hide Group").clicked() {
                                    self.persist.hidden_groups.insert(gid);
                                    if matches!(self.persist.scope, Scope::Group { group_id, .. } if group_id == gid) {
                                        self.navigate(Scope::MyInventory { folder: None });
                                    }
                                    self.mark_dirty();
                                    ui.close();
                                }
                            });
                            self.draw_sidebar_folders(ui, &root, Some(gid), None, 28.0);
                        }
                        if !self.persist.hidden_groups.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                if ui.link(RichText::new(format!("Show {} hidden group(s)", self.persist.hidden_groups.len())).size(11.0).color(LINK)).clicked() {
                                    self.persist.hidden_groups.clear();
                                    self.mark_dirty();
                                }
                            });
                        }
                    } else if let Some(Err(_)) = &self.groups {
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            if ui.link(RichText::new("Retry loading groups").size(11.5).color(LINK)).clicked() {
                                self.fetch_groups();
                            }
                        });
                    }
                }
            }

            ui.add_space(6.0);

            // --- RECENT ---
            if sidebar_section(ui, "RECENT", &mut self.recent_expanded) {
                if sidebar_item(
                    ui,
                    "clock",
                    "Uploads",
                    matches!(self.persist.scope, Scope::RecentUploads),
                    Some(self.persist.recent_uploads.len()),
                    10.0,
                )
                .clicked()
                {
                    self.navigate(Scope::RecentUploads);
                }
            }
        });
    }

    fn draw_sidebar_folders(&mut self, ui: &mut egui::Ui, root: &str, group_id: Option<u64>, parent: Option<&str>, indent: f32) {
        let children = self.child_folders(root, parent);
        for f in children {
            let active = match &self.persist.scope {
                Scope::MyInventory { folder } if group_id.is_none() => folder.as_deref() == Some(f.id.as_str()),
                Scope::Group { group_id: g, folder } if group_id == Some(*g) => folder.as_deref() == Some(f.id.as_str()),
                _ => false,
            };
            let resp = sidebar_item(ui, "folder", &f.name, active, Some(f.asset_ids.len()), indent);
            // Drop target for dragging an asset into this folder.
            if resp.dnd_hover_payload::<FolderAssetDrag>().is_some() {
                ui.painter().rect_stroke(resp.rect, 4.0, Stroke::new(1.5, theme::SELECT), StrokeKind::Inside);
            }
            if let Some(drag) = resp.dnd_release_payload::<FolderAssetDrag>() {
                self.add_asset_to_folder(drag.0, &f.id);
            }
            if resp.clicked() {
                let target = match group_id {
                    Some(gid) => Scope::Group { group_id: gid, folder: Some(f.id.clone()) },
                    None => Scope::MyInventory { folder: Some(f.id.clone()) },
                };
                self.navigate(target);
            }
            resp.context_menu(|ui| {
                if ui.button("New Subfolder...").clicked() {
                    self.folder_dialog = Some(FolderDialog::Create {
                        root: root.to_string(),
                        parent: Some(f.id.clone()),
                        name: "New Folder".into(),
                    });
                    ui.close();
                }
                if ui.button("Rename...").clicked() {
                    self.folder_dialog = Some(FolderDialog::Rename {
                        folder_id: f.id.clone(),
                        name: f.name.clone(),
                    });
                    ui.close();
                }
                if ui.button(RichText::new("Delete Folder").color(theme::ERROR)).clicked() {
                    self.delete_folder(&f.id);
                    ui.close();
                }
            });
            self.draw_sidebar_folders(ui, root, group_id, Some(&f.id), indent + 12.0);
        }
    }

    // ------------------------------------------------------------------ main content

    fn draw_main(&mut self, ui: &mut egui::Ui, rect: Rect, visible: &[AssetEntry], dctx: &DrawCtx<'_>) {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));

        // Scope-specific banners / signed-out / empty states.
        match &self.persist.scope {
            Scope::Places => {
                if dctx.universe_id.is_none() {
                    draw_empty_state(
                        &mut child,
                        "map-pin",
                        "Local Place",
                        "Save or publish this place to Roblox to view and manage all places in your experience.",
                        Some(("Save to Roblox", Action::SaveToRoblox)),
                        &mut self.actions,
                    );
                    return;
                }
                if !self.signed_in {
                    draw_empty_state(
                        &mut child,
                        "user-off",
                        "Signed Out",
                        "Sign in to your Roblox account to view experience places.",
                        Some(("Sign In", Action::OpenAccount)),
                        &mut self.actions,
                    );
                    return;
                }
                if self.places.loading && self.places.places.is_empty() {
                    draw_loading_state(&mut child, "Loading places...");
                    return;
                }
                if let Some(err) = self.places.error.clone() {
                    draw_error_state(&mut child, &err, || {});
                    return;
                }
            }
            Scope::MyInventory { .. } | Scope::Group { .. } => {
                if !self.signed_in {
                    draw_empty_state(
                        &mut child,
                        "user-off",
                        "Sign In to Browse Inventory",
                        "Sign in to access your Roblox inventory, creations, and group assets.",
                        Some(("Sign In", Action::OpenAccount)),
                        &mut self.actions,
                    );
                    return;
                }
                if let Some(root) = self.persist.scope.folder_root() {
                    if let Some(cache) = self.inventories.get(&root) {
                        if cache.loading && cache.items.is_empty() {
                            draw_loading_state(&mut child, "Loading assets...");
                            return;
                        }
                        if let Some(err) = &cache.error {
                            if cache.items.is_empty() {
                                let msg = err.describe();
                                let mut retry = false;
                                draw_error_state(&mut child, &msg, || retry = true);
                                if retry {
                                    self.refresh_current(dctx.universe_id);
                                }
                                return;
                            }
                        }
                    }
                }
            }
            Scope::Experience => {
                if visible.is_empty() && self.query.is_empty() && self.active_filter_count() == 0 {
                    draw_empty_state(
                        &mut child,
                        "box",
                        "No Assets Referenced in Place",
                        "Meshes, images, sounds, animations, and packages used by instances in this place will appear here automatically.",
                        Some(("Import Assets", Action::OpenImportDialog)),
                        &mut self.actions,
                    );
                    return;
                }
            }
            Scope::RecentUploads => {
                if visible.is_empty() && self.query.is_empty() {
                    draw_empty_state(
                        &mut child,
                        "upload",
                        "No Recent Uploads",
                        "Files you upload via Import will appear here for quick insertion.",
                        Some(("Import Assets", Action::OpenImportDialog)),
                        &mut self.actions,
                    );
                    return;
                }
            }
        }

        let subfolders: Vec<Folder> = if let Some(root) = self.persist.scope.folder_root() {
            if self.query.trim().is_empty() {
                self.child_folders(&root, self.persist.scope.folder_id())
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        if visible.is_empty() && subfolders.is_empty() {
            draw_empty_state(
                &mut child,
                "search",
                "No Matching Assets",
                "Try clearing your search or adjusting the active asset type filters.",
                None,
                &mut self.actions,
            );
            return;
        }

        // Background context menu (New Folder / Import / Refresh).
        let bg_resp = child.interact(rect, child.id().with("am_bg_ctx"), Sense::click());
        if bg_resp.clicked() {
            self.selected.clear();
            self.anchor = None;
        }
        bg_resp.context_menu(|ui| {
            if let Some(root) = self.persist.scope.folder_root() {
                if ui.button("New Folder...").clicked() {
                    self.folder_dialog = Some(FolderDialog::Create {
                        root,
                        parent: self.persist.scope.folder_id().map(str::to_string),
                        name: "New Folder".into(),
                    });
                    ui.close();
                }
            }
            if ui.button("Import Assets...").clicked() {
                self.actions.push(Action::OpenImportDialog);
                ui.close();
            }
            if ui.button("Refresh").clicked() {
                self.refresh_current(dctx.universe_id);
                ui.close();
            }
        });

        match self.persist.view {
            ViewMode::Grid => self.draw_grid(&mut child, &subfolders, visible),
            ViewMode::List => self.draw_list(&mut child, &subfolders, visible),
        }
    }

    // ------------------------------------------------------------------ grid view

    fn draw_grid(&mut self, ui: &mut egui::Ui, subfolders: &[Folder], visible: &[AssetEntry]) {
        let pad = 8.0;
        let gap = 8.0;
        let tile_w = self.persist.tile_size.clamp(72.0, 180.0);
        let tile_h = tile_w + 42.0;

        // Only the rows on screen are drawn (a group with 8290 experiences
        // assets drew every tile each frame: 18 fps).
        egui::ScrollArea::vertical().id_salt("am_grid_scroll").show_viewport(ui, |ui, viewport| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(pad);
            let avail_w = (ui.available_width() - pad * 2.0).max(tile_w);
            let cols = (((avail_w + gap) / (tile_w + gap)).floor() as usize).max(1);
            let actual_w = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32).min(tile_w + 24.0);

            enum GridItem<'a> {
                Folder(&'a Folder),
                Asset(&'a AssetEntry),
            }
            let mut items: Vec<GridItem<'_>> = Vec::with_capacity(subfolders.len() + visible.len());
            for f in subfolders {
                items.push(GridItem::Folder(f));
            }
            for a in visible {
                items.push(GridItem::Asset(a));
            }

            let rows = items.len().div_ceil(cols);
            let row_h = tile_h + gap;
            let first = (((viewport.min.y - pad) / row_h).floor().max(0.0) as usize).min(rows);
            let last = ((((viewport.max.y - pad) / row_h).ceil().max(0.0) as usize) + 1).min(rows).max(first);
            ui.add_space(first as f32 * row_h);
            for row in first..last {
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), tile_h), egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.add_space(pad);
                    ui.spacing_mut().item_spacing.x = gap;
                    for col in 0..cols {
                        let idx = row * cols + col;
                        let Some(item) = items.get(idx) else { break };
                        match item {
                            GridItem::Folder(f) => self.draw_folder_card(ui, f, Vec2::new(actual_w, tile_h)),
                            GridItem::Asset(a) => self.draw_asset_card(ui, a, visible, Vec2::new(actual_w, tile_h)),
                        }
                    }
                });
                ui.add_space(gap);
            }
            ui.add_space((rows - last) as f32 * row_h);

            self.draw_load_more_footer(ui);
            ui.add_space(pad);
        });
    }

    fn draw_folder_card(&mut self, ui: &mut egui::Ui, folder: &Folder, size: Vec2) {
        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
        let dnd_hover = resp.dnd_hover_payload::<FolderAssetDrag>().is_some();
        let bg = if dnd_hover {
            theme::SELECT_BG
        } else if resp.hovered() {
            TILE_HOVER
        } else {
            TILE
        };
        ui.painter().rect_filled(r, 6.0, bg);
        if dnd_hover {
            ui.painter().rect_stroke(r, 6.0, Stroke::new(1.5, theme::SELECT), StrokeKind::Inside);
        }

        let thumb_h = size.x;
        let thumb_r = Rect::from_min_size(r.min + Vec2::splat(4.0), Vec2::new(size.x - 8.0, thumb_h - 8.0));
        ui.painter().rect_filled(thumb_r, 4.0, theme::PANEL_DARK);
        icon(ui.painter(), Rect::from_center_size(thumb_r.center(), Vec2::splat(thumb_r.width() * 0.48)), "folder-filled", FOLDER_GOLD);

        let text_r = Rect::from_min_max(Pos2::new(r.min.x + 6.0, r.min.y + thumb_h), r.max - Vec2::new(6.0, 4.0));
        ui.painter().with_clip_rect(text_r).text(
            Pos2::new(text_r.min.x, text_r.min.y + 2.0),
            Align2::LEFT_TOP,
            &folder.name,
            medium(12.0),
            theme::TEXT,
        );
        ui.painter().with_clip_rect(text_r).text(
            Pos2::new(text_r.min.x, text_r.min.y + 18.0),
            Align2::LEFT_TOP,
            format!("{} items", folder.asset_ids.len()),
            FontId::proportional(10.5),
            theme::TEXT_DIM,
        );

        if let Some(drag) = resp.dnd_release_payload::<FolderAssetDrag>() {
            self.add_asset_to_folder(drag.0, &folder.id);
        }
        if resp.double_clicked() {
            let next = self.persist.scope.with_folder(Some(folder.id.clone()));
            self.navigate(next);
        }
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() {
                let next = self.persist.scope.with_folder(Some(folder.id.clone()));
                self.navigate(next);
                ui.close();
            }
            if ui.button("Rename...").clicked() {
                self.folder_dialog = Some(FolderDialog::Rename {
                    folder_id: folder.id.clone(),
                    name: folder.name.clone(),
                });
                ui.close();
            }
            if ui.button(RichText::new("Delete").color(theme::ERROR)).clicked() {
                self.delete_folder(&folder.id);
                ui.close();
            }
        });
    }

    fn draw_asset_card(&mut self, ui: &mut egui::Ui, entry: &AssetEntry, visible: &[AssetEntry], size: Vec2) {
        let selected = self.selected.contains(&entry.id);
        let (r, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let bg = if selected {
            theme::SELECT_BG
        } else if resp.hovered() {
            TILE_HOVER
        } else {
            TILE
        };
        ui.painter().rect_filled(r, 6.0, bg);
        if selected {
            ui.painter().rect_stroke(r, 6.0, Stroke::new(1.5, theme::SELECT), StrokeKind::Inside);
        }

        let thumb_h = size.x;
        let thumb_r = Rect::from_min_size(r.min + Vec2::splat(4.0), Vec2::new(size.x - 8.0, thumb_h - 8.0));
        ui.painter().rect_filled(thumb_r, 4.0, theme::PANEL_DARK);

        // Thumbnail image (async from Roblox thumbnails API).
        match self.thumbs.get(entry.id, 150) {
            Thumb::Ready(tex) => {
                ui.painter().image(
                    tex.id(),
                    thumb_r,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            _ => {
                icon(
                    ui.painter(),
                    Rect::from_center_size(thumb_r.center(), Vec2::splat(thumb_r.width() * 0.42)),
                    entry.kind.icon(),
                    kind_color(entry.kind),
                );
            }
        }

        // Bottom-left type badge pill.
        let badge_r = Rect::from_min_size(Pos2::new(thumb_r.min.x + 4.0, thumb_r.max.y - 20.0), Vec2::splat(16.0));
        ui.painter().rect_filled(badge_r, 3.0, Color32::from_rgba_unmultiplied(20, 22, 27, 210));
        icon(ui.painter(), badge_r.shrink(2.0), entry.kind.icon(), kind_color(entry.kind));

        // Start Place pill badge.
        if entry.is_start_place {
            let sp_r = Rect::from_min_size(thumb_r.min + Vec2::new(4.0, 4.0), Vec2::new(40.0, 16.0));
            ui.painter().rect_filled(sp_r, 3.0, theme::ACCENT);
            ui.painter().text(sp_r.center(), Align2::CENTER_CENTER, "Start", bold(10.0), Color32::WHITE);
        }

        // Audio hover Play/Stop button.
        let mut clicked_audio = false;
        if entry.kind == AssetKind::Audio && (resp.hovered() || self.playing_audio == Some(entry.id)) {
            let playing = self.playing_audio == Some(entry.id);
            let btn_r = Rect::from_min_size(Pos2::new(thumb_r.max.x - 24.0, thumb_r.max.y - 24.0), Vec2::splat(20.0));
            let bresp = ui.interact(btn_r, ui.id().with(("am_aud", entry.id)), Sense::click());
            ui.painter().circle_filled(btn_r.center(), 10.0, if playing { theme::ACCENT } else { Color32::from_rgba_unmultiplied(20, 22, 27, 220) });
            icon(
                ui.painter(),
                btn_r.shrink(4.0),
                if playing { "player-stop-filled" } else { "player-play-filled" },
                Color32::WHITE,
            );
            if bresp.clicked() {
                clicked_audio = true;
                if playing {
                    self.actions.push(Action::StopAudio);
                } else {
                    self.actions.push(Action::PreviewAudio(entry.id));
                }
            }
        }

        // Asset name + subtle type/usage subtitle.
        let text_r = Rect::from_min_max(Pos2::new(r.min.x + 6.0, r.min.y + thumb_h), r.max - Vec2::new(6.0, 4.0));
        ui.painter().with_clip_rect(text_r).text(
            Pos2::new(text_r.min.x, text_r.min.y + 2.0),
            Align2::LEFT_TOP,
            &entry.name,
            FontId::proportional(11.5),
            if selected { Color32::WHITE } else { theme::TEXT },
        );
        let sub = if !entry.usages.is_empty() {
            format!("{} • {}x", entry.kind.label(), entry.usages.len())
        } else if !entry.creator_name.is_empty() {
            entry.creator_name.clone()
        } else {
            entry.kind.label().to_string()
        };
        ui.painter().with_clip_rect(text_r).text(
            Pos2::new(text_r.min.x, text_r.min.y + 18.0),
            Align2::LEFT_TOP,
            sub,
            FontId::proportional(10.0),
            theme::TEXT_DIM,
        );

        // Selection, double-click, drag-and-drop.
        if !clicked_audio && resp.clicked() {
            self.handle_select(ui, entry.id, visible);
        }
        if !clicked_audio && resp.double_clicked() {
            if entry.kind == AssetKind::Place {
                self.actions.push(Action::OpenPlace(entry.id));
            } else {
                self.actions.push(Action::Insert { entry: entry.clone(), into_selection: false });
            }
        }
        if resp.drag_started() && entry.kind != AssetKind::Place {
            if ui.input(|i| i.modifiers.alt) {
                egui::DragAndDrop::set_payload(ui.ctx(), FolderAssetDrag(entry.id));
            } else {
                egui::DragAndDrop::set_payload(ui.ctx(), ToolboxDrag(entry.id));
            }
        }

        self.attach_asset_context_menu(&resp, entry);
    }

    // ------------------------------------------------------------------ list view

    fn draw_list(&mut self, ui: &mut egui::Ui, subfolders: &[Folder], visible: &[AssetEntry]) {
        // Sticky column header.
        let hdr_h = 26.0;
        let (hdr_r, hdr_resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), hdr_h), Sense::click());
        ui.painter().rect_filled(hdr_r, 0.0, theme::PANEL_DARK);
        ui.painter().line_segment(
            [Pos2::new(hdr_r.min.x, hdr_r.max.y), Pos2::new(hdr_r.max.x, hdr_r.max.y)],
            Stroke::new(1.0, theme::DIVIDER),
        );

        let cols = self.persist.columns.clone();
        let show_usages = cols.usages && matches!(self.persist.scope, Scope::Experience);
        let w = hdr_r.width();
        let mut right_used = 0.0;
        let w_usages = if show_usages { 72.0 } else { 0.0 };
        let w_mod = if cols.modified { 92.0 } else { 0.0 };
        let w_creator = if cols.creator { 120.0 } else { 0.0 };
        let w_id = if cols.id { 96.0 } else { 0.0 };
        let w_type = if cols.asset_type { 84.0 } else { 0.0 };
        right_used += w_usages + w_mod + w_creator + w_id + w_type;
        let w_name = (w - right_used).max(140.0);

        let mut x = hdr_r.min.x;
        let mut draw_hdr = |label: &str, width: f32, sort: Option<SortBy>| {
            if width <= 0.0 {
                return;
            }
            let r = Rect::from_min_size(Pos2::new(x, hdr_r.min.y), Vec2::new(width, hdr_h));
            let resp = ui.interact(r, ui.id().with(("am_col", label)), Sense::click());
            let arrow = match sort {
                Some(s) if self.persist.sort_by == s => match self.persist.sort_dir {
                    SortDir::Asc => " ▲",
                    SortDir::Desc => " ▼",
                },
                _ => "",
            };
            ui.painter().with_clip_rect(r.shrink2(Vec2::new(6.0, 0.0))).text(
                Pos2::new(r.min.x + 8.0, r.center().y),
                Align2::LEFT_CENTER,
                format!("{label}{arrow}"),
                medium(11.5),
                if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM },
            );
            if let Some(s) = sort {
                if resp.clicked() {
                    if self.persist.sort_by == s {
                        self.persist.sort_dir = self.persist.sort_dir.flip();
                    } else {
                        self.persist.sort_by = s;
                        self.persist.sort_dir = SortDir::Asc;
                    }
                    self.mark_dirty();
                }
            }
            x += width;
        };

        draw_hdr("Name", w_name, Some(SortBy::Name));
        draw_hdr("Type", w_type, Some(SortBy::AssetType));
        draw_hdr("Asset ID", w_id, Some(SortBy::Id));
        draw_hdr("Creator", w_creator, Some(SortBy::Creator));
        draw_hdr("Modified", w_mod, Some(SortBy::Modified));
        draw_hdr("Usages", w_usages, None);

        // Right-click header to toggle columns.
        hdr_resp.context_menu(|ui| {
            ui.label(RichText::new("Columns").font(bold(11.5)).color(theme::TEXT_DIM));
            ui.separator();
            if ui.checkbox(&mut self.persist.columns.asset_type, "Type").changed() { self.mark_dirty(); }
            if ui.checkbox(&mut self.persist.columns.id, "Asset ID").changed() { self.mark_dirty(); }
            if ui.checkbox(&mut self.persist.columns.creator, "Creator").changed() { self.mark_dirty(); }
            if ui.checkbox(&mut self.persist.columns.modified, "Modified").changed() { self.mark_dirty(); }
            if ui.checkbox(&mut self.persist.columns.usages, "Usages (In Experience)").changed() { self.mark_dirty(); }
        });

        let row_h = 28.0;
        egui::ScrollArea::vertical().id_salt("am_list_scroll").show_viewport(ui, |ui, viewport| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut row_idx = 0usize;

            for f in subfolders {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
                let bg = if resp.hovered() { TILE_HOVER } else if row_idx % 2 == 1 { ROW_ALT } else { theme::PANEL };
                ui.painter().rect_filled(r, 0.0, bg);
                icon(ui.painter(), Rect::from_min_size(r.min + Vec2::new(8.0, 6.0), Vec2::splat(16.0)), "folder-filled", FOLDER_GOLD);
                ui.painter().text(Pos2::new(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, &f.name, medium(12.0), theme::TEXT);
                if w_type > 0.0 {
                    ui.painter().text(Pos2::new(r.min.x + w_name + 8.0, r.center().y), Align2::LEFT_CENTER, "Folder", FontId::proportional(11.5), theme::TEXT_DIM);
                }
                if resp.double_clicked() {
                    let next = self.persist.scope.with_folder(Some(f.id.clone()));
                    self.navigate(next);
                }
                row_idx += 1;
            }

            // Only the rows on screen are drawn (see draw_grid).
            let start = subfolders.len() as f32 * row_h;
            let n = visible.len();
            let first = (((viewport.min.y - start) / row_h).floor().max(0.0) as usize).min(n);
            let last = ((((viewport.max.y - start) / row_h).ceil().max(0.0) as usize) + 1).min(n).max(first);
            ui.add_space(first as f32 * row_h);
            row_idx += first;
            for entry in &visible[first..last] {
                let selected = self.selected.contains(&entry.id);
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click_and_drag());
                let bg = if selected {
                    theme::SELECT_BG
                } else if resp.hovered() {
                    TILE_HOVER
                } else if row_idx % 2 == 1 {
                    ROW_ALT
                } else {
                    theme::PANEL
                };
                ui.painter().rect_filled(r, 0.0, bg);

                let icon_r = Rect::from_min_size(r.min + Vec2::new(8.0, 4.0), Vec2::splat(20.0));
                match self.thumbs.get(entry.id, 150) {
                    Thumb::Ready(tex) => {
                        ui.painter().image(tex.id(), icon_r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                    _ => {
                        icon(ui.painter(), icon_r.shrink(2.0), entry.kind.icon(), kind_color(entry.kind));
                    }
                }

                let mut cx = r.min.x;
                let name_clip = Rect::from_min_size(Pos2::new(cx + 34.0, r.min.y), Vec2::new((w_name - 38.0).max(40.0), row_h));
                ui.painter().with_clip_rect(name_clip).text(
                    Pos2::new(cx + 34.0, r.center().y),
                    Align2::LEFT_CENTER,
                    if entry.is_start_place { format!("{}  [Start]", entry.name) } else { entry.name.clone() },
                    FontId::proportional(12.0),
                    if selected { Color32::WHITE } else { theme::TEXT },
                );
                cx += w_name;

                if w_type > 0.0 {
                    ui.painter().text(Pos2::new(cx + 8.0, r.center().y), Align2::LEFT_CENTER, entry.kind.label(), FontId::proportional(11.5), kind_color(entry.kind));
                    cx += w_type;
                }
                if w_id > 0.0 {
                    ui.painter().text(Pos2::new(cx + 8.0, r.center().y), Align2::LEFT_CENTER, entry.id.to_string(), FontId::monospace(11.0), theme::TEXT_DIM);
                    cx += w_id;
                }
                if w_creator > 0.0 {
                    let clip = Rect::from_min_size(Pos2::new(cx + 8.0, r.min.y), Vec2::new((w_creator - 12.0).max(20.0), row_h));
                    ui.painter().with_clip_rect(clip).text(Pos2::new(cx + 8.0, r.center().y), Align2::LEFT_CENTER, &entry.creator_name, FontId::proportional(11.5), theme::TEXT_DIM);
                    cx += w_creator;
                }
                if w_mod > 0.0 {
                    ui.painter().text(Pos2::new(cx + 8.0, r.center().y), Align2::LEFT_CENTER, format_short_date(&entry.updated), FontId::proportional(11.0), theme::TEXT_DIM);
                    cx += w_mod;
                }
                if w_usages > 0.0 {
                    ui.painter().text(Pos2::new(cx + 8.0, r.center().y), Align2::LEFT_CENTER, format!("{}", entry.usages.len()), FontId::proportional(11.5), theme::TEXT_DIM);
                }

                if resp.clicked() {
                    self.handle_select(ui, entry.id, visible);
                }
                if resp.double_clicked() {
                    if entry.kind == AssetKind::Place {
                        self.actions.push(Action::OpenPlace(entry.id));
                    } else {
                        self.actions.push(Action::Insert { entry: entry.clone(), into_selection: false });
                    }
                }
                if resp.drag_started() && entry.kind != AssetKind::Place {
                    egui::DragAndDrop::set_payload(ui.ctx(), ToolboxDrag(entry.id));
                }
                self.attach_asset_context_menu(&resp, entry);
                row_idx += 1;
            }
            ui.add_space((n - last) as f32 * row_h);

            self.draw_load_more_footer(ui);
        });
    }

    fn draw_load_more_footer(&mut self, ui: &mut egui::Ui) {
        let Some(root) = self.persist.scope.folder_root() else { return };
        let Some(cache) = self.inventories.get(&root) else { return };
        if cache.loading {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.spinner();
                ui.label(RichText::new("Loading more assets...").size(11.5).color(theme::TEXT_DIM));
            });
        } else if cache.has_more {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if ui.button("Load More").clicked() {
                    let gid = match self.persist.scope {
                        Scope::Group { group_id, .. } => Some(group_id),
                        _ => None,
                    };
                    self.load_more_inventory(&root, gid);
                }
            });
        }
    }

    // ------------------------------------------------------------------ details drawer

    fn draw_details(&mut self, ui: &mut egui::Ui, rect: Rect, visible: &[AssetEntry], dctx: &DrawCtx<'_>) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_DARK);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)));

        child.horizontal(|ui| {
            ui.label(RichText::new("Asset Details").font(bold(12.5)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_btn(ui, "x", 20.0, false, true, "Close Details").clicked() {
                    self.persist.details_open = false;
                    self.mark_dirty();
                }
            });
        });
        child.separator();

        let sel_id = self.selected.last().copied();
        let entry = sel_id.and_then(|id| visible.iter().find(|a| a.id == id).cloned().or_else(|| self.find_entry(id)));

        let Some(entry) = entry else {
            child.add_space(24.0);
            child.vertical_centered(|ui| {
                ui.label(RichText::new("Select an asset to view its details.").size(12.0).color(theme::TEXT_DIM));
            });
            return;
        };

        egui::ScrollArea::vertical().id_salt("am_details_scroll").show(&mut child, |ui| {
            // Preview image.
            let prev_w = ui.available_width().min(180.0);
            ui.vertical_centered(|ui| {
                let (pr, _) = ui.allocate_exact_size(Vec2::splat(prev_w), Sense::hover());
                ui.painter().rect_filled(pr, 6.0, theme::PANEL);
                match self.thumbs.get(entry.id, 420) {
                    Thumb::Ready(tex) => {
                        ui.painter().image(tex.id(), pr.shrink(2.0), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                    }
                    _ => {
                        icon(ui.painter(), Rect::from_center_size(pr.center(), Vec2::splat(52.0)), entry.kind.icon(), kind_color(entry.kind));
                    }
                }
            });

            ui.add_space(8.0);
            ui.label(RichText::new(&entry.name).font(bold(13.5)));
            ui.label(RichText::new(entry.kind.label()).size(11.5).color(kind_color(entry.kind)));
            ui.add_space(8.0);

            // Primary action buttons.
            ui.horizontal(|ui| {
                if entry.kind == AssetKind::Place {
                    if primary_btn(ui, "Open Place").clicked() {
                        self.actions.push(Action::OpenPlace(entry.id));
                    }
                } else if primary_btn(ui, "Insert").clicked() {
                    self.actions.push(Action::Insert { entry: entry.clone(), into_selection: false });
                }
                if ui.button("Copy ID").clicked() {
                    self.actions.push(Action::CopyText(format!("rbxassetid://{}", entry.id), format!("Copied rbxassetid://{}", entry.id)));
                }
            });

            if entry.kind == AssetKind::Audio {
                ui.add_space(4.0);
                let playing = self.playing_audio == Some(entry.id);
                if ui.button(if playing { "⏹ Stop Preview" } else { "▶ Play Audio Preview" }).clicked() {
                    if playing {
                        self.actions.push(Action::StopAudio);
                    } else {
                        self.actions.push(Action::PreviewAudio(entry.id));
                    }
                }
            }

            ui.add_space(10.0);
            ui.separator();

            detail_row(ui, "Asset ID", &entry.id.to_string());
            detail_row(ui, "URI", &format!("rbxassetid://{}", entry.id));
            if !entry.creator_name.is_empty() {
                detail_row(ui, "Creator", &entry.creator_name);
            }
            if !entry.created.is_empty() {
                detail_row(ui, "Created", format_short_date(&entry.created));
            }
            if !entry.updated.is_empty() {
                detail_row(ui, "Updated", format_short_date(&entry.updated));
            }
            if !entry.description.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Description").size(11.0).color(theme::TEXT_DIM));
                ui.label(RichText::new(&entry.description).size(11.5));
            }

            if !entry.usages.is_empty() {
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Usages in Place ({})", entry.usages.len())).font(medium(11.5)));
                    if ui.link(RichText::new("Select all").size(11.0).color(LINK)).clicked() {
                        self.actions.push(Action::SelectInstances(entry.usages.clone()));
                    }
                });
                for &r in entry.usages.iter().take(20) {
                    let label = (dctx.instance_name)(r);
                    if ui.selectable_label(false, RichText::new(label).size(11.5)).clicked() {
                        self.actions.push(Action::SelectInstances(vec![r]));
                    }
                }
            }

            ui.add_space(10.0);
            let url = if entry.kind == AssetKind::Place {
                format!("https://www.roblox.com/games/{}", entry.id)
            } else {
                format!("https://create.roblox.com/dashboard/creations/store/{}/configure", entry.id)
            };
            if ui.link(RichText::new("Open on Roblox ↗").size(11.5).color(LINK)).clicked() {
                self.actions.push(Action::OpenUrl(url));
            }
        });
    }

    // ------------------------------------------------------------------ status bar

    fn draw_status_bar(&mut self, ui: &mut egui::Ui, rect: Rect, count: usize) {
        ui.painter().rect_filled(rect, 0.0, theme::PANEL_DARK);
        ui.painter().line_segment(
            [Pos2::new(rect.min.x, rect.min.y), Pos2::new(rect.max.x, rect.min.y)],
            Stroke::new(1.0, theme::DIVIDER),
        );
        let inner = rect.shrink2(Vec2::new(8.0, 2.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));

        let left_text = if self.selected.is_empty() {
            format!("{count} item{}", if count == 1 { "" } else { "s" })
        } else {
            format!("{} selected of {count} item{}", self.selected.len(), if count == 1 { "" } else { "s" })
        };
        child.label(RichText::new(left_text).size(11.0).color(theme::TEXT_DIM));

        if self.pool.busy.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            child.add_space(8.0);
            child.spinner();
        }

        child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if self.persist.view == ViewMode::Grid {
                let slider = egui::Slider::new(&mut self.persist.tile_size, 76.0..=164.0)
                    .show_value(false)
                    .trailing_fill(true);
                if ui.add_sized(Vec2::new(84.0, 14.0), slider).on_hover_text("Tile Size").changed() {
                    self.mark_dirty();
                }
                ui.label(RichText::new("Size").size(10.5).color(theme::TEXT_DIM));
            }
        });
    }

    // ------------------------------------------------------------------ context menu & selection

    fn handle_select(&mut self, ui: &egui::Ui, id: u64, visible: &[AssetEntry]) {
        let mods = ui.input(|i| i.modifiers);
        if mods.shift {
            if let Some(anchor) = self.anchor {
                let a = visible.iter().position(|e| e.id == anchor);
                let b = visible.iter().position(|e| e.id == id);
                if let (Some(i0), Some(i1)) = (a, b) {
                    let (lo, hi) = if i0 <= i1 { (i0, i1) } else { (i1, i0) };
                    self.selected = visible[lo..=hi].iter().map(|e| e.id).collect();
                    return;
                }
            }
        }
        if mods.ctrl || mods.command {
            if let Some(pos) = self.selected.iter().position(|x| *x == id) {
                self.selected.remove(pos);
            } else {
                self.selected.push(id);
            }
            self.anchor = Some(id);
        } else {
            self.selected = vec![id];
            self.anchor = Some(id);
        }
    }

    fn attach_asset_context_menu(&mut self, resp: &egui::Response, entry: &AssetEntry) {
        let entry = entry.clone();
        resp.context_menu(|ui| {
            if entry.kind == AssetKind::Place {
                if ui.button("Open Place").clicked() {
                    self.actions.push(Action::OpenPlace(entry.id));
                    ui.close();
                }
                if ui.button("Copy Place ID").clicked() {
                    self.actions.push(Action::CopyText(entry.id.to_string(), format!("Copied Place ID {}", entry.id)));
                    ui.close();
                }
                ui.separator();
                if ui.button("Open on Roblox").clicked() {
                    self.actions.push(Action::OpenUrl(format!("https://www.roblox.com/games/{}", entry.id)));
                    ui.close();
                }
                return;
            }

            if ui.button("Insert").clicked() {
                self.actions.push(Action::Insert { entry: entry.clone(), into_selection: false });
                ui.close();
            }
            if ui.button("Insert Into Selection").clicked() {
                self.actions.push(Action::Insert { entry: entry.clone(), into_selection: true });
                ui.close();
            }
            ui.separator();
            if ui.button("Copy ID (rbxassetid://...)").clicked() {
                self.actions.push(Action::CopyText(
                    format!("rbxassetid://{}", entry.id),
                    format!("Copied rbxassetid://{}", entry.id),
                ));
                ui.close();
            }
            if ui.button("Copy Numeric ID").clicked() {
                self.actions.push(Action::CopyText(entry.id.to_string(), format!("Copied {}", entry.id)));
                ui.close();
            }
            ui.separator();
            if ui.button("View Details").clicked() {
                self.selected = vec![entry.id];
                self.persist.details_open = true;
                self.mark_dirty();
                ui.close();
            }
            if !entry.usages.is_empty() && ui.button(format!("Select Usages ({})", entry.usages.len())).clicked() {
                self.actions.push(Action::SelectInstances(entry.usages.clone()));
                ui.close();
            }
            if let Some(root) = self.persist.scope.folder_root() {
                let folders: Vec<Folder> = self.persist.folders.iter().filter(|f| f.root == root).cloned().collect();
                if !folders.is_empty() {
                    ui.menu_button("Add to Folder", |ui| {
                        for f in &folders {
                            if ui.button(&f.name).clicked() {
                                self.add_asset_to_folder(entry.id, &f.id);
                                ui.close();
                            }
                        }
                    });
                }
                if let Some(cur_fid) = self.persist.scope.folder_id().map(str::to_string) {
                    if ui.button("Remove from Folder").clicked() {
                        self.remove_asset_from_folder(entry.id, &cur_fid);
                        ui.close();
                    }
                }
            }
            ui.separator();
            if ui.button("Open on Creator Hub").clicked() {
                self.actions.push(Action::OpenUrl(format!(
                    "https://create.roblox.com/dashboard/creations/store/{}/configure",
                    entry.id
                )));
                ui.close();
            }
        });
    }

    // ------------------------------------------------------------------ modals

    fn draw_folder_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.folder_dialog.take() else { return };
        let title = match &dlg {
            FolderDialog::Create { .. } => "New Folder",
            FolderDialog::Rename { .. } => "Rename Folder",
        };
        let mut open = true;
        let mut commit = false;
        let mut cancelled = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_min_width(260.0);
                let name = match &mut dlg {
                    FolderDialog::Create { name, .. } | FolderDialog::Rename { name, .. } => name,
                };
                ui.label("Folder name:");
                let resp = ui.add(egui::TextEdit::singleline(name).desired_width(250.0));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_btn(ui, "Save").clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if cancelled {
            open = false;
        }
        if commit {
            match dlg {
                FolderDialog::Create { root, parent, name } => {
                    self.create_folder(root, parent, name);
                }
                FolderDialog::Rename { folder_id, name } => {
                    self.rename_folder(&folder_id, name);
                }
            }
        } else if open {
            self.folder_dialog = Some(dlg);
        }
    }

    fn draw_import_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.import_modal.take() else { return };
        let mut open = true;
        let mut close_clicked = false;
        let mut start_upload = false;
        let mut local_imports: Vec<(usize, std::path::PathBuf)> = Vec::new();
        let groups = self.groups.as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default();

        egui::Window::new("Asset Manager — Bulk Import")
            .collapsible(false)
            .resizable(true)
            .default_size(Vec2::new(540.0, 380.0))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Destination:");
                    let cur_label = match modal.target_group {
                        None => "My Inventory".to_string(),
                        Some(gid) => groups
                            .iter()
                            .find(|(id, _)| *id == gid)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| format!("Group {gid}")),
                    };
                    egui::ComboBox::from_id_salt("am_imp_dest")
                        .selected_text(cur_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut modal.target_group, None, "My Inventory");
                            for (gid, gname) in &groups {
                                ui.selectable_value(&mut modal.target_group, Some(*gid), gname);
                            }
                        });
                });
                ui.separator();

                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for (idx, item) in modal.items.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let (ir, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                            icon(ui.painter(), ir, item.kind.icon(), kind_color(item.kind));
                            ui.add_enabled(
                                matches!(item.status, ImportStatus::Pending),
                                egui::TextEdit::singleline(&mut item.display_name).desired_width(180.0),
                            );
                            ui.label(RichText::new(item.asset_type).size(11.5).color(theme::TEXT_DIM));
                            ui.label(RichText::new(format_bytes(item.size_bytes)).size(11.0).color(theme::TEXT_DIM));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &item.status {
                                ImportStatus::Pending => {
                                    ui.label(RichText::new("Ready").size(11.0).color(theme::TEXT_DIM));
                                }
                                ImportStatus::Uploading => {
                                    ui.spinner();
                                    ui.label(RichText::new("Uploading...").size(11.0).color(LINK));
                                }
                                ImportStatus::Done(id) => {
                                    ui.label(RichText::new(format!("✔ {id}")).size(11.0).color(GOOD));
                                }
                                ImportStatus::LocalImported => {
                                    ui.label(RichText::new("✔ Inserted locally").size(11.0).color(GOOD));
                                }
                                ImportStatus::Failed(e) => {
                                    ui.label(RichText::new("Failed").size(11.0).color(theme::ERROR)).on_hover_text(e);
                                }
                            });
                        });
                        let _ = idx;
                    }
                });

                ui.separator();
                ui.horizontal(|ui| {
                    let has_pending = modal.items.iter().any(|i| matches!(i.status, ImportStatus::Pending));
                    if self.signed_in {
                        ui.add_enabled_ui(has_pending && !modal.uploading, |ui| {
                            if primary_btn(ui, "Upload to Roblox").clicked() {
                                start_upload = true;
                            }
                        });
                    } else {
                        ui.label(RichText::new("Sign in to upload to Roblox.").size(11.5).color(theme::WARN));
                        if ui.button("Sign In").clicked() {
                            self.actions.push(Action::OpenAccount);
                        }
                    }
                    let has_local_models = modal.items.iter().any(|i| {
                        matches!(i.status, ImportStatus::Pending)
                            && matches!(i.path.extension().and_then(|e| e.to_str()), Some("rbxm" | "rbxmx"))
                    });
                    if has_local_models && ui.button("Insert .rbxm/.rbxmx Locally").clicked() {
                        for (idx, item) in modal.items.iter_mut().enumerate() {
                            if matches!(item.status, ImportStatus::Pending)
                                && matches!(item.path.extension().and_then(|e| e.to_str()), Some("rbxm" | "rbxmx"))
                            {
                                item.status = ImportStatus::LocalImported;
                                local_imports.push((idx, item.path.clone()));
                            }
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });
            });

        if close_clicked {
            open = false;
        }

        for (_, p) in local_imports {
            self.actions.push(Action::ImportLocalModel(p));
        }

        if start_upload {
            if let Some(sess) = self.session.as_ref().map(|s| s.downloader()) {
                modal.uploading = true;
                let gid = modal.target_group;
                for (index, item) in modal.items.iter_mut().enumerate() {
                    if !matches!(item.status, ImportStatus::Pending) {
                        continue;
                    }
                    item.status = ImportStatus::Uploading;
                    let item_clone = item.clone();
                    let sess_clone = sess.downloader();
                    self.pool.spawn(move |_| {
                        let result = super::api::upload_import_item(&sess_clone, &item_clone, gid);
                        Some(super::Msg::UploadFinished { index, result })
                    });
                }
            }
        }

        if open {
            self.import_modal = Some(modal);
        }
    }

    fn refresh_current(&mut self, universe_id: Option<u64>) {
        match &self.persist.scope {
            Scope::Places => {
                if let Some(u) = universe_id {
                    self.ensure_places(u, true);
                }
            }
            Scope::Experience => {
                self.experience_rev = u64::MAX;
            }
            Scope::MyInventory { .. } => self.ensure_inventory("user", None, true),
            Scope::Group { group_id, .. } => {
                let gid = *group_id;
                let key = format!("group:{gid}");
                self.ensure_inventory(&key, Some(gid), true);
            }
            Scope::RecentUploads => {}
        }
        self.thumbs.retry_failed();
    }

    fn group_name(&self, gid: u64) -> Option<String> {
        self.groups
            .as_ref()?
            .as_ref()
            .ok()?
            .iter()
            .find(|(id, _)| *id == gid)
            .map(|(_, n)| n.clone())
    }
}

// ------------------------------------------------------------------ small UI helpers

fn sidebar_section(ui: &mut egui::Ui, title: &str, expanded: &mut bool) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 4.0, TILE);
    }
    let chev = if *expanded { "chevron-down" } else { "chevron-right" };
    icon(ui.painter(), Rect::from_min_size(r.min + Vec2::new(4.0, 5.0), Vec2::splat(12.0)), chev, theme::TEXT_DIM);
    ui.painter().with_clip_rect(r.shrink2(Vec2::new(4.0, 0.0))).text(
        Pos2::new(r.min.x + 19.0, r.center().y),
        Align2::LEFT_CENTER,
        title,
        bold(10.5),
        theme::TEXT_DIM,
    );
    if resp.clicked() {
        *expanded = !*expanded;
    }
    *expanded
}

fn sidebar_item(ui: &mut egui::Ui, icon_name: &str, label: &str, active: bool, count: Option<usize>, indent: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    let bg = if active {
        theme::SELECT_BG
    } else if resp.hovered() {
        TILE
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(r, 4.0, bg);
    }
    let fg = if active { Color32::WHITE } else { theme::TEXT };
    let ic = if icon_name == "folder" { FOLDER_GOLD } else if active { Color32::WHITE } else { theme::TEXT_DIM };
    icon(ui.painter(), Rect::from_min_size(Pos2::new(r.min.x + indent, r.min.y + 5.0), Vec2::splat(14.0)), icon_name, ic);

    let count_w = if count.is_some() { 30.0 } else { 4.0 };
    let text_clip = Rect::from_min_max(Pos2::new(r.min.x + indent + 19.0, r.min.y), Pos2::new(r.max.x - count_w, r.max.y));
    ui.painter().with_clip_rect(text_clip).text(
        Pos2::new(text_clip.min.x, r.center().y),
        Align2::LEFT_CENTER,
        label,
        if active { medium(12.0) } else { FontId::proportional(12.0) },
        fg,
    );
    if let Some(n) = count {
        ui.painter().text(
            Pos2::new(r.max.x - 6.0, r.center().y),
            Align2::RIGHT_CENTER,
            n.to_string(),
            FontId::proportional(10.5),
            if active { Color32::from_rgb(190, 215, 255) } else { theme::TEXT_DIM },
        );
    }
    resp
}

fn draw_chip(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let text = format!("{label}  ×");
    let galley = ui.painter().layout_no_wrap(text, FontId::proportional(11.0), theme::TEXT);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 14.0, 20.0), Sense::click());
    ui.painter().rect_filled(r, 10.0, if resp.hovered() { TILE_HOVER } else { CHIP_BG });
    ui.painter().galley(Pos2::new(r.min.x + 7.0, r.center().y - galley.size().y * 0.5), galley, theme::TEXT);
    resp
}

fn primary_btn(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), medium(12.0), Color32::WHITE);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(galley.size().x + 20.0, 26.0), Sense::click());
    let bg = if resp.hovered() { Color32::from_rgb(70, 114, 255) } else { theme::ACCENT };
    ui.painter().rect_filled(r, 4.0, bg);
    ui.painter().galley(r.center() - galley.size() * 0.5, galley, Color32::WHITE);
    resp
}

fn detail_row(ui: &mut egui::Ui, key: &str, val: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{key}:")).size(11.5).color(theme::TEXT_DIM));
        ui.label(RichText::new(val).size(11.5));
    });
}

fn draw_empty_state(
    ui: &mut egui::Ui,
    icon_name: &str,
    title: &str,
    body: &str,
    cta: Option<(&str, Action)>,
    actions: &mut Vec<Action>,
) {
    ui.vertical_centered(|ui| {
        let avail_h = ui.available_height();
        ui.add_space((avail_h * 0.25).max(16.0));
        let (ir, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
        icon(ui.painter(), ir, icon_name, theme::TEXT_DIM);
        ui.add_space(6.0);
        ui.label(RichText::new(title).font(bold(13.5)));
        ui.add_space(3.0);
        ui.add(egui::Label::new(RichText::new(body).size(12.0).color(theme::TEXT_DIM)).wrap());
        if let Some((btn_label, act)) = cta {
            ui.add_space(10.0);
            if primary_btn(ui, btn_label).clicked() {
                actions.push(act);
            }
        }
    });
}

fn draw_loading_state(ui: &mut egui::Ui, msg: &str) {
    ui.vertical_centered(|ui| {
        let avail_h = ui.available_height();
        ui.add_space((avail_h * 0.32).max(20.0));
        ui.spinner();
        ui.add_space(6.0);
        ui.label(RichText::new(msg).size(12.0).color(theme::TEXT_DIM));
    });
}

fn draw_error_state(ui: &mut egui::Ui, msg: &str, mut on_retry: impl FnMut()) {
    ui.vertical_centered(|ui| {
        let avail_h = ui.available_height();
        ui.add_space((avail_h * 0.28).max(20.0));
        ui.label(RichText::new("Could not load assets").font(bold(13.0)).color(theme::ERROR));
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(msg).size(11.5).color(theme::TEXT_DIM)).wrap());
        ui.add_space(8.0);
        if ui.button("Retry").clicked() {
            on_retry();
        }
    });
}
