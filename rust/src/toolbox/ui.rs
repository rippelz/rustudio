//! Drawing the Toolbox panel in Studio's layout (checks/toolbox/ref/NOTES.md,
//! "Panel layout"), in rustudio's dark Studio theme (theme.rs).

use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use super::api::{CreatorFilter, ListReq, Sort, Source};
use super::categories::{self, Kind, Tab};
use super::insert::Target;
use super::model::{self, Asset, Price};
use super::preview::PState;
use super::thumbs::{self, Thumb};
use super::{Ask, Nav};
use crate::app::Editor;
use rustudio_engine::theme;

/// The drag payload of a card (drop on the 3D view or an Explorer row).
#[derive(Debug, Clone, Copy)]
pub struct ToolboxDrag(pub u64);

/// Studio's link blue on the dark theme.
const LINK: Color32 = Color32::from_rgb(82, 156, 255);
const GOOD: Color32 = Color32::from_rgb(57, 196, 104);
const TILE: Color32 = Color32::from_rgb(44, 45, 51);
const TILE_HOVER: Color32 = Color32::from_rgb(56, 58, 66);
const CHIP: Color32 = Color32::from_rgb(52, 54, 61);
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;

#[derive(Default)]
pub struct UiState {
    pub hamburger: bool,
    pub expanded: HashSet<&'static str>,
    pub filters_open: bool,
    pub dropdown: Option<&'static str>,
    pub scroll_to_top: bool,
    pub installing: HashSet<u64>,
    pub deep_searches: Vec<(String, String)>,
    pub deep_inserts: Vec<u64>,
    pub deep_waits: HashMap<u64, u32>,
    pub search_focused: bool,
    pub more_menu: bool,
    pub restored: HashSet<String>,
    pub show_tree: bool,
    pub show_description: bool,
    /// Last frame's grid cell count drawn (perf report).
    pub cells_drawn: usize,
    pub creator_text: String,
    pub deep_frames: u32,
    pub deep_asked: HashSet<u64>,
    /// Harness: suggestions shown without focus, a card drawn hovered.
    pub force_suggest: bool,
    pub force_hover: Option<u64>,
    /// Harness: the answer given to the next prompt as soon as it shows
    /// (`toolbox:answer-when-asked:<answer>`), whenever the download ends.
    pub answer_when_asked: Option<String>,
    /// Harness: `toolbox:await-ask:<seconds>` holding the schedule until a
    /// prompt shows: (harness time unheld when it began, the hold before
    /// it, the wall-clock deadline). Downloads take wall time while
    /// --fixed-dt frames run as fast as they can, so a dialog that follows
    /// a download has no fixed harness time.
    pub await_ask: Option<(f32, f32, std::time::Instant, f32)>,
    /// Harness: `toolbox:suggest-pick:<n>` waiting for the autocomplete
    /// reply to the text typed (the action that came before it).
    pub pick_waiting: Option<usize>,
    /// Harness: the dock width asked for the Toolbox (`toolbox:ui:width:<px>`,
    /// applied by the app's dock).
    pub want_width: Option<f32>,
    /// What the panel drew last frame (`toolbox:layout:<tag>` reports it).
    pub layout: Layout,
    /// Harness: Toolbox actions taken (a dock width applied counts too), the
    /// count the panel's last draw had seen, and `toolbox:layout:<tag>`s
    /// waiting for a draw that saw every earlier action (a long frame can
    /// run a width change and the report in the same frame).
    pub action_seq: u64,
    pub drawn_seq: u64,
    pub layout_waiting: Vec<String>,
    pub session_checked: Option<std::time::Instant>,
    /// Cards / rows drawn this frame (asset id, rect) and the open dialog's
    /// buttons (label, rect): where the harness's `ptr:` actions point.
    pub card_rects: Vec<(u64, Rect)>,
    pub ask_buttons: Vec<(String, Rect)>,
}

/// Column counts the store drew (per kind of row), and the panel width.
#[derive(Default, Debug, Clone, Copy)]
pub struct Layout {
    pub width: f32,
    pub tile_cols: usize,
    pub lane_cards: usize,
    pub grid_cols: usize,
    pub grid_cell: f32,
}

/// Category tiles / subcategory pills: 2 a row at Studio's ~280 px panel
/// (checks/toolbox/ref/NOTES.md), more as it widens, each at least this wide.
const TILE_MIN: f32 = 120.0;
/// Swimlane cards: 3 a row at ~280 px (NOTES.md), more as it widens.
const LANE_MIN: f32 = 76.0;

/// Tiles a row at width `w` (never fewer than Studio's 2) and their width.
pub fn tile_cols(w: f32) -> (usize, f32) {
    let n = grid(w, TILE_MIN).0.max(2);
    (n, (w - GAP * (n as f32 - 1.0)) / n as f32)
}

/// Cards in a swimlane row at width `w` (never fewer than Studio's 3).
pub fn lane_cards(w: f32) -> (usize, f32) {
    let n = grid(w, LANE_MIN).0.max(3);
    (n, ((w - GAP * (n as f32 - 1.0)) / n as f32).max(40.0))
}

pub fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Popups drawn in the panel's dark style (Areas take the context's
/// default style, which can be the light one).
fn toolbox_popup(
    ui: &egui::Ui, id: &str, pos: Pos2, add: impl FnOnce(&mut egui::Ui),
) -> egui::InnerResponse<()> {
    let frame = egui::Frame::new()
        .fill(theme::PANEL_DARK)
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .corner_radius(4.0)
        .inner_margin(6.0);
    egui::Popup::new(ui.id().with(id), ui.ctx().clone(), pos, ui.layer_id())
        .kind(egui::PopupKind::Menu)
        .align_alternatives(&[])
        .frame(frame)
        .show(|ui| {
            ui.set_style(ui.ctx().style_of(egui::Theme::Dark));
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
            add(ui);
        })
        .expect("toolbox popup is open")
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn icon(p: &egui::Painter, r: Rect, name: &str, c: Color32) {
    rustudio_engine::ti::paint(p, r, name, c);
}

/// An icon button (square), returns the response.
fn icon_button(ui: &mut egui::Ui, name: &str, size: f32, active: bool, tip: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if active { theme::SELECT_BG } else if resp.hovered() { TILE_HOVER } else { TILE };
    ui.painter().rect_filled(r, 4.0, bg);
    icon(ui.painter(), r.shrink(size * 0.22), name, if active { Color32::WHITE } else { theme::TEXT });
    if tip.is_empty() { resp } else { resp.on_hover_text(tip) }
}

/// A rounded pill button ("Sort: Relevance ▾").
fn pill(ui: &mut egui::Ui, text: &str, open: bool, chevron: bool) -> egui::Response {
    let font = FontId::proportional(13.0);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), theme::TEXT).size().x) + if chevron { 34.0 } else { 22.0 };
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
    let bg = if open { theme::SELECT_BG } else if resp.hovered() { TILE_HOVER } else { CHIP };
    ui.painter().rect_filled(r, 13.0, bg);
    ui.painter().text(Pos2::new(r.min.x + 11.0, r.center().y), Align2::LEFT_CENTER, text, font, theme::TEXT);
    if chevron {
        icon(ui.painter(), Rect::from_center_size(Pos2::new(r.max.x - 14.0, r.center().y), Vec2::splat(12.0)), if open { "chevron-up" } else { "chevron-down" }, theme::TEXT);
    }
    resp
}

/// Text cut to `lines` lines with an ellipsis.
fn clipped(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32, lines: usize) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, width.max(10.0));
    job.wrap.max_rows = lines;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    ui.fonts_mut(|f| f.layout_job(job))
}

fn skeleton(ui: &egui::Ui, r: Rect) {
    let t = ui.input(|i| i.time) as f32;
    let a = 0.55 + 0.25 * (t * 3.0).sin().abs();
    ui.painter().rect_filled(r, 6.0, TILE.gamma_multiply(a));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
}

/// Columns and cell width for a grid in `w` px with cells at least `min` wide.
pub fn grid(w: f32, min: f32) -> (usize, f32) {
    let cols = (((w + GAP) / (min + GAP)).floor() as usize).max(1);
    let cell = ((w - GAP * (cols as f32 - 1.0)) / cols as f32).max(20.0);
    (cols, cell)
}

/// What a card / page asks the editor to do.
enum Cmd {
    /// A search suggestion chosen (its query in its category).
    Choose(super::Suggestion),
    Insert(u64),
    Open(u64),
    Nav(Nav),
    Back,
    Home,
    Search(String),
    LoadMore(ListReq),
    Retry(ListReq),
    SignIn,
    Play(u64),
    Pause(u64),
    Vote(u64, Option<bool>),
    Favorite(u64, bool),
    Copy(String),
    Url(String),
    Answer(bool, bool),
    CancelInsert(u64),
}

impl Editor {
    /// The Toolbox panel.
    pub fn toolbox_ui(&mut self, ui: &mut egui::Ui) {
        let t0 = std::time::Instant::now();
        let mut cmds: Vec<Cmd> = Vec::new();
        self.toolbox.ui.drawn_seq = self.toolbox.ui.action_seq;
        self.toolbox.ui.layout.width = ui.available_width();
        self.toolbox.ui.card_rects.clear();
        self.toolbox.ui.ask_buttons.clear();
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        self.toolbox_tabs(ui);
        ui.add_space(2.0);
        if !self.toolbox.signed_in {
            signed_out(ui, &mut cmds);
        } else {
            match self.toolbox.st.tab {
                Tab::Store => self.toolbox_store(ui, &mut cmds),
                t => self.toolbox_list_tab(ui, t, &mut cmds),
            }
        }
        self.toolbox_inserts_line(ui, &mut cmds);
        self.toolbox_dialogs(ui.ctx(), &mut cmds);
        self.toolbox_drag_ghost(ui.ctx());
        for c in cmds {
            self.toolbox_cmd(ui.ctx(), c);
        }
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.toolbox.ui_ms.push(ms);
    }

    fn toolbox_cmd(&mut self, ctx: &egui::Context, c: Cmd) {
        let tb = &mut self.toolbox;
        match c {
            Cmd::Insert(id) => {
                self.toolbox_insert(id, Target::Click);
            }
            Cmd::Open(id) => {
                tb.want_details(vec![id]);
                tb.want_favorites(id);
                tb.want_owned(id);
                tb.ui.more_menu = false;
                tb.ui.show_tree = false;
                tb.push(Nav::Asset(id));
            }
            Cmd::Nav(n) => {
                if let Nav::Results { query, .. } = &n {
                    tb.st.query = query.clone();
                }
                tb.st.filters.facets.clear();
                tb.push(n)
            }
            Cmd::Back => tb.back(),
            Cmd::Home => tb.home(),
            Cmd::Search(q) => tb.search(&q),
            Cmd::Choose(s) => tb.choose_suggestion(&s),
            Cmd::LoadMore(r) => tb.load_more(&r),
            Cmd::Retry(r) => tb.retry(&r),
            Cmd::SignIn => self.game.account.open = true,
            Cmd::Play(id) => tb.play(id),
            Cmd::Pause(id) => tb.pause(id),
            Cmd::Vote(id, v) => tb.mutate(super::api::vote_request(id, v), "vote"),
            Cmd::Favorite(id, on) => {
                let uid = tb.user_id;
                tb.mutate(super::api::favorite_request(uid, id, on), "save");
            }
            Cmd::Copy(t) => ctx.copy_text(t),
            Cmd::Url(u) => open_url(&u),
            Cmd::Answer(a, b) => self.toolbox_answer(a, b),
            Cmd::CancelInsert(j) => tb.cancel_insert(j),
        }
    }

    fn toolbox_tabs(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        let h = 32.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
        let n = Tab::ALL.len() as f32;
        let icons = ["shopping-bag", "layout-grid", "clock", "bulb"];
        for (i, t) in Tab::ALL.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(rect.min.x + w * i as f32 / n, rect.min.y), Vec2::new(w / n, h));
            let resp = ui.interact(r, ui.id().with(("tbtab", i)), Sense::click()).on_hover_text(t.label());
            let sel = self.toolbox.st.tab == *t;
            let bg = if sel { theme::ACTIVE_TAB } else if resp.hovered() { theme::HOVER_BG } else { theme::PANEL_DARK };
            ui.painter().rect_filled(r.shrink2(Vec2::new(0.5, 0.0)), 0.0, bg);
            if sel {
                ui.painter().rect_filled(Rect::from_min_size(Pos2::new(r.min.x, r.max.y - 2.0), Vec2::new(r.width(), 2.0)), 0.0, theme::ACCENT);
            }
            let name = if sel { format!("{}-filled", icons[i]) } else { icons[i].to_string() };
            let name = if rustudio_engine::ti::has(&name) { name } else { icons[i].to_string() };
            icon(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(18.0)), &name, if sel { LINK } else { theme::TEXT });
            if resp.clicked() {
                self.toolbox.st.tab = *t;
                self.toolbox.ui.dropdown = None;
            }
        }
    }

    // ---------------------------------------------------------------- store

    fn toolbox_store(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        // Header: ≡ Creator Store
        let ham_anchor = ui
            .horizontal(|ui| {
                ui.add_space(4.0);
                if icon_button(ui, "menu-2", 24.0, self.toolbox.ui.hamburger, "Categories").clicked() {
                    self.toolbox.ui.hamburger = !self.toolbox.ui.hamburger;
                }
                ui.label(egui::RichText::new("Creator Store").font(bold(14.0)).color(theme::TEXT));
            })
            .response
            .rect
            .left_bottom();
        // Search row
        let mut submit = None;
        let search_rect = ui
            .horizontal(|ui| {
                ui.add_space(4.0);
                let w = (ui.available_width() - 70.0).max(60.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::hover());
                ui.painter().rect(r, 4.0, theme::PANEL_DARK, Stroke::new(1.0, if self.toolbox.ui.search_focused { theme::SELECT } else { theme::DIVIDER }), StrokeKind::Inside);
                icon(ui.painter(), Rect::from_center_size(Pos2::new(r.min.x + 14.0, r.center().y), Vec2::splat(15.0)), "search", theme::TEXT_DIM);
                let text_r = Rect::from_min_max(Pos2::new(r.min.x + 26.0, r.min.y + 2.0), Pos2::new(r.max.x - 26.0, r.max.y - 2.0));
                let te = egui::TextEdit::singleline(&mut self.toolbox.st.query).hint_text("Search").frame(egui::Frame::NONE).desired_width(text_r.width()).vertical_align(egui::Align::Center);
                let resp = ui.put(text_r, te);
                self.toolbox.ui.search_focused = resp.has_focus();
                if resp.changed() {
                    let (cat, q) = (self.toolbox.current_cat(), self.toolbox.st.query.clone());
                    self.toolbox.want_suggestions(cat.as_deref(), &q);
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = Some(self.toolbox.st.query.clone());
                }
                if !self.toolbox.st.query.is_empty() {
                    let xr = Rect::from_center_size(Pos2::new(r.max.x - 13.0, r.center().y), Vec2::splat(18.0));
                    let x = ui.interact(xr, ui.id().with("tbclear"), Sense::click());
                    icon(ui.painter(), xr.shrink(2.0), "x", if x.hovered() { theme::TEXT } else { theme::TEXT_DIM });
                    if x.clicked() {
                        self.toolbox.st.query.clear();
                        self.toolbox.suggest = Default::default();
                        if matches!(self.toolbox.nav(), Nav::Results { .. }) {
                            cmds.push(Cmd::Back);
                        }
                    }
                }
                if icon_button(ui, "bookmark", 28.0, false, "Saved: your saved Creator Store items").clicked() {
                    cmds.push(Cmd::Url("https://create.roblox.com/store/saved".into()));
                }
                let active = self.toolbox.st.filters.active();
                let fb = icon_button(ui, "adjustments-horizontal", 28.0, self.toolbox.ui.filters_open, "Filters");
                if active {
                    ui.painter().circle_filled(fb.rect.right_top() + Vec2::new(-4.0, 4.0), 3.5, theme::ACCENT);
                }
                if fb.clicked() {
                    self.toolbox.ui.filters_open = !self.toolbox.ui.filters_open;
                }
                r
            })
            .inner;
        if let Some(q) = submit {
            self.toolbox.suggest = Default::default();
            cmds.push(Cmd::Search(q));
        }
        ui.add_space(2.0);
        ui.separator();
        let body_top = ui.cursor().min.y;
        match self.toolbox.nav().clone() {
            Nav::Home => self.store_home(ui, cmds),
            Nav::Category(k) => self.store_category(ui, &k, cmds),
            Nav::Results { cat, query, tile } => self.store_results(ui, &cat, &query, tile.as_deref(), cmds),
            Nav::Asset(id) => self.store_asset(ui, id, cmds),
        }
        // Over the body: suggestions, the category tree, the filter sheet.
        if (self.toolbox.ui.search_focused || self.toolbox.ui.force_suggest) && !self.toolbox.st.query.trim().is_empty() {
            // (asked again when the category changed under the same text)
            let (cat, q) = (self.toolbox.current_cat(), self.toolbox.st.query.clone());
            self.toolbox.want_suggestions(cat.as_deref(), &q);
            self.suggestions_popup(ui, search_rect, cmds);
        }
        if self.toolbox.ui.hamburger {
            self.hamburger_popup(ui, ham_anchor, cmds);
        }
        if self.toolbox.ui.filters_open {
            self.filter_sheet(ui, body_top);
        }
    }

    fn suggestions_popup(&mut self, ui: &mut egui::Ui, anchor: Rect, cmds: &mut Vec<Cmd>) {
        let rows = self.toolbox.suggestions_for(&self.toolbox.st.query).to_vec();
        if rows.is_empty() {
            return;
        }
        toolbox_popup(ui, "tbsuggest", anchor.left_bottom() + Vec2::new(0.0, 2.0), |ui| {
            ui.set_width(anchor.width() - 12.0);
            for s in rows.iter().take(8) {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, 3.0, theme::HOVER_BG);
                }
                icon(ui.painter(), Rect::from_center_size(Pos2::new(r.min.x + 12.0, r.center().y), Vec2::splat(14.0)), cat_icon(s.icon), theme::TEXT_DIM);
                let tw = r.width() - 32.0;
                let q = clipped(ui, &s.query, bold(13.0), theme::TEXT, tw, 1);
                ui.painter().galley(Pos2::new(r.min.x + 28.0, r.min.y + 3.0), q, theme::TEXT);
                let under = clipped(ui, &format!("in {}", s.label), FontId::proportional(11.5), theme::TEXT_DIM, tw, 1);
                ui.painter().galley(Pos2::new(r.min.x + 28.0, r.max.y - 3.0 - under.size().y), under, theme::TEXT_DIM);
                // (pressed while the text field still has focus)
                if resp.is_pointer_button_down_on() || resp.clicked() {
                    self.toolbox.suggest = Default::default();
                    cmds.push(Cmd::Choose(s.clone()));
                }
            }
        });
    }

    fn hamburger_popup(&mut self, ui: &mut egui::Ui, anchor: Pos2, cmds: &mut Vec<Cmd>) {
        let w = (ui.available_width() - 16.0).clamp(180.0, 260.0);
        let max_h = (ui.clip_rect().max.y - anchor.y - 20.0).max(160.0).min(520.0);
        let area = toolbox_popup(ui, "tbham", anchor + Vec2::new(8.0, 0.0), |ui| {
            ui.set_width(w);
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                for t in categories::TOP {
                    let open = self.toolbox.ui.expanded.contains(t.key) || t.key == "3d" && self.toolbox.ui.expanded.is_empty();
                    let kids = categories::children(t.key);
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 3.0, theme::HOVER_BG);
                    }
                    icon(ui.painter(), Rect::from_center_size(Pos2::new(r.min.x + 14.0, r.center().y), Vec2::splat(15.0)), cat_icon(t.icon), theme::TEXT);
                    ui.painter().text(Pos2::new(r.min.x + 32.0, r.center().y), Align2::LEFT_CENTER, t.label, bold(13.0), theme::TEXT);
                    if !kids.is_empty() {
                        icon(ui.painter(), Rect::from_center_size(Pos2::new(r.max.x - 14.0, r.center().y), Vec2::splat(12.0)), if open { "chevron-down" } else { "chevron-right" }, theme::TEXT_DIM);
                    }
                    if resp.clicked() {
                        if kids.is_empty() {
                            cmds.push(Cmd::Home);
                            cmds.push(Cmd::Nav(Nav::Category(t.key.into())));
                            self.toolbox.ui.hamburger = false;
                        } else if open {
                            self.toolbox.ui.expanded.remove(t.key);
                            self.toolbox.ui.expanded.insert("");
                        } else {
                            self.toolbox.ui.expanded.insert(t.key);
                        }
                    }
                    if open && !kids.is_empty() {
                        let all = format!("All {}", t.label);
                        let mut rows: Vec<(String, Nav)> = vec![(all, Nav::Category(t.key.into()))];
                        for k in &kids {
                            rows.push((k.label.to_string(), Nav::Results { cat: k.key.into(), query: String::new(), tile: None }));
                        }
                        for (label, nav) in rows {
                            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
                            if resp.hovered() {
                                ui.painter().rect_filled(r, 3.0, theme::HOVER_BG);
                            }
                            ui.painter().text(Pos2::new(r.min.x + 32.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), theme::TEXT);
                            if resp.clicked() {
                                cmds.push(Cmd::Home);
                                if !matches!(nav, Nav::Category(_)) {
                                    cmds.push(Cmd::Nav(Nav::Category(t.key.into())));
                                }
                                cmds.push(Cmd::Nav(nav));
                                self.toolbox.ui.hamburger = false;
                            }
                        }
                        ui.separator();
                    }
                }
            });
        });
        if area.response.clicked_elsewhere() {
            self.toolbox.ui.hamburger = false;
        }
    }

    fn view_key(&self) -> String {
        match self.toolbox.st.tab {
            Tab::Store => format!("store:{:?}", self.toolbox.nav()),
            t => format!("{t:?}:{:?}", self.toolbox.tab_req(t)),
        }
    }

    /// A vertical scroll area for the current view that remembers its offset
    /// (across hiding and restarts) and reports when the end is near.
    fn scroll_view(&mut self, ui: &mut egui::Ui, add: impl FnOnce(&mut Self, &mut egui::Ui, Rect) -> ()) -> bool {
        let key = self.view_key();
        let mut sa = egui::ScrollArea::vertical().id_salt(("tbscroll", key.clone())).auto_shrink([false, false]);
        if self.toolbox.ui.scroll_to_top {
            sa = sa.vertical_scroll_offset(0.0);
            self.toolbox.ui.scroll_to_top = false;
        } else if !self.toolbox.ui.restored.contains(&key) {
            if let Some(off) = self.toolbox.st.scroll.get(&key) {
                sa = sa.vertical_scroll_offset(*off);
            }
        }
        self.toolbox.ui.restored.insert(key.clone());
        let out = sa.show_viewport(ui, |ui, vp| add(self, ui, vp));
        let off = out.state.offset.y;
        self.toolbox.st.scroll.insert(key, off);
        let content = out.content_size.y;
        let view = out.inner_rect.height();
        off + view > content - 500.0
    }

    fn store_home(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        self.scroll_view(ui, |ed, ui, _vp| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Categories").font(bold(14.0)).color(theme::TEXT));
            let w = ui.available_width() - PAD;
            let (n, cw) = tile_cols(w);
            ed.toolbox.ui.layout.tile_cols = n;
            for row in categories::TOP.chunks(n) {
                ui.horizontal(|ui| {
                    for c in row {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(cw, 46.0), Sense::click());
                        ui.painter().rect_filled(r, 4.0, if resp.hovered() { TILE_HOVER } else { TILE });
                        icon(ui.painter(), Rect::from_center_size(Pos2::new(r.center().x, r.min.y + 15.0), Vec2::splat(15.0)), cat_icon(c.icon), theme::TEXT);
                        ui.painter().text(Pos2::new(r.center().x, r.max.y - 12.0), Align2::CENTER_CENTER, c.label, FontId::proportional(13.0), theme::TEXT);
                        if resp.clicked() {
                            cmds.push(Cmd::Nav(Nav::Category(c.key.into())));
                        }
                    }
                });
            }
            for (section, title) in categories::HOME_SECTIONS {
                let r = super::Toolbox::home_req(section);
                ed.swimlane(ui, title, &r, Nav::Results { cat: format!("home:{section}"), query: String::new(), tile: None }, cmds);
            }
        });
    }

    fn swimlane(&mut self, ui: &mut egui::Ui, title: &str, r: &ListReq, see_all: Nav, cmds: &mut Vec<Cmd>) {
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(PAD);
            let resp = ui.add(egui::Label::new(egui::RichText::new("See All  ›").size(13.0).color(theme::TEXT)).sense(Sense::click()));
            if resp.clicked() {
                cmds.push(Cmd::Nav(see_all));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(title).font(bold(14.0)).color(theme::TEXT)).truncate());
            });
        });
        let l = self.toolbox.ensure(r).clone();
        let w = ui.available_width() - PAD;
        let (n, cell) = lane_cards(w);
        self.toolbox.ui.layout.lane_cards = n;
        let ids: Vec<u64> = l.ids.iter().copied().take(n).collect();
        ui.horizontal(|ui| {
            if ids.is_empty() {
                if let Some(e) = &l.error {
                    ui.colored_label(theme::ERROR, e.describe());
                } else if l.loading {
                    for _ in 0..n {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(cell, cell + 44.0), Sense::hover());
                        skeleton(ui, Rect::from_min_size(r.min, Vec2::splat(cell)));
                    }
                } else {
                    ui.colored_label(theme::TEXT_DIM, "Nothing here yet.");
                }
            }
            for id in ids {
                self.card(ui, id, cell, true, cmds);
            }
        });
    }

    fn store_category(&mut self, ui: &mut egui::Ui, key: &str, cmds: &mut Vec<Cmd>) {
        let Some(c) = categories::find(key) else {
            cmds.push(Cmd::Home);
            return;
        };
        let kids = categories::children(key);
        self.scroll_view(ui, |ed, ui, _vp| {
            breadcrumb(ui, &["Home", c.label], cmds);
            if !c.blurb.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(c.blurb).size(12.5).color(theme::TEXT_DIM)).wrap());
            }
            if key == "audio" {
                ed.audio_page(ui, cmds);
                return;
            }
            let w = ui.available_width() - PAD;
            let (n, cw) = tile_cols(w);
            ed.toolbox.ui.layout.tile_cols = n;
            for row in kids.chunks(n) {
                ui.horizontal(|ui| {
                    for k in row {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(cw, 28.0), Sense::click());
                        ui.painter().rect_filled(r, 4.0, if resp.hovered() { TILE_HOVER } else { TILE });
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, k.label, FontId::proportional(13.0), theme::TEXT);
                        if resp.clicked() {
                            cmds.push(Cmd::Nav(Nav::Results { cat: k.key.into(), query: String::new(), tile: None }));
                        }
                    }
                });
            }
            let all = ed.toolbox.store_req(key, "", None);
            ed.swimlane(ui, "Discover Something New", &all, Nav::Results { cat: key.into(), query: String::new(), tile: None }, cmds);
            for k in &kids {
                let r = ed.toolbox.store_req(k.key, "", None);
                ed.swimlane(ui, k.label, &r, Nav::Results { cat: k.key.into(), query: String::new(), tile: None }, cmds);
            }
        });
    }

    fn audio_page(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        for (title, tiles) in categories::AUDIO_SECTIONS {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(*title).font(bold(14.0)).color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(PAD);
                    if ui.add(egui::Label::new(egui::RichText::new("See All  ›").size(13.0).color(theme::TEXT)).sense(Sense::click())).clicked() {
                        cmds.push(Cmd::Nav(Nav::Results { cat: "audio".into(), query: String::new(), tile: tiles.first().map(|t| t.key.to_string()) }));
                    }
                });
            });
            egui::ScrollArea::horizontal().id_salt(("tbaudio", *title)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for t in tiles.iter() {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(104.0, 104.0), Sense::click());
                        let c = Color32::from_rgb((t.color >> 16) as u8, (t.color >> 8) as u8, t.color as u8);
                        ui.painter().rect_filled(r, 6.0, if resp.hovered() { c.gamma_multiply(1.25) } else { c });
                        let g = clipped(ui, t.label, bold(13.0), Color32::WHITE, r.width() - 16.0, 2);
                        ui.painter().galley(r.min + Vec2::new(8.0, 8.0), g, Color32::WHITE);
                        icon(ui.painter(), Rect::from_center_size(r.center() + Vec2::new(10.0, 14.0), Vec2::splat(38.0)), if title.starts_with("SFX") { "volume" } else { "music" }, Color32::from_white_alpha(170));
                        if resp.clicked() {
                            cmds.push(Cmd::Nav(Nav::Results { cat: "audio".into(), query: String::new(), tile: Some(t.key.into()) }));
                        }
                    }
                });
            });
        }
    }

    fn results_req(&self, cat: &str, query: &str, tile: Option<&str>) -> ListReq {
        match cat.strip_prefix("home:") {
            Some(section) => super::Toolbox::home_req(section),
            None => self.toolbox.store_req(cat, query, tile),
        }
    }

    fn store_results(&mut self, ui: &mut egui::Ui, cat: &str, query: &str, tile: Option<&str>, cmds: &mut Vec<Cmd>) {
        let req = self.results_req(cat, query, tile);
        self.toolbox.ensure(&req);
        let c = categories::find(cat);
        let kind = c.as_ref().map(|c| c.kind).unwrap_or(Kind::Model);
        let near_end = self.scroll_view(ui, |ed, ui, vp| {
            // breadcrumb: Home / Top / Sub
            let mut crumbs: Vec<String> = vec!["Home".into()];
            if let Some(section) = cat.strip_prefix("home:") {
                crumbs.push(categories::HOME_SECTIONS.iter().find(|s| s.0 == section).map(|s| s.1).unwrap_or(section).to_string());
            } else if let Some(c) = &c {
                let top = categories::top_of(c.key);
                if c.key != "models" {
                    crumbs.push(top.label.into());
                }
                if c.key != top.key {
                    crumbs.push(c.label.into());
                }
                if let Some(t) = tile.and_then(super::audio_tile) {
                    crumbs.push(t.label.into());
                }
            }
            let crumb_refs: Vec<&str> = crumbs.iter().map(String::as_str).collect();
            breadcrumb(ui, &crumb_refs, cmds);
            if let Some(c) = &c {
                let b = categories::blurb(c.key);
                if !b.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(b).size(12.5).color(theme::TEXT_DIM)).wrap());
                }
            }
            if !cat.starts_with("home:") {
                ed.filter_pills(ui, kind);
            }
            let l = ed.toolbox.list(&req).cloned().unwrap_or_default();
            if !l.facets.is_empty() {
                egui::ScrollArea::horizontal().id_salt("tbfacets").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for f in &l.facets {
                            let on = ed.toolbox.st.filters.facets.contains(f);
                            if pill(ui, f, on, false).clicked() {
                                if on {
                                    ed.toolbox.st.filters.facets.retain(|x| x != f);
                                } else {
                                    ed.toolbox.st.filters.facets.push(f.clone());
                                }
                            }
                        }
                    });
                });
            }
            if let Some(cq) = &l.corrected {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Did you mean").font(bold(15.0)).color(theme::ERROR));
                    if ui.add(egui::Label::new(egui::RichText::new(cq).font(bold(15.0)).color(LINK)).sense(Sense::click())).clicked() {
                        cmds.push(Cmd::Search(cq.clone()));
                    }
                });
            }
            if let Some(t) = l.total {
                if !query.is_empty() {
                    ui.label(egui::RichText::new(format!("Showing {t} results for {query}")).size(12.5).color(theme::TEXT_DIM));
                }
            }
            ed.results_grid(ui, &req, &l, kind, vp, cmds);
        });
        if near_end {
            if let Some(l) = self.toolbox.list(&req) {
                if l.can_load_more() && l.pages > 0 {
                    cmds.push(Cmd::LoadMore(req));
                }
            }
        }
    }

    fn filter_pills(&mut self, ui: &mut egui::Ui, kind: Kind) {
        let f = &self.toolbox.st.filters;
        let sort = match f.sort {
            Sort::Relevance => "Relevance",
            Sort::Ratings => "Rating",
            Sort::Newest => "Newest",
        };
        let price = if f.free_only {
            "Free".to_string()
        } else {
            match (f.min_cents, f.max_cents) {
                (None, None) => "Any".to_string(),
                (a, b) => format!("${}–${}", a.unwrap_or(0) / 100, b.map(|b| (b / 100).to_string()).unwrap_or("50+".into())),
            }
        };
        let mut open = self.toolbox.ui.dropdown;
        // Studio's pills per category: Models Sort / Price / Contains, Audio
        // Sort / Type / Duration, Plugins Price, Decals / Videos / Fonts none.
        if matches!(kind, Kind::Decal | Kind::Video | Kind::Font) {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let s = if kind == Kind::Plugin { None } else { Some(pill(ui, &format!("Sort: {sort}"), open == Some("sort"), true)) };
            let Some(s) = s else {
                let p = pill(ui, &format!("Price: {price}"), open == Some("price"), true);
                if p.clicked() {
                    open = if open == Some("price") { None } else { Some("price") };
                }
                if open == Some("price") {
                    popup_below(ui, &p, "tbprice", |ui| {
                        ui.label(egui::RichText::new("Price").font(bold(13.0)).color(theme::TEXT));
                        price_editor(ui, &mut self.toolbox.st.filters);
                    });
                }
                return;
            };
            if s.clicked() {
                open = if open == Some("sort") { None } else { Some("sort") };
            }
            if open == Some("sort") {
                popup_below(ui, &s, "tbsort", |ui| {
                    for (label, v) in [("Relevance", Sort::Relevance), ("Rating", Sort::Ratings)] {
                        if ui.selectable_label(self.toolbox.st.filters.sort == v, label).clicked() {
                            self.toolbox.st.filters.sort = v;
                            open = None;
                        }
                    }
                });
            }
            if kind == Kind::Audio {
                let ty = self.toolbox.st.filters.audio_type.clone().map(|t| if t == "SoundEffect" { "Sound Effects".to_string() } else { t }).unwrap_or("All".into());
                let p = pill(ui, &format!("Type: {ty}"), open == Some("atype"), true);
                if p.clicked() {
                    open = if open == Some("atype") { None } else { Some("atype") };
                }
                if open == Some("atype") {
                    popup_below(ui, &p, "tbatype", |ui| {
                        for (label, v) in [("All", None), ("Music", Some("Music")), ("Sound Effects", Some("SoundEffect"))] {
                            if ui.selectable_label(self.toolbox.st.filters.audio_type.as_deref() == v, label).clicked() {
                                self.toolbox.st.filters.audio_type = v.map(str::to_string);
                                open = None;
                            }
                        }
                    });
                }
                let len = match (self.toolbox.st.filters.audio_min_s, self.toolbox.st.filters.audio_max_s) {
                    (None, None) => "All".to_string(),
                    (a, b) => format!("{}–{}", model::duration_label(a.unwrap_or(0) as f64), b.map(|b| model::duration_label(b as f64)).unwrap_or("max".into())),
                };
                let p = pill(ui, &format!("Duration: {len}"), open == Some("length"), true);
                if p.clicked() {
                    open = if open == Some("length") { None } else { Some("length") };
                }
                if open == Some("length") {
                    popup_below(ui, &p, "tblen", |ui| {
                        for (label, a, b) in [("All", None, None), ("Under 0:10", None, Some(10)), ("0:10 – 1:00", Some(10), Some(60)), ("1:00 – 3:00", Some(60), Some(180)), ("Over 3:00", Some(180), None)] {
                            if ui.selectable_label(self.toolbox.st.filters.audio_min_s == a && self.toolbox.st.filters.audio_max_s == b, label).clicked() {
                                self.toolbox.st.filters.audio_min_s = a;
                                self.toolbox.st.filters.audio_max_s = b;
                                open = None;
                            }
                        }
                    });
                }
            } else {
                let p = pill(ui, &format!("Price: {price}"), open == Some("price"), true);
                if p.clicked() {
                    open = if open == Some("price") { None } else { Some("price") };
                }
                if open == Some("price") {
                    popup_below(ui, &p, "tbprice", |ui| {
                        ui.label(egui::RichText::new("Price").font(bold(13.0)).color(theme::TEXT));
                        price_editor(ui, &mut self.toolbox.st.filters);
                    });
                }
                if matches!(kind, Kind::Model | Kind::Package) {
                    let n = self.toolbox.st.filters.contains.len();
                    let p = pill(ui, &if n == 0 { "Contains".to_string() } else { format!("Contains ({n})") }, open == Some("contains"), true);
                    if p.clicked() {
                        open = if open == Some("contains") { None } else { Some("contains") };
                    }
                    if open == Some("contains") {
                        popup_below(ui, &p, "tbcontains", |ui| contains_editor(ui, &mut self.toolbox.st.filters));
                    }
                }
            }
        });
        self.toolbox.ui.dropdown = open;
    }

    /// The cards of a list: a virtual grid (only visible rows are laid out),
    /// skeletons while the next page loads, empty / error states.
    fn results_grid(&mut self, ui: &mut egui::Ui, req: &ListReq, l: &super::List, kind: Kind, vp: Rect, cmds: &mut Vec<Cmd>) {
        let audio = kind == Kind::Audio;
        let w = ui.available_width() - PAD;
        let (cols, cell) = if audio { (1, w) } else { grid(w, 88.0) };
        self.toolbox.ui.layout.grid_cols = cols;
        self.toolbox.ui.layout.grid_cell = cell;
        let row_h = if audio { 26.0 } else { cell + 44.0 };
        if audio && n_or_loading(l) {
            audio_header(ui, w);
        }
        let n = l.ids.len();
        let skel = if l.loading { if n == 0 { cols * 3 } else { cols } } else { 0 };
        let rows = (n + skel).div_ceil(cols);
        let top = ui.cursor().min.y;
        // visible rows (vp is in content coordinates, the cursor too)
        let origin = ui.min_rect().min.y;
        let first = (((vp.min.y + origin - top) / row_h).floor().max(0.0)) as usize;
        let last = ((((vp.max.y + origin - top) / row_h).ceil()) as usize + 1).min(rows);
        let mut drawn = 0;
        if first > 0 {
            ui.add_space(first as f32 * row_h);
        }
        for row in first..last {
            ui.horizontal(|ui| {
                ui.set_height(row_h - 2.0);
                for c in 0..cols {
                    let i = row * cols + c;
                    if i < n {
                        let id = l.ids[i];
                        if audio {
                            self.audio_row(ui, id, w, i, cmds);
                        } else {
                            self.card(ui, id, cell, false, cmds);
                        }
                        drawn += 1;
                    } else if i < n + skel {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(cell, row_h - 8.0), Sense::hover());
                        skeleton(ui, Rect::from_min_size(r.min, if audio { Vec2::new(cell, 46.0) } else { Vec2::splat(cell) }));
                    }
                }
            });
        }
        if last < rows {
            ui.add_space((rows - last) as f32 * row_h);
        }
        self.toolbox.ui.cells_drawn = drawn;
        if n == 0 && !l.loading {
            ui.add_space(24.0);
            if let Some(e) = &l.error {
                error_state(ui, &e.describe(), || cmds.push(Cmd::Retry(req.clone())));
            } else if l.pages > 0 {
                ui.vertical_centered(|ui| {
                    icon(ui.painter(), Rect::from_center_size(ui.cursor().center_top() + Vec2::new(0.0, 18.0), Vec2::splat(30.0)), "search", theme::TEXT_DIM);
                    ui.add_space(40.0);
                    ui.label(egui::RichText::new("No results found").font(bold(14.0)).color(theme::TEXT));
                    ui.label(egui::RichText::new("Try different keywords or remove filters.").size(12.5).color(theme::TEXT_DIM));
                });
            }
        } else if let Some(e) = &l.error {
            error_state(ui, &e.describe(), || cmds.push(Cmd::Retry(req.clone())));
        }
        ui.add_space(12.0);
    }

    /// A Creator Store card (`small`: a swimlane's).
    fn card(&mut self, ui: &mut egui::Ui, id: u64, cell: f32, small: bool, cmds: &mut Vec<Cmd>) {
        let a = self.toolbox.asset(id).cloned().unwrap_or(Asset { id, ..Default::default() });
        let h = cell + 44.0;
        let (r, _) = ui.allocate_exact_size(Vec2::new(cell, h), Sense::hover());
        if !ui.is_rect_visible(r) {
            return;
        }
        let resp = ui.interact(r, ui.id().with(("tbcard", id, small)), Sense::click_and_drag());
        let hovered = resp.hovered() || resp.dragged() || self.toolbox.ui.force_hover == Some(id);
        let thumb_r = Rect::from_min_size(r.min, Vec2::splat(cell));
        ui.painter().rect_filled(thumb_r, 6.0, if hovered { TILE_HOVER } else { TILE });
        self.paint_thumb(ui, id, thumb_r.shrink(cell * 0.06), thumbs::SIZE);
        // (Studio's store cards carry no script badge; the detail page warns.)
        if false {
            let br = Rect::from_min_size(thumb_r.left_bottom() + Vec2::new(4.0, -20.0), Vec2::splat(16.0));
            ui.painter().rect_filled(br, 3.0, Color32::from_black_alpha(150));
            icon(ui.painter(), br.shrink(2.0), "code", Color32::WHITE);
        }
        let mut y = thumb_r.max.y + 4.0;
        let verified = a.creator.verified;
        let name = if a.name.is_empty() { "…".to_string() } else { a.name.clone() };
        let g = clipped(ui, &name, FontId::proportional(12.5), LINK, cell, 2);
        let gh = g.size().y;
        ui.painter().galley(Pos2::new(r.min.x + 2.0, y), g, LINK);
        y += gh + 1.0;
        // (Studio's decal / video / font cards show no price line)
        let price = if matches!(Kind::from_asset_type(a.type_id), Some(Kind::Decal | Kind::Video | Kind::Font)) { String::new() } else { a.price.label(true) };
        ui.painter().text(Pos2::new(r.min.x + 2.0, y), Align2::LEFT_TOP, &price, FontId::proportional(11.5), theme::TEXT);
        if hovered && !small {
            // Studio's hover: a border, the magnifier, creator and votes.
            ui.painter().rect_stroke(r.expand(2.0), 4.0, Stroke::new(1.0, theme::TEXT_DIM), StrokeKind::Outside);
            let mr = Rect::from_min_size(Pos2::new(thumb_r.max.x - 26.0, thumb_r.min.y + 2.0), Vec2::splat(24.0));
            let m = ui.interact(mr, ui.id().with(("tbmag", id)), Sense::click());
            ui.painter().rect_filled(mr, 4.0, if m.hovered() { theme::SELECT_BG } else { Color32::from_black_alpha(140) });
            icon(ui.painter(), mr.shrink(4.0), "zoom-in", Color32::WHITE);
            if m.clicked() {
                cmds.push(Cmd::Open(id));
                return;
            }
            // creator + votes drawn on a panel below the card
            let extra = Rect::from_min_size(Pos2::new(r.min.x - 2.0, r.max.y + 2.0), Vec2::new(cell + 4.0, 36.0));
            let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with(("tbhover", id))));
            p.rect(extra, 4.0, theme::PANEL, Stroke::new(1.0, theme::TEXT_DIM), StrokeKind::Inside);
            let cn = clipped(ui, &a.creator.name, FontId::proportional(11.5), LINK, cell - 18.0, 1);
            p.galley(extra.min + Vec2::new(6.0, 3.0), cn.clone(), LINK);
            if verified {
                icon(&p, Rect::from_min_size(extra.min + Vec2::new(8.0 + cn.size().x, 3.0), Vec2::splat(12.0)), "rosette-discount-check-filled", LINK);
            }
            if let Some(pct) = a.votes.percent.filter(|_| a.votes.show) {
                icon(&p, Rect::from_min_size(extra.min + Vec2::new(6.0, 19.0), Vec2::splat(12.0)), "thumb-up-filled", GOOD);
                p.text(extra.min + Vec2::new(22.0, 19.0), Align2::LEFT_TOP, format!("{pct}%"), FontId::proportional(11.5), GOOD);
                p.text(extra.min + Vec2::new(52.0, 19.0), Align2::LEFT_TOP, model::short_count(a.votes.count), FontId::proportional(11.5), theme::TEXT_DIM);
            }
        }
        self.toolbox.ui.card_rects.push((id, resp.rect));
        if resp.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), ToolboxDrag(id));
        }
        if resp.clicked() {
            cmds.push(Cmd::Insert(id));
        }
        if resp.secondary_clicked() {
            cmds.push(Cmd::Open(id));
        }
        resp.on_hover_text_at_pointer(if a.kind_name().is_empty() { name } else { format!("{name}\n{}", a.kind_name()) });
    }

    /// One row of Studio's audio table: icon (music note / waveform, the
    /// play button), Name, Creator, Tags (genre), Length; alternate rows shaded.
    fn audio_row(&mut self, ui: &mut egui::Ui, id: u64, w: f32, index: usize, cmds: &mut Vec<Cmd>) {
        let a = self.toolbox.asset(id).cloned().unwrap_or(Asset { id, ..Default::default() });
        let (r, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
        if !ui.is_rect_visible(r) {
            return;
        }
        let resp = ui.interact(r, ui.id().with(("tbaudiorow", id)), Sense::click_and_drag());
        let bg = if resp.hovered() { TILE_HOVER } else if index % 2 == 0 { TILE } else { theme::PANEL };
        ui.painter().rect_filled(r, 0.0, bg);
        let cols = audio_cols(w);
        let au = a.audio.clone().unwrap_or_default();
        let playing = self.toolbox.player.playing == Some(id) && !self.toolbox.player.paused;
        let loading = self.toolbox.player.loading == Some(id);
        let pr = Rect::from_center_size(Pos2::new(r.min.x + 14.0, r.center().y), Vec2::splat(18.0));
        let pb = ui.interact(pr, ui.id().with(("tbplay", id)), Sense::click());
        let music = au.audio_type == "Music";
        let glyph = if loading { "dots" } else if playing { "player-pause-filled" } else if pb.hovered() { "player-play-filled" } else if music { "music" } else { "wave-sine" };
        icon(ui.painter(), pr.shrink(1.0), glyph, Color32::from_rgb(240, 140, 60));
        if pb.clicked() {
            cmds.push(if playing { Cmd::Pause(id) } else { Cmd::Play(id) });
        }
        let cell = |ui: &egui::Ui, x: f32, w: f32, t: &str, c: Color32| {
            let g = clipped(ui, t, FontId::proportional(12.0), c, w - 4.0, 1);
            ui.painter().galley(Pos2::new(x, r.center().y - g.size().y / 2.0), g, c);
        };
        cell(ui, r.min.x + cols[0], cols[1] - cols[0], if a.name.is_empty() { "…" } else { &a.name }, LINK);
        let who = if au.artist.is_empty() { a.creator.name.clone() } else { au.artist.clone() };
        cell(ui, r.min.x + cols[1], cols[2] - cols[1], &who, theme::TEXT);
        cell(ui, r.min.x + cols[2], cols[3] - cols[2], &capital(&au.genre), theme::TEXT);
        ui.painter().text(Pos2::new(r.max.x - 6.0, r.center().y), Align2::RIGHT_CENTER, model::duration_label(au.duration), FontId::proportional(12.0), theme::TEXT);
        if self.toolbox.player.playing == Some(id) && au.duration > 0.0 {
            let t = (self.toolbox.player.position(id) / au.duration).clamp(0.0, 1.0) as f32;
            ui.painter().rect_filled(Rect::from_min_size(Pos2::new(r.min.x, r.max.y - 2.0), Vec2::new(r.width() * t, 2.0)), 0.0, theme::ACCENT);
        }
        if let Some((eid, e)) = &self.toolbox.player.error {
            if *eid == id {
                resp.clone().on_hover_text(e.clone());
            }
        }
        self.toolbox.ui.card_rects.push((id, resp.rect));
        if resp.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), ToolboxDrag(id));
        }
        if resp.clicked() && !pb.hovered() {
            cmds.push(Cmd::Insert(id));
        }
        if resp.secondary_clicked() {
            cmds.push(Cmd::Open(id));
        }
    }

    fn paint_thumb(&mut self, ui: &egui::Ui, id: u64, r: Rect, size: u32) {
        match self.toolbox.thumbs.get(id, size) {
            Thumb::Ready(t) => {
                let s = t.size_vec2();
                let k = (r.width() / s.x).min(r.height() / s.y);
                let ir = Rect::from_center_size(r.center(), s * k);
                ui.painter().image(t.id(), ir, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            Thumb::Loading => skeleton(ui, r),
            Thumb::Failed => {
                icon(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(r.width() * 0.35)), "photo", theme::TEXT_DIM);
            }
        }
    }

    // --------------------------------------------------------- detail page

    fn store_asset(&mut self, ui: &mut egui::Ui, id: u64, cmds: &mut Vec<Cmd>) {
        let a = self.toolbox.asset(id).cloned().unwrap_or(Asset { id, ..Default::default() });
        let kind = Kind::from_asset_type(a.type_id).unwrap_or(Kind::Model);
        let three_d = matches!(kind, Kind::Model | Kind::MeshPart | Kind::Package) && a.type_id != 0;
        if three_d && self.toolbox.preview.wants(id) {
            self.toolbox.preview.start(id);
            self.toolbox.pool.run(move |h| {
                let r = super::insert::download(h, id, &|| false)
                    .and_then(|b| rustudio_core::doc::Document::decode(&b).map_err(super::api::Failure::Other));
                Some(super::Msg::PreviewModel { id, result: r })
            });
        }
        let footer_h = 44.0;
        let avail = ui.available_rect_before_wrap();
        let body = Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - footer_h));
        let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
        self.scroll_view(&mut body_ui, |ed, ui, _vp| {
            let mut crumbs = vec!["Back".to_string()];
            if let Some(c) = categories::find(&a.category_path) {
                crumbs.push(categories::top_of(c.key).label.into());
                if c.key != categories::top_of(c.key).key {
                    crumbs.push(c.label.into());
                }
            } else if a.type_id != 0 {
                crumbs.push(kind.noun().to_string());
            }
            let cr: Vec<&str> = crumbs.iter().map(String::as_str).collect();
            breadcrumb(ui, &cr, cmds);
            if ed.toolbox.missing.contains(&id) && a.name.is_empty() {
                error_state(ui, "This asset doesn't exist, was removed or moderated, or is private.", || cmds.push(Cmd::Back));
                return;
            }
            ui.add(egui::Label::new(egui::RichText::new(if a.name.is_empty() { "Loading…" } else { &a.name }).font(bold(16.0)).color(theme::TEXT)).wrap());
            if !a.creator.name.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("By @{}", a.creator.name)).size(12.5).color(theme::TEXT_DIM));
                    if a.creator.verified {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icon(ui.painter(), r, "rosette-discount-check-filled", LINK);
                    }
                });
            }
            ui.add_space(4.0);
            let w = ui.available_width() - PAD;
            ed.asset_media(ui, &a, kind, three_d, w, cmds);
            ui.add_space(4.0);
            // votes + vote buttons, Save
            ui.horizontal(|ui| {
                if let Some(pct) = a.votes.percent.filter(|_| a.votes.show) {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    icon(ui.painter(), r, "thumb-up-filled", GOOD);
                    ui.label(egui::RichText::new(format!("{pct}%")).color(GOOD).size(12.5));
                    ui.label(egui::RichText::new(format!("({} votes)", model::short_count(a.votes.count))).size(12.5).color(theme::TEXT_DIM));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(PAD);
                    if icon_button(ui, "thumb-down", 26.0, false, "Vote down").clicked() {
                        cmds.push(Cmd::Vote(id, Some(false)));
                    }
                    if icon_button(ui, "thumb-up", 26.0, a.votes.has_voted, "Vote up").clicked() {
                        cmds.push(Cmd::Vote(id, Some(true)));
                    }
                });
            });
            let (fav_n, fav_mine) = ed.toolbox.favorites.get(&id).copied().unwrap_or((None, None));
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(PAD);
                    let saved = fav_mine == Some(true);
                    let label = format!("{}{}", if saved { "Saved" } else { "Save" }, fav_n.map(|n| format!(" ({})", model::short_count(n))).unwrap_or_default());
                    if pill(ui, &label, saved, false).clicked() {
                        cmds.push(Cmd::Favorite(id, !saved));
                    }
                });
            });
            if a.has_scripts {
                ui.add_space(4.0);
                egui::Frame::new().fill(Color32::from_rgb(70, 58, 24)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                    ui.set_width(w - 12.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("⚠").color(theme::WARN));
                        let n = a.count("script").unwrap_or(a.script_count);
                        ui.label(egui::RichText::new(if n > 0 { format!("This {} contains {n} script(s). Scripts can change your experience; insert only if you trust the creator.", kind.noun().to_lowercase()) } else { "This asset contains scripts. Insert only if you trust the creator.".into() }).size(12.0));
                    });
                });
            }
            ui.add_space(6.0);
            ui.label(egui::RichText::new(format!("{} Details", kind.noun())).font(bold(14.0)).color(theme::TEXT));
            let mut rows: Vec<(String, String)> = Vec::new();
            rows.push(("Creator".into(), a.creator.name.clone()));
            rows.push(("Created".into(), model::short_date(&a.created)));
            rows.push(("Updated".into(), model::short_date(&a.updated)));
            if let Some(t) = a.triangles {
                rows.push(("Triangle Count".into(), thousands(t)));
            }
            if let Some(v) = a.vertices {
                rows.push(("Vertices Count".into(), thousands(v)));
            }
            for (k, label) in [("meshPart", "MeshPart Count"), ("script", "Script Count"), ("decal", "Decal Count"), ("animation", "Animation Count"), ("audio", "Audio Count"), ("tool", "Tool Count")] {
                if let Some(n) = a.count(k).filter(|n| *n > 0) {
                    rows.push((label.into(), thousands(n)));
                }
            }
            if let Some(au) = &a.audio {
                for (k, v) in [("Artist", &au.artist), ("Title", &au.title), ("Album", &au.album), ("Genre", &au.genre), ("Type", &au.audio_type)] {
                    if !v.is_empty() {
                        rows.push((k.into(), v.clone()));
                    }
                }
                rows.push(("Duration".into(), model::duration_label(au.duration)));
            }
            rows.push(("Price".into(), match a.price { Price::Free => "Free".into(), p => p.label(false) }));
            rows.push(("Asset ID".into(), id.to_string()));
            rows.push(("Type".into(), kind.noun().into()));
            for (k, v) in rows {
                ui.horizontal(|ui| {
                    ui.set_width(w);
                    ui.label(egui::RichText::new(k).size(12.5).color(theme::TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let link = v == a.creator.name && !v.is_empty();
                        let t = egui::RichText::new(&v).size(12.5).color(if link { LINK } else { theme::TEXT });
                        if ui.add(egui::Label::new(t).sense(Sense::click())).clicked() && link {
                            let profile = if a.creator.kind == 2 { format!("https://www.roblox.com/groups/{}", a.creator.id) } else { format!("https://www.roblox.com/users/{}/profile", a.creator.id) };
                            cmds.push(Cmd::Url(profile));
                        }
                    });
                });
                ui.add(egui::Separator::default().spacing(2.0));
            }
            if !a.description.trim().is_empty() {
                let open = ed.toolbox.ui.show_description;
                if ui.add(egui::Label::new(egui::RichText::new(format!("{} Description", if open { "▾" } else { "▸" })).font(bold(13.0)).color(theme::TEXT)).sense(Sense::click())).clicked() {
                    ed.toolbox.ui.show_description = !open;
                }
                if open {
                    ui.add(egui::Label::new(egui::RichText::new(a.description.trim()).size(12.0)).wrap());
                }
            }
            if let Some(tree) = ed.toolbox.preview.trees.get(&id).cloned() {
                let open = ed.toolbox.ui.show_tree;
                if ui.add(egui::Label::new(egui::RichText::new(format!("{} Contents ({} objects)", if open { "▾" } else { "▸" }, tree.len())).font(bold(13.0))).sense(Sense::click())).clicked() {
                    ed.toolbox.ui.show_tree = !open;
                }
                if open {
                    for row in tree.iter().take(400) {
                        ui.horizontal(|ui| {
                            ui.add_space(row.depth as f32 * 12.0);
                            crate::icons::class_icon(ui, &row.class, 14.0);
                            ui.label(egui::RichText::new(&row.name).size(12.0));
                        });
                    }
                }
            }
            if a.creator.id != 0 {
                let more = ListReq { source: Source::Creator { id: a.creator.id, group: a.creator.kind == 2 }, ..ListReq::store(kind, "") };
                ed.swimlane(ui, &format!("More From {}", a.creator.name), &more, Nav::Results { cat: kind_cat(kind).into(), query: String::new(), tile: None }, cmds);
            }
            ui.add_space(8.0);
        });
        // Footer: … and the main button
        let fr = Rect::from_min_max(Pos2::new(avail.min.x, avail.max.y - footer_h + 4.0), avail.max);
        let mut f = ui.new_child(egui::UiBuilder::new().max_rect(fr).layout(egui::Layout::left_to_right(egui::Align::Center)));
        f.add_space(8.0);
        let more = f.add_sized(Vec2::new(56.0, 30.0), egui::Button::new("•••").fill(TILE));
        if more.clicked() {
            self.toolbox.ui.more_menu = !self.toolbox.ui.more_menu;
        }
        if self.toolbox.ui.more_menu {
            let pos = more.rect.left_top() - Vec2::new(0.0, 112.0);
            let area = toolbox_popup(&f, "tbmore", pos, |ui| {
                ui.set_width(150.0);
                if ui.button("Copy Link").clicked() {
                    cmds.push(Cmd::Copy(super::api::store_url(id, kind)));
                    self.toolbox.ui.more_menu = false;
                }
                if ui.button("Copy Asset ID").clicked() {
                    cmds.push(Cmd::Copy(id.to_string()));
                    self.toolbox.ui.more_menu = false;
                }
                if ui.button("View in Browser").clicked() {
                    cmds.push(Cmd::Url(super::api::store_url(id, kind)));
                    self.toolbox.ui.more_menu = false;
                }
                if ui.button("Report Item").clicked() {
                    cmds.push(Cmd::Url(super::api::report_url(id)));
                    self.toolbox.ui.more_menu = false;
                }
            });
            if area.response.clicked_elsewhere() && !more.clicked() {
                self.toolbox.ui.more_menu = false;
            }
        }
        let (label, enabled) = self.main_button(&a, kind);
        let bw = (f.available_width() - 8.0).max(60.0);
        let b = f.add_enabled(enabled, egui::Button::new(egui::RichText::new(label).font(bold(13.0)).color(Color32::WHITE)).fill(theme::ACCENT).min_size(Vec2::new(bw, 30.0)));
        if b.clicked() {
            cmds.push(Cmd::Insert(id));
        }
        ui.allocate_rect(avail, Sense::hover());
    }

    /// Insert / Install / Installed / Get / Buy, as the account stands.
    fn main_button(&self, a: &Asset, kind: Kind) -> (String, bool) {
        if kind == Kind::Plugin {
            let installed = self.plugin_store.store.get(&crate::plugin_store::cloud_id(a.id)).is_some();
            if self.plugin_store.busy.contains(&crate::plugin_store::cloud_id(a.id)) {
                return ("Installing…".into(), false);
            }
            if installed {
                return ("Installed".into(), false);
            }
            let owned = self.toolbox.owned.get(&a.id).copied().unwrap_or(false);
            return (if owned || !a.price.is_free() && a.price != Price::Unknown { if owned { "Install".into() } else { format!("Buy {}", a.price.label(false)) } } else { "Get".into() }, true);
        }
        if let Price::Cents(_) = a.price {
            if !self.toolbox.owned.get(&a.id).copied().unwrap_or(false) {
                return (format!("Buy {}", a.price.label(false)), true);
            }
        }
        if self.toolbox.inserts.iter().any(|j| j.id == a.id) {
            return ("Inserting…".into(), false);
        }
        (kind.action().into(), a.type_id != 0 || a.id != 0)
    }

    fn asset_media(&mut self, ui: &mut egui::Ui, a: &Asset, kind: Kind, three_d: bool, w: f32, cmds: &mut Vec<Cmd>) {
        let id = a.id;
        let side = w.max(80.0);
        let (r, resp) = ui.allocate_exact_size(Vec2::new(side, side), Sense::click_and_drag());
        ui.painter().rect_filled(r, 6.0, TILE);
        if three_d {
            let px = ui.ctx().pixels_per_point();
            self.toolbox.preview.show(id, [(r.width() * px) as u32, (r.height() * px) as u32]);
            let st = match self.toolbox.preview.state.get(&id) {
                Some(PState::Installed { .. }) => Ok(true),
                Some(PState::Failed(e)) => Err(e.clone()),
                _ => Ok(false),
            };
            match (self.toolbox.preview.texture, st) {
                (Some((tid, t, _)), Ok(true)) if tid == id => {
                    ui.painter().image(t, r.shrink(2.0), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
                (_, Err(e)) => {
                    self.paint_thumb(ui, id, r.shrink(r.width() * 0.08), 420);
                    ui.painter().text(r.center_bottom() - Vec2::new(0.0, 10.0), Align2::CENTER_BOTTOM, &e, FontId::proportional(11.0), theme::TEXT_DIM);
                }
                _ => {
                    self.paint_thumb(ui, id, r.shrink(r.width() * 0.08), 420);
                    ui.painter().text(r.center_bottom() - Vec2::new(0.0, 10.0), Align2::CENTER_BOTTOM, "Loading 3D preview…", FontId::proportional(11.0), theme::TEXT_DIM);
                }
            }
            if resp.dragged() {
                let d = resp.drag_delta();
                let o = &mut self.toolbox.preview.orbit;
                o.yaw -= d.x * 0.01;
                o.pitch = (o.pitch - d.y * 0.01).clamp(-1.45, 1.45);
            }
            if resp.hovered() {
                let s = ui.input(|i| i.smooth_scroll_delta.y);
                if s != 0.0 {
                    let o = &mut self.toolbox.preview.orbit;
                    o.zoom = (o.zoom * (1.0 - s * 0.002)).clamp(0.25, 4.0);
                }
            }
            resp.on_hover_text("Drag to turn, scroll to zoom");
            return;
        }
        self.paint_thumb(ui, id, r.shrink(r.width() * 0.06), 420);
        match kind {
            Kind::Audio => {
                let au = a.audio.clone().unwrap_or_default();
                let playing = self.toolbox.player.playing == Some(id) && !self.toolbox.player.paused;
                let pr = Rect::from_center_size(r.center(), Vec2::splat(56.0));
                let pb = ui.interact(pr, ui.id().with(("tbbigplay", id)), Sense::click());
                ui.painter().circle_filled(pr.center(), 28.0, if pb.hovered() { theme::ACCENT } else { Color32::from_black_alpha(170) });
                icon(ui.painter(), pr.shrink(14.0), if self.toolbox.player.loading == Some(id) { "dots" } else if playing { "player-pause-filled" } else { "player-play-filled" }, Color32::WHITE);
                if pb.clicked() {
                    cmds.push(if playing { Cmd::Pause(id) } else { Cmd::Play(id) });
                }
                let pos = self.toolbox.player.position(id);
                let bar = Rect::from_min_size(Pos2::new(r.min.x + 10.0, r.max.y - 18.0), Vec2::new(r.width() - 20.0, 4.0));
                ui.painter().rect_filled(bar, 2.0, theme::DIVIDER);
                if au.duration > 0.0 {
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(bar.width() * (pos / au.duration).clamp(0.0, 1.0) as f32, 4.0)), 2.0, theme::ACCENT);
                }
                ui.painter().text(bar.left_top() - Vec2::new(0.0, 4.0), Align2::LEFT_BOTTOM, format!("{} / {}", model::duration_label(pos), model::duration_label(au.duration)), FontId::proportional(11.0), theme::TEXT);
                if let Some((eid, e)) = &self.toolbox.player.error {
                    if *eid == id {
                        ui.colored_label(theme::ERROR, e);
                    }
                }
            }
            Kind::Video => {
                ui.painter().circle_filled(r.center(), 26.0, Color32::from_black_alpha(150));
                icon(ui.painter(), Rect::from_center_size(r.center(), Vec2::splat(26.0)), "player-play-filled", Color32::WHITE);
                ui.painter().text(r.left_bottom() + Vec2::new(8.0, -8.0), Align2::LEFT_BOTTOM, "Video", FontId::proportional(11.0), theme::TEXT);
            }
            _ => {}
        }
    }

    // ------------------------------------------------- inventory / recent / creations

    fn toolbox_list_tab(&mut self, ui: &mut egui::Ui, tab: Tab, cmds: &mut Vec<Cmd>) {
        let cats = categories::list_cats(tab);
        let sel = match tab {
            Tab::Inventory => &mut self.toolbox.st.inv_cat,
            Tab::Recent => &mut self.toolbox.st.recent_cat,
            _ => &mut self.toolbox.st.creations_cat,
        };
        if *sel >= cats.len() {
            *sel = 0;
        }
        let mut cur = *sel;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let w = if tab == Tab::Inventory { (ui.available_width() * 0.45).max(110.0) } else { ui.available_width() - 10.0 };
            egui::ComboBox::from_id_salt(("tblistcat", tab as u8)).width(w).selected_text(cats[cur].label).show_ui(ui, |ui| {
                for (i, c) in cats.iter().enumerate() {
                    ui.selectable_value(&mut cur, i, c.label);
                }
            });
            if tab == Tab::Inventory {
                let te = egui::TextEdit::singleline(&mut self.toolbox.st.inv_query).hint_text("Search").desired_width(ui.available_width() - 8.0);
                ui.add(te);
            }
        });
        match tab {
            Tab::Inventory => self.toolbox.st.inv_cat = cur,
            Tab::Recent => self.toolbox.st.recent_cat = cur,
            _ => self.toolbox.st.creations_cat = cur,
        }
        if cats[cur].group {
            self.toolbox.want_groups();
            let groups = match &self.toolbox.groups {
                Some(Ok(g)) => g.clone(),
                _ => Vec::new(),
            };
            if self.toolbox.st.group.is_none_or(|g| !groups.iter().any(|x| x.0 == g)) {
                self.toolbox.st.group = groups.first().map(|g| g.0);
            }
            let mut g = self.toolbox.st.group;
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let name = groups.iter().find(|x| Some(x.0) == g).map(|x| x.1.clone()).unwrap_or_else(|| if groups.is_empty() { "No groups".into() } else { "Group".into() });
                egui::ComboBox::from_id_salt("tbgroup").width(ui.available_width() - 10.0).selected_text(name).show_ui(ui, |ui| {
                    for (id, n) in &groups {
                        ui.selectable_value(&mut g, Some(*id), n);
                    }
                });
            });
            self.toolbox.st.group = g;
        }
        if !self.toolbox.st.banner_closed {
            egui::Frame::new().fill(Color32::from_rgb(72, 62, 30)).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 24.0);
                        ui.add(egui::Label::new(egui::RichText::new("⚠ Toolbox inventory will be deprecated soon. Manage your inventory using the new Asset Manager.").size(12.5)).wrap());
                        if ui.add(egui::Label::new(egui::RichText::new("Read more").underline().size(12.5)).sense(Sense::click())).clicked() {
                            cmds.push(Cmd::Url("https://devforum.roblox.com/c/updates/announcements".into()));
                        }
                    });
                    if icon_button(ui, "x", 20.0, false, "").clicked() {
                        self.toolbox.st.banner_closed = true;
                    }
                });
            });
        }
        let Some(req) = self.toolbox.tab_req(tab) else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(egui::RichText::new("You aren't in a group that can create assets.").color(theme::TEXT_DIM)));
            return;
        };
        self.toolbox.ensure(&req);
        let mut l = self.toolbox.list(&req).cloned().unwrap_or_default();
        // Recent: inserts made in rustudio first, then Roblox's list.
        if tab == Tab::Recent {
            let kind = req.kind;
            let local: Vec<u64> = self.toolbox.st.recent_local.iter().filter(|(_, t)| Kind::from_asset_type(*t).map(|k| k == kind || (kind == Kind::Model && k == Kind::Package)).unwrap_or(false)).map(|r| r.0).collect();
            let missing: Vec<u64> = local.iter().copied().filter(|i| self.toolbox.asset(*i).is_none()).collect();
            if !missing.is_empty() {
                self.toolbox.want_details(missing);
            }
            let mut ids = local;
            for i in &l.ids {
                if !ids.contains(i) {
                    ids.push(*i);
                }
            }
            l.ids = ids;
        }
        let kind = req.kind;
        let near_end = self.scroll_view(ui, |ed, ui, vp| ed.small_grid(ui, &req, &l, kind, vp, cmds));
        if near_end && l.can_load_more() && l.pages > 0 {
            cmds.push(Cmd::LoadMore(req));
        }
    }

    /// Inventory-style 3-column grid of small cards.
    fn small_grid(&mut self, ui: &mut egui::Ui, req: &ListReq, l: &super::List, kind: Kind, vp: Rect, cmds: &mut Vec<Cmd>) {
        let w = ui.available_width() - PAD;
        let (cols, cell) = grid(w, 72.0);
        let row_h = cell + 52.0;
        let n = l.ids.len();
        let skel = if l.loading { if n == 0 { cols * 3 } else { cols } } else { 0 };
        let rows = (n + skel).div_ceil(cols);
        let origin = ui.min_rect().min.y;
        let top = ui.cursor().min.y;
        let first = (((vp.min.y + origin - top) / row_h).floor().max(0.0)) as usize;
        let last = ((((vp.max.y + origin - top) / row_h).ceil()) as usize + 1).min(rows);
        if first > 0 {
            ui.add_space(first as f32 * row_h);
        }
        for row in first..last {
            ui.horizontal(|ui| {
                ui.set_height(row_h - 2.0);
                for c in 0..cols {
                    let i = row * cols + c;
                    if i < n {
                        let id = l.ids[i];
                        let a = self.toolbox.asset(id).cloned().unwrap_or(Asset { id, ..Default::default() });
                        let (r, _) = ui.allocate_exact_size(Vec2::new(cell, row_h - 6.0), Sense::hover());
                        if !ui.is_rect_visible(r) {
                            continue;
                        }
                        let resp = ui.interact(r, ui.id().with(("tbsmall", id)), Sense::click_and_drag());
                        let tr = Rect::from_min_size(r.min, Vec2::splat(cell));
                        ui.painter().rect_filled(tr, 4.0, if resp.hovered() { TILE_HOVER } else { TILE });
                        if kind == Kind::Model && a.has_scripts {
                            ui.painter().circle_filled(tr.center(), cell * 0.33, Color32::from_rgb(30, 31, 36));
                            icon(ui.painter(), Rect::from_center_size(tr.center(), Vec2::splat(cell * 0.36)), "code", Color32::WHITE);
                        } else {
                            self.paint_thumb(ui, id, tr.shrink(3.0), thumbs::SIZE);
                        }
                        let g = clipped(ui, if a.name.is_empty() { "…" } else { &a.name }, FontId::proportional(12.0), LINK, cell, 2);
                        let gh = g.size().y;
                        ui.painter().galley(Pos2::new(r.min.x + 1.0, tr.max.y + 3.0), g, LINK);
                        let price = a.price.label(false);
                        if !price.is_empty() {
                            ui.painter().text(Pos2::new(r.min.x + 1.0, tr.max.y + 5.0 + gh), Align2::LEFT_TOP, price, FontId::proportional(11.5), theme::TEXT);
                        }
                        if resp.hovered() {
                            ui.painter().rect_stroke(r.expand(1.0), 4.0, Stroke::new(1.0, theme::TEXT_DIM), StrokeKind::Outside);
                        }
                        self.toolbox.ui.card_rects.push((id, resp.rect));
                        if resp.drag_started() {
                            egui::DragAndDrop::set_payload(ui.ctx(), ToolboxDrag(id));
                        }
                        if resp.clicked() {
                            cmds.push(Cmd::Insert(id));
                        }
                        let tip = format!("{}\n{} · {}", a.name, kind.noun(), id);
                        resp.context_menu(|ui| {
                            if ui.button(kind.action()).clicked() {
                                cmds.push(Cmd::Insert(id));
                                ui.close();
                            }
                            if ui.button("View details").clicked() {
                                self.toolbox.st.tab = Tab::Store;
                                cmds.push(Cmd::Open(id));
                                ui.close();
                            }
                            if ui.button("Copy Asset ID").clicked() {
                                cmds.push(Cmd::Copy(id.to_string()));
                                ui.close();
                            }
                            if ui.button("Configure / View in Browser").clicked() {
                                cmds.push(Cmd::Url(format!("https://create.roblox.com/dashboard/creations/store/{id}/configure")));
                                ui.close();
                            }
                        }).map(|_| ()).unwrap_or_else(|| {
                            resp.on_hover_text(tip);
                        });
                    } else if i < n + skel {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(cell, row_h - 6.0), Sense::hover());
                        skeleton(ui, Rect::from_min_size(r.min, Vec2::splat(cell)));
                    }
                }
            });
        }
        if last < rows {
            ui.add_space((rows - last) as f32 * row_h);
        }
        if n == 0 && !l.loading {
            ui.add_space(24.0);
            if let Some(e) = &l.error {
                error_state(ui, &e.describe(), || cmds.push(Cmd::Retry(req.clone())));
            } else if l.pages > 0 {
                ui.vertical_centered(|ui| ui.label(egui::RichText::new("No items to show.").color(theme::TEXT_DIM)));
            }
        } else if let Some(e) = &l.error {
            error_state(ui, &e.describe(), || cmds.push(Cmd::Retry(req.clone())));
        }
    }

    // --------------------------------------------------------- overlays

    fn filter_sheet(&mut self, ui: &mut egui::Ui, top: f32) {
        let clip = ui.clip_rect();
        let full = Rect::from_min_max(Pos2::new(clip.min.x, top), clip.max);
        let sheet = Rect::from_min_max(full.min + Vec2::new(12.0, 6.0), full.max - Vec2::new(10.0, 10.0));
        let mut close = false;
        let mut f = self.toolbox.st.filters.clone();
        let kind = self.toolbox.current_cat().and_then(|c| categories::find(&c)).map(|c| c.kind).unwrap_or(Kind::Model);
        egui::Area::new(ui.id().with("tbsheet")).order(egui::Order::Foreground).fixed_pos(full.min).show(ui.ctx(), |ui| {
            ui.set_style(ui.ctx().style_of(egui::Theme::Dark));
            ui.visuals_mut().override_text_color = Some(theme::TEXT);
            ui.painter().rect_filled(full, 0.0, Color32::from_black_alpha(120));
            let bg = ui.interact(full, ui.id().with("tbsheetclick"), Sense::click());
            if bg.clicked() && !sheet.contains(bg.interact_pointer_pos().unwrap_or(Pos2::ZERO)) {
                close = true;
            }
            let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(sheet));
            let ui = &mut inner;
            egui::Frame::new().fill(theme::PANEL).stroke(Stroke::new(1.0, theme::DIVIDER)).corner_radius(4.0).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(sheet.width() - 20.0);
                egui::ScrollArea::vertical().max_height(sheet.height() - 20.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("Sort By").font(bold(13.0)).color(theme::TEXT));
                    ui.radio_value(&mut f.sort, Sort::Relevance, "Relevance");
                    ui.radio_value(&mut f.sort, Sort::Ratings, "Rating");
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("Creators").font(bold(13.0)).color(theme::TEXT));
                    ui.checkbox(&mut f.include_unverified, "Include unverified creators");
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("⚠").color(theme::WARN));
                        ui.label(egui::RichText::new("Assets from unverified sources add more risk of violative behavior").size(11.5).color(theme::TEXT_DIM));
                    });
                    ui.radio_value(&mut f.creators, CreatorFilter::All, "All Creators");
                    ui.radio_value(&mut f.creators, CreatorFilter::Roblox, "Roblox");
                    ui.add(egui::TextEdit::singleline(&mut self.toolbox.ui.creator_text).hint_text("Search by Creator").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(egui::RichText::new("Search").color(Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                                f.creator_name = self.toolbox.ui.creator_text.trim().to_string();
                                f.creators = CreatorFilter::All;
                            }
                            if !f.creator_name.is_empty() {
                                ui.label(egui::RichText::new(format!("Creator: {}", f.creator_name)).size(11.5));
                            }
                        });
                    });
                    ui.add_space(6.0);
                    if kind == Kind::Audio {
                        ui.label(egui::RichText::new("Length (seconds)").font(bold(13.0)).color(theme::TEXT));
                        let mut lo = f.audio_min_s.unwrap_or(0);
                        let mut hi = f.audio_max_s.unwrap_or(600);
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut lo).range(0..=600).suffix(" s"));
                            ui.label("–");
                            ui.add(egui::DragValue::new(&mut hi).range(0..=600).suffix(" s"));
                        });
                        f.audio_min_s = (lo > 0).then_some(lo);
                        f.audio_max_s = (hi < 600).then_some(hi.max(lo));
                        ui.label(egui::RichText::new("Artist").font(bold(13.0)).color(theme::TEXT));
                        ui.add(egui::TextEdit::singleline(&mut f.artist).hint_text("Artist").desired_width(f32::INFINITY));
                        ui.label(egui::RichText::new("Album").font(bold(13.0)).color(theme::TEXT));
                        ui.add(egui::TextEdit::singleline(&mut f.album).hint_text("Album").desired_width(f32::INFINITY));
                    } else {
                        ui.label(egui::RichText::new("Price").font(bold(13.0)).color(theme::TEXT));
                        price_editor(ui, &mut f);
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Contains").font(bold(13.0)).color(theme::TEXT));
                        ui.label(egui::RichText::new("Only show items that contain one or more of the following instances").size(11.5).color(theme::TEXT_DIM));
                        contains_editor(ui, &mut f);
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(egui::RichText::new("Reset filters").color(Color32::WHITE)).fill(theme::ACCENT)).clicked() {
                            f = super::api::Filters::default();
                            self.toolbox.ui.creator_text.clear();
                        }
                        if ui.button("Done").clicked() {
                            close = true;
                        }
                    });
                });
            });
        });
        self.toolbox.st.filters = f;
        if close {
            self.toolbox.ui.filters_open = false;
        }
    }

    fn toolbox_inserts_line(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
        if self.toolbox.inserts.is_empty() {
            return;
        }
        let jobs: Vec<(u64, u64, String, usize, f32)> = self
            .toolbox
            .inserts
            .iter()
            .map(|j| (j.job, j.id, j.name.clone(), j.stage.load(std::sync::atomic::Ordering::Relaxed), j.started.elapsed().as_secs_f32()))
            .collect();
        let clip = ui.clip_rect();
        let h = 26.0 * jobs.len() as f32;
        let r = Rect::from_min_max(Pos2::new(clip.min.x, clip.max.y - h), clip.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)));
        child.painter().rect_filled(r, 0.0, theme::PANEL_DARK);
        for (job, id, name, stage, t) in jobs {
            child.horizontal(|ui| {
                ui.add_space(6.0);
                ui.add(egui::Spinner::new().size(12.0));
                let n = if name.is_empty() { id.to_string() } else { name };
                ui.label(egui::RichText::new(format!("{} {n}", super::insert::STAGES.get(stage).copied().unwrap_or("…"))).size(12.0));
                if t > 0.5 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        if ui.small_button("Cancel").clicked() {
                            cmds.push(Cmd::CancelInsert(job));
                        }
                    });
                }
            });
        }
    }

    fn toolbox_dialogs(&mut self, ctx: &egui::Context, cmds: &mut Vec<Cmd>) {
        let Some(ask) = &self.toolbox.ask else { return };
        let (title, text, buttons): (&str, String, Vec<(&str, bool, bool)>) = match ask {
            Ask::Scripts { ready, count } => (
                "Warning",
                format!(
                    "\"{}\" contains {count} script(s). Scripts can change your experience in unexpected ways. Only insert assets from creators you trust.",
                    ready.asset.name
                ),
                vec![("Insert", true, false), ("Cancel", false, false)],
            ),
            Ask::ToolHome { ready } => (
                "Insert Tool",
                format!("\"{}\" is a Tool. Put it in the Workspace or in the StarterPack (players get it when they spawn)?", ready.asset.name),
                vec![("Workspace", true, false), ("StarterPack", true, true), ("Cancel", false, false)],
            ),
            Ask::GetPlugin { asset } => (
                "Get Plugin",
                format!("Would you like to get \"{}\" by {} for Free? It is added to your inventory and installed.", asset.name, asset.creator.name),
                vec![("Get", true, false), ("Cancel", false, false)],
            ),
            Ask::Paid { asset } => (
                "Buy Item",
                format!("\"{}\" costs {}. rustudio doesn't buy anything: open its Creator Store page to buy it there.", asset.name, asset.price.label(false)),
                vec![("Open Store Page", true, false), ("Cancel", false, false)],
            ),
        };
        egui::Window::new(title).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.add(egui::Label::new(text).wrap());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, yes, starter) in buttons {
                    let b = if yes && !starter { egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(theme::ACCENT) } else { egui::Button::new(label) };
                    let resp = ui.add(b);
                    self.toolbox.ui.ask_buttons.push((label.to_string(), resp.rect));
                    if resp.clicked() {
                        cmds.push(Cmd::Answer(yes, starter));
                    }
                }
            });
        });
    }

    fn toolbox_drag_ghost(&mut self, ctx: &egui::Context) {
        let Some(p) = egui::DragAndDrop::payload::<ToolboxDrag>(ctx) else { return };
        let Some(pos) = ctx.pointer_hover_pos() else { return };
        let t = self.toolbox.thumbs.get(p.0, thumbs::SIZE);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tbdragghost")));
        let r = Rect::from_min_size(pos + Vec2::new(8.0, 8.0), Vec2::splat(56.0));
        painter.rect_filled(r, 4.0, TILE.gamma_multiply(0.9));
        if let Thumb::Ready(t) = t {
            painter.image(t.id(), r.shrink(3.0), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::from_white_alpha(220));
        }
        if !ctx.input(|i| i.pointer.any_down()) {
            // Released outside a drop target.
            if ctx.input(|i| i.pointer.any_released()) {
                ctx.request_repaint();
            } else {
                egui::DragAndDrop::clear_payload(ctx);
            }
        }
    }
}

impl Asset {
    fn kind_name(&self) -> &'static str {
        Kind::from_asset_type(self.type_id).map(|k| k.noun()).unwrap_or("")
    }
}

fn n_or_loading(l: &super::List) -> bool {
    !l.ids.is_empty() || l.loading
}

/// Column x offsets of the audio table: Name, Creator, Tags, Length.
fn audio_cols(w: f32) -> [f32; 4] {
    let len = 44.0;
    let rest = (w - 28.0 - len).max(60.0);
    [28.0, 28.0 + rest * 0.5, 28.0 + rest * 0.75, w - len]
}

fn audio_header(ui: &mut egui::Ui, w: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
    let c = audio_cols(w);
    for (x, t) in [(c[0], "Name"), (c[1], "Creator"), (c[2], "Tags")] {
        ui.painter().text(Pos2::new(r.min.x + x, r.center().y), Align2::LEFT_CENTER, t, FontId::proportional(12.0), theme::TEXT_DIM);
    }
    ui.painter().text(Pos2::new(r.max.x - 6.0, r.center().y), Align2::RIGHT_CENTER, "Length", FontId::proportional(12.0), theme::TEXT_DIM);
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn kind_cat(k: Kind) -> &'static str {
    match k {
        Kind::Model => "models",
        Kind::Audio => "audio",
        Kind::Decal => "2d__decals",
        Kind::MeshPart => "3d__meshparts",
        Kind::Plugin => "plugins",
        Kind::Font => "2d__fonts",
        Kind::Video => "2d__videos",
        Kind::Animation => "animations",
        Kind::Package => "gameplay__packages",
    }
}

fn cat_icon(name: &str) -> &'static str {
    match name {
        "cube" => "cube",
        "fx" => "sparkles",
        "image" => "photo",
        "gameplay" => "puzzle",
        "plugin" => "plug",
        "audio" => "volume",
        _ => "box",
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn breadcrumb(ui: &mut egui::Ui, parts: &[&str], cmds: &mut Vec<Cmd>) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
        icon(ui.painter(), r, "chevron-left", if resp.hovered() { LINK } else { theme::TEXT });
        if resp.clicked() {
            cmds.push(Cmd::Back);
        }
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                ui.label(egui::RichText::new("/").color(theme::TEXT_DIM));
            }
            let last = i + 1 == parts.len();
            let t = egui::RichText::new(*p).font(bold(13.0)).color(if last { theme::TEXT } else { theme::TEXT });
            let resp = ui.add(egui::Label::new(t).sense(Sense::click()));
            if resp.clicked() && !last {
                if i == 0 {
                    cmds.push(if *p == "Back" { Cmd::Back } else { Cmd::Home });
                } else {
                    cmds.push(Cmd::Back);
                }
            }
        }
    });
}

fn signed_out(ui: &mut egui::Ui, cmds: &mut Vec<Cmd>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
        icon(ui.painter(), r, "lock", theme::TEXT_DIM);
        ui.label(egui::RichText::new("Sign in to use the Toolbox").font(bold(14.0)).color(theme::TEXT));
        ui.add(egui::Label::new(egui::RichText::new("Browse the Creator Store, your inventory and your creations after you sign in to Roblox.").size(12.5).color(theme::TEXT_DIM)).wrap());
        ui.add_space(8.0);
        if ui.add(egui::Button::new(egui::RichText::new("Sign In").color(Color32::WHITE)).fill(theme::ACCENT)).clicked() {
            cmds.push(Cmd::SignIn);
        }
    });
}

fn error_state(ui: &mut egui::Ui, msg: &str, mut retry: impl FnMut()) {
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
        icon(ui.painter(), r, if msg.contains("connection") { "wifi-off" } else { "alert-triangle" }, theme::WARN);
        ui.add(egui::Label::new(egui::RichText::new(msg).size(12.5)).wrap());
        if ui.button("Retry").clicked() {
            retry();
        }
    });
}

fn popup_below(ui: &mut egui::Ui, anchor: &egui::Response, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    toolbox_popup(ui, id, anchor.rect.left_bottom() + Vec2::new(0.0, 4.0), |ui| {
        ui.set_min_width(anchor.rect.width().max(140.0));
        add(ui);
    });
}

fn price_editor(ui: &mut egui::Ui, f: &mut super::api::Filters) {
    let mut lo = f.min_cents.unwrap_or(0) as f32 / 100.0;
    let mut hi = f.max_cents.map(|c| c as f32 / 100.0).unwrap_or(50.0);
    ui.add_enabled_ui(!f.free_only, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut lo).range(0.0..=50.0).prefix("$").speed(0.1).fixed_decimals(0));
            ui.label("–");
            ui.add(egui::DragValue::new(&mut hi).range(0.0..=50.0).prefix("$").speed(0.1).fixed_decimals(0));
        });
        ui.add(egui::Slider::new(&mut hi, 0.0..=50.0).show_value(false).text("max"));
    });
    let lo_c = (lo.max(0.0) * 100.0).round() as u64;
    let hi_c = (hi.max(lo) * 100.0).round() as u64;
    f.min_cents = (lo_c > 0).then_some(lo_c);
    f.max_cents = (hi_c < 5000).then_some(hi_c);
    ui.checkbox(&mut f.free_only, "Limit to Free Only");
}

fn contains_editor(ui: &mut egui::Ui, f: &mut super::api::Filters) {
    for k in ["Scripts", "MeshParts", "Decals", "Tools", "Animations", "Audio"] {
        let mut on = f.contains.iter().any(|c| c == k);
        if ui.checkbox(&mut on, k).changed() {
            if on {
                f.contains.push(k.into());
            } else {
                f.contains.retain(|c| c != k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wide_panel_rows() {
        // Studio's ~280 px panel (content ~260): 2 category tiles, 3 swimlane
        // cards, 2 result columns (checks/toolbox/ref/NOTES.md).
        assert_eq!(super::tile_cols(260.0).0, 2);
        assert_eq!(super::lane_cards(260.0).0, 3);
        assert_eq!(super::grid(260.0, 88.0).0, 2);
        // Narrower never drops below those.
        assert_eq!(super::tile_cols(150.0).0, 2);
        assert_eq!(super::lane_cards(150.0).0, 3);
        // Wider: more of each, at about the same size.
        assert_eq!(super::tile_cols(600.0).0, 4);
        assert_eq!(super::lane_cards(600.0).0, 7);
        assert_eq!(super::grid(600.0, 88.0).0, 6);
        let (n, w) = super::lane_cards(600.0);
        assert!((w * n as f32 + 8.0 * (n as f32 - 1.0) - 600.0).abs() < 0.01);
    }

    #[test]
    fn grid_reflows_with_width() {
        // Studio: 2 columns at its default ~280 px panel; more as it widens.
        assert_eq!(super::grid(200.0, 88.0).0, 2);
        assert_eq!(super::grid(270.0, 88.0).0, 2);
        assert_eq!(super::grid(420.0, 88.0).0, 4);
        assert_eq!(super::grid(60.0, 88.0).0, 1);
        let (c, w) = super::grid(600.0, 88.0);
        assert!((c as f32 * w + (c as f32 - 1.0) * 8.0 - 600.0).abs() < 0.01);
    }
}
