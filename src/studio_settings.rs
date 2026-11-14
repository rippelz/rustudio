//! File > Studio Settings (Alt+S): Studio's native Qt settings dialog
//! (RobloxSettingsDialog) and the values behind settings().
//!
//! Studio 0.747 (Vinegar, checks/studio-settings/inventory.md) shows a
//! search box, a page list (Studio, Selection, Script Editor, Diagnostics,
//! Network, Physics, Rendering), each page a property grid of settings()
//! members grouped by category, and Reset all settings / Close. The pages,
//! labels, order and defaults are generated into studio_settings_schema.rs.
//!
//! One store for the process: the dialog edits it, settings().Studio and
//! friends read and write it from Luau (src/domains/services.rs), and the
//! editor reads the wired ones where it used fixed numbers before (camera
//! speeds, selection / hover outline, script editor and Output fonts and
//! colours, script timeout, soft snap margin, physics sleep, main volume…).
//!
//! Kept in `$XDG_CONFIG_HOME/rustudio/studio-settings.json` (only values
//! that differ from the defaults), for runs a person drives
//! (`layout_store::enabled`); tests and harness runs use the defaults and
//! never write. `RUSTUDIO_STUDIO_SETTINGS=<file>` picks the file for any
//! run (the persistence test uses it).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use egui::{Color32, FontId, Pos2, Rect, Sense, Vec2};

#[path = "studio_settings_schema.rs"]
mod schema;
pub use schema::DEFS;

/// A row's value type, as the dialog edits it.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Bool,
    Int,
    Float,
    /// Enum name and its items (name, value, as the dialog spells it).
    Enum(&'static str, &'static [(&'static str, u32, &'static str)]),
    Color,
    Str,
    /// QFont: family and point size.
    Font,
    /// QDir: a folder.
    Dir,
    /// Shown greyed (Diagnostics' counts and version).
    ReadOnly,
}

/// A row's default.
#[derive(Clone, Copy, Debug)]
pub enum Dflt {
    B(bool),
    N(f64),
    E(u32),
    C(f64, f64, f64),
    S(&'static str),
    F(&'static str, f64),
    /// A folder under rustudio's data directory (Studio: %LOCALAPPDATA%\Roblox\<name>).
    Dir(&'static str),
    Live,
}

pub struct Def {
    pub page: &'static str,
    pub cat: &'static str,
    pub label: &'static str,
    /// The settings() item's class and the member's name.
    pub class: &'static str,
    pub prop: &'static str,
    pub kind: Kind,
    pub dflt: Dflt,
}

/// The dialog's pages in order.
pub const PAGES: &[&str] = &["Studio", "Selection", "Script Editor", "Diagnostics", "Network", "Physics", "Rendering"];


#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    Num(f64),
    Enum(u32),
    Color([f32; 3]),
    Str(String),
    Font(String, f64),
}

impl Def {
    pub fn default_val(&self) -> Val {
        match self.dflt {
            Dflt::B(b) => Val::Bool(b),
            Dflt::N(n) => Val::Num(n),
            Dflt::E(e) => Val::Enum(e),
            Dflt::C(r, g, b) => Val::Color([r as f32, g as f32, b as f32]),
            Dflt::S(s) => Val::Str(s.to_string()),
            Dflt::F(f, pt) => Val::Font(f.to_string(), pt),
            Dflt::Dir(d) => Val::Str(default_dir(d)),
            Dflt::Live => Val::Str(String::new()),
        }
    }

    fn key(&self) -> String {
        format!("{}.{}", self.class, self.prop)
    }
}

fn default_dir(name: &str) -> String {
    crate::paths::data().join("rustudio").join(name).display().to_string()
}

/// Index of (class, member) in DEFS.
pub fn index(class: &str, prop: &str) -> Option<usize> {
    static IDX: OnceLock<HashMap<(&'static str, &'static str), usize>> = OnceLock::new();
    IDX.get_or_init(|| DEFS.iter().enumerate().map(|(i, d)| ((d.class, d.prop), i)).collect())
        .get(&(class, prop))
        .copied()
}

// ------------------------------------------------------------------ store

struct Store {
    vals: Vec<Val>,
    file: Option<PathBuf>,
    /// Changed since the last write, and when.
    dirty: Option<Instant>,
}

fn store() -> &'static RwLock<Store> {
    static S: OnceLock<RwLock<Store>> = OnceLock::new();
    S.get_or_init(|| {
        let file = std::env::var_os("RUSTUDIO_STUDIO_SETTINGS").map(PathBuf::from);
        let mut s = Store { vals: DEFS.iter().map(Def::default_val).collect(), file: None, dirty: None };
        if let Some(f) = file {
            load_into(&mut s, &f);
            s.file = Some(f);
        }
        RwLock::new(s)
    })
}

fn default_file() -> PathBuf {
    crate::paths::config().join("rustudio").join("studio-settings.json")
}

/// Startup: a run a person drives reads and keeps the user's settings
/// (RUSTUDIO_STUDIO_SETTINGS already chose a file otherwise).
pub fn init(persist: bool) {
    let mut s = store().write().unwrap();
    if s.file.is_none() && persist {
        let f = default_file();
        load_into(&mut s, &f);
        s.file = Some(f);
    }
    drop(s);
    apply_all();
}

fn load_into(s: &mut Store, f: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(f) else { return };
    let map: serde_json::Map<String, serde_json::Value> = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("rustudio: ignoring {}: {e}", f.display());
            return;
        }
    };
    for (i, d) in DEFS.iter().enumerate() {
        if let Some(v) = map.get(&d.key()).and_then(|j| from_json(d, j)) {
            s.vals[i] = v;
        }
    }
}

fn to_json(d: &Def, v: &Val) -> serde_json::Value {
    use serde_json::json;
    match (v, d.kind) {
        (Val::Bool(b), _) => json!(b),
        (Val::Num(n), _) => json!(n),
        (Val::Enum(e), Kind::Enum(_, items)) => {
            json!(items.iter().find(|x| x.1 == *e).map(|x| x.0.to_string()).unwrap_or_else(|| e.to_string()))
        }
        (Val::Enum(e), _) => json!(e),
        (Val::Color(c), _) => json!(c),
        (Val::Str(s), _) => json!(s),
        (Val::Font(f, pt), _) => json!({ "family": f, "size": pt }),
    }
}

fn from_json(d: &Def, j: &serde_json::Value) -> Option<Val> {
    Some(match d.kind {
        Kind::Bool => Val::Bool(j.as_bool()?),
        Kind::Int | Kind::Float => Val::Num(j.as_f64().filter(|n| n.is_finite())?),
        Kind::Enum(_, items) => Val::Enum(items.iter().find(|x| Some(x.0) == j.as_str())?.1),
        Kind::Color => {
            let a = j.as_array()?;
            let c = |i: usize| a.get(i).and_then(|x| x.as_f64()).map(|x| x as f32);
            Val::Color([c(0)?, c(1)?, c(2)?])
        }
        Kind::Str | Kind::Dir => Val::Str(j.as_str()?.to_string()),
        Kind::Font => Val::Font(j.get("family")?.as_str()?.to_string(), j.get("size")?.as_f64()?),
        Kind::ReadOnly => return None,
    })
}

fn file_text(s: &Store) -> String {
    let mut map = serde_json::Map::new();
    for (i, d) in DEFS.iter().enumerate() {
        if s.vals[i] != d.default_val() {
            map.insert(d.key(), to_json(d, &s.vals[i]));
        }
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}

/// Writes pending changes now (the dialog closing, the app quitting).
pub fn flush() {
    let (file, text) = {
        let mut s = store().write().unwrap();
        if s.dirty.take().is_none() {
            return;
        }
        let Some(f) = s.file.clone() else { return };
        (f, file_text(&s))
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Written beside it, then renamed: a crash mid-write keeps the old.
    let tmp = file.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
}

/// Each frame: writes changes half a second after the last one (a dragged
/// value doesn't rewrite the file every frame).
pub fn tick() {
    let due = store().read().unwrap().dirty.is_some_and(|t| t.elapsed().as_secs_f32() > 0.5);
    if due {
        flush();
    }
}

pub fn get(class: &str, prop: &str) -> Option<Val> {
    let i = index(class, prop)?;
    Some(store().read().unwrap().vals[i].clone())
}

/// Sets a value (the dialog, settings() from Luau). Returns whether it changed.
pub fn set(class: &str, prop: &str, v: Val) -> bool {
    let Some(i) = index(class, prop) else { return false };
    {
        let mut s = store().write().unwrap();
        if s.vals[i] == v {
            return false;
        }
        s.vals[i] = v;
        s.dirty = Some(Instant::now());
    }
    apply(i);
    true
}

/// Every setting back to its default (Reset all settings).
pub fn reset_all() {
    {
        let mut s = store().write().unwrap();
        s.vals = DEFS.iter().map(Def::default_val).collect();
        s.dirty = Some(Instant::now());
    }
    apply_all();
}

/// Pushes a value into the engine parts that keep their own copy.
fn apply(i: usize) {
    let d = &DEFS[i];
    if (d.class, d.prop) == ("Studio", "Main Volume") {
        crate::audio::set_studio_main(num("Studio", "Main Volume", 1.0) as f32);
    }
}

fn apply_all() {
    for i in 0..DEFS.len() {
        apply(i);
    }
}

// ------------------------------------------------------------------ typed reads (the wired settings)

pub fn num(class: &str, prop: &str, or: f64) -> f64 {
    match get(class, prop) {
        Some(Val::Num(n)) if n.is_finite() => n,
        _ => or,
    }
}

pub fn flag(class: &str, prop: &str, or: bool) -> bool {
    match get(class, prop) {
        Some(Val::Bool(b)) => b,
        _ => or,
    }
}

pub fn color(class: &str, prop: &str, or: Color32) -> Color32 {
    match get(class, prop) {
        Some(Val::Color(c)) => color32(c),
        _ => or,
    }
}

/// A QFont setting's size in egui points (Studio's 10 pt = rustudio's
/// 13 px text, the size the editor used before the setting existed).
pub fn font_px(class: &str, prop: &str) -> f32 {
    match get(class, prop) {
        Some(Val::Font(_, pt)) if pt.is_finite() && pt > 0.0 => (pt as f32 * 1.3).clamp(6.0, 72.0),
        _ => 13.0,
    }
}

pub fn studio(prop: &str, or: f64) -> f64 {
    num("Studio", prop, or)
}

/// Studio Settings > Camera > Camera Speed: fly studs/s (mech::FLY_SPEED at 1.5).
pub fn fly_speed() -> f32 {
    crate::mech::FLY_BASE_STEP * studio("Camera Speed", crate::mech::CAMERA_SPEED as f64) as f32 * crate::mech::FLY_TICK_HZ
}

/// Camera > Shift Speed: the wheel's studs per notch with Shift, and the
/// fly speed factor with Shift.
pub fn camera_shift_speed() -> f32 {
    studio("Camera Shift Speed", crate::mech::FLY_SHIFT_FACTOR as f64) as f32
}

/// Camera > Scroll Speed: studs per wheel notch.
pub fn camera_scroll_speed() -> f32 {
    studio("Camera Mouse Wheel Speed", crate::mech::WHEEL_STEP as f64) as f32
}

/// Camera > Scroll Towards Cursor Position.
pub fn camera_zoom_to_mouse() -> bool {
    flag("Studio", "Camera Zoom to Mouse Position", crate::mech::WHEEL_ZOOM_TO_MOUSE)
}

/// Camera > Pan Speed (a factor, default 1).
pub fn camera_pan_speed() -> f32 {
    studio("Camera Pan Speed", 1.0).max(0.0) as f32
}

/// Script Editor > Theme: the colours the editor's highlighter uses.
pub struct ScriptColors {
    pub text: Color32,
    pub background: Color32,
    pub keyword: Color32,
    pub builtin: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub operator: Color32,
}

pub fn script_colors() -> ScriptColors {
    let s = store().read().unwrap();
    let c = |prop: &str| match index("Studio", prop).map(|i| &s.vals[i]) {
        Some(Val::Color(c)) => color32(*c),
        _ => Color32::GRAY,
    };
    ScriptColors {
        text: c("Text Color"),
        background: c("Background Color"),
        keyword: c("Keyword Color"),
        builtin: c("Built-in Function Color"),
        string: c("String Color"),
        number: c("Number Color"),
        comment: c("Comment Color"),
        operator: c("Operator Color"),
    }
}

/// Selection > Selection Line Thickness (2 = mech::SELECTION_OUTLINE_PX).
pub fn selection_outline_px() -> f32 {
    (crate::mech::SELECTION_OUTLINE_PX * studio("Selection Line Thickness", 2.0) as f32 / 2.0).clamp(0.0, 32.0)
}

/// Selection > Hover Line Thickness (4 = mech::HOVER_OUTLINE_PX).
pub fn hover_outline_px() -> f32 {
    (crate::mech::HOVER_OUTLINE_PX * studio("Hover Line Thickness", 4.0) as f32 / 4.0).clamp(0.0, 32.0)
}

/// Studio shows Color3 as floor(c × 255).
fn byte(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 1e-4).floor() as u8
}

fn color32(c: [f32; 3]) -> Color32 {
    Color32::from_rgb(byte(c[0]), byte(c[1]), byte(c[2]))
}

// ------------------------------------------------------------------ live values (Diagnostics)

/// Diagnostics' read-only rows: InstanceCount, PlayerCount, DataModel, JobCount.
static LIVE: RwLock<[i64; 4]> = RwLock::new([0, 0, 1, 0]);

pub fn set_live(v: [i64; 4]) {
    *LIVE.write().unwrap() = v;
}

pub fn live(prop: &str) -> Option<i64> {
    let l = *LIVE.read().unwrap();
    Some(match prop {
        "InstanceCount" => l[0],
        "PlayerCount" => l[1],
        "DataModel" => l[2],
        "JobCount" => l[3],
        _ => return None,
    })
}

pub fn version() -> String {
    format!("{} (rustudio)", env!("CARGO_PKG_VERSION"))
}

/// What a row shows in its value column.
pub fn shown(d: &Def, v: &Val) -> String {
    match (v, d.kind) {
        (_, Kind::ReadOnly) => match d.prop {
            "RobloxVersion" => version(),
            p => live(p).unwrap_or(0).to_string(),
        },
        (Val::Bool(b), _) => b.to_string(),
        (Val::Num(n), Kind::Int) => format!("{}", n.round() as i64),
        (Val::Num(n), _) => fmt_num(*n),
        (Val::Enum(e), Kind::Enum(_, items)) => {
            items.iter().find(|x| x.1 == *e).map(|x| x.2.to_string()).unwrap_or_else(|| e.to_string())
        }
        (Val::Enum(e), _) => e.to_string(),
        (Val::Color(c), _) => format!("[{}, {}, {}]", byte(c[0]), byte(c[1]), byte(c[2])),
        (Val::Str(s), _) => s.clone(),
        (Val::Font(f, pt), _) => format!("{f} : {}pt, Normal", fmt_num(*pt)),
    }
}

/// Qt's %g-like number: 1.5, 0.2, 15, 0.04.
fn fmt_num(n: f64) -> String {
    let s = format!("{:.6}", n);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// Parses what was typed into a row (None: keep the old value).
pub fn parse(d: &Def, text: &str) -> Option<Val> {
    let t = text.trim();
    Some(match d.kind {
        Kind::Int => Val::Num(t.parse::<f64>().ok().filter(|n| n.is_finite())?.round()),
        Kind::Float => Val::Num(t.parse::<f64>().ok().filter(|n| n.is_finite())?),
        Kind::Color => {
            let n: Vec<f32> = t
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .filter_map(|x| x.trim().parse::<f32>().ok())
                .collect();
            if n.len() != 3 {
                return None;
            }
            Val::Color([n[0] / 255.0, n[1] / 255.0, n[2] / 255.0].map(|x| x.clamp(0.0, 1.0)))
        }
        Kind::Str | Kind::Dir => Val::Str(text.to_string()),
        Kind::Font => {
            // "Family : 12pt, Normal" or just a size.
            let (fam, rest) = t.split_once(':').unwrap_or(("", t));
            let pt: f64 = rest.trim().trim_end_matches(", Normal").trim_end_matches("pt").trim().parse().ok()?;
            let fam = if fam.trim().is_empty() { "Fira Mono" } else { fam.trim() };
            Val::Font(fam.to_string(), pt.clamp(1.0, 96.0))
        }
        _ => return None,
    })
}

// ------------------------------------------------------------------ the dialog

/// Colours of Studio's dialog (sampled from checks/studio-settings/vinegar).
mod c {
    use egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(25, 26, 31);
    pub const LIST: Color32 = Color32::from_rgb(39, 41, 48);
    pub const LIST_SEL: Color32 = Color32::from_rgb(19, 35, 93);
    pub const LIST_HOVER: Color32 = Color32::from_rgb(45, 48, 58);
    pub const SEARCH: Color32 = Color32::from_rgb(40, 41, 49);
    pub const ROW: Color32 = Color32::from_rgb(32, 34, 39);
    pub const ROW_HOVER: Color32 = Color32::from_rgb(41, 43, 50);
    pub const BUTTON: Color32 = Color32::from_rgb(53, 55, 65);
    pub const TEXT: Color32 = Color32::from_rgb(222, 223, 226);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(110, 112, 118);
    pub const CHECK_ON: Color32 = Color32::from_rgb(247, 247, 248);
    pub const CHECK_OFF: Color32 = Color32::from_rgb(44, 46, 54);
    pub const CHECK_MARK: Color32 = Color32::from_rgb(32, 34, 39);
}

const ROW_H: f32 = 22.0;
const LIST_ROW_H: f32 = 30.0;
const FONT: f32 = 12.0;

#[derive(Default)]
pub struct Dialog {
    pub open: bool,
    pub page: usize,
    pub search: String,
    /// Collapsed categories (page, category).
    collapsed: std::collections::HashSet<(usize, &'static str)>,
    /// The row being typed into and its text.
    editing: Option<(usize, String)>,
    confirm_reset: bool,
    /// Fill the window (the comparison captures, sized like Vinegar's).
    pub fill: bool,
    /// The dialog's rect last frame (captures, tests).
    pub rect: Option<Rect>,
    /// Scroll the grid here once (captures: "Script Editor/700").
    scroll_to: Option<f32>,
    /// What was clickable last frame and where (the self-test clicks these).
    pub hits: Vec<(Hit, Rect)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Page(usize),
    /// A row's value cell (DEFS index).
    Value(usize),
    Close,
}

impl Dialog {
    /// Where `h` was drawn last frame.
    pub fn hit(&self, h: Hit) -> Option<Rect> {
        self.hits.iter().find(|(x, _)| *x == h).map(|(_, r)| *r)
    }
}

impl Dialog {
    /// Opens on `page` ("Physics"; "Studio/600": scrolled 600 px down;
    /// "Studio?speed": searching for "speed").
    pub fn open_at(&mut self, page: &str, fill: bool) {
        self.open = true;
        self.fill = fill;
        let (page, query) = page.split_once('?').unwrap_or((page, ""));
        self.search = query.to_string();
        let (page, scroll) = page.split_once('/').unwrap_or((page, ""));
        self.scroll_to = scroll.parse().ok();
        if let Some(i) = PAGES.iter().position(|p| p.eq_ignore_ascii_case(page)) {
            self.page = i;
        }
    }

    fn matches(&self, d: &Def) -> bool {
        let q = self.search.trim().to_lowercase();
        q.is_empty() || d.label.to_lowercase().contains(&q)
    }

    pub fn ui(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let screen = ctx.content_rect();
        let mut open = true;
        let mut win = egui::Window::new("Studio Settings")
            .id(egui::Id::new("studio_settings_dialog"))
            .collapsible(false)
            .frame(egui::Frame::window(&ctx.global_style()).fill(c::BG).inner_margin(egui::Margin::same(0)).corner_radius(0));
        if self.fill {
            win = win.title_bar(false).fixed_rect(screen);
        } else {
            win = win
                .open(&mut open)
                .resizable(true)
                .default_size(Vec2::new(900.0, 620.0).min(screen.size() - Vec2::splat(40.0)))
                .default_pos(screen.center() - Vec2::new(450.0, 310.0).min(screen.size() * 0.5 - Vec2::splat(20.0)));
        }
        let mut close = false;
        let resp = win.show(ctx, |ui| close = self.body(ui));
        self.rect = resp.map(|r| r.response.rect);
        if !open || close {
            self.open = false;
            self.editing = None;
            flush();
        }
    }

    /// The search bar, page list, grid and buttons; true when Close was pressed.
    fn body(&mut self, ui: &mut egui::Ui) -> bool {
        self.hits.clear();
        let full = ui.available_rect_before_wrap();
        let full = if full.height() < 200.0 { Rect::from_min_size(full.min, Vec2::new(full.width(), 560.0)) } else { full };
        ui.allocate_rect(full, Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(full, 0.0, c::BG);

        // Search bar (Studio: 26 px, the magnifier, "Search...").
        let sr = Rect::from_min_size(full.min + Vec2::new(0.0, 10.0), Vec2::new(full.width(), 26.0));
        p.rect_filled(sr, 0.0, c::SEARCH);
        crate::ti::paint(&p, Rect::from_center_size(Pos2::new(sr.min.x + 15.0, sr.center().y), Vec2::splat(16.0)), "search", c::TEXT_DIM);
        let te = Rect::from_min_max(Pos2::new(sr.min.x + 28.0, sr.min.y + 3.0), Pos2::new(sr.max.x - 26.0, sr.max.y - 3.0));
        let mut search = self.search.clone();
        ui.put(
            te,
            egui::TextEdit::singleline(&mut search)
                .id(egui::Id::new("studio_settings_search"))
                .hint_text(egui::RichText::new("Search...").color(c::TEXT_DIM))
                .frame(egui::Frame::NONE)
                .font(FontId::proportional(FONT))
                .text_color(c::TEXT),
        );
        if !search.is_empty() {
            let xr = Rect::from_center_size(Pos2::new(sr.max.x - 14.0, sr.center().y), Vec2::splat(14.0));
            let x = ui.interact(xr, egui::Id::new("studio_settings_search_x"), Sense::click());
            crate::ti::paint(&p, xr, "x", if x.hovered() { c::TEXT } else { c::TEXT_DIM });
            if x.clicked() {
                search.clear();
            }
        }
        if search != self.search {
            self.search = search;
            self.editing = None;
        }

        // Pages that have a matching row; the shown page stays one of them.
        let visible: Vec<usize> = (0..PAGES.len())
            .filter(|&i| DEFS.iter().any(|d| d.page == PAGES[i] && self.matches(d)))
            .collect();
        if !visible.contains(&self.page) {
            if let Some(&f) = visible.first() {
                self.page = f;
            }
        }

        let top = sr.max.y + 6.0;
        let bottom = full.max.y - 44.0;
        let list_w = (full.width() / 3.0).round();
        let list = Rect::from_min_max(Pos2::new(full.min.x, top), Pos2::new(full.min.x + list_w, bottom));
        p.rect_filled(list, 0.0, c::LIST);
        for (n, &i) in visible.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(list.min.x, list.min.y + n as f32 * LIST_ROW_H), Vec2::new(list_w, LIST_ROW_H));
            let resp = ui.interact(r, egui::Id::new(("studio_settings_page", i)), Sense::click());
            self.hits.push((Hit::Page(i), r));
            if i == self.page {
                p.rect_filled(r, 0.0, c::LIST_SEL);
            } else if resp.hovered() {
                p.rect_filled(r, 0.0, c::LIST_HOVER);
            }
            p.text(Pos2::new(r.min.x + 4.0, r.center().y), egui::Align2::LEFT_CENTER, PAGES[i], FontId::proportional(FONT), c::TEXT);
            if resp.clicked() && i != self.page {
                self.page = i;
                self.editing = None;
            }
        }

        let grid = Rect::from_min_max(Pos2::new(list.max.x + 5.0, top), Pos2::new(full.max.x - 2.0, bottom));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(grid).layout(egui::Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(grid);
        let mut area = egui::ScrollArea::vertical()
            .id_salt(("studio_settings_grid", self.page, self.search.clone()))
            .auto_shrink([false, false]);
        if let Some(y) = self.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }
        area.show(&mut child, |ui| self.grid(ui));

        // Buttons.
        let mut close = false;
        let font = FontId::proportional(FONT);
        let button = |ui: &mut egui::Ui, r: Rect, id: &str, text: &str| {
            let resp = ui.interact(r, egui::Id::new(id), Sense::click());
            let fill = if resp.hovered() { c::LIST_HOVER.gamma_multiply(1.2) } else { c::BUTTON };
            ui.painter().rect_filled(r, 0.0, fill);
            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, font.clone(), c::TEXT);
            resp.clicked()
        };
        let by = full.max.y - 22.0;
        let rw = ui.fonts_mut(|f| f.layout_no_wrap("Reset all settings".into(), font.clone(), c::TEXT).size().x) + 10.0;
        if button(ui, Rect::from_center_size(Pos2::new(full.min.x + 4.0 + rw / 2.0, by), Vec2::new(rw, 24.0)), "studio_settings_reset", "Reset all settings") {
            self.confirm_reset = true;
        }
        let close_r = Rect::from_center_size(Pos2::new(full.max.x - 4.0 - 37.0, by), Vec2::new(74.0, 24.0));
        self.hits.push((Hit::Close, close_r));
        if button(ui, close_r, "studio_settings_close", "Close") {
            close = true;
        }
        if self.confirm_reset {
            self.reset_confirm(ui.ctx());
        }
        close
    }

    fn reset_confirm(&mut self, ctx: &egui::Context) {
        egui::Modal::new(egui::Id::new("studio_settings_reset_confirm")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.label(egui::RichText::new("Reset Settings").strong());
            ui.add_space(6.0);
            ui.label("Reset every Studio setting to its default?");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Reset All Settings").clicked() {
                    reset_all();
                    self.editing = None;
                    self.confirm_reset = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_reset = false;
                }
            });
        });
    }

    /// The page's property grid.
    fn grid(&mut self, ui: &mut egui::Ui) {
        let page = PAGES[self.page];
        let rows: Vec<usize> = (0..DEFS.len()).filter(|&i| DEFS[i].page == page && self.matches(&DEFS[i])).collect();
        let w = ui.available_width();
        let font = FontId::proportional(FONT);
        // The value column starts after the page's widest label (Qt sizes
        // the first column to its contents).
        let label_w = rows
            .iter()
            .map(|&i| ui.fonts_mut(|f| f.layout_no_wrap(DEFS[i].label.to_string(), font.clone(), c::TEXT).size().x))
            .fold(0.0f32, f32::max);
        let col = 22.0 + label_w + 8.0;
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let mut cat = "";
        for &i in &rows {
            let d = &DEFS[i];
            if d.cat != cat {
                cat = d.cat;
                let (r, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
                let p = ui.painter();
                let open = !self.collapsed.contains(&(self.page, cat));
                let tri = Rect::from_center_size(Pos2::new(r.min.x + 9.0, r.center().y), Vec2::splat(12.0));
                crate::ti::paint(p, tri, if open { "caret-down-filled" } else { "caret-right-filled" }, c::TEXT);
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    cat,
                    0.0,
                    egui::TextFormat {
                        font_id: FontId::new(FONT, egui::FontFamily::Name("bold".into())),
                        color: c::TEXT,
                        extra_letter_spacing: 1.5,
                        ..Default::default()
                    },
                );
                let g = ui.fonts_mut(|f| f.layout_job(job));
                p.galley(Pos2::new(r.min.x + 22.0, r.center().y - g.size().y / 2.0), g, c::TEXT);
                if resp.clicked() {
                    if open {
                        self.collapsed.insert((self.page, cat));
                    } else {
                        self.collapsed.remove(&(self.page, cat));
                    }
                }
            }
            if self.collapsed.contains(&(self.page, d.cat)) {
                continue;
            }
            self.row(ui, i, w, col);
        }
    }

    fn row(&mut self, ui: &mut egui::Ui, i: usize, w: f32, col: f32) {
        let d = &DEFS[i];
        let (r, resp) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
        let inner = Rect::from_min_max(r.min, Pos2::new(r.max.x, r.max.y - 1.0));
        let p = ui.painter().clone();
        p.rect_filled(inner, 0.0, if resp.hovered() { c::ROW_HOVER } else { c::ROW });
        // The column divider.
        p.vline(r.min.x + col - 4.0, inner.y_range(), egui::Stroke::new(1.0, c::BG));
        let ro = matches!(d.kind, Kind::ReadOnly);
        let tc = if ro { c::TEXT_DIM } else { c::TEXT };
        let font = FontId::proportional(FONT);
        p.text(Pos2::new(r.min.x + 22.0, r.center().y), egui::Align2::LEFT_CENTER, d.label, font.clone(), tc);
        let v = store().read().unwrap().vals[i].clone();
        let vx = r.min.x + col;
        let vr = Rect::from_min_max(Pos2::new(vx, inner.min.y), inner.max);
        let vresp = ui.interact(vr, egui::Id::new(("studio_settings_value", i)), Sense::click());
        if ui.clip_rect().intersects(vr) {
            self.hits.push((Hit::Value(i), vr.intersect(ui.clip_rect())));
        }
        match (d.kind, &v) {
            (Kind::Bool, Val::Bool(b)) => {
                let br = Rect::from_center_size(Pos2::new(vx + 10.0, r.center().y), Vec2::splat(15.0));
                if *b {
                    p.rect_filled(br, 2.0, c::CHECK_ON);
                    crate::ti::paint(&p, br.shrink(1.0), "check", c::CHECK_MARK);
                } else {
                    p.rect_filled(br, 2.0, c::CHECK_OFF);
                }
                if vresp.clicked() {
                    set(d.class, d.prop, Val::Bool(!b));
                }
            }
            (Kind::Enum(_, items), Val::Enum(e)) => {
                p.text(Pos2::new(vx + 3.0, r.center().y), egui::Align2::LEFT_CENTER, shown(d, &v), font.clone(), tc);
                let id = egui::Id::new(("studio_settings_enum", i));
                if vresp.clicked() {
                    egui::Popup::open_id(ui.ctx(), id);
                }
                let width = vr.width().clamp(140.0, 320.0);
                let mut picked = None;
                egui::Popup::new(id, ui.ctx().clone(), egui::PopupAnchor::Position(vr.left_bottom()), ui.layer_id())
                    .open_memory(None)
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
                    .width(width)
                    .frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(2)).corner_radius(0))
                    .show(|ui| {
                        ui.set_width(width - 4.0);
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        egui::ScrollArea::vertical().max_height(19.0 * 12.0).auto_shrink([false, true]).show(ui, |ui| {
                            for (_, n, s) in items.iter() {
                                let (ir, iresp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 19.0), Sense::click());
                                if *n == *e || iresp.hovered() {
                                    ui.painter().rect_filled(ir, 0.0, if *n == *e { c::LIST_SEL } else { c::LIST_HOVER });
                                }
                                ui.painter().text(Pos2::new(ir.min.x + 6.0, ir.center().y), egui::Align2::LEFT_CENTER, *s, FontId::proportional(FONT), c::TEXT);
                                if iresp.clicked() {
                                    picked = Some(*n);
                                }
                            }
                        });
                    });
                if let Some(n) = picked {
                    set(d.class, d.prop, Val::Enum(n));
                }
            }
            (Kind::ReadOnly, _) => {
                p.text(Pos2::new(vx + 3.0, r.center().y), egui::Align2::LEFT_CENTER, shown(d, &v), font.clone(), c::TEXT_DIM);
            }
            _ => {
                // Typed values: the text, or a line edit while editing.
                let mut tx = vx + 3.0;
                if let Val::Color(cv) = &v {
                    let sw = Rect::from_center_size(Pos2::new(vx + 8.0, r.center().y), Vec2::splat(10.0));
                    p.rect_filled(sw, 0.0, color32(*cv));
                    tx = vx + 19.0;
                }
                let editing = self.editing.as_ref().is_some_and(|(e, _)| *e == i);
                if editing {
                    let er = Rect::from_min_max(Pos2::new(tx - 2.0, inner.min.y + 1.0), Pos2::new(inner.max.x - 2.0, inner.max.y - 1.0));
                    let mut text = self.editing.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
                    let id = egui::Id::new(("studio_settings_edit", i));
                    let te = ui.put(
                        er,
                        egui::TextEdit::singleline(&mut text).id(id).font(font.clone()).margin(egui::Margin::symmetric(2, 1)),
                    );
                    if !te.has_focus() && !te.lost_focus() {
                        te.request_focus();
                    }
                    let enter = ui.input(|inp| inp.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|inp| inp.key_pressed(egui::Key::Escape));
                    if esc {
                        self.editing = None;
                    } else if te.lost_focus() || enter {
                        if let Some(nv) = parse(d, &text) {
                            set(d.class, d.prop, nv);
                        }
                        self.editing = None;
                    } else {
                        self.editing = Some((i, text));
                    }
                } else {
                    p.text(Pos2::new(tx, r.center().y), egui::Align2::LEFT_CENTER, shown(d, &v), font.clone(), tc);
                    if vresp.clicked() {
                        let start = match &v {
                            Val::Num(_) | Val::Color(_) | Val::Font(..) => shown(d, &v),
                            Val::Str(s) => s.clone(),
                            _ => String::new(),
                        };
                        self.editing = Some((i, start));
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ Luau (settings())

/// The settings() items the dialog has rows for (their members go through
/// the store; GameSettings, LuaSettings and TaskScheduler stay as they were).
pub fn is_item_class(class: &str) -> bool {
    matches!(class, "Studio" | "DebugSettings" | "NetworkSettings" | "PhysicsSettings" | "RenderSettings")
}

/// Reads settings().<item>.<member> for a row in the store: None for
/// members the dialog doesn't have (the generic path answers those).
pub fn lua_get(lua: &mlua::Lua, class: &str, key: &str) -> mlua::Result<Option<mlua::Value>> {
    use mlua::Value;
    let Some(i) = index(class, key) else { return Ok(None) };
    let d = &DEFS[i];
    let v = store().read().unwrap().vals[i].clone();
    let rt_err = |m: String| mlua::Error::RuntimeError(m);
    Ok(Some(match (d.kind, v) {
        (Kind::ReadOnly, _) => match key {
            "RobloxVersion" => Value::String(lua.create_string(version())?),
            k => Value::Integer(live(k).unwrap_or(0)),
        },
        // Qt types have no Luau form (Studio: "Unable to get property Font, type QFont").
        (Kind::Font, _) => return Err(rt_err(format!("Unable to get property {key}, type QFont"))),
        (Kind::Dir, _) => return Err(rt_err(format!("Unable to get property {key}, type QDir"))),
        (Kind::Bool, Val::Bool(b)) => Value::Boolean(b),
        (Kind::Int, Val::Num(n)) => Value::Integer(n.round() as i64),
        (Kind::Float, Val::Num(n)) => Value::Number(n),
        // Theme is a StudioTheme object and DisplayLanguage a string in
        // Studio; rustudio has neither object, so they're left unanswered.
        (Kind::Enum(en, _), Val::Enum(_)) if en == "StudioTheme" || en == "Language" => return Ok(None),
        (Kind::Enum(en, _), Val::Enum(e)) => match crate::ltypes::LEnumItem::from_value(en, e) {
            Some(item) => crate::ltypes::enum_value(lua, item)?,
            None => return Ok(None),
        },
        (Kind::Color, Val::Color(c)) => Value::UserData(lua.create_userdata(crate::ltypes::LColor3(c))?),
        (Kind::Str, Val::Str(s)) => Value::String(lua.create_string(s)?),
        _ => return Ok(None),
    }))
}

/// settings().<item>.<member> = value: Ok(true) when the store took it.
pub fn lua_set(class: &str, key: &str, value: &mlua::Value) -> mlua::Result<Option<bool>> {
    use mlua::Value;
    let Some(i) = index(class, key) else { return Ok(None) };
    let d = &DEFS[i];
    let bad = |want: &str| {
        mlua::Error::RuntimeError(format!("Unable to assign property {key}. {want} expected, got {}", value.type_name()))
    };
    let num = |v: &Value| -> Option<f64> {
        match v {
            Value::Integer(n) => Some(*n as f64),
            Value::Number(n) => Some(*n),
            // Studio converts a string; one that isn't a number reads as 0.
            Value::String(s) => Some(s.to_str().ok()?.trim().parse::<f64>().unwrap_or(0.0)),
            _ => None,
        }
    };
    let nv = match d.kind {
        Kind::ReadOnly => {
            return Err(mlua::Error::RuntimeError(format!("Unable to assign property {key}. Property is read only")));
        }
        Kind::Font | Kind::Dir => return Ok(None),
        Kind::Bool => match value {
            Value::Boolean(b) => Val::Bool(*b),
            Value::Nil => Val::Bool(false),
            _ => return Err(bad("bool")),
        },
        Kind::Int => Val::Num(num(value).ok_or_else(|| bad("int"))?.trunc()),
        Kind::Float => Val::Num(num(value).ok_or_else(|| bad("float"))?),
        Kind::Str => match value {
            Value::String(s) => Val::Str(s.to_str()?.to_string()),
            Value::Integer(_) | Value::Number(_) => Val::Str(num(value).map(|n| n.to_string()).unwrap_or_default()),
            _ => return Err(bad("string")),
        },
        Kind::Enum(en, items) => {
            if en == "StudioTheme" || en == "Language" {
                return Ok(None);
            }
            let e = enum_from_lua(value, en, items).ok_or_else(|| bad(&format!("Enum.{en}")))?;
            if !items.iter().any(|x| x.1 == e) {
                return Err(bad(&format!("Enum.{en}")));
            }
            Val::Enum(e)
        }
        Kind::Color => match value {
            Value::UserData(u) if u.borrow::<crate::ltypes::LColor3>().is_ok() => Val::Color(u.borrow::<crate::ltypes::LColor3>()?.0),
            _ => return Err(bad("Color3")),
        },
    };
    set(class, key, nv);
    Ok(Some(true))
}

/// An EnumItem of `en`, its number, or an item's name.
fn enum_from_lua(v: &mlua::Value, en: &str, items: &[(&str, u32, &str)]) -> Option<u32> {
    match v {
        mlua::Value::UserData(u) => {
            let e = u.borrow::<crate::ltypes::LEnumItem>().ok()?;
            (e.enum_type == en).then_some(e.value)
        }
        mlua::Value::Integer(n) => u32::try_from(*n).ok(),
        mlua::Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 => Some(*n as u32),
        mlua::Value::String(s) => {
            let s = s.to_str().ok()?;
            items.iter().find(|x| x.0 == &*s).map(|x| x.1)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_consistent() {
        // One row per member; every page listed; the dialog's counts.
        let mut seen = std::collections::HashSet::new();
        for d in DEFS {
            assert!(seen.insert((d.class, d.prop)), "{}.{} twice", d.class, d.prop);
            assert!(PAGES.contains(&d.page), "{}", d.page);
            assert!(is_item_class(d.class), "{}", d.class);
        }
        let count = |p: &str| DEFS.iter().filter(|d| d.page == p).count();
        assert_eq!(count("Selection"), 25);
        assert_eq!(count("Diagnostics"), 8);
        assert_eq!(count("Network"), 13);
        assert_eq!(count("Rendering"), 11);
        // Studio's defaults as Vinegar answered them.
        assert_eq!(DEFS[index("Studio", "Camera Speed").unwrap()].default_val(), Val::Num(1.5));
        assert_eq!(shown(&DEFS[index("Studio", "Select Color").unwrap()], &DEFS[index("Studio", "Select Color").unwrap()].default_val()), "[25, 153, 255]");
        assert_eq!(shown(&DEFS[index("Studio", "Hover Over Color").unwrap()], &DEFS[index("Studio", "Hover Over Color").unwrap()].default_val()), "[178, 229, 255]");
        let d = &DEFS[index("Studio", "Output Font").unwrap()];
        assert_eq!(shown(d, &d.default_val()), "Fira Mono : 10pt, Normal");
        let d = &DEFS[index("RenderSettings", "EditQualityLevel").unwrap()];
        assert_eq!(shown(d, &d.default_val()), "Level 21");
        let d = &DEFS[index("PhysicsSettings", "PhysicsEnvironmentalThrottle").unwrap()];
        assert_eq!(shown(d, &d.default_val()), "Default Auto");
        let d = &DEFS[index("Studio", "Line Thickness").unwrap()];
        assert_eq!(shown(d, &d.default_val()), "0.04");
    }

    #[test]
    fn json_round_trip() {
        for d in DEFS {
            if matches!(d.kind, Kind::ReadOnly) {
                continue;
            }
            let v = d.default_val();
            assert_eq!(from_json(d, &to_json(d, &v)).as_ref(), Some(&v), "{}", d.label);
        }
    }

    /// settings() as Studio has it (Vinegar 0.747, checks/studio-settings/
    /// inventory.md): Instances, PluginSecurity members readable from the
    /// command bar, RobloxScript ones refused, writes reaching the store and
    /// firing Changed, read-only and QFont members refused, game scripts
    /// kept out of settings() altogether.
    #[test]
    fn luau_settings_reads_and_writes_the_store() {
        use crate::runtime::security::{with_main_identity, COMMAND, GAME};
        let rt = crate::runtime::Runtime::new().unwrap();
        let mut w = crate::runtime::World::new(crate::doc::Document::new_place(), crate::runtime::RunMode::Run);
        w.side = crate::runtime::Side::Server;
        rt.enter(w);
        rt.bind_globals().unwrap();
        let run = |id: u8, code: &str| -> String {
            with_main_identity(&rt.lua, id, || rt.lua.load(code).set_name("=harness").eval::<String>())
                .unwrap_or_else(|e| format!("ERR {e}"))
        };
        let out = run(
            COMMAND,
            r#"local s = settings()
            local kids = {}
            for _, c in s:GetChildren() do table.insert(kids, c.ClassName .. "=" .. c.Name) end
            return table.concat({s.Name, s.ClassName, s.Studio.ClassName, s:GetService("PhysicsSettings").Name,
                table.concat(kids, ","), tostring(s.Studio["Camera Speed"]), tostring(s.Studio["Select Color"]),
                tostring(s.Rendering.EditQualityLevel), tostring(s.Physics.AllowSleep),
                select(2, pcall(function() return s.Studio["Camera Pan Speed"] end)),
                select(2, pcall(function() return s.Studio.Font end)),
                select(2, pcall(function() s.Diagnostics.InstanceCount = 3 end))}, " | ")"#,
        );
        let parts: Vec<&str> = out.split(" | ").collect();
        assert_eq!(parts.len(), 12, "{out}");
        assert_eq!(&parts[..4], ["Global Settings", "GlobalSettings", "Studio", "Physics"], "{out}");
        assert_eq!(
            parts[4],
            "GameSettings=Game Options,LuaSettings=Lua,DebugSettings=Diagnostics,PhysicsSettings=Physics,\
             Studio=Studio,NetworkSettings=Network,RenderSettings=Rendering,TaskScheduler=Task Scheduler"
        );
        assert_eq!(parts[5], "1.5", "{out}");
        assert!(parts[6].starts_with("0.1, 0.6"), "{out}");
        assert_eq!(parts[7], "Enum.QualityLevel.Level21", "{out}");
        assert_eq!(parts[8], "true", "{out}");
        assert!(parts[9].contains("lacking capability RobloxScript"), "{out}");
        assert!(parts[10].contains("Unable to get property Font, type QFont"), "{out}");
        assert!(parts[11].contains("read only"), "{out}");

        // Writes reach the store (rows nothing else in the tests reads) and
        // fire Changed; the store's value comes back.
        let out = run(
            COMMAND,
            r#"local st = settings().Studio
            SEEN = {}
            st.Changed:Connect(function(p) table.insert(SEEN, p) end)
            st["Tab Width"] = 2
            st["Show Whitespace"] = true
            st["Whitespace Color"] = Color3.fromRGB(255, 0, 0)
            st["Auto Indent Rule"] = Enum.AutoIndentRule.Relative
            local bad = select(2, pcall(function() st["Auto Indent Rule"] = Enum.QualityLevel.Level01 end))
            return table.concat({st["Tab Width"], tostring(st["Show Whitespace"]), tostring(st["Whitespace Color"]),
                tostring(st["Auto Indent Rule"]), tostring(bad)}, " | ")"#,
        );
        assert_eq!(get("Studio", "Tab Width"), Some(Val::Num(2.0)), "{out}");
        assert_eq!(get("Studio", "Show Whitespace"), Some(Val::Bool(true)), "{out}");
        assert_eq!(get("Studio", "Whitespace Color"), Some(Val::Color([1.0, 0.0, 0.0])), "{out}");
        assert_eq!(get("Studio", "Auto Indent Rule"), Some(Val::Enum(2)), "{out}");
        assert!(out.starts_with("2 | true | 1, 0, 0 | Enum.AutoIndentRule.Relative | "), "{out}");
        for _ in 0..3 {
            rt.step(1.0 / 60.0);
        }
        let seen = run(COMMAND, "return table.concat(SEEN, \",\")");
        assert_eq!(seen, "Tab Width,Show Whitespace,Whitespace Color,Auto Indent Rule");
        for (p, d) in [("Tab Width", Val::Num(4.0)), ("Show Whitespace", Val::Bool(false)), ("Auto Indent Rule", Val::Enum(1))] {
            set("Studio", p, d);
        }
        let i = index("Studio", "Whitespace Color").unwrap();
        set("Studio", "Whitespace Color", DEFS[i].default_val());

        // Game scripts can't reach settings() at all.
        let out = run(GAME, "return tostring(select(2, pcall(settings)))");
        assert!(out.contains("lacking capability Plugin"), "{out}");
    }

    #[test]
    fn parses_typed_values() {
        let d = &DEFS[index("Studio", "Select Color").unwrap()];
        assert_eq!(parse(d, "[255, 0, 51]"), Some(Val::Color([1.0, 0.0, 0.2])));
        assert_eq!(parse(d, "nope"), None);
        let d = &DEFS[index("Studio", "Font").unwrap()];
        assert_eq!(parse(d, "Fira Mono : 14pt, Normal"), Some(Val::Font("Fira Mono".into(), 14.0)));
        let d = &DEFS[index("Studio", "Tab Width").unwrap()];
        assert_eq!(parse(d, " 2 "), Some(Val::Num(2.0)));
    }
}
