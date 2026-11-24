//! The Scripts tab: the Luau script profiler (`rustudio_engine::script_profiler`).
//!
//! Left: every script that ran (context, activity, rate, ms per frame,
//! heap), sortable. Right: the selected script's functions as a call tree
//! (line, total, self, calls per frame); clicking a row opens the script at
//! that line. Bottom: a flame view of the selected function's average frame.
//! The toolbar starts / stops a recording, filters by context or text,
//! picks the sampling rate and exact timing, and exports.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, Ui};
use rustudio_core::profiler::Cat;
use rustudio_engine::script_profiler::{self as sp, ScriptRow, Snapshot, TreeNode, VmKind};

use crate::TabCtx;

// ------------------------------------------------------------------ palette

/// The profiler window's palette (crate::theme) plus this tab's badges.
mod pal {
    pub use crate::theme::{ACCENT, BAD, BAR_TEXT, DEEP, FAINT, LINE, LINE2, MUTED, PANEL, PANEL2, TEXT, WARN, WHITE};
    use egui::Color32;
    pub const SEL: Color32 = crate::theme::SEL_ROW;
    pub const HOVER: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x34);
    pub const ENGINE: Color32 = Color32::from_rgb(0x6c, 0x72, 0x80);
    /// (background, text) of the context badges.
    pub const SERVER: (Color32, Color32) = (Color32::from_rgb(0x3b, 0x2f, 0x22), Color32::from_rgb(0xf0, 0xc5, 0x8a));
    pub const CLIENT: (Color32, Color32) = (Color32::from_rgb(0x22, 0x34, 0x4a), Color32::from_rgb(0x9c, 0xcb, 0xff));
    pub const EDIT: (Color32, Color32) = (Color32::from_rgb(0x30, 0x32, 0x3b), Color32::from_rgb(0xb8, 0xbc, 0xc6));
    pub const NATIVE: (Color32, Color32) = (Color32::from_rgb(0x24, 0x40, 0x2f), Color32::from_rgb(0x8f, 0xe0, 0xb0));
}

fn ui_font(size: f32) -> FontId {
    FontId::proportional(size)
}

fn num_font() -> FontId {
    FontId::monospace(12.0)
}

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContextFilter {
    #[default]
    All,
    Server,
    Client,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ScriptCol {
    Script,
    Context,
    #[default]
    Activity,
    Rate,
    MsPerFrame,
    Heap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FnCol {
    Function,
    Line,
    #[default]
    Total,
    Self_,
    Calls,
}

/// The tab's own state (sorting, expanded rows, filters). Persisted with
/// the window's layout, so keep it serde.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ScriptsTabState {
    pub context: ContextFilter,
    pub filter: String,
    pub sort: ScriptCol,
    pub sort_desc: bool,
    pub fn_sort: FnCol,
    pub fn_sort_desc: bool,
    /// Sampling rate in Hz (1 kHz or 10 kHz in the toolbar).
    pub sample_hz: u32,
    /// Exact per-function timing while recording (costs more).
    pub exact: bool,
    /// Selected script: (VM, full name).
    pub selected: Option<(VmKind, String)>,
    /// Selected function row (its path key in the tree).
    pub selected_fn: Option<String>,
    /// Collapsed function rows (path keys).
    pub collapsed: HashSet<String>,
    /// Width of the script list (px).
    pub list_width: f32,
    #[serde(skip)]
    cache: Cache,
}

impl Default for ScriptsTabState {
    fn default() -> Self {
        ScriptsTabState {
            context: ContextFilter::All,
            filter: String::new(),
            sort: ScriptCol::Activity,
            sort_desc: true,
            fn_sort: FnCol::Total,
            fn_sort_desc: true,
            sample_hz: 1000,
            exact: false,
            selected: None,
            selected_fn: None,
            collapsed: HashSet::new(),
            list_width: 690.0,
            cache: Cache::default(),
        }
    }
}

#[derive(Clone, Default, Debug)]
struct Cache {
    snap: Option<Arc<Snapshot>>,
    at: Option<Instant>,
    /// Identity of the capture extension the snapshot came from (files).
    file_key: Option<usize>,
    live_on: bool,
}

/// Snapshots a few times a second while live (the tables are readable,
/// and the snapshot costs a lock on the gameplay thread's data).
const REFRESH: Duration = Duration::from_millis(250);

// ------------------------------------------------------------------ open requests

/// "Open the script at this line" from a click in the tab: the editor
/// takes it (`take_open_request`) and opens its script editor there.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenRequest {
    pub vm: VmKind,
    /// Full name of the script ("ServerScriptService.NPC.NPCController").
    pub script: String,
    pub line: u32,
}

static OPEN: Mutex<Option<OpenRequest>> = Mutex::new(None);

pub fn take_open_request() -> Option<OpenRequest> {
    OPEN.lock().ok()?.take()
}

fn request_open(vm: VmKind, script: &str, line: u32) {
    if let Ok(mut o) = OPEN.lock() {
        *o = Some(OpenRequest { vm, script: script.to_string(), line });
    }
}

// ------------------------------------------------------------------ data

impl ScriptsTabState {
    pub(crate) fn set_live_view(&mut self, live: bool) {
        if self.cache.live_on != live {
            self.cache.live_on = live;
            sp::set_view_live(live);
            if live && sp::recording() {
                self.exact = sp::exact();
                self.sample_hz = sp::sample_hz();
            }
        }
    }
}

fn data(state: &mut ScriptsTabState, ctx: &TabCtx<'_>) -> Arc<Snapshot> {
    // An opened file (or a frozen capture that carries the profile).
    if !ctx.live {
        if let Some(v) = ctx.capture.extensions.get("scripts") {
            let key = v as *const serde_json::Value as usize;
            if state.cache.file_key != Some(key) {
                state.cache.file_key = Some(key);
                state.cache.snap = serde_json::from_value::<Snapshot>(v.clone()).ok().map(Arc::new);
            }
            if let Some(s) = &state.cache.snap {
                return s.clone();
            }
        }
        if let Some(s) = &state.cache.snap {
            return s.clone();
        }
    }
    state.cache.file_key = None;
    let stale = state.cache.at.is_none_or(|t| t.elapsed() >= REFRESH);
    if stale || state.cache.snap.is_none() {
        let t0 = Instant::now();
        state.cache.snap = Some(Arc::new(sp::snapshot()));
        state.cache.at = Some(Instant::now());
        if std::env::var_os("RUSTUDIO_SCRIPT_PROFILE_DEBUG").is_some() {
            let snap = state.cache.snap.as_ref().unwrap();
            eprintln!(
                "scripts tab: snapshot {:.2} ms, lock held {:.3} ms ({} scripts, {} tree + {} exact nodes, {} functions)",
                t0.elapsed().as_secs_f64() * 1000.0,
                sp::last_snapshot_lock_ns() as f64 / 1e6,
                snap.scripts.len(),
                snap.tree.len(),
                snap.exact_tree.len(),
                snap.functions.len()
            );
        }
    }
    state.cache.snap.clone().unwrap_or_default()
}

fn frames_of(snap: &Snapshot, vm: VmKind) -> f64 {
    snap.vms.iter().find(|v| v.vm == vm).map(|v| v.frames).unwrap_or(0).max(1) as f64
}

fn short_name(path: &str) -> (&str, &str) {
    match path.rfind('.') {
        Some(i) if !path.starts_with('[') => (&path[i + 1..], &path[..i]),
        _ => (path, ""),
    }
}

fn fmt_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.2} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{b:.0} B")
    }
}

fn fmt_calls(c: f64) -> String {
    if c == 0.0 {
        "0".into()
    } else if (c - c.round()).abs() < 0.05 && c >= 1.0 {
        format!("{c:.0}")
    } else if c >= 10.0 {
        format!("{c:.1}")
    } else {
        format!("{c:.2}")
    }
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.2}")
    }
}

fn fn_label(n: &TreeNode) -> String {
    if n.name.is_empty() {
        // A script's top-level code: unnamed, "defined" on line 0 or 1, and
        // the first frame of a resumed script.
        if !n.c && n.line <= 1 && n.depth <= 1 { "(main chunk)".into() } else { "(anonymous)".into() }
    } else {
        n.name.to_string()
    }
}

// ------------------------------------------------------------------ widgets

/// A toolbar button: 24 px tall, panel2 fill, line border.
fn button(ui: &mut Ui, text: &str, color: Color32) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), ui_font(12.0), color);
    let size = vec2(galley.size().x + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { Color32::from_rgb(0x33, 0x35, 0x3e) } else { pal::PANEL2 };
    ui.painter().rect(rect, CornerRadius::same(3), fill, Stroke::new(1.0, pal::LINE), StrokeKind::Inside);
    ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, color);
    resp
}

fn segmented(ui: &mut Ui, items: &[&str], on: usize) -> Option<usize> {
    crate::theme::seg(ui, items, on)
}

fn badge(painter: &egui::Painter, x: f32, cy: f32, text: &str, (bg, fg): (Color32, Color32)) -> f32 {
    let g = painter.layout_no_wrap(text.to_string(), ui_font(10.0), fg);
    let r = Rect::from_min_size(pos2(x, cy - g.size().y / 2.0 - 1.0), vec2(g.size().x + 10.0, g.size().y + 2.0));
    painter.rect_filled(r, CornerRadius::same(2), bg);
    painter.galley(pos2(r.left() + 5.0, r.top() + 1.0), g, fg);
    r.right()
}

fn context_badge(vm: VmKind) -> (&'static str, (Color32, Color32)) {
    match vm {
        VmKind::Server => ("Server", pal::SERVER),
        VmKind::Client => ("Client", pal::CLIENT),
        VmKind::Edit => ("Edit", pal::EDIT),
    }
}

/// Draws text clipped to `max_w` (an ellipsis when it doesn't fit).
fn text_clip(painter: &egui::Painter, pos: egui::Pos2, align: Align2, text: &str, font: FontId, color: Color32, max_w: f32) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_w.max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = painter.layout_job(job);
    let rect = align.anchor_size(pos, g.size());
    painter.galley(rect.min, g, color);
}

/// One header cell; returns true when clicked (sort).
fn header_cell(ui: &mut Ui, rect: Rect, label: &str, right: bool, sorted: Option<bool>, id: &str) -> bool {
    let resp = ui.interact(rect, ui.id().with(("hdr", id, label)), Sense::click());
    let arrow = match sorted {
        Some(true) => " ▾",
        Some(false) => " ▴",
        None => "",
    };
    let color = if resp.hovered() { pal::TEXT } else { pal::MUTED };
    let text = format!("{label}{arrow}");
    let (pos, align) = if right {
        (pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER)
    } else {
        (pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER)
    };
    ui.painter().text(pos, align, text, ui_font(11.5), color);
    resp.clicked()
}

// ------------------------------------------------------------------ the tab

/// Draws the tab into `ui`.
pub fn show(ui: &mut Ui, state: &mut ScriptsTabState, ctx: &TabCtx<'_>) {
    let snap = data(state, ctx);
    if ctx.live {
        ui.ctx().request_repaint_after(REFRESH);
    }
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, CornerRadius::ZERO, crate::theme::BG);

    // Toolbar row.
    let bar = Rect::from_min_size(full.min, vec2(full.width(), 40.0));
    toolbar(ui, bar, state, &snap, ctx);

    // The flame view keeps its designed 118 px while there's room; a short
    // (docked) panel gives the tables the space and drops it below 260 px.
    let room = full.height() - bar.height();
    let flame_h = if room < 260.0 { 0.0 } else { (room * 0.3).clamp(64.0, 118.0) };
    let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), pos2(full.right(), full.bottom() - flame_h));
    let flame = Rect::from_min_max(pos2(full.left(), body.bottom()), full.max);

    // The list keeps its width (690 px as designed) but never more than
    // 55 % of a narrow panel, so the functions stay readable.
    let list_w = state.list_width.min(body.width() * 0.55).clamp(300.0, (body.width() - 280.0).max(300.0));
    let left = Rect::from_min_max(body.min, pos2(body.left() + list_w, body.bottom()));
    let right = Rect::from_min_max(pos2(left.right() + 1.0, body.top()), body.max);
    ui.painter().vline(left.right() + 0.5, body.y_range(), Stroke::new(1.0, pal::LINE));
    // Drag the divider.
    let div = Rect::from_center_size(pos2(left.right(), body.center().y), vec2(6.0, body.height()));
    let dresp = ui.interact(div, ui.id().with("scripts-divider"), Sense::drag());
    if dresp.hovered() || dresp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if dresp.dragged() {
        state.list_width = (list_w + dresp.drag_delta().x).max(300.0);
    }

    let rows = script_rows(&snap, state);
    if state.selected.as_ref().is_none_or(|(vm, p)| !rows.iter().any(|r| r.vm == *vm && &*r.path == p)) {
        state.selected = rows.first().map(|r| (r.vm, r.path.to_string()));
    }
    script_list(ui, left, state, &snap, &rows, ctx);
    let tree = function_rows(&snap, state);
    function_table(ui, right, state, &snap, &tree, ctx);
    if flame_h > 0.0 {
        flame_view(ui, flame, state, &snap, &tree, ctx);
    }
    ui.allocate_rect(full, Sense::hover());
}

fn toolbar(ui: &mut Ui, bar: Rect, state: &mut ScriptsTabState, snap: &Snapshot, ctx: &TabCtx<'_>) {
    ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, pal::LINE2));
    let inner = bar.shrink2(vec2(10.0, 8.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing.x = 10.0;

    // Record / stop.
    let recording = snap.recording && ctx.live;
    let secs = snap.recorded_s as u64;
    let kept = snap.samples > 0 || !snap.exact_tree.is_empty();
    let label = if recording {
        format!("● Recording {:02}:{:02}", secs / 60, secs % 60)
    } else if !ctx.live {
        // A file or a frozen capture: what it holds.
        match (kept, secs) {
            (true, 0) => "Recorded".to_string(),
            (true, _) => format!("Recorded {:02}:{:02}", secs / 60, secs % 60),
            _ => "No recording".to_string(),
        }
    } else if kept {
        format!("● Record · {:02}:{:02} kept", secs / 60, secs % 60)
    } else {
        "● Record".to_string()
    };
    let color = if recording { pal::BAD } else if ctx.live { pal::TEXT } else { pal::MUTED };
    let r = button(ui, &label, color);
    if r.clicked() && ctx.live {
        if recording {
            sp::stop_recording();
            sp::set_exact(false);
        } else {
            sp::set_sample_hz(state.sample_hz);
            sp::set_exact(state.exact);
            sp::start_recording(false);
        }
        state.cache.at = None;
    }
    r.on_hover_text(if recording {
        "Stop recording (the call tree and totals stay until you clear them)"
    } else {
        "Record a sampled call tree of every script (and exact per-function timing if it's on)"
    });

    let on = match state.context {
        ContextFilter::All => 0,
        ContextFilter::Server => 1,
        ContextFilter::Client => 2,
    };
    if let Some(i) = segmented(ui, &["All", "Server", "Client"], on) {
        state.context = [ContextFilter::All, ContextFilter::Server, ContextFilter::Client][i];
    }

    // Search box.
    crate::theme::search(ui, &mut state.filter, "Filter scripts or functions…", 220.0);

    ui.label(egui::RichText::new("Sampling").font(ui_font(13.0)).color(pal::MUTED));
    let hz_on = if state.sample_hz >= 10_000 { 1 } else { 0 };
    let r = segmented(ui, &["1 kHz", "10 kHz"], hz_on);
    if let Some(i) = r {
        state.sample_hz = if i == 0 { 1000 } else { 10_000 };
        sp::set_sample_hz(state.sample_hz);
    }
    // (what's on screen: a capture holding only one kind of tree shows it)
    let showing_exact = state.exact && !snap.exact_tree.is_empty() || snap.tree.is_empty() && !snap.exact_tree.is_empty();
    let ex_on = if showing_exact || (state.exact && snap.exact_tree.is_empty() && snap.tree.is_empty()) { 0 } else { 1 };
    if let Some(i) = segmented(ui, &["Exact timing", "Samples only"], ex_on) {
        state.exact = i == 0;
        if recording {
            sp::set_exact(state.exact);
        }
        state.cache.at = None;
    }

    // Export at the right edge.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let er = button(ui, "Export ▾", pal::TEXT);
        egui::Popup::menu(&er).show(|ui| {
            ui.set_min_width(220.0);
            if ui.button("Script profile (.json)").clicked() {
                export(snap, "json");
            }
            if ui.button("Flame graph, sampled (.folded)").clicked() {
                export(snap, "folded");
            }
            if ui.add_enabled(!snap.exact_tree.is_empty(), egui::Button::new("Flame graph, exact (.folded)")).clicked() {
                export(snap, "exact");
            }
            ui.separator();
            if ui.add_enabled(ctx.live, egui::Button::new("Clear recorded data")).clicked() {
                sp::clear_recording();
                state.cache.at = None;
            }
        });
    });
}

/// Writes the export next to the other captures (~/rustudio-profiles) and
/// says where.
fn export(snap: &Snapshot, kind: &str) {
    let dir = rustudio_core::paths::home().join("rustudio-profiles");
    let _ = std::fs::create_dir_all(&dir);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (path, body) = match kind {
        "json" => (dir.join(format!("scripts-{stamp}.json")), serde_json::to_string_pretty(snap).unwrap_or_default()),
        "exact" => (dir.join(format!("scripts-{stamp}.exact.folded")), snap.folded_exact()),
        _ => (dir.join(format!("scripts-{stamp}.folded")), snap.folded()),
    };
    match std::fs::write(&path, body) {
        Ok(()) => log::info!("Script profile written to {}", path.display()),
        Err(e) => log::warn!("Can't write {}: {e}", path.display()),
    }
}

// ------------------------------------------------------------------ script list

fn script_rows(snap: &Snapshot, state: &ScriptsTabState) -> Vec<ScriptRow> {
    let needle = state.filter.trim().to_lowercase();
    let mut rows: Vec<ScriptRow> = snap
        .scripts
        .iter()
        .filter(|r| match state.context {
            ContextFilter::All => true,
            ContextFilter::Server => r.vm == VmKind::Server,
            ContextFilter::Client => r.vm == VmKind::Client,
        })
        .filter(|r| {
            needle.is_empty()
                || r.path.to_lowercase().contains(&needle)
                || snap.functions.iter().any(|f| f.vm == r.vm && f.script == r.path && f.name.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect();
    let key = |r: &ScriptRow| -> f64 {
        match state.sort {
            ScriptCol::Activity => r.activity_pct,
            ScriptCol::Rate => r.rate_hz as f64,
            ScriptCol::MsPerFrame => r.ms_per_frame,
            ScriptCol::Heap => r.heap_bytes as f64,
            _ => 0.0,
        }
    };
    rows.sort_by(|a, b| {
        let o = match state.sort {
            ScriptCol::Script => short_name(&a.path).0.to_lowercase().cmp(&short_name(&b.path).0.to_lowercase()).reverse(),
            ScriptCol::Context => a.vm.cmp(&b.vm).reverse(),
            _ => key(a).total_cmp(&key(b)).then(a.ms_per_frame.total_cmp(&b.ms_per_frame)),
        };
        if state.sort_desc { o.reverse() } else { o }
    });
    rows
}

fn script_list(ui: &mut Ui, rect: Rect, state: &mut ScriptsTabState, snap: &Snapshot, rows: &[ScriptRow], ctx: &TabCtx<'_>) {
    let painter = ui.painter_at(rect);
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), 32.0));
    painter.rect_filled(head, CornerRadius::ZERO, pal::PANEL);
    painter.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, pal::LINE));
    // Column layout (right edges), scaled from the 690 px design.
    let w = rect.width();
    let cols: [(ScriptCol, &str, f32, bool); 6] = [
        (ScriptCol::Script, "Script", 0.0, false),
        (ScriptCol::Context, "Context", 184.0 / 690.0, false),
        (ScriptCol::Activity, "Activity", 305.0 / 690.0, false),
        (ScriptCol::Rate, "Rate", 518.0 / 690.0, true),
        (ScriptCol::MsPerFrame, "ms / frame", 609.0 / 690.0, true),
        (ScriptCol::Heap, "Heap", 1.0, true),
    ];
    let x_of = |f: f32| rect.left() + f * w;
    // Header cells: left columns start at their x, right ones end there.
    for (i, (col, label, f, right)) in cols.iter().enumerate() {
        let cell = if *right {
            let prev = if i > 0 { x_of(cols[i - 1].2) } else { rect.left() };
            Rect::from_min_max(pos2(prev, head.top()), pos2(x_of(*f), head.bottom()))
        } else {
            let next = cols.get(i + 1).map(|c| if c.3 { x_of(c.2) - 80.0 } else { x_of(c.2) }).unwrap_or(rect.right());
            Rect::from_min_max(pos2(x_of(*f), head.top()), pos2(next, head.bottom()))
        };
        let sorted = (state.sort == *col).then_some(state.sort_desc);
        if header_cell(ui, cell, label, *right, sorted, "scripts") {
            if state.sort == *col {
                state.sort_desc = !state.sort_desc;
            } else {
                state.sort = *col;
                state.sort_desc = !matches!(col, ScriptCol::Script | ScriptCol::Context);
            }
        }
    }

    // (two lines: name and parent path)
    let row_h = ctx.style.row_height() + 15.0;
    let body = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    if rows.is_empty() {
        let msg = if snap.scripts.is_empty() {
            if ctx.live { "No script has run yet. Start Play, or press ● Record." } else { "This capture has no script profile." }
        } else {
            "No script matches the filter."
        };
        child.painter().text(pos2(body.center().x, body.top() + 40.0), Align2::CENTER_CENTER, msg, ui_font(13.0), pal::MUTED);
        return;
    }
    egui::ScrollArea::vertical().id_salt("scripts-list").auto_shrink([false, false]).show_rows(
        &mut child,
        row_h,
        rows.len(),
        |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in &rows[range] {
                let (row, resp) = ui.allocate_exact_size(vec2(body.width(), row_h), Sense::click());
                let p = ui.painter();
                let selected = state.selected.as_ref().is_some_and(|(vm, path)| *vm == r.vm && path.as_str() == &*r.path);
                if selected {
                    p.rect_filled(row, CornerRadius::ZERO, pal::SEL);
                } else if resp.hovered() {
                    p.rect_filled(row, CornerRadius::ZERO, pal::HOVER);
                }
                p.hline(row.x_range(), row.bottom() - 0.5, Stroke::new(1.0, pal::LINE2));
                let x = |f: f32| row.left() + f * row.width();
                let (name, parent) = short_name(&r.path);
                let two = !parent.is_empty();
                let ny = if two { row.top() + row_h * 0.36 } else { row.center().y };
                text_clip(p, pos2(row.left() + 10.0, ny), Align2::LEFT_CENTER, name, ui_font(13.0), pal::TEXT, x(184.0 / 690.0) - row.left() - 16.0);
                if two {
                    text_clip(p, pos2(row.left() + 10.0, row.top() + row_h * 0.7), Align2::LEFT_CENTER, parent, ui_font(11.0), pal::FAINT, x(184.0 / 690.0) - row.left() - 16.0);
                }
                let (ctxl, colors) = context_badge(r.vm);
                let bx = badge(p, x(184.0 / 690.0) + 10.0, row.center().y, ctxl, colors);
                if r.native {
                    badge(p, bx + 4.0, row.center().y, "native", pal::NATIVE);
                }
                // Activity: a 3 px-per-% bar, then the value.
                let ax = x(305.0 / 690.0) + 10.0;
                // (3 px per %, as designed, within what the column leaves
                // beside the value and the Rate column)
                let bar_max = ((x(518.0 / 690.0) - 62.0) - ax - 62.0).clamp(12.0, 120.0);
                let bar_w = (r.activity_pct as f32 * 3.0).clamp(if r.activity_pct > 0.0 { 2.0 } else { 0.0 }, bar_max);
                if bar_w > 0.0 {
                    p.rect_filled(Rect::from_min_size(pos2(ax, row.center().y - 3.0), vec2(bar_w, 6.0)), CornerRadius::same(3), ctx.style.cat_color(Cat::Script));
                }
                let act_color = if r.activity_pct >= 10.0 { pal::WARN } else { pal::TEXT };
                p.text(pos2(ax + bar_w + 6.0, row.center().y), Align2::LEFT_CENTER, format!("{:.1} %", r.activity_pct), num_font(), act_color);
                p.text(pos2(x(518.0 / 690.0) - 10.0, row.center().y), Align2::RIGHT_CENTER, format!("{}/s", r.rate_hz), num_font(), pal::TEXT);
                p.text(pos2(x(609.0 / 690.0) - 10.0, row.center().y), Align2::RIGHT_CENTER, fmt_ms(r.ms_per_frame), num_font(), pal::TEXT);
                p.text(pos2(row.right() - 10.0, row.center().y), Align2::RIGHT_CENTER, fmt_bytes(r.heap_bytes), num_font(), pal::TEXT);
                if resp.clicked() {
                    state.selected = Some((r.vm, r.path.to_string()));
                    state.selected_fn = None;
                }
                let frames = frames_of(snap, r.vm);
                resp.on_hover_ui(|ui| {
                    ui.label(egui::RichText::new(&*r.path).strong());
                    ui.label(format!("{} VM{}", r.vm.label(), if r.native { " · --!native" } else { "" }));
                    egui::Grid::new("script-tip").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                        let kv = |ui: &mut Ui, k: &str, v: String| {
                            ui.label(egui::RichText::new(k).color(pal::MUTED));
                            ui.label(egui::RichText::new(v).font(num_font()));
                            ui.end_row();
                        };
                        kv(ui, "Activity (last second)", format!("{:.2} %", r.activity_pct));
                        kv(ui, "Resumes (last second)", format!("{}", r.rate_hz));
                        kv(ui, "Self ms / frame", format!("{:.3}", r.ms_per_frame));
                        kv(ui, "Self total", format!("{:.1} ms", r.self_ms));
                        kv(ui, "Incl. nested scripts", format!("{:.1} ms", r.total_ms));
                        kv(ui, "Resumes", format!("{}", r.resumes));
                        kv(ui, "Avg / resume", format!("{:.3} ms", r.avg_ms));
                        kv(ui, "Slowest resume", format!("{:.2} ms", r.max_ms));
                        kv(ui, "Frames", format!("{frames:.0}"));
                        kv(ui, "Luau heap", fmt_bytes(r.heap_bytes));
                        kv(ui, "Live threads", format!("{}", r.threads));
                        if r.samples > 0 {
                            kv(ui, "Samples", format!("{}", r.samples));
                        }
                    });
                    if !r.entries.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new("Entered by").color(pal::MUTED));
                        egui::Grid::new("script-entries").num_columns(4).spacing([12.0, 2.0]).show(ui, |ui| {
                            for e in r.entries.iter().take(8) {
                                let what = match (&e.event, &e.handler) {
                                    (Some(ev), Some(h)) => format!("{} · {ev} → {h}", e.kind.label()),
                                    (Some(ev), None) => format!("{} · {ev}", e.kind.label()),
                                    (None, Some(h)) => format!("{} · {h}", e.kind.label()),
                                    (None, None) => e.kind.label().to_string(),
                                };
                                ui.label(what);
                                ui.label(egui::RichText::new(format!("{}×", e.calls)).font(num_font()));
                                ui.label(egui::RichText::new(format!("avg {:.3} ms", e.avg_ms)).font(num_font()));
                                ui.label(egui::RichText::new(format!("max {:.2} ms", e.max_ms)).font(num_font()));
                                ui.end_row();
                            }
                        });
                    }
                });
            }
        },
    );
}

// ------------------------------------------------------------------ function tree

/// A visible function row.
#[derive(Clone)]
struct FnRow {
    /// Index into the tree used.
    node: usize,
    depth: u32,
    key: String,
    has_children: bool,
    collapsed: bool,
}

struct FnTree<'a> {
    nodes: &'a [TreeNode],
    exact: bool,
    root: Option<usize>,
    rows: Vec<FnRow>,
    /// Children per node (in tree order).
    kids: Vec<Vec<usize>>,
}

fn function_rows<'a>(snap: &'a Snapshot, state: &ScriptsTabState) -> FnTree<'a> {
    let exact = state.exact && !snap.exact_tree.is_empty() || snap.tree.is_empty() && !snap.exact_tree.is_empty();
    let nodes: &[TreeNode] = if exact { &snap.exact_tree } else { &snap.tree };
    let mut kids = vec![Vec::new(); nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            kids[p as usize].push(i);
        }
    }
    let root = state.selected.as_ref().and_then(|(vm, path)| {
        nodes.iter().position(|n| n.parent.is_none() && n.vm == *vm && &*n.script == path.as_str())
    });
    let needle = state.filter.trim().to_lowercase();
    let frames = state.selected.as_ref().map(|s| frames_of(snap, s.0)).unwrap_or(1.0);
    let sort_key = |n: &TreeNode| -> f64 {
        match state.fn_sort {
            FnCol::Self_ => n.self_ms,
            FnCol::Calls => n.calls as f64 / frames,
            FnCol::Line => -(n.line as f64),
            _ => n.total_ms,
        }
    };
    // Matches (for the filter): a node shows when it or a descendant matches.
    let mut shows = vec![needle.is_empty(); nodes.len()];
    if !needle.is_empty() {
        for i in (0..nodes.len()).rev() {
            if fn_label(&nodes[i]).to_lowercase().contains(&needle) || shows[i] {
                shows[i] = true;
                if let Some(p) = nodes[i].parent {
                    shows[p as usize] = true;
                }
            }
        }
        // A matching script shows its whole tree.
        if let Some(r) = root {
            if nodes[r].script.to_lowercase().contains(&needle) {
                shows.iter_mut().for_each(|s| *s = true);
            }
        }
    }
    let mut rows = Vec::new();
    if let Some(r) = root {
        let mut stack: Vec<(usize, u32, String)> = Vec::new();
        let mut top: Vec<usize> = kids[r].clone();
        let sort = |v: &mut Vec<usize>| {
            v.sort_by(|&a, &b| {
                let o = if state.fn_sort == FnCol::Function {
                    fn_label(&nodes[a]).to_lowercase().cmp(&fn_label(&nodes[b]).to_lowercase()).reverse()
                } else {
                    sort_key(&nodes[a]).total_cmp(&sort_key(&nodes[b]))
                };
                if state.fn_sort_desc { o } else { o.reverse() }
            })
        };
        sort(&mut top);
        for &k in top.iter() {
            stack.push((k, 0, String::new()));
        }
        while let Some((i, depth, parent_key)) = stack.pop() {
            if !shows[i] {
                continue;
            }
            let n = &nodes[i];
            let key = format!("{parent_key}/{}:{}:{}", n.script, fn_label(n), n.line);
            let collapsed = state.collapsed.contains(&key) && needle.is_empty();
            let mut ch = kids[i].clone();
            ch.retain(|&c| shows[c]);
            rows.push(FnRow { node: i, depth, key: key.clone(), has_children: !ch.is_empty(), collapsed });
            if !collapsed {
                sort(&mut ch);
                for c in ch {
                    stack.push((c, depth + 1, key.clone()));
                }
            }
        }
    }
    FnTree { nodes, exact, root, rows, kids }
}

fn function_table(ui: &mut Ui, rect: Rect, state: &mut ScriptsTabState, snap: &Snapshot, tree: &FnTree<'_>, ctx: &TabCtx<'_>) {
    let painter = ui.painter_at(rect);
    // Caption.
    let cap = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
    painter.hline(cap.x_range(), cap.bottom() - 0.5, Stroke::new(1.0, pal::LINE2));
    let title = state.selected.as_ref().map(|(_, p)| short_name(p).0.to_string()).unwrap_or_default();
    let g = painter.layout_no_wrap(title.clone(), egui::FontId::new(13.0, egui::FontFamily::Proportional), pal::WHITE);
    let tw = g.size().x;
    painter.galley(pos2(cap.left() + 12.0, cap.center().y - g.size().y / 2.0), g.clone(), pal::WHITE);
    // (bold: draw twice, offset by a fraction of a pixel)
    painter.galley(pos2(cap.left() + 12.4, cap.center().y - g.size().y / 2.0), g, pal::WHITE);
    let mode = if tree.exact { "exact" } else { "sampled" };
    let caption = if title.is_empty() {
        "Select a script".to_string()
    } else {
        format!(" · functions · avg over recording ({mode}) · click a row to open the script at that line")
    };
    text_clip(&painter, pos2(cap.left() + 12.0 + tw + 2.0, cap.center().y), Align2::LEFT_CENTER, &caption, ui_font(13.0), pal::MUTED, rect.right() - (cap.left() + 12.0 + tw + 2.0) - 8.0);

    let head = Rect::from_min_size(pos2(rect.left(), cap.bottom()), vec2(rect.width(), 30.0));
    painter.rect_filled(head, CornerRadius::ZERO, pal::PANEL);
    painter.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, pal::LINE));
    // Numeric columns from the right edge at their designed widths (Calls
    // 122, Self 81, Total 90, Line 67); a narrow panel drops Self, then Line.
    let calls_label = if tree.exact { "Calls / frame" } else { "Samples" };
    let w = rect.width();
    let show_self = w >= 520.0;
    let show_line = w >= 400.0;
    let mut cols: Vec<(FnCol, &str, f32)> = Vec::new();
    let mut edge = rect.right();
    cols.push((FnCol::Calls, calls_label, edge));
    edge -= if w >= 520.0 { 122.0 } else { 96.0 };
    if show_self {
        cols.push((FnCol::Self_, "Self ms", edge));
        edge -= 81.0;
    }
    cols.push((FnCol::Total, "Total ms", edge));
    edge -= 90.0;
    if show_line {
        cols.push((FnCol::Line, "Line", edge));
        edge -= 67.0;
    }
    cols.reverse();
    let name_right = edge;
    let col_x = |c: FnCol| cols.iter().find(|x| x.0 == c).map(|x| x.2);
    let mut clicked_col = None;
    if header_cell(ui, Rect::from_min_max(head.min, pos2(name_right, head.bottom())), "Function", false, (state.fn_sort == FnCol::Function).then_some(state.fn_sort_desc), "fns") {
        clicked_col = Some(FnCol::Function);
    }
    let mut prev = name_right;
    for (col, label, right) in cols.iter() {
        let cell = Rect::from_min_max(pos2(prev, head.top()), pos2(*right, head.bottom()));
        if header_cell(ui, cell, label, true, (state.fn_sort == *col).then_some(state.fn_sort_desc), "fns") {
            clicked_col = Some(*col);
        }
        prev = *right;
    }
    if let Some(c) = clicked_col {
        if state.fn_sort == c {
            state.fn_sort_desc = !state.fn_sort_desc;
        } else {
            state.fn_sort = c;
            state.fn_sort_desc = !matches!(c, FnCol::Function | FnCol::Line);
        }
    }

    let body = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    if tree.root.is_none() || tree.rows.is_empty() {
        let msg = if state.selected.is_none() {
            ""
        } else if !snap.recording && snap.samples == 0 && snap.exact_tree.is_empty() {
            "No call tree yet: press ● Record while the game runs."
        } else if tree.root.is_none() {
            "No samples landed in this script yet (it ran too briefly)."
        } else {
            "No function matches the filter."
        };
        child.painter().text(pos2(body.center().x, body.top() + 40.0), Align2::CENTER_CENTER, msg, ui_font(13.0), pal::MUTED);
        return;
    }
    let frames = state.selected.as_ref().map(|s| frames_of(snap, s.0)).unwrap_or(1.0);
    let row_h = ctx.style.row_height();
    let mut toggle: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("scripts-fns").auto_shrink([false, false]).show_rows(
        &mut child,
        row_h,
        tree.rows.len(),
        |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for fr in &tree.rows[range] {
                let n = &tree.nodes[fr.node];
                let (row, resp) = ui.allocate_exact_size(vec2(body.width(), row_h), Sense::click());
                let p = ui.painter();
                let selected = state.selected_fn.as_deref() == Some(fr.key.as_str());
                if selected {
                    p.rect_filled(row, CornerRadius::ZERO, pal::SEL);
                } else if resp.hovered() {
                    p.rect_filled(row, CornerRadius::ZERO, pal::HOVER);
                }
                p.hline(row.x_range(), row.bottom() - 0.5, Stroke::new(1.0, pal::LINE2));
                let indent = row.left() + 10.0 + fr.depth as f32 * 16.0;
                let cy = row.center().y;
                // Twisty.
                let tw_rect = Rect::from_min_size(pos2(indent - 2.0, row.top()), vec2(16.0, row_h));
                if fr.has_children {
                    let t = if fr.collapsed { "▸" } else { "▾" };
                    p.text(pos2(indent, cy), Align2::LEFT_CENTER, t, ui_font(13.0), pal::FAINT);
                }
                let name_color = if n.engine { pal::MUTED } else { pal::TEXT };
                let label = fn_label(n);
                let shown = if n.engine { format!("{label}  ({})", n.script) } else { label.clone() };
                text_clip(p, pos2(indent + 14.0, cy), Align2::LEFT_CENTER, &shown, ui_font(13.0), name_color, (name_right - indent - 20.0).max(40.0));
                let line = if n.c { "[C]".to_string() } else if n.line > 0 { n.line.to_string() } else { "—".into() };
                let line_color = if !n.c && n.line == 0 { pal::FAINT } else { pal::ACCENT };
                if let Some(x) = col_x(FnCol::Line) {
                    p.text(pos2(x - 10.0, cy), Align2::RIGHT_CENTER, line, num_font(), line_color);
                }
                if let Some(x) = col_x(FnCol::Total) {
                    p.text(pos2(x - 10.0, cy), Align2::RIGHT_CENTER, fmt_ms(n.total_ms / frames), num_font(), pal::TEXT);
                }
                if let Some(x) = col_x(FnCol::Self_) {
                    p.text(pos2(x - 10.0, cy), Align2::RIGHT_CENTER, fmt_ms(n.self_ms / frames), num_font(), pal::TEXT);
                }
                let last = if tree.exact { fmt_calls(n.calls as f64 / frames) } else { n.total_samples.to_string() };
                p.text(pos2(row.right() - 10.0, cy), Align2::RIGHT_CENTER, last, num_font(), pal::TEXT);

                let twisty_hit = fr.has_children && resp.clicked() && resp.interact_pointer_pos().is_some_and(|pt| tw_rect.contains(pt));
                if twisty_hit || (fr.has_children && resp.double_clicked()) {
                    toggle = Some(fr.key.clone());
                } else if resp.clicked() {
                    state.selected_fn = Some(fr.key.clone());
                    if !n.c && !n.engine && n.line > 0 {
                        // Open where the function's hottest line is, else its definition.
                        let line = snap
                            .functions
                            .iter()
                            .find(|f| f.vm == n.vm && f.script == n.script && f.name == n.name && f.line == n.line)
                            .and_then(|f| f.hot_lines.first().map(|h| h.0))
                            .filter(|_| !tree.exact)
                            .unwrap_or(n.line);
                        request_open(n.vm, &n.script, line);
                    }
                }
                let tip_frames = frames;
                resp.on_hover_ui(|ui| function_tip(ui, n, snap, tip_frames, tree.exact));
            }
        },
    );
    if let Some(k) = toggle {
        if !state.collapsed.remove(&k) {
            state.collapsed.insert(k);
        }
    }
}

fn function_tip(ui: &mut Ui, n: &TreeNode, snap: &Snapshot, frames: f64, exact: bool) {
    ui.label(egui::RichText::new(fn_label(n)).strong());
    let where_ = if n.c {
        "C function (engine)".to_string()
    } else if n.line > 0 {
        format!("{} : {}", n.script, n.line)
    } else {
        n.script.to_string()
    };
    ui.label(egui::RichText::new(where_).color(pal::MUTED));
    egui::Grid::new("fn-tip").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
        let kv = |ui: &mut Ui, k: &str, v: String| {
            ui.label(egui::RichText::new(k).color(pal::MUTED));
            ui.label(egui::RichText::new(v).font(num_font()));
            ui.end_row();
        };
        kv(ui, "Total / frame", format!("{:.3} ms", n.total_ms / frames));
        kv(ui, "Self / frame", format!("{:.3} ms", n.self_ms / frames));
        kv(ui, "Total, whole recording", format!("{:.1} ms", n.total_ms));
        if exact {
            kv(ui, "Calls", format!("{}", n.calls));
            kv(ui, "Calls / frame", fmt_calls(n.calls as f64 / frames));
            kv(ui, "Avg / call", format!("{:.4} ms", n.total_ms / n.calls.max(1) as f64));
            kv(ui, "Slowest call", format!("{:.3} ms", n.max_ms));
        } else {
            kv(ui, "Samples (total / self)", format!("{} / {}", n.total_samples, n.self_samples));
        }
        if n.native {
            kv(ui, "Code", if exact { "native (timed interpreted)".into() } else { "native".to_string() });
        }
    });
    if let Some(f) = snap.functions.iter().find(|f| f.vm == n.vm && f.script == n.script && f.name == n.name && f.line == n.line) {
        if !f.hot_lines.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new("Hot lines (self samples)").color(pal::MUTED));
            for (l, c) in f.hot_lines.iter().take(5) {
                ui.label(egui::RichText::new(format!("line {l:>5}   {c}")).font(num_font()));
            }
        }
    }
    if !n.c && n.line > 0 {
        ui.label(egui::RichText::new("Click to open the script at this line").color(pal::FAINT));
    }
}

// ------------------------------------------------------------------ flame view

fn flame_view(ui: &mut Ui, rect: Rect, state: &mut ScriptsTabState, snap: &Snapshot, tree: &FnTree<'_>, ctx: &TabCtx<'_>) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, pal::DEEP);
    painter.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, pal::LINE));
    // The selected function, else the script's heaviest entry.
    let sel = state
        .selected_fn
        .as_ref()
        .and_then(|k| tree.rows.iter().find(|r| &r.key == k))
        .map(|r| r.node)
        .or_else(|| tree.root.and_then(|r| tree.kids[r].iter().copied().max_by(|&a, &b| tree.nodes[a].total_ms.total_cmp(&tree.nodes[b].total_ms))));
    let script = state.selected.as_ref().map(|(_, p)| short_name(p).0.to_string()).unwrap_or_default();
    let Some(top) = sel else {
        painter.text(pos2(rect.left() + 10.0, rect.top() + 14.0), Align2::LEFT_CENTER, "Flame view", ui_font(11.5), pal::MUTED);
        return;
    };
    let frames = state.selected.as_ref().map(|s| frames_of(snap, s.0)).unwrap_or(1.0);
    let tn = &tree.nodes[top];
    painter.text(
        pos2(rect.left() + 10.0, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        format!("Flame view · {script}.{} (avg frame)", fn_label(tn)),
        ui_font(11.5),
        pal::MUTED,
    );
    let area = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top() + 28.0), pos2(rect.right() - 10.0, rect.bottom() - 4.0));
    let total = tn.total_ms.max(1e-9);
    let scale = area.width() as f64 / total;
    let base = ctx.style.cat_color(Cat::Script);
    let row_h = 20.0;
    // Breadth: (node or self-of-node, depth, x0 in ms)
    let mut stack: Vec<(Option<usize>, usize, u32, f64, f64)> = vec![(Some(top), top, 0, 0.0, tn.total_ms)];
    let mut click: Option<usize> = None;
    while let Some((node, owner, depth, x0, w_ms)) = stack.pop() {
        let y = area.top() + depth as f32 * row_h;
        if y + 17.0 > area.bottom() + 1.0 {
            continue;
        }
        let x = area.left() + (x0 * scale) as f32;
        let w = (w_ms * scale) as f32;
        if w < 1.0 {
            continue;
        }
        let r = Rect::from_min_size(pos2(x, y), vec2((w - 1.0).max(1.0), 17.0));
        let (name, color) = match node {
            Some(i) => {
                let n = &tree.nodes[i];
                let c = if n.engine { pal::ENGINE } else { base };
                (fn_label(n), c)
            }
            None => ("(self)".to_string(), base),
        };
        let shade = 1.0 - depth as f32 * 0.08;
        let color = Color32::from_rgb(
            (color.r() as f32 * shade) as u8,
            (color.g() as f32 * shade) as u8,
            (color.b() as f32 * shade) as u8,
        );
        painter.rect_filled(r, CornerRadius::same(2), color);
        let label = format!("{name} · {:.2} ms", w_ms / frames);
        if r.width() > 24.0 {
            text_clip(&painter, pos2(r.left() + 5.0, r.center().y), Align2::LEFT_CENTER, &label, ui_font(11.0), pal::BAR_TEXT, r.width() - 8.0);
        }
        let resp = ui.interact(r, ui.id().with(("flame", owner, node.is_none(), depth)), Sense::click());
        if let Some(i) = node {
            if resp.clicked() {
                click = Some(i);
            }
            let n = &tree.nodes[i];
            resp.on_hover_ui(|ui| function_tip(ui, n, snap, frames, tree.exact));
            // Children left to right (heaviest first), then the node's self.
            let mut ch = tree.kids[i].clone();
            ch.sort_by(|&a, &b| tree.nodes[b].total_ms.total_cmp(&tree.nodes[a].total_ms));
            let mut cx = x0;
            for c in ch {
                let cw = tree.nodes[c].total_ms;
                stack.push((Some(c), c, depth + 1, cx, cw));
                cx += cw;
            }
            let self_ms = n.self_ms;
            if !tree.kids[i].is_empty() && self_ms * scale > 2.0 {
                stack.push((None, i, depth + 1, cx, self_ms));
            }
        } else {
            resp.on_hover_text(format!("Time in the function itself: {:.3} ms / frame", w_ms / frames));
        }
    }
    if let Some(i) = click {
        if let Some(r) = tree.rows.iter().find(|r| r.node == i) {
            state.selected_fn = Some(r.key.clone());
        }
    }
}
