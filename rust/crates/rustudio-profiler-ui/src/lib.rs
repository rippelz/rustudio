//! The profiler window, shared by the editor (View > MicroProfiler) and the
//! standalone player (Ctrl+F6). It reads `rustudio_core::profiler`
//! captures, live or opened from a file, and follows the approved mockup:
//! a title bar with the tabs and capture controls, a stats bar, the frame
//! strip, the tab, and the category legend.
//!
//! Tabs are modules; `scripts` is the Luau script profiler's tab, fed by
//! `rustudio_engine::script_profiler` (the script lane's).

pub mod prefs;
pub mod scripts;
pub mod theme;

mod counters;
mod customize;
#[cfg(target_os = "android")]
pub mod android_dialog;
mod dialogs;
mod gpu;
mod strip;
mod table;
mod timeline;

use std::collections::HashMap;
use std::time::Instant;

use egui::{Align, Align2, Layout, Pos2, Rect, Sense, Ui, UiBuilder, Vec2};
use rustudio_core::profiler::{self, analysis, Capture, Cat, FrameKind, FrameRec};

use prefs::{Prefs, TabId};
use theme::*;

/// What every tab gets from the window: the capture it shows, the selected
/// time range, and the user's look settings.
pub struct TabCtx<'a> {
    /// The capture on screen (live snapshot or an opened file).
    pub capture: &'a rustudio_core::profiler::Capture,
    /// The selected range in capture nanoseconds (a frame, a dragged range),
    /// or None for "everything in the capture".
    pub range_ns: Option<(u64, u64)>,
    /// Recording live (false: paused, frozen, or a file).
    pub live: bool,
    pub style: &'a Style,
}

/// The window's look, shared by every tab so they read as one panel.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Style {
    /// sRGB colour per category (indexed by `Cat as usize`).
    pub cat_rgb: Vec<u32>,
    /// Compact rows (true) or comfortable ones.
    pub compact: bool,
    /// Show instruction counts instead of times where both exist.
    pub instr_mode: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            cat_rgb: rustudio_core::profiler::Cat::ALL.iter().map(|c| c.default_rgb()).collect(),
            compact: false,
            instr_mode: false,
        }
    }
}

impl Style {
    pub fn cat_color(&self, cat: rustudio_core::profiler::Cat) -> egui::Color32 {
        let rgb = self.cat_rgb.get(cat as usize).copied().unwrap_or_else(|| cat.default_rgb());
        egui::Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// Table row height for the chosen density.
    pub fn row_height(&self) -> f32 {
        if self.compact { 22.0 } else { 29.0 }
    }
}

/// What is on screen: the live recording or a file.
enum Source {
    Live,
    File { cap: Box<Capture>, name: String },
}

/// The selection the strip sets and every tab reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Sel {
    /// The newest complete frame (follows the recording).
    Latest,
    /// The display frame starting at this time.
    Frame(u64),
    /// A dragged range of frames [start, end].
    Range(u64, u64),
}

/// Something a click asked for, done once the capture lock is released.
pub(crate) enum Act {
    Live,
    Pause,
    Resume,
    CaptureNext(u32),
    ToggleSpike,
    SetInstr(bool),
    Open,
    Export { chrome: bool, selection: bool },
    Config(profiler::Config),
}

/// The profiler window's state. One per window; `show` draws it.
pub struct ProfilerWindow {
    pub prefs: Prefs,
    prefs_saved: Prefs,
    source: Source,
    pub(crate) sel: Sel,
    pub(crate) tl: timeline::State,
    pub(crate) table: table::State,
    pub(crate) counters: counters::State,
    pub(crate) scripts: scripts::ScriptsTabState,
    pub(crate) customize_open: bool,
    menu: Menu,
    menu_anchor: Pos2,
    more_anchor: Pos2,
    /// The host can open scripts (the editor): the sidebar offers "Open script".
    pub can_open_scripts: bool,
    pub(crate) open_script: Option<(String, Option<u32>)>,
    /// How far the title bar had to shrink (0 = full labels).
    pub(crate) title_level: u8,
    /// Per-frame category breakdown (ms), by frame start: frames don't change
    /// once gathered, so the strip computes each once.
    pub(crate) breakdown: HashMap<u64, [f32; Cat::ALL.len()]>,
    dialogs: dialogs::Dialogs,
    toast: Option<(String, Instant)>,
    enabled: bool,
    /// The config as of this frame (read before the capture is locked).
    pub(crate) cfg: profiler::Config,
    /// The open file's name while a file is drawn.
    pub(crate) file_name: Option<String>,
    export_anchor: Pos2,
    #[allow(clippy::type_complexity)]
    stats_cache: Option<(Instant, Sel, usize, analysis::FrameStats, f64, Option<(f64, f64)>)>,
}

impl Default for ProfilerWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProfilerWindow {
    fn drop(&mut self) {
        self.set_open(false);
    }
}

impl ProfilerWindow {
    pub fn new() -> Self {
        let prefs = Prefs::load();
        ProfilerWindow {
            prefs_saved: prefs.clone(),
            prefs,
            source: Source::Live,
            sel: Sel::Latest,
            tl: Default::default(),
            table: Default::default(),
            counters: Default::default(),
            scripts: Default::default(),
            customize_open: false,
            menu: Menu::None,
            menu_anchor: Pos2::ZERO,
            more_anchor: Pos2::ZERO,
            title_level: 0,
            can_open_scripts: false,
            open_script: None,
            breakdown: HashMap::new(),
            dialogs: Default::default(),
            toast: None,
            enabled: false,
            cfg: profiler::config(),
            file_name: None,
            export_anchor: Pos2::ZERO,
            stats_cache: None,
        }
    }

    /// Releases the window's live recording when its panel is hidden.
    pub fn hide(&mut self) -> bool {
        self.scripts.set_live_view(false);
        if self.enabled && profiler::active() && profiler::capture_requested() {
            return true;
        }
        self.set_open(false);
        false
    }

    /// Acquire or release this window's recording reference.
    pub fn set_open(&mut self, open: bool) {
        if !open {
            self.scripts.set_live_view(false);
        }
        if open != self.enabled {
            self.enabled = open;
            if open {
                profiler::enable();
            } else {
                profiler::disable();
            }
        }
    }

    /// Opens a capture file in the window (also from a CLI or drop).
    pub fn open_file(&mut self, path: &std::path::Path) {
        match profiler::export::open(path) {
            Ok(cap) => {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.toast(format!("Opened {name}"));
                self.source = Source::File { cap: Box::new(cap), name };
                self.sel = Sel::Latest;
                self.breakdown.clear();
                self.tl = Default::default();
                self.table.invalidate();
            }
            Err(e) => self.toast(e),
        }
    }

    /// For previews and checks: select frame N (or "A-B" frames) of the open
    /// capture, pin that frame's longest deep scope, open Customize.
    pub fn preview_setup(&mut self, sel: Option<&str>, pin: bool, customize: bool) {
        let Source::File { cap, .. } = &self.source else { return };
        let f = cap.render_frames();
        if let Some(sel) = sel {
            if let Some((a, b)) = sel.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    if a < f.len() && b < f.len() {
                        self.sel = Sel::Range(f[a].start, f[b].end);
                    }
                }
            } else if let Ok(i) = sel.parse::<usize>() {
                if let Some(fr) = f.get(i) {
                    self.sel = Sel::Frame(fr.start);
                }
            }
        }
        if pin {
            let (t0, t1) = self.sel_range(cap);
            let best = cap
                .tracks
                .iter()
                .flat_map(|t| t.spans_in(t0, t1).filter(|s| s.depth >= 2).map(move |s| (t, *s)))
                .max_by_key(|(_, s)| s.dur());
            if let Some((t, s)) = best {
                self.tl.pinned = Some(timeline::SpanKey { track: t.id, start: s.start, end: s.end, name: s.name, depth: s.depth });
            }
        }
        self.customize_open = customize;
    }

    /// A script the user asked to open from the sidebar: (full name, line).
    pub fn take_open_script(&mut self) -> Option<(String, Option<u32>)> {
        self.open_script.take()
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    /// Draws the whole window into `ui` (its available rect).
    pub fn show(&mut self, ui: &mut Ui) {
        self.poll_dialogs();
        self.set_open(matches!(self.source, Source::Live));
        let _p = profiler::scope("profiler_window", Cat::Ui);
        let status = profiler::status();
        self.cfg = profiler::config();
        let mut acts = Vec::new();
        let source = std::mem::replace(&mut self.source, Source::Live);
        match &source {
            Source::Live => profiler::with_capture(|cap| self.frame_ui(ui, cap, true, &status, &mut acts)),
            Source::File { cap, name } => {
                let name = name.clone();
                self.frame_ui_file(ui, cap, &name, &status, &mut acts)
            }
        }
        // (an action may have opened a file meanwhile)
        if matches!(self.source, Source::Live) {
            self.source = source;
        }
        let live = matches!(self.source, Source::Live);
        for a in acts {
            self.apply(a, live);
        }
        if matches!(self.source, Source::Live) {
            self.set_open(true);
            let active = profiler::active();
            self.scripts.set_live_view(active && self.prefs.tab == TabId::Scripts);
            if active {
                ui.ctx().request_repaint();
            }
        } else {
            self.set_open(false);
        }
        if self.prefs != self.prefs_saved {
            self.prefs.save();
            self.prefs_saved = self.prefs.clone();
        }
    }

    fn frame_ui_file(&mut self, ui: &mut Ui, cap: &Capture, name: &str, status: &profiler::Status, acts: &mut Vec<Act>) {
        self.file_name = Some(name.to_string());
        self.frame_ui(ui, cap, false, status, acts);
        self.file_name = None;
    }

    fn apply(&mut self, a: Act, live: bool) {
        match a {
            Act::Live => {
                if !live {
                    self.source = Source::Live;
                    self.breakdown.clear();
                    self.sel = Sel::Latest;
                    self.tl = Default::default();
                    self.table.invalidate();
                }
                profiler::resume();
            }
            Act::Pause => profiler::pause(),
            Act::Resume => {
                profiler::resume();
                self.sel = Sel::Latest;
            }
            Act::CaptureNext(n) => {
                self.source = Source::Live;
                self.breakdown.clear();
                self.sel = Sel::Latest;
                profiler::capture_next(n);
                self.toast(format!("Capturing the next {n} frames"));
            }
            Act::ToggleSpike => {
                let mut c = profiler::config();
                c.spike_capture = !c.spike_capture;
                if c.spike_capture {
                    profiler::resume();
                }
                profiler::set_and_save_config(c);
            }
            Act::SetInstr(on) => {
                let mut c = profiler::config();
                if c.instr_mode != on {
                    c.instr_mode = on;
                    self.prefs.style.instr_mode = on;
                    self.breakdown.clear();
                    profiler::set_and_save_config(c);
                }
            }
            Act::Config(c) => {
                if c.instr_mode != self.cfg.instr_mode {
                    self.breakdown.clear();
                }
                profiler::set_and_save_config(c);
            }
            Act::Open => self.dialogs.open(),
            Act::Export { chrome, selection } => self.dialogs.save(chrome, selection),
        }
    }

    fn poll_dialogs(&mut self) {
        match self.dialogs.poll() {
            Some(dialogs::Done::Open(path)) => self.open_file(&path),
            Some(dialogs::Done::Save { path, selection }) => {
                let mut cap = match &self.source {
                    Source::Live => profiler::snapshot(),
                    Source::File { cap, .. } => (**cap).clone(),
                };
                if selection {
                    let (t0, t1) = self.sel_range(&cap);
                    cap = cap.slice(t0, t1);
                }
                match profiler::export::save(&cap, &path) {
                    Ok(()) => self.toast(format!("Saved {}", path.display())),
                    Err(e) => self.toast(format!("Couldn't save {}: {e}", path.display())),
                }
            }
            None => {}
        }
    }

    // ------------------------------------------------------------ selection

    /// The selected frame, if the selection is one frame.
    pub(crate) fn sel_frame(&self, cap: &Capture) -> Option<(usize, FrameRec)> {
        let f = cap.render_frames();
        let last = f.len().checked_sub(1)?;
        match self.sel {
            Sel::Latest => Some((last, f[last])),
            Sel::Frame(start) => {
                let i = f.partition_point(|x| x.start < start);
                if i < f.len() && f[i].start == start {
                    Some((i, f[i]))
                } else {
                    None
                }
            }
            Sel::Range(..) => None,
        }
    }

    /// The selected time range (a frame, a dragged range, or the newest frame).
    pub(crate) fn sel_range(&self, cap: &Capture) -> (u64, u64) {
        if let Sel::Range(a, b) = self.sel {
            return (a, b);
        }
        if let Some((_, f)) = self.sel_frame(cap) {
            return (f.start, f.end);
        }
        cap.time_range().unwrap_or((0, 1))
    }

    /// A range for statistics: the dragged range, else the last `n` frames
    /// (a single frame tells little about averages).
    pub(crate) fn stats_range(&self, cap: &Capture, n: usize) -> (u64, u64) {
        match self.sel {
            Sel::Range(a, b) => (a, b),
            Sel::Frame(_) => self.sel_range(cap),
            Sel::Latest => {
                let f = cap.render_frames();
                match (f.len().checked_sub(n.min(f.len())), f.back()) {
                    (Some(i), Some(last)) if !f.is_empty() => (f[i].start, last.end),
                    _ => cap.time_range().unwrap_or((0, 1)),
                }
            }
        }
    }

    // ------------------------------------------------------------ the frame

    fn frame_ui(&mut self, ui: &mut Ui, cap: &Capture, live: bool, status: &profiler::Status, acts: &mut Vec<Act>) {
        let full = ui.available_rect_before_wrap();
        ui.painter().rect_filled(full, 0.0, BG);
        ui.allocate_rect(full, Sense::hover());
        if self.prefs.hidden_tabs.contains(&self.prefs.tab) {
            self.prefs.tab = TabId::Timeline;
        }
        let mut y = full.top();
        let title = Rect::from_min_size(Pos2::new(full.left(), y), Vec2::new(full.width(), TITLEBAR_H));
        y += TITLEBAR_H;
        let toolbar = Rect::from_min_size(Pos2::new(full.left(), y), Vec2::new(full.width(), TOOLBAR_H));
        y += TOOLBAR_H;
        let legend = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - LEGEND_H), full.right_bottom());
        let has_strip = matches!(self.prefs.tab, TabId::Timeline | TabId::Table | TabId::Gpu);
        // (a short window gets a shorter strip so the tab keeps room)
        let strip_h = if full.height() < 620.0 { 74.0 } else { STRIP_H };
        // (very short: no strip at all, the tab needs the room; the stats
        // bar still shows the frame numbers)
        let has_strip = has_strip && full.height() >= 380.0;
        let strip = Rect::from_min_size(Pos2::new(full.left(), y), Vec2::new(full.width(), strip_h));
        if has_strip {
            y += strip_h;
        }
        let body = Rect::from_min_max(Pos2::new(full.left(), y), Pos2::new(full.right(), legend.top()));

        self.title_bar(ui, title, live, status, acts);
        {
            let _p = profiler::scope("pw_stats_bar", Cat::Ui);
            self.stats_bar(ui, toolbar, cap, live);
        }
        if has_strip {
            let _p = profiler::scope("pw_strip", Cat::Ui);
            strip::show(self, &mut region(ui, strip, "strip"), cap, live);
        }
        let _tab = profiler::scope(
            match self.prefs.tab {
                TabId::Timeline => "pw_timeline",
                TabId::Table => "pw_table",
                TabId::Counters => "pw_counters",
                TabId::Gpu => "pw_gpu",
                TabId::Scripts => "pw_scripts",
            },
            Cat::Ui,
        );
        let mut b = region(ui, body, ("body", self.prefs.tab));
        b.set_clip_rect(body);
        match self.prefs.tab {
            TabId::Timeline => timeline::show(self, &mut b, cap, live),
            TabId::Table => table::show(self, &mut b, cap, live),
            TabId::Counters => counters::show(self, &mut b, cap),
            TabId::Gpu => gpu::show(self, &mut b, cap),
            TabId::Scripts => {
                let range = matches!(self.sel, Sel::Frame(_) | Sel::Range(..)).then(|| self.sel_range(cap));
                let ctx = TabCtx { capture: cap, range_ns: range, live: live && !status.paused && status.frozen.is_none(), style: &self.prefs.style };
                scripts::show(&mut b, &mut self.scripts, &ctx);
            }
        }
        self.legend(ui, legend);
        if self.customize_open {
            customize::show(self, ui, title.right_bottom(), cap, acts);
        }
        if self.menu == Menu::Export {
            self.export_popup(ui, acts);
        }
        if let Some((text, at)) = &self.toast {
            if at.elapsed().as_secs_f32() < 4.0 {
                let w = text_width(ui, text, &small()) + 24.0;
                let r = Rect::from_center_size(Pos2::new(full.center().x, legend.top() - 24.0), Vec2::new(w, 26.0));
                let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.id().with("toast")));
                p.rect(r, 3.0, POP_BG, egui::Stroke::new(1.0, POP_LINE), egui::StrokeKind::Inside);
                p.text(r.center(), Align2::CENTER_CENTER, text, small(), TEXT);
                ui.ctx().request_repaint();
            } else {
                self.toast = None;
            }
        }
    }

    fn title_bar(&mut self, ui: &mut Ui, r: Rect, live: bool, status: &profiler::Status, acts: &mut Vec<Act>) {
        fill(ui, r, DEEP);
        hline(ui, r.left(), r.right(), r.bottom() - 1.0, LINE);
        let paused = !live || status.paused || status.frozen.is_some();
        let (live_label, dot) = if !live {
            ("File", FAINT)
        } else if let Some(profiler::Frozen::Spike { .. }) = status.frozen {
            ("Frozen · spike", BAD)
        } else if status.frozen.is_some() {
            ("Captured", WARN)
        } else if status.paused {
            ("Paused", FAINT)
        } else {
            ("Live", GOOD)
        };
        let cap_text = |level: u8| match status.capturing_left {
            Some(n) if live => format!("Capturing… {n}"),
            _ if level >= 1 => format!("Capture {}", self.prefs.capture_frames),
            _ => format!("Capture {} frames", self.prefs.capture_frames),
        };
        let spike_text = |level: u8| if level >= 1 { "Spike".to_string() } else { format!("Spike ≥ {:.0} ms", self.cfg.spike_ms) };
        let tabs: Vec<TabId> = TabId::ALL.into_iter().filter(|t| !self.prefs.hidden_tabs.contains(t)).collect();

        // Measure each layout, widest first, and use the first that fits:
        // 0 full labels; 1 short labels; 2 icons, no title; 3 the less used
        // buttons in a "⋯" menu; 4 the tabs in a dropdown too.
        let bw = |ui: &Ui, text: &str, lead: f32| if text.is_empty() { lead + 20.0 } else { text_width(ui, text, &small()) + lead + 20.0 };
        let segw = |ui: &Ui, opts: &[&str]| opts.iter().map(|o| text_width(ui, o, &small()) + 18.0).sum::<f32>() + 2.0;
        let right_w = |ui: &Ui, level: u8| -> f32 {
            let gap = if level >= 1 { 5.0 } else { 7.0 };
            let live_w = bw(ui, if level >= 2 { "" } else { live_label }, 14.0);
            let pause_w = bw(ui, if level >= 1 { "" } else if paused { "Resume" } else { "Pause" }, 18.0);
            if level >= 3 {
                return live_w + pause_w + bw(ui, "⋯", 0.0) + gap * 2.0;
            }
            let items = [
                bw(ui, &cap_text(level), 0.0),
                bw(ui, &spike_text(level), if self.cfg.spike_capture { 14.0 } else { 0.0 }),
                segw(ui, &["ms", "instr"]),
                bw(ui, if level >= 2 { "Open" } else { "Open…" }, 0.0),
                bw(ui, if level >= 2 { "" } else { "Export ▾" }, if level >= 2 { 18.0 } else { 0.0 }),
                bw(ui, if level >= 1 { "" } else { "Customize" }, 18.0),
            ];
            live_w + pause_w + items.iter().sum::<f32>() + gap * 7.0
        };
        let tabs_w = |ui: &Ui, level: u8| -> f32 {
            let title = if level >= 2 { 0.0 } else { text_width(ui, "Profiler", &strong(13.0, ui)) + 18.0 };
            if level >= 4 {
                return title + text_width(ui, &format!("{} ▾", self.prefs.tab.title()), &body()) + 24.0;
            }
            let pad = if level >= 2 { 16.0 } else { 28.0 };
            title + tabs.iter().map(|t| text_width(ui, t.title(), &body()) + pad).sum::<f32>()
        };
        // (under 800 px the Timeline's sidebar can be hidden: its toggle)
        let side_toggle = self.prefs.tab == TabId::Timeline && r.width() < 800.0;
        let avail = r.width() - 20.0 - 12.0 - if side_toggle { 38.0 + 5.0 } else { 0.0 };
        let level = (0..=4u8).find(|&l| tabs_w(ui, l) + right_w(ui, l) <= avail).unwrap_or(4);

        let mut c = region(ui, r.shrink2(Vec2::new(10.0, 0.0)), "title");
        c.spacing_mut().item_spacing.x = 0.0;
        c.with_layout(Layout::left_to_right(Align::Center), |ui| {
            if level < 2 {
                ui.label(egui::RichText::new("Profiler").font(strong(13.0, ui)).color(WHITE));
                ui.add_space(18.0);
            }
            if level >= 4 {
                let label = format!("{} ▾", self.prefs.tab.title());
                let w = text_width(ui, &label, &body()) + 24.0;
                let (tr, resp) = ui.allocate_exact_size(Vec2::new(w, TITLEBAR_H), Sense::click());
                ui.painter().text(tr.center(), Align2::CENTER_CENTER, &label, body(), WHITE);
                ui.painter().rect_filled(Rect::from_min_max(Pos2::new(tr.left(), tr.bottom() - 2.0), tr.right_bottom()), 0.0, ACCENT);
                if resp.clicked() {
                    self.menu = if self.menu == Menu::Tabs { Menu::None } else { Menu::Tabs };
                }
                self.menu_anchor = tr.left_bottom();
            } else {
                for t in &tabs {
                    let on = self.prefs.tab == *t;
                    let w = text_width(ui, t.title(), &body()) + if level >= 2 { 16.0 } else { 28.0 };
                    let (tr, resp) = ui.allocate_exact_size(Vec2::new(w, TITLEBAR_H), Sense::click());
                    let color = if on { WHITE } else if resp.hovered() { TEXT } else { MUTED };
                    ui.painter().text(tr.center(), Align2::CENTER_CENTER, t.title(), body(), color);
                    if on {
                        ui.painter().rect_filled(Rect::from_min_max(Pos2::new(tr.left(), tr.bottom() - 2.0), tr.right_bottom()), 0.0, ACCENT);
                    }
                    if resp.clicked() {
                        self.prefs.tab = *t;
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = if level >= 1 { 5.0 } else { 7.0 };
                if side_toggle {
                    let open = self.tl.side_open.unwrap_or(r.width() >= 600.0);
                    let icon = if open { "layout-sidebar-right-collapse" } else { "layout-sidebar-right-expand" };
                    if button_ex(ui, "", if open { BtnKind::On } else { BtnKind::Normal }, None, Some(icon))
                        .on_hover_text(if open { "Hide the sidebar" } else { "Show the sidebar" })
                        .clicked()
                    {
                        self.tl.side_open = Some(!open);
                    }
                }
                if level >= 3 {
                    let mr = button_ex(ui, "⋯", if self.menu == Menu::More { BtnKind::On } else { BtnKind::Normal }, None, None).on_hover_text("Capture, units, open, export, customize");
                    if mr.clicked() {
                        self.menu = if self.menu == Menu::More { Menu::None } else { Menu::More };
                    }
                    self.more_anchor = mr.rect.right_bottom();
                } else {
                    if button_ex(ui, if level >= 1 { "" } else { "Customize" }, if self.customize_open { BtnKind::On } else { BtnKind::Normal }, None, Some("settings"))
                        .on_hover_text("Customize")
                        .clicked()
                    {
                        self.customize_open = !self.customize_open;
                        self.menu = Menu::None;
                    }
                    let er = button_ex(ui, if level >= 2 { "" } else { "Export ▾" }, if self.menu == Menu::Export { BtnKind::On } else { BtnKind::Normal }, None, (level >= 2).then_some("download"))
                        .on_hover_text("Export");
                    if er.clicked() {
                        self.menu = if self.menu == Menu::Export { Menu::None } else { Menu::Export };
                        self.customize_open = false;
                    }
                    self.export_anchor = er.rect.left_bottom();
                    if button(ui, if level >= 2 { "Open" } else { "Open…" }).clicked() {
                        acts.push(Act::Open);
                    }
                    if let Some(i) = seg(ui, &["ms", "instr"], self.cfg.instr_mode as usize) {
                        acts.push(Act::SetInstr(i == 1));
                    }
                    let armed = self.cfg.spike_capture;
                    if button_ex(ui, &spike_text(level), if armed { BtnKind::On } else { BtnKind::Normal }, armed.then_some(BAD), None)
                        .on_hover_text(format!("Freeze the capture just after the next frame slower than {:.1} ms", self.cfg.spike_ms))
                        .clicked()
                    {
                        acts.push(Act::ToggleSpike);
                    }
                    if button(ui, &cap_text(level)).on_hover_text("Clear and record exactly this many frames, then freeze").clicked() {
                        acts.push(Act::CaptureNext(self.prefs.capture_frames));
                    }
                }
                let pause_label = if level >= 1 { "" } else if paused { "Resume" } else { "Pause" };
                if paused {
                    if button_ex(ui, pause_label, BtnKind::Normal, None, Some("player-play")).on_hover_text("Resume").clicked() {
                        acts.push(if live { Act::Resume } else { Act::Live });
                    }
                } else if button_ex(ui, pause_label, BtnKind::Normal, None, Some("player-pause")).on_hover_text("Pause").clicked() {
                    acts.push(Act::Pause);
                }
                if button_ex(ui, if level >= 2 { "" } else { live_label }, BtnKind::Normal, Some(dot), None)
                    .on_hover_text(format!("{live_label} · back to the live recording"))
                    .clicked()
                {
                    acts.push(Act::Live);
                }
            });
        });
        self.title_level = level;
        // the dropdowns
        match self.menu {
            Menu::Tabs => {
                let mut pick = None;
                let at = self.menu_anchor;
                let outside = menu_popup(ui, "tabs-menu", at, 180.0, |ui| {
                    for t in &tabs {
                        if menu_item(ui, t.title(), None, *t == self.prefs.tab) {
                            pick = Some(*t);
                        }
                    }
                });
                if let Some(t) = pick {
                    self.prefs.tab = t;
                    self.menu = Menu::None;
                } else if outside {
                    self.menu = Menu::None;
                }
            }
            Menu::More => {
                let mut close = false;
                let at = Pos2::new(self.more_anchor.x - 260.0, self.more_anchor.y);
                let instr = self.cfg.instr_mode;
                let armed = self.cfg.spike_capture;
                let outside = menu_popup(ui, "more-menu", at, 260.0, |ui| {
                    if menu_item(ui, &cap_text(0), Some("Clear and record exactly this many frames"), false) {
                        acts.push(Act::CaptureNext(self.prefs.capture_frames));
                        close = true;
                    }
                    if menu_item(ui, &format!("Spike capture ≥ {:.0} ms", self.cfg.spike_ms), Some(if armed { "Armed: freezes after the next spike" } else { "Freeze after the next slow frame" }), armed) {
                        acts.push(Act::ToggleSpike);
                        close = true;
                    }
                    if menu_item(ui, if instr { "Units: instructions" } else { "Units: milliseconds" }, Some("Switch between ms and instruction counts"), false) {
                        acts.push(Act::SetInstr(!instr));
                        close = true;
                    }
                    if menu_item(ui, "Open…", Some("A .rsprof capture or a Chrome trace"), false) {
                        acts.push(Act::Open);
                        close = true;
                    }
                    for (label, chrome, selection) in [("Export capture (.rsprof)", false, false), ("Export Chrome trace (.json)", true, false), ("Export selection (.rsprof)", false, true)] {
                        if menu_item(ui, label, None, false) {
                            acts.push(Act::Export { chrome, selection });
                            close = true;
                        }
                    }
                    if menu_item(ui, "Customize…", None, self.customize_open) {
                        self.customize_open = !self.customize_open;
                        close = true;
                    }
                });
                if close || outside {
                    self.menu = Menu::None;
                }
            }
            _ => {}
        }
    }

    fn stats_bar(&mut self, ui: &mut Ui, r: Rect, cap: &Capture, live: bool) {
        fill(ui, r, PANEL);
        hline(ui, r.left(), r.right(), r.bottom() - 1.0, LINE2);
        let frames = cap.render_frames();
        // FPS over the last second; the rest over the selection range or the ring
        let (t0, t1) = match self.sel {
            Sel::Range(a, b) => (a, b),
            _ => cap.time_range().unwrap_or((0, 1)),
        };
        // (sorting the ring and the CPU/GPU split cost real time: 4 times a second)
        let stale = self.stats_cache.as_ref().is_none_or(|c| c.0.elapsed().as_millis() > 250 || c.1 != self.sel || (!live && c.2 != frames.len()));
        if stale {
            let st = analysis::frame_stats(analysis::frames_in(cap, FrameKind::Render, t0, t1));
            let fps = match frames.back() {
                Some(last) => {
                    let since = last.end.saturating_sub(1_000_000_000);
                    let n = frames.iter().rev().take_while(|f| f.start >= since).count();
                    let span = frames.iter().rev().take(n).map(|f| f.dur()).sum::<u64>();
                    if span > 0 { n as f64 / (span as f64 / 1e9) } else { 0.0 }
                }
                None => 0.0,
            };
            let (b0, b1) = self.stats_range(cap, 60);
            let bal = analysis::cpu_gpu_balance(cap, b0, b1);
            self.stats_cache = Some((Instant::now(), self.sel, frames.len(), st, fps, bal));
        }
        let (_, _, _, st, fps, balance) = self.stats_cache.expect("filled above");
        let mut c = region(ui, r.shrink2(Vec2::new(10.0, 0.0)), "stats");
        c.spacing_mut().item_spacing.x = 0.0;
        let instr = self.cfg.instr_mode;
        c.with_layout(Layout::left_to_right(Align::Center), |ui| {
            stat(ui, &format!("{fps:.1}"), "FPS", WHITE, true, true);
            stat(ui, &format!("{:.1} ms", st.avg_ms), "FRAME AVG", WHITE, true, false);
            stat(ui, &format!("{:.1} ms", st.p95_ms), "P95", WHITE, true, false);
            let bad = st.max_ms > self.cfg.spike_ms as f64;
            stat(ui, &format!("{:.1} ms", st.max_ms), "MAX", if bad { BAD } else { WHITE }, true, false);
            // which side holds the frames back
            match balance {
                Some((cpu, gpu)) if gpu > 0.0 => {
                    let (label, sub) = if gpu > cpu * 1.1 { ("GPU-bound", format!("CPU {cpu:.1} MS")) } else { ("CPU-bound", format!("GPU {gpu:.1} MS")) };
                    stat(ui, label, &sub, WARN, false, false);
                }
                Some((cpu, _)) => stat(ui, "CPU", &format!("{cpu:.1} MS · GPU TIMING OFF"), WARN, false, false),
                None => stat(ui, "—", "NO FRAMES YET", MUTED, false, false),
            }
            let (head, rest) = match &self.file_name {
                Some(name) => (format!("File · {name}"), format!("{} · {}", cap.meta.app, cap.meta.host)),
                None => {
                    let ctxs = if cap.meta.context.is_empty() { "Edit".to_string() } else { cap.meta.context.clone() };
                    let mut parts = ctxs.splitn(2, " · ");
                    (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string())
                }
            };
            let cap_frames = (self.cfg.history_secs as f64 * fps.max(1.0)).round() as usize;
            let sub = if live {
                format!("{rest} · {} / {} frames", frames.len(), cap_frames.max(frames.len()))
            } else {
                format!("{rest} · {} frames", frames.len())
            };
            let sub = if instr { format!("{sub} · instruction counts") } else { sub };
            ui.add_space(10.0);
            let sub = sub.trim_start_matches(" · ").to_uppercase();
            let want = text_width(ui, &head, &strong(13.0, ui)).max(text_width(ui, &sub, &tiny()) * 1.06) + 12.0;
            // (the pinned counters keep at least half the room left)
            let ctx_w = want.min((ui.available_width() * 0.5).max(140.0));
            ui.allocate_ui_with_layout(Vec2::new(ctx_w, TOOLBAR_H), Layout::top_down(Align::Min), |ui| {
                ui.set_width(ctx_w);
                ui.add_space(5.0);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(egui::Label::new(egui::RichText::new(head).font(strong(13.0, ui)).color(TEXT)).truncate());
                ui.add(egui::Label::new(egui::RichText::new(sub).font(tiny()).color(MUTED).extra_letter_spacing(0.5)).truncate());
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let pinned: Vec<String> = self.prefs.pinned_counters.iter().rev().cloned().collect();
                for key in pinned {
                    // (only as many as fit beside the stats)
                    if ui.available_width() < 150.0 {
                        break;
                    }
                    if let Some(card) = counters::card(&key) {
                        counters::chip(ui, cap, card, &self.prefs.style);
                    }
                }
            });
        });
    }

    fn legend(&mut self, ui: &mut Ui, r: Rect) {
        fill(ui, r, PANEL);
        hline(ui, r.left(), r.right(), r.top(), LINE);
        let mut c = region(ui, r.shrink2(Vec2::new(10.0, 0.0)), "legend");
        c.spacing_mut().item_spacing.x = 14.0;
        c.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let cats = legend_cats();
            for (i, cat) in cats.iter().copied().enumerate() {
                let on = self.prefs.cat_visible(cat);
                let name = cat.name();
                let w = text_width(ui, name, &small()) + 15.0;
                // (what doesn't fit is in Customize: "+N" opens it)
                let rest = cats.len() - i;
                if ui.available_width() < w + if rest > 1 { 40.0 } else { 0.0 } {
                    let more = format!("+{rest}");
                    let (mr, mresp) = ui.allocate_exact_size(Vec2::new(text_width(ui, &more, &small()) + 4.0, 20.0), Sense::click());
                    ui.painter().text(mr.left_center(), Align2::LEFT_CENTER, more, small(), if mresp.hovered() { TEXT } else { MUTED });
                    if mresp.on_hover_text("More categories: Customize").clicked() {
                        self.customize_open = true;
                    }
                    break;
                }
                let (lr, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::click());
                let a = if on { 1.0 } else { 0.38 };
                ui.painter().rect_filled(Rect::from_min_size(Pos2::new(lr.left(), lr.center().y - 5.0), Vec2::splat(10.0)), 2.0, self.prefs.style.cat_color(cat).gamma_multiply(a));
                ui.painter().text(Pos2::new(lr.left() + 15.0, lr.center().y), Align2::LEFT_CENTER, name, small(), MUTED.gamma_multiply(a));
                if resp.clicked() {
                    let shift = ui.input(|i| i.modifiers.shift);
                    let h = &mut self.prefs.hidden_cats;
                    if shift {
                        let solo = legend_cats().into_iter().all(|c| (c == cat) != h.contains(&c));
                        h.clear();
                        if !solo {
                            h.extend(legend_cats().into_iter().filter(|c| *c != cat));
                        }
                    } else if let Some(i) = h.iter().position(|c| *c == cat) {
                        h.remove(i);
                    } else {
                        h.push(cat);
                    }
                    self.breakdown.clear();
                }
            }
            let hint = "Click a category to hide it · Shift-click to solo";
            if ui.available_width() > text_width(ui, hint, &small()) + 20.0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(hint).font(small()).color(MUTED));
                });
            }
        });
    }

    fn export_popup(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let at = self.export_anchor + Vec2::new(0.0, 4.0);
        let area = egui::Area::new(ui.id().with("export_menu")).fade_in(false).order(egui::Order::Foreground).fixed_pos(at);
        let resp = area.show(ui.ctx(), |ui| {
            egui::Frame::new().fill(POP_BG).stroke(egui::Stroke::new(1.0, POP_LINE)).corner_radius(5).inner_margin(8).show(ui, |ui| {
                ui.set_width(250.0);
                let items = [
                    ("Capture (.rsprof)", false, false, "Everything, re-opens in this window"),
                    ("Chrome trace (.json)", true, false, "Opens in Perfetto and chrome://tracing"),
                    ("Selection (.rsprof)", false, true, "Only the selected frames"),
                    ("Selection as Chrome trace", true, true, "Only the selected frames"),
                ];
                for (label, chrome, sel, hint) in items {
                    let r = ui.allocate_exact_size(Vec2::new(250.0, 40.0), Sense::click());
                    if r.1.hovered() {
                        ui.painter().rect_filled(r.0, 3.0, SEG_ON);
                    }
                    ui.painter().text(r.0.left_top() + Vec2::new(8.0, 6.0), Align2::LEFT_TOP, label, body(), TEXT);
                    ui.painter().text(r.0.left_top() + Vec2::new(8.0, 23.0), Align2::LEFT_TOP, hint, tiny(), MUTED);
                    if r.1.clicked() {
                        acts.push(Act::Export { chrome, selection: sel });
                        self.menu = Menu::None;
                    }
                }
            });
        });
        if resp.response.clicked_elsewhere() && !ui.input(|i| i.pointer.any_pressed() && false) {
            // (the Export button toggles it itself)
            let over_button = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| (p - self.export_anchor).length() < 40.0 && p.y < self.export_anchor.y);
            if !over_button {
                self.menu = Menu::None;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Menu {
    None,
    Export,
    More,
    Tabs,
}

/// A dropdown at `at`; true when the user pressed outside it (and below the
/// title bar, so the button that opened it can close it itself).
fn menu_popup(ui: &mut Ui, id: &str, at: Pos2, width: f32, add: impl FnOnce(&mut Ui)) -> bool {
    let at = Pos2::new(at.x.max(ui.ctx().content_rect().left() + 4.0), at.y + 4.0);
    let r = egui::Area::new(ui.id().with(("menu", id))).fade_in(false).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(POP_BG).stroke(egui::Stroke::new(1.0, POP_LINE)).corner_radius(5).inner_margin(6).show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    });
    ui.input(|i| i.pointer.any_pressed())
        && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !r.response.rect.contains(p) && p.y > at.y)
}

/// One row of a dropdown: label, optional hint under it; true when clicked.
fn menu_item(ui: &mut Ui, label: &str, hint: Option<&str>, on: bool) -> bool {
    let h = if hint.is_some() { 40.0 } else { 26.0 };
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    if resp.hovered() || on {
        ui.painter().rect_filled(r, 3.0, if on { SEG_ON } else { PANEL2 });
    }
    ui.painter().text(r.left_top() + Vec2::new(8.0, if hint.is_some() { 6.0 } else { 5.0 }), Align2::LEFT_TOP, label, body(), TEXT);
    if let Some(hint) = hint {
        ui.painter().text(r.left_top() + Vec2::new(8.0, 23.0), Align2::LEFT_TOP, hint, tiny(), MUTED);
    }
    resp.clicked()
}

/// The categories in the legend (and the customise list), in display order.
pub(crate) fn legend_cats() -> Vec<Cat> {
    use Cat::*;
    vec![Render, Gpu, Physics, Script, Ui, Gui, Animation, Network, Assets, Audio, Input, Pathfinding, Wait, Other]
}

/// One "value over LABEL" block of the stats bar.
fn stat(ui: &mut Ui, value: &str, label: &str, color: egui::Color32, mono_value: bool, first: bool) {
    let vf = if mono_value { mono_big() } else { strong(13.0, ui) };
    let w = text_width(ui, value, &vf).max(text_width(ui, label, &tiny()) + 4.0) + if first { 12.0 } else { 20.0 };
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, TOOLBAR_H), Sense::hover());
    let x = r.left() + if first { 2.0 } else { 10.0 };
    ui.painter().text(Pos2::new(x, r.top() + 5.0), Align2::LEFT_TOP, value, vf, color);
    ui.painter().text(Pos2::new(x, r.top() + 23.0), Align2::LEFT_TOP, label, tiny(), MUTED);
    vline(ui, r.right() - 1.0, r.top() + 6.0, r.bottom() - 6.0, LINE2);
}

/// A child Ui over `rect`.
pub(crate) fn region(ui: &mut Ui, rect: Rect, salt: impl std::hash::Hash + std::fmt::Debug) -> Ui {
    ui.new_child(UiBuilder::new().max_rect(rect).id_salt(salt).layout(Layout::top_down(Align::Min)))
}

/// A scope name for display ("gpu: ssao" → "SSAO"-ish: the GPU prefix off).
pub(crate) fn display_name(name: &str) -> &str {
    name.strip_prefix("gpu: ").unwrap_or(name)
}

/// The script a Script scope names, if it names one: an instance path
/// ("ServerScriptService.NPC.NPCController"), optionally with a line
/// ("…:142", "… : 142", "… line 142").
pub(crate) fn script_location(name: &str) -> Option<(String, Option<u32>)> {
    let name = name.trim();
    let (path, line) = match name.rsplit_once(':').or_else(|| name.rsplit_once(" line ")) {
        Some((p, l)) if l.trim().parse::<u32>().is_ok() => (p.trim(), l.trim().parse::<u32>().ok()),
        _ => (name, None),
    };
    let looks_like_path = path.contains('.') && path.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !path.contains(' ');
    looks_like_path.then(|| (path.to_string(), line))
}

/// 1352 → "1 352" (the mockup's frame numbers).
pub(crate) fn spaced(v: u64) -> String {
    rustudio_core::profiler::capture::group_thousands(v as i64).replace(',', " ")
}

/// A duration (ns) as the window shows it: "5.50 ms", "812 µs".
pub(crate) fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1e6)
}
