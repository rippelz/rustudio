//! The editor's side of plugin GUIs: DockWidgetPluginGui panels, what
//! plugins put in CoreGui over the edit viewport, and the edit-mode input
//! plugins hear (their GUI events, UserInputService, the active plugin's
//! PluginMouse, handle adornments, StartDrag).
//!
//! **Panels.** Each widget is a dock tab (`Tab::Plugin(n)`) in the same
//! docking system as Explorer and Properties, keyed `plugin:edit_<plugin
//! name>_<widget id>` as Studio keys its dock layout (measured in Studio,
//! F61), so a reloaded plugin's widget comes back in its slot. The tab is
//! open while the widget's `Enabled` is true; closing it with its × calls
//! the plugin's BindToClose callback or sets `Enabled` false (which fires
//! the changed signal). It opens where Studio docks its InitialDockState
//! (`panels::plugin_placement`), a Float in its own window at
//! FloatingXSize × FloatingYSize less Studio's frame (2 × 24 px, F47).
//!
//! **Drawing.** Each panel has its own `gui::Gui` drawing the widget as a
//! LayerCollector sized to the panel (`gui::Roots::Widget`), with the same
//! renderer, caches and memos as the game's GUIs (redrawn only when the
//! document marks touch it). CoreGui has one more `Gui` over the edit
//! viewport (`Roots::CoreGui`: ScreenGuis, BillboardGuis / SurfaceGuis,
//! handle adornments under CoreGui or Workspace). One journal reader
//! (`doc::Reader::PluginGui`) feeds all of them.
//!
//! **Input.** The GUIs queue events as the game's do; with the viewport's
//! pointer, buttons, wheel and keys they are fired in the edit VM at the
//! start of the next plugin tick (`plugin_ui_flush`), on the plugins' GUI
//! channel (`gui::share_channel(1)`), so TextBox focus, hover state,
//! handle hits and drags never mix with a Play session's.
//!
//! **Restore.** Studio saves the dock layout at a graceful exit and, for a
//! widget that was docked, restores it next session: HostWidgetWasRestored
//! true and Enabled = the saved state unless InitialEnabledShouldOverride
//! Restore (F54). Here: `plugin_widgets.json` beside layout.json (plain
//! interactive runs), or `RUSTUDIO_PLUGIN_LAYOUT=FILE` (tests), written at
//! exit.
//!
//! **Play.** Studio loads every plugin again in the Play server and client
//! DataModels, and their copies make their own
//! widgets. Each widget's panel belongs to its DataModel (`Slot.at`): as
//! the Plugins tab shows the toolbars of the DataModel in front (measured,
//! checks/plugins/ui/README.md U13), the dock shows the widgets of the
//! DataModel in front: the edit DataModel's while editing, the shown
//! side's during Play (Client / Server toggles them), the edit widgets
//! again at Stop, with their Enabled untouched (hiding a panel is not its
//! ×). Studio keys its dock layout per DataModel (`edit_…`, F54, F61); the
//! Play keys here are `PlayServer_<plugin>_<id>` / `PlayClient_<plugin>_<id>`
//! after the loader's DataModel names (Studio's own Play keys and whether
//! it shows Play widgets this way are unmeasured: Studio tests). Play
//! widgets are not restored across sessions. Their input fires in their
//! own VM: the client's on GUI channel 0 (its DataModel's one focused
//! TextBox, stepped by the game), the server's on channel 2 (stepped here).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use egui::{Pos2, Rect, Sense};
use mlua::Value;
use rbx_dom_weak::types::{Ref, Variant};
use serde::{Deserialize, Serialize};

use crate::app::{Editor, Tab};
use crate::game::Game;
use rustudio_core::doc::Document;
use rustudio_engine::gui::{Gui, GuiBox, GuiEvent, Roots};

use super::ui_ribbon::Where;
use super::{PluginId, UiKind};
use crate::plugin_host::api::widgets as W;
use rustudio_engine::runtime::Side;

pub const KEY_PREFIX: &str = "plugin:";

/// A DataModel's prefix in widget keys (`edit_<plugin>_<id>` as Studio's
/// dock layout; the Play ones are rustudio's, after the loader's names).
pub fn context_prefix(at: Where) -> &'static str {
    match at {
        Where::Edit => "edit",
        Where::Play(Side::Server) => "PlayServer",
        Where::Play(Side::Client) => "PlayClient",
    }
}

/// The GUI channel (`gui::share_channel`) a context's widgets use.
fn channel(at: Where) -> u8 {
    match at {
        Where::Edit => 1,
        Where::Play(Side::Client) => 0,
        Where::Play(Side::Server) => 2,
    }
}

fn side_ix(side: Side) -> usize {
    match side {
        Side::Server => 0,
        Side::Client => 1,
    }
}

/// Play plugin widgets' boxes in the Play worlds' `gui_layout` (the
/// client's game GUI replaces that map when its layout changes: these
/// stay).
static PLAY_LAID: Mutex<Option<HashSet<Ref>>> = Mutex::new(None);

fn play_laid() -> std::sync::MutexGuard<'static, Option<HashSet<Ref>>> {
    PLAY_LAID.lock().unwrap_or_else(|e| e.into_inner())
}

/// A Play copy's new widget has a box before its panel is laid out
/// (api_widgets.rs).
pub fn note_play_laid(r: Ref) {
    play_laid().get_or_insert_with(HashSet::new).insert(r);
}

/// The client's game GUI laid out anew (`new`): the plugin widgets' boxes
/// of the last layout (`old`) stay in it.
pub fn keep_play_layout(old: &HashMap<Ref, GuiBox>, new: &mut HashMap<Ref, GuiBox>) {
    let g = play_laid();
    let Some(set) = g.as_ref() else { return };
    for r in set {
        if let Some(b) = old.get(r) {
            new.entry(*r).or_insert(*b);
        }
    }
}

// ------------------------------------------------------------ registry
// Global, so panels.rs (placement, window titles) and the VM side of the
// API (api_widgets.rs: restore, initial size, relative mouse) can read it.

struct RegSlot {
    key: &'static str,
    state: String,
    title: String,
    float: [f32; 2],
}

#[derive(Default)]
struct Registry {
    slots: Vec<RegSlot>,
    /// Restore state from the last session, by `edit_<plugin>_<id>`.
    saved: Option<HashMap<String, Saved>>,
    /// What this session will save.
    now: HashMap<String, Saved>,
    /// Each drawn widget's live state, by its instance.
    live: HashMap<Ref, Live>,
    /// Content size of the last drawn widget per dock state (a new
    /// widget's AbsoluteSize before it is laid out).
    dock_sizes: HashMap<String, [f32; 2]>,
    /// The edit viewport's size.
    view: [f32; 2],
    /// The main window's dock area (a Float window stays inside it).
    dock_area: egui::Vec2,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Live {
    /// The mouse in widget pixels (None: not over it).
    pub rel: Option<[f32; 2]>,
    pub size: [f32; 2],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct Saved {
    enabled: bool,
    /// Docked in the main window (Studio restores only those).
    docked: bool,
}

fn reg() -> std::sync::MutexGuard<'static, Registry> {
    static R: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// `Tab::Plugin(n)`'s key (`plugin:edit_<plugin>_<id>`).
pub fn slot_key(n: u32) -> &'static str {
    reg().slots.get(n as usize).map(|s| s.key).unwrap_or("plugin:")
}

/// A plugin panel key's static form and InitialDockState name.
pub fn slot_placement(key: &str) -> Option<(&'static str, String)> {
    if !key.starts_with(KEY_PREFIX) {
        return None;
    }
    let r = reg();
    r.slots.iter().find(|s| s.key == key).map(|s| (s.key, s.state.clone()))
}

/// A panel window's title when it holds a plugin widget (its key starts
/// with the widget's key: `free_key` may add `-2`).
pub fn window_title(win_key: &str) -> Option<String> {
    if !win_key.starts_with(KEY_PREFIX) {
        return None;
    }
    let r = reg();
    r.slots
        .iter()
        .filter(|s| win_key.starts_with(s.key))
        .max_by_key(|s| s.key.len())
        .map(|s| if s.title.is_empty() { " ".to_string() } else { s.title.clone() })
}

/// Studio's client area for a Float widget's FloatingXSize × FloatingYSize:
/// the window less its frame (2 × 24 px: 320×240 → 318×216, F47), a
/// 250×350 window for sizes of 0 (DataStore Editor: 0, 0 → 248×326), and
/// never under 100 px high (Time+: 200×100 → 198×100; corpus records).
pub fn float_client(fx: f32, fy: f32) -> [f32; 2] {
    let fx = if fx <= 0.0 { 250.0 } else { fx };
    let fy = if fy <= 0.0 { 350.0 } else { fy };
    [(fx - 2.0).max(1.0), (fy - 24.0).max(100.0)]
}

/// A panel window's frame around its content: the dock's tab bar and the
/// window's margin (theme::dock_style), logical pixels.
const WINDOW_FRAME: [f32; 2] = [22.0, 46.0];

/// A Float widget's window size, so its content is Studio's client area:
/// FloatingXSize − 2 × FloatingYSize − 24 (F47, F53: 318×216 for 320×240).
pub fn set_dock_area(r: Rect) {
    reg().dock_area = r.size();
}

pub fn float_size(tab: &Tab) -> Option<[f32; 2]> {
    let Tab::Plugin(n) = tab else { return None };
    reg()
        .slots
        .get(*n as usize)
        .filter(|s| s.state == "Float")
        .map(|s| {
            let c = float_client(s.float[0], s.float[1]);
            [c[0] + WINDOW_FRAME[0], c[1] + WINDOW_FRAME[1]]
        })
}

/// The mouse over widget `r` in its pixels (0, 0 when not over it, as
/// Studio's GetRelativeMousePosition with no mouse: F47) and its size.
pub fn live(r: Ref) -> Live {
    reg().live.get(&r).copied().unwrap_or_default()
}

/// What Studio restores for widget `key` (`edit_<plugin>_<id>`): Some(the
/// saved Enabled) when a docked panel of it was saved last session.
pub fn restored(key: &str) -> Option<bool> {
    let mut r = reg();
    if r.saved.is_none() {
        r.saved = Some(load_saved());
    }
    r.saved.as_ref().and_then(|m| m.get(key)).filter(|s| s.docked).map(|s| s.enabled)
}

/// A new widget's content size before its panel is laid out: a Float's
/// window less Studio's frame (F47: 320×240 → 318×216); docked, the size
/// of the last widget drawn with the same dock state (or a share of the
/// view), as Studio reports a pre-layout size that changes once after.
pub fn initial_size(state: &str, fx: f64, fy: f64) -> [f32; 2] {
    if state == "Float" {
        return float_client(fx as f32, fy as f32);
    }
    let r = reg();
    if let Some(s) = r.dock_sizes.get(state) {
        return *s;
    }
    let v = if r.view[0] > 0.0 { r.view } else { [1265.0, 836.0] };
    match state {
        "Left" => [278.0, 265.0],
        "Right" => [174.0, 287.0],
        "Top" => [v[0], 150.0],
        _ => [v[0] + 466.0, 151.0],
    }
}

fn layout_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RUSTUDIO_PLUGIN_LAYOUT") {
        return Some(PathBuf::from(p));
    }
    None
}

fn load_saved() -> HashMap<String, Saved> {
    let Some(p) = layout_path().or_else(|| PERSIST.lock().ok().and_then(|g| g.clone())) else {
        return HashMap::new();
    };
    std::fs::read_to_string(p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Where a plain interactive run keeps it (set by the editor: next to
/// layout.json); None for test runs.
static PERSIST: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Writes this session's widget states (Studio: at a graceful exit).
pub fn save_layout() {
    let Some(p) = layout_path().or_else(|| PERSIST.lock().ok().and_then(|g| g.clone())) else {
        return;
    };
    let r = reg();
    let mut all = r.saved.clone().unwrap_or_else(load_saved);
    for (k, v) in &r.now {
        all.insert(k.clone(), v.clone());
    }
    if let Ok(s) = serde_json::to_string_pretty(&all) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, s);
    }
}

// ------------------------------------------------------------ editor state

/// One widget panel (`Tab::Plugin(n)`, n = index).
struct Slot {
    /// `edit_<plugin>_<id>` (`PlayServer_…` / `PlayClient_…` for Play's copies).
    key: String,
    /// The DataModel (and VM) the widget lives in.
    at: Where,
    r: Ref,
    plugin: Option<PluginId>,
    state: String,
    gui: Gui,
    /// Its tab exists (the dock), and whether it should (Enabled, and its
    /// DataModel in front).
    open: bool,
    want: bool,
    /// The widget's Enabled.
    enabled: bool,
    /// Asked the dock to open / close it and not seen it happen yet.
    asked: Option<bool>,
    /// Drawn this frame / last frame, where, and the mouse over it.
    drawn: u64,
    rect: Option<Rect>,
    focused: bool,
    /// Frames left to size a Float's window to its content, and the size
    /// asked of its window last.
    fix_left: u8,
    win: Option<egui::Vec2>,
    last_fix_rect: Option<egui::Vec2>,
    /// An Explorer drag left it (or was cancelled): its GUI keeps the hover
    /// it had, with no MouseLeave, until the pointer is over it again with
    /// no drag (Studio 0.747, checks/plugins/ref/captures/xdrag.txt S2-S4).
    hover_stale: bool,
    /// True once `gui` has synced marks while visible; transitioning from
    /// hidden to visible forces a full mark invalidation so changes made
    /// while hidden are picked up immediately without syncing hidden widgets.
    synced_visible: bool,
}

/// An input event for the edit VM, in order.
enum In {
    Gui(Vec<GuiEvent>),
    /// UserInputService: state, UserInputType, KeyCode, position, delta, gameProcessed.
    Uis {
        state: &'static str,
        uit: &'static str,
        key: String,
        pos: [f32; 3],
        delta: [f32; 2],
        gp: bool,
    },
    /// The active plugin's PluginMouse: event name (+ the key for KeyDown / KeyUp).
    Mouse(&'static str, Option<String>),
    /// The widget's × was clicked.
    Close(Ref),
    /// The harness shows a widget (Enabled = true).
    Show(Ref),
    /// WindowFocused (true) / WindowFocusReleased on a widget.
    Focus(Ref, bool),
    /// PluginDragEntered / Moved / Left / Dropped on a widget.
    Drag(Ref, &'static str),
    /// The same for Explorer instances dragged onto a widget (Studio's
    /// fixed "roblox/selection" data: api_widgets.rs `explorer_drag_fire`).
    XDrag(Ref, &'static str),
    /// The drag ended (dropped or released elsewhere).
    DragEnd,
    /// The active plugin's PluginMouse.DragEnter: Toolbox instances dragged
    /// into the 3D view (toolbox/glue.rs `toolbox_drag_in`). Edit only: the
    /// 3D view's plugin input is the edit VM's (`plugin_viewport`).
    MouseDragEnter(Vec<Ref>),
    /// MouseService.MouseEnterStudioViewport (true) / MouseLeaveStudioViewport.
    ViewportHover(bool),
}

/// The pointer over the edit viewport this frame (for PluginMouse, Mouse
/// rays and UserInputService:GetMouseLocation).
#[derive(Clone, Copy, Default)]
struct ViewIn {
    rect: Option<Rect>,
    xy: Option<[f32; 2]>,
    ray: (glam::Vec3, glam::Vec3),
    hit: Option<(Ref, glam::Vec3)>,
}

#[derive(Default)]
pub struct PluginUi {
    slots: Vec<Slot>,
    /// CoreGui over the edit viewport.
    core: Gui,
    /// Input for the edit VM, and for the Play server / client VMs (their
    /// widgets), in order.
    queue: Vec<In>,
    play_q: [Vec<In>; 2],
    /// The Play widgets' boxes last given to the server / client world.
    play_sent: [HashMap<Ref, GuiBox>; 2],
    view: ViewIn,
    frame: u64,
    /// The widget the keyboard is in (a TextBox in it has focus).
    text_focus: Option<u32>,
    /// A plugin drag (plugin:StartDrag) is under way; the widget it is over.
    drag_over: Option<Ref>,
    /// An Explorer drag is under way (last frame); the widget it is over;
    /// it was cancelled (Escape) with the button still held, and until the
    /// release no widget hears the pointer.
    xdrag_live: bool,
    xdrag_over: Option<Ref>,
    xdrag_dead: bool,
    /// Buttons held over the viewport (the PluginMouse's Button1Up etc.).
    view_buttons: [bool; 2],
    keys_down: HashSet<String>,
    keys_sent: HashSet<String>,
    layout_sent: HashMap<Ref, GuiBox>,
    ui_rev_seen: u64,
    had_play: bool,
    /// workspace.CurrentCamera as last written from the editor's view
    /// (CFrame, Focus position, FieldOfView): a plugin's write since moves
    /// the view.
    cam_sent: Option<(rustudio_core::math::Xform, glam::Vec3, f32)>,
    /// `--action plugin-widget:…:shot:FILE`: (widget key, file) waiting for
    /// the frame's picture.
    shots: Vec<(String, PathBuf)>,
    shot_asked: bool,
    /// Input that comes after the handle events of the next step (a press
    /// on a handle), fired after the tick; the left button went down on one.
    late: Vec<In>,
    handle_press: bool,
    debug_seen: HashSet<String>,
    /// This plugin tick's frame time (ScrollingFrames glide by it).
    pub(crate) dt: f32,
    /// The left press over the view went to a plugin (CoreGui, a handle, an
    /// active plugin's mouse): its release is not the editor's click, even
    /// when the plugin hands the tool back in between (Model Reflect).
    pub press_taken: bool,
    /// `plugin-widget:…` actions waiting for a freshly awakened deferred plugin
    /// to finish creating its DockWidgetPluginGui slot.
    pending_widget_actions: Vec<(String, u8)>,
}

impl PluginUi {
    /// A plugin GUI's TextBox has the keyboard (the editor's shortcuts and
    /// camera keys leave it alone).
    pub fn has_text_focus(&self) -> bool {
        self.text_focus.is_some() || self.core.focus.is_some()
    }

    /// ViewportFrames drawn inside edit plugin widgets or CoreGui.
    pub fn collect_viewport_frames(&self, out: &mut HashSet<Ref>) {
        out.extend(self.core.viewport_frames().iter().copied());
        for s in &self.slots {
            if s.at == Where::Edit && s.enabled && s.open {
                out.extend(s.gui.viewport_frames().iter().copied());
            }
        }
    }

    /// Render requests for ViewportFrames inside edit plugin widgets or CoreGui.
    pub fn collect_vf_requests(
        &self,
        doc: &Document,
        out: &mut HashMap<Ref, rustudio_render::render::vf::Request>,
    ) {
        out.extend(self.core.vf_requests(doc));
        for s in &self.slots {
            if s.at == Where::Edit && s.enabled && s.open {
                out.extend(s.gui.vf_requests(doc));
            }
        }
    }

    /// Laid-out render size of a ViewportFrame in an edit plugin widget or CoreGui.
    pub fn vf_size_of(&self, r: Ref) -> Option<[u32; 2]> {
        if let Some(sz) = self.core.vf_size_of(r) {
            return Some(sz);
        }
        for s in &self.slots {
            if s.at == Where::Edit && s.enabled && s.open {
                if let Some(sz) = s.gui.vf_size_of(r) {
                    return Some(sz);
                }
            }
        }
        None
    }

    /// Feeds rendered ViewportFrame textures back to edit plugin widgets and CoreGui.
    pub fn set_vf_textures(&mut self, tex: impl Fn(Ref) -> Option<(egui::TextureId, [u32; 2])>) {
        self.core.set_vf_textures(&tex);
        for s in &mut self.slots {
            if s.at == Where::Edit && s.enabled && s.open {
                s.gui.set_vf_textures(&tex);
            }
        }
    }

    /// The input queue of the VM a widget of `at` lives in.
    fn q(&mut self, at: Where) -> &mut Vec<In> {
        match at {
            Where::Edit => &mut self.queue,
            Where::Play(side) => &mut self.play_q[side_ix(side)],
        }
    }
}

/// A Gui for plugin GUIs (no top bar inset).
fn plugin_gui() -> Gui {
    let mut g = Gui::default();
    g.input_inset = Some(0.0);
    g
}

/// The edit document plugins run on (the saved one while Play runs).
fn edit_doc(ed: &mut Editor) -> &mut Document {
    match ed.game.play_saved_doc_mut() {
        Some(d) => {
            let d: *mut Document = d;
            // SAFETY: re-borrow of the same &mut (the borrow checker can't
            // see the None branch doesn't hold it).
            unsafe { &mut *d }
        }
        None => &mut ed.game.doc,
    }
}

fn str_prop(d: &Document, r: Ref, name: &str) -> String {
    match d.prop(r, name) {
        Some(Variant::String(s)) => s,
        Some(Variant::ContentId(c)) => c.as_str().to_string(),
        Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

fn key_name(k: egui::Key) -> Option<&'static str> {
    use egui::Key::*;
    Some(match k {
        A => "A",
        B => "B",
        C => "C",
        D => "D",
        E => "E",
        F => "F",
        G => "G",
        H => "H",
        I => "I",
        J => "J",
        K => "K",
        L => "L",
        M => "M",
        N => "N",
        O => "O",
        P => "P",
        Q => "Q",
        R => "R",
        S => "S",
        T => "T",
        U => "U",
        V => "V",
        W => "W",
        X => "X",
        Y => "Y",
        Z => "Z",
        Num0 => "Zero",
        Num1 => "One",
        Num2 => "Two",
        Num3 => "Three",
        Num4 => "Four",
        Num5 => "Five",
        Num6 => "Six",
        Num7 => "Seven",
        Num8 => "Eight",
        Num9 => "Nine",
        Enter => "Return",
        Escape => "Escape",
        Tab => "Tab",
        Backspace => "Backspace",
        Space => "Space",
        Delete => "Delete",
        Insert => "Insert",
        Home => "Home",
        End => "End",
        PageUp => "PageUp",
        PageDown => "PageDown",
        ArrowUp => "Up",
        ArrowDown => "Down",
        ArrowLeft => "Left",
        ArrowRight => "Right",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        Minus => "Minus",
        Plus => "Plus",
        Equals => "Equals",
        Comma => "Comma",
        Period => "Period",
        Slash => "Slash",
        Backslash => "BackSlash",
        Semicolon => "Semicolon",
        Quote => "Quote",
        OpenBracket => "LeftBracket",
        CloseBracket => "RightBracket",
        Backtick => "Backquote",
        _ => return None,
    })
}

impl Editor {
    fn plugins_on(&self) -> bool {
        !self.plugins.is_idle()
    }

    /// The tab title of `Tab::Plugin(n)`: the widget's Title.
    pub fn plugin_widget_title(&self, n: u32) -> String {
        let t = reg().slots.get(n as usize).map(|s| s.title.clone()).unwrap_or_default();
        if t.is_empty() {
            " ".into()
        } else {
            t
        }
    }

    /// The user clicked the panel's ×.
    pub fn plugin_widget_close(&mut self, n: u32) {
        if let Some(s) = self.plugin_ui.slots.get(n as usize) {
            if s.r.is_some() {
                let (r, at) = (s.r, s.at);
                self.plugin_ui.q(at).push(In::Close(r));
            }
        }
    }

    /// The DataModel whose widgets the dock shows (and whose toolbars the
    /// Plugins tab shows): the edit one, or the Play side in front.
    pub fn plugin_front(&self) -> Where {
        match self.game.play.as_ref() {
            Some(p) => Where::Play(p.showing),
            None => Where::Edit,
        }
    }

    /// The DataModel a widget context lives in, between frames.
    fn plugin_context_doc(&mut self, at: Where) -> Option<*mut Document> {
        match at {
            Where::Edit => Some(edit_doc(self) as *mut Document),
            Where::Play(side) => self.game.play_side_doc_mut(side).map(|d| d as *mut Document),
        }
    }

    /// After the plugin tick: widgets made / enabled / disabled / renamed
    /// by plugins → panels opened / closed / retitled; the GUIs read the
    /// journal's marks.
    pub fn plugin_ui_after_tick(&mut self) {
        self.plugin_camera_follow();
        let late = std::mem::take(&mut self.plugin_ui.late);
        if !late.is_empty() {
            let active_mouse = self.plugins.active_mouse();
            let doc: *mut Document = edit_doc(self);
            // SAFETY: as in plugin_widget_ui.
            let doc = unsafe { &mut *doc };
            self.plugins.with_edit(doc, |rt| fire_inputs(rt, late, active_mouse, None));
        }
        let play_now = self.game.play.is_some();
        let ui_rev = self.plugins.ui_rev();
        static PLUGIN_DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let debug_on = *PLUGIN_DEBUG.get_or_init(|| std::env::var_os("RUSTUDIO_PLUGIN_DEBUG").is_some());
        if !play_now
            && !self.plugin_ui.had_play
            && self.plugin_ui.slots.is_empty()
            && ui_rev == self.plugin_ui.ui_rev_seen
            && self.plugin_ui.pending_widget_actions.is_empty()
            && !debug_on
        {
            let doc: *mut Document = edit_doc(self);
            // SAFETY: as in plugin_widget_ui.
            let doc = unsafe { &mut *doc };
            let marks = doc.marks_for(rustudio_core::doc::Reader::PluginGui);
            self.plugin_ui.core.input_inset = Some(0.0);
            self.plugin_ui.core.sync_plugin(doc, &marks);
            return;
        }
        self.plugin_ui.ui_rev_seen = ui_rev;
        self.plugin_ui.had_play = play_now;
        let items = self.plugins.ui_items();
        // RUSTUDIO_PLUGIN_DEBUG: the plugins' toolbar buttons (for --action plugin-button:…)
        if debug_on {
            for it in &items {
                if let UiKind::Button { id, text, .. } = &it.kind {
                    let line = format!("[plugin-debug] button id={id:?} text={text:?}");
                    if self.plugin_ui.debug_seen.insert(line.clone()) {
                        eprintln!("{line}");
                    }
                }
            }
        }
        let names: HashMap<PluginId, String> = self.plugins.loaded().into_iter().map(|p| (p.id, p.name)).collect();
        // Every DataModel's widgets: the edit VM's, and the Play copies'.
        let mut sets: Vec<(Where, Vec<super::UiItem>)> = vec![(Where::Edit, items)];
        for side in self.game.play_sides() {
            let its = if self.game.has_play_worker() {
                self.plugins.play_widgets(side)
            } else {
                self.game.play_side_lua(side)
                    .and_then(|l| l.app_data_ref::<super::vm::PluginVm>().map(|v| v.ui.clone()))
                    .unwrap_or_default()
            };
            sets.push((Where::Play(side), its));
        }
        if self.game.play.is_none() {
            // (a session ended: its widgets' state goes with it)
            self.plugin_ui.play_q = Default::default();
            self.plugin_ui.play_sent = Default::default();
            *play_laid() = None;
        }
        let mut seen: HashSet<usize> = HashSet::new();
        for (at, items) in &sets {
            let at = *at;
            for it in items {
                let (id, state) = match &it.kind {
                    UiKind::DockWidget { id, info } => (id.clone(), info.initial_dock_state.clone()),
                    UiKind::QWidget { id, .. } => (id.clone(), "Float".to_string()),
                    _ => continue,
                };
                let Some(pname) = names.get(&it.plugin) else { continue };
                let key = format!("{}_{pname}_{id}", context_prefix(at));
                let float = match &it.kind {
                    UiKind::DockWidget { info, .. } => [info.float_x as f32, info.float_y as f32],
                    UiKind::QWidget { float_x, float_y, .. } => [*float_x as f32, *float_y as f32],
                    _ => [0.0, 0.0],
                };
                let idx = match self.plugin_ui.slots.iter().position(|s| s.key == key) {
                    Some(i) => i,
                    None => {
                        let full: &'static str = Box::leak(format!("{KEY_PREFIX}{key}").into_boxed_str());
                        reg().slots.push(RegSlot {
                            key: full,
                            state: state.clone(),
                            title: String::new(),
                            float,
                        });
                        self.plugin_ui.slots.push(Slot {
                            key: key.clone(),
                            at,
                            r: Ref::none(),
                            plugin: None,
                            state: state.clone(),
                            gui: plugin_gui(),
                            open: false,
                            want: false,
                            enabled: false,
                            asked: None,
                            drawn: 0,
                            rect: None,
                            focused: false,
                            fix_left: 4,
                            win: None,
                            last_fix_rect: None,
                            hover_stale: false,
                            synced_visible: false,
                        });
                        self.plugin_ui.slots.len() - 1
                    }
                };
                seen.insert(idx);
                let s = &mut self.plugin_ui.slots[idx];
                if s.r != it.r {
                    // (a reloaded plugin's new widget, or a new Play
                    // session's: same panel, fresh GUI state)
                    reg().live.remove(&s.r);
                    s.r = it.r;
                    s.gui = plugin_gui();
                    s.focused = false;
                    s.synced_visible = false;
                }
                s.plugin = Some(it.plugin);
                s.state = state.clone();
                if let Some(rs) = reg().slots.get_mut(idx) {
                    rs.state = state;
                    rs.float = float;
                }
            }
        }
        let front = self.plugin_front();
        let mut titles: Vec<(usize, String, bool)> = Vec::new();
        for (at, _) in &sets {
            let at = *at;
            let Some(doc) = self.plugin_context_doc(at) else {
                for slot in self.plugin_ui.slots.iter_mut().filter(|s| s.at == at) {
                    slot.want = false;
                }
                continue;
            };
            // SAFETY: the document stays put while this runs (the editor's
            // own, or one Play holds; nothing here moves either).
            let doc = unsafe { &mut *doc };
            let marks = doc.marks_for(rustudio_core::doc::Reader::PluginGui);
            for (i, s) in self.plugin_ui.slots.iter_mut().enumerate() {
                if s.at != at {
                    continue;
                }
                if !seen.contains(&i) {
                    s.r = Ref::none();
                    s.plugin = None;
                }
                let alive = s.r.is_some() && doc.exists(s.r);
                let enabled = alive && matches!(doc.prop(s.r, "Enabled"), Some(Variant::Bool(true)));
                s.enabled = enabled;
                s.want = enabled && at == front;
                let title = if alive { str_prop(doc, s.r, "Title") } else { String::new() };
                titles.push((i, title, enabled));
                if enabled && (s.open || s.want) {
                    if s.synced_visible {
                        s.gui.sync_widget(doc, &marks);
                    } else {
                        s.gui.sync_widget(doc, &rustudio_core::doc::Marks::All);
                        s.synced_visible = true;
                    }
                } else {
                    s.synced_visible = false;
                }
            }
            if at == Where::Edit {
                self.plugin_ui.core.input_inset = Some(0.0);
                self.plugin_ui.core.sync_plugin(doc, &marks);
            }
        }
        // Widgets of a DataModel that is gone (a Play session stopped).
        let contexts: Vec<Where> = sets.iter().map(|(a, _)| *a).collect();
        for s in self.plugin_ui.slots.iter_mut().filter(|s| !contexts.contains(&s.at)) {
            if s.r.is_some() {
                reg().live.remove(&s.r);
            }
            s.r = Ref::none();
            s.plugin = None;
            s.enabled = false;
            s.want = false;
            s.focused = false;
        }
        if self.plugin_ui.text_focus.is_some_and(|n| self.plugin_ui.slots.get(n as usize).is_none_or(|s| !s.want)) {
            self.plugin_ui.text_focus = None;
        }
        {
            let mut r = reg();
            for (i, t, en) in titles {
                if let Some(rs) = r.slots.get_mut(i) {
                    rs.title = t;
                }
                let s = &self.plugin_ui.slots[i];
                // (Play's widgets are not restored: only the edit layout is kept)
                if s.r.is_some() && s.at == Where::Edit {
                    let docked = s.state != "Float";
                    r.now.insert(s.key.clone(), Saved { enabled: en, docked });
                }
            }
        }
        // Retry any `plugin-widget:` actions that woke a deferred plugin and
        // were waiting for its DockWidget slot to appear.
        if !self.plugin_ui.pending_widget_actions.is_empty() {
            let pending = std::mem::take(&mut self.plugin_ui.pending_widget_actions);
            let mut dummy_raw = egui::RawInput::default();
            for (act, left) in pending {
                if !self.try_plugin_widget_action(&act, &mut dummy_raw, false) {
                    if left > 0 {
                        self.plugin_ui.pending_widget_actions.push((act, left - 1));
                    } else {
                        self.push_log(crate::app::Level::Warn, format!("--action {act}: deferred plugin widget did not appear"));
                    }
                }
            }
        }
        // Panels to open / close (the dock does it at the end of the frame).
        for (i, s) in self.plugin_ui.slots.iter_mut().enumerate() {
            if s.want != s.open && s.asked != Some(s.want) {
                s.asked = Some(s.want);
                self.ui.panel_requests.push((Tab::Plugin(i as u32), Some(s.want)));
            }
        }
    }

    /// Float widgets whose window must grow (or shrink) by this much for
    /// their content to be FloatingXSize − 2 × FloatingYSize − 24 (a few
    /// frames after they open; the user's resizing is theirs after).
    pub fn plugin_float_fixes(&mut self) -> Vec<(Tab, egui::Vec2)> {
        let sizes: Vec<Option<[f32; 2]>> = (0..self.plugin_ui.slots.len())
            .map(|i| float_size(&Tab::Plugin(i as u32)))
            .collect();
        let mut out = Vec::new();
        let r = reg();
        for (i, s) in self.plugin_ui.slots.iter_mut().enumerate() {
            if s.state != "Float" || !s.open {
                s.fix_left = 4;
                s.win = None;
                s.last_fix_rect = None;
                continue;
            }
            let (Some(rect), Some(rs)) = (s.rect, r.slots.get(i)) else {
                continue;
            };
            if s.fix_left == 0 || s.drawn + 1 < self.plugin_ui.frame {
                continue;
            }
            s.fix_left -= 1;
            let c = float_client(rs.float[0], rs.float[1]);
            let want = egui::vec2(c[0], c[1]);
            let d = want - rect.size();
            if std::env::var_os("RUSTUDIO_PLUGIN_DEBUG").is_some() {
                eprintln!("[plugin-float] {} content {:?} want {want:?} win {:?} area {:?}", s.key, rect.size(), s.win, r.dock_area);
            }
            // (the last correction changed nothing: the window is held by the
            // main window's bounds; leave it)
            if s.last_fix_rect == Some(rect.size()) {
                s.fix_left = 0;
                continue;
            }
            s.last_fix_rect = Some(rect.size());
            if d.length() > 0.5 && want.x > 0.0 && want.y > 0.0 {
                // (the window's content size as last set: float_size at first)
                let base = s.win.or(sizes[i].map(|v| egui::vec2(v[0], v[1]))).unwrap_or(rect.size());
                let size = (base + d).min(r.dock_area - egui::vec2(8.0, 8.0)).max(egui::vec2(60.0, 40.0));
                s.win = Some(size);
                out.push((Tab::Plugin(i as u32), size));
            }
        }
        out
    }

    /// Which plugin panels the dock holds now (after its requests ran).
    pub fn plugin_ui_note_open(&mut self, is_open: impl Fn(&Tab) -> bool) {
        for (i, s) in self.plugin_ui.slots.iter_mut().enumerate() {
            let open = is_open(&Tab::Plugin(i as u32));
            if open != s.open || s.asked == Some(open) {
                s.asked = None;
            }
            s.open = open;
        }
    }

    /// A plugin widget's panel content (`Tab::Plugin(n)`).
    pub fn plugin_widget_ui(&mut self, ui: &mut egui::Ui, n: u32) {
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let n = n as usize;
        let Some(r) = self.plugin_ui.slots.get(n).map(|s| s.r).filter(|r| r.is_some()) else {
            return;
        };
        self.plugin_ui.slots[n].drawn = self.plugin_ui.frame;
        self.plugin_ui.slots[n].rect = Some(rect);
        let painter = ui.painter_at(rect);
        let doc_dir = self.game.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let at = self.plugin_ui.slots[n].at;
        let _ch = rustudio_engine::gui::share_channel(channel(at));
        let state = self.plugin_ui.slots[n].state.clone();
        // Explorer instances dragged over the widget (see explorer_drag_widget).
        let xd = crate::explorer::explorer_drag_active(&ctx);
        {
            let mut g = reg();
            g.dock_sizes.insert(state, [rect.width(), rect.height()]);
            let rel = ctx
                .input(|i| i.pointer.latest_pos())
                .filter(|p| rect.contains(*p) && (resp.hovered() || self.plugin_ui.drag_over == Some(r) || xd))
                .map(|p| [(p.x - rect.min.x).floor(), (p.y - rect.min.y).floor()]);
            g.live.insert(
                r,
                Live {
                    rel,
                    size: [rect.width(), rect.height()],
                },
            );
        }
        // (its own DataModel: the edit one, or its Play side's)
        let Some(doc) = self.plugin_context_doc(at) else { return };
        // SAFETY: the document is not moved while this runs (the VMs
        // borrow theirs only inside plugin_tick / with_edit / the Play tick).
        let doc = unsafe { &mut *doc };
        let Editor {
            plugin_ui,
            game: Game { cam, assets, render_state, fixed_dt, .. },
            ..
        } = self;
        let slot = &mut plugin_ui.slots[n];
        slot.gui.draw(
            &ctx,
            &painter,
            rustudio_engine::gui::DrawIn {
                render_state: render_state.as_ref(),
                doc,
                cam,
                rect,
                store: assets,
                doc_dir: doc_dir.as_deref(),
                playing: false,
                fixed_step: fixed_dt.is_some(),
                roots: Roots::Widget(r),
            },
        );
        let mut q: Vec<In> = Vec::new();
        match explorer_drag_widget(&ctx, rect, r, xd, &mut plugin_ui.xdrag_over, plugin_ui.xdrag_dead, slot, &mut q) {
            XGui::Normal => {
                let _took = slot.gui.input(&ctx, &resp, rect);
            }
            XGui::Over(p) => {
                // (egui hovers only the dragged row: the GUI hears the pointer as given)
                slot.gui.pointer_override = Some(p);
                let _took = slot.gui.input(&ctx, &resp, rect);
                slot.gui.pointer_override = None;
            }
            XGui::Drop(p) => slot.gui.drop_release(p, rect),
            XGui::Skip => {}
        }
        let typing = slot.gui.keyboard(&ctx, doc);
        let events = std::mem::take(&mut slot.gui.events);
        // Window focus: a press in the panel focuses it; elsewhere releases.
        let pressed_in = resp.hovered() && ctx.input(|i| i.pointer.any_pressed());
        let pressed_out = ctx.input(|i| i.pointer.any_pressed()) && !resp.hovered();
        if pressed_in && !slot.focused {
            slot.focused = true;
            q.push(In::Focus(r, true));
        } else if pressed_out && slot.focused {
            slot.focused = false;
            q.push(In::Focus(r, false));
        }
        if typing {
            plugin_ui.text_focus = Some(n as u32);
            // (egui's keyboard focus: the editor's shortcuts leave the keys alone)
            resp.request_focus();
        } else if plugin_ui.text_focus == Some(n as u32) {
            plugin_ui.text_focus = None;
            resp.surrender_focus();
        }
        if !events.is_empty() {
            q.push(In::Gui(events));
        }
        // (UserInputService hears nothing of input inside a widget: Studio
        // 0.747, captures/studio-widgets.txt)
        if slot.gui.layout.len() > 0 || plugin_ui.drag_over.is_some() || xd {
            ctx.request_repaint();
        }
        plugin_ui.q(at).extend(q);
    }

    /// CoreGui over the edit viewport, drawn after the view's own GUIs;
    /// then its input, the pointer for PluginMouse, and whether the
    /// editor's tools must leave this press alone (a CoreGui button, a
    /// handle, or an active plugin with the exclusive mouse took it).
    pub fn plugin_viewport(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, resp: &egui::Response) -> bool {
        if !self.plugins_on() || self.game.play.is_some() {
            self.plugin_ui.view = ViewIn::default();
            return false;
        }
        reg().view = [rect.width(), rect.height()];
        let _ch = rustudio_engine::gui::share_channel(1);
        let doc_dir = self.game.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
        // (a Toolbox card dragged in and inserted: the plugin drags it, so
        // its mouse follows the pointer though egui's drag owns it)
        let pointer = resp
            .hover_pos()
            .or_else(|| if self.toolbox_drag_in_live() { ctx.pointer_hover_pos() } else { None })
            .filter(|p| rect.contains(*p));
        let was_over = self.plugin_ui.view.xy.is_some();
        let now_over = pointer.is_some();
        if was_over != now_over {
            self.plugin_ui.queue.push(In::ViewportHover(now_over));
        }
        let ray = pointer.map(|p| self.game.cam.ray(rect, p));
        let hit = ray.as_ref().and_then(|r| self.plugin_pick(r).map(|(t, d)| (t, r.at(d))));
        self.plugin_ui.view = ViewIn {
            rect: Some(rect),
            xy: pointer.map(|p| [p.x - rect.min.x, p.y - rect.min.y]),
            ray: ray.as_ref().map(|r| (r.origin, r.dir)).unwrap_or_default(),
            hit,
        };
        let Editor {
            plugin_ui,
            game: Game { doc, cam, assets, render_state, fixed_dt, .. },
            ..
        } = self;
        if std::env::var_os("RUSTUDIO_NO_GUI").is_none() {
            plugin_ui.core.draw(
                ctx,
                painter,
                rustudio_engine::gui::DrawIn {
                    render_state: render_state.as_ref(),
                    doc,
                    cam,
                    rect,
                    store: assets,
                    doc_dir: doc_dir.as_deref(),
                    playing: false,
                    fixed_step: fixed_dt.is_some(),
                    roots: Roots::CoreGui,
                },
            );
        }
        let gui_took = plugin_ui.core.input(ctx, resp, rect);
        plugin_ui.core.keyboard(ctx, doc);
        let events = std::mem::take(&mut plugin_ui.core.events);
        if !events.is_empty() {
            plugin_ui.queue.push(In::Gui(events));
        }
        let over_gui = pointer.is_some_and(|p| plugin_ui.core.object_at(p).is_some());
        let xy = plugin_ui.view.xy.unwrap_or([0.0, 0.0]);
        // The active plugin's PluginMouse (F57: not over CoreGui GUI).
        let active = self.plugins.active_plugin().is_some();
        let (adorn_hot, adorn_handles) = {
            let s = rustudio_engine::gui::share();
            (s.adorn_hot, s.adorn_handles)
        };
        let mut took = gui_took || (adorn_hot && pointer.is_some());
        if active && pointer.is_some() && !over_gui {
            let q = &mut self.plugin_ui.queue;
            let (presses, moved) = ctx.input(|i| {
                let p: Vec<(egui::PointerButton, bool)> = i
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::PointerButton { button, pressed, .. } => Some((*button, *pressed)),
                        _ => None,
                    })
                    .collect();
                (p, i.pointer.delta() != egui::Vec2::ZERO)
            });
            // A press on a handle is the handle's: the PluginMouse gets no
            // Button1Down, only the Button1Up (captures/studio-core.txt).
            let handle = adorn_handles;
            if moved {
                q.push(In::Mouse("Move", None));
            }
            for (b, down) in presses {
                let (ev, i) = match (b, down) {
                    (egui::PointerButton::Primary, true) => ("Button1Down", 0),
                    (egui::PointerButton::Primary, false) => ("Button1Up", 0),
                    (egui::PointerButton::Secondary, true) => ("Button2Down", 1),
                    (egui::PointerButton::Secondary, false) => ("Button2Up", 1),
                    _ => continue,
                };
                self.plugin_ui.view_buttons[i] = down;
                if handle && down && i == 0 {
                    self.plugin_ui.handle_press = true;
                } else if !down && i == 0 && std::mem::take(&mut self.plugin_ui.handle_press) {
                    // (after the handle's MouseButton1Up, which its step fires)
                    self.plugin_ui.late.push(In::Mouse(ev, None));
                } else {
                    self.plugin_ui.queue.push(In::Mouse(ev, None));
                }
                // exclusive mouse: the editor's selection tool leaves it
                took = true;
            }
            let wheel: f32 = ctx.input(|i| {
                i.raw
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                        _ => None,
                    })
                    .sum()
            });
            if wheel > 0.0 {
                self.plugin_ui.queue.push(In::Mouse("WheelForward", None));
            } else if wheel < 0.0 {
                self.plugin_ui.queue.push(In::Mouse("WheelBackward", None));
            }
            if ctx.input(|i| i.pointer.primary_down()) {
                took = true;
            }
        }
        // The active plugin's PluginMouse.Icon over the view (Studio draws
        // the plugin's cursor image in place of the arrow).
        if let (Some(p), Some(mouse)) = (pointer, self.plugins.active_mouse()) {
            let icon = str_prop(&self.game.doc, mouse, "Icon");
            if !icon.is_empty() && !over_gui {
                let doc_dir = self.game.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
                let Editor { plugin_ui, game: Game { assets, .. }, .. } = self;
                if plugin_ui.core.draw_cursor(ctx, painter, assets, doc_dir.as_deref(), &icon, p) {
                    ctx.set_cursor_icon(egui::CursorIcon::None);
                }
            }
        }
        // UserInputService after the PluginMouse (F57: Button1Down came
        // before UIS.InputBegan); gameProcessed over CoreGui GUI.
        if pointer.is_some() {
            // (a press on a handle: after the handle's MouseButton1Down)
            let q = if adorn_hot && !over_gui {
                &mut self.plugin_ui.late
            } else {
                &mut self.plugin_ui.queue
            };
            push_buttons(ctx, q, over_gui || adorn_handles, xy);
        }
        if !(active && pointer.is_some() && !over_gui) {
            // (a release outside still ends a press the mouse had)
            for i in 0..2 {
                let down = ctx.input(|inp| {
                    if i == 0 {
                        inp.pointer.primary_down()
                    } else {
                        inp.pointer.secondary_down()
                    }
                });
                if self.plugin_ui.view_buttons[i] && !down {
                    self.plugin_ui.view_buttons[i] = false;
                    self.plugin_ui
                        .queue
                        .push(In::Mouse(if i == 0 { "Button1Up" } else { "Button2Up" }, None));
                }
            }
        }
        took
    }

    /// Keys for the edit VM (UserInputService, the active PluginMouse's
    /// KeyDown / KeyUp): call once a frame where the editor reads keys.
    pub fn plugin_keys(&mut self, ctx: &egui::Context) {
        if !self.plugins_on() || self.game.play.is_some() {
            return;
        }
        let text_box = self.plugin_ui.has_text_focus();
        if ctx.egui_wants_keyboard_input() && !text_box {
            return;
        }
        let keys: Vec<(egui::Key, bool)> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed,
                        repeat: false,
                        ..
                    } => Some((*key, *pressed)),
                    _ => None,
                })
                .collect()
        });
        let active = self.plugins.active_plugin().is_some();
        // (keys typed into a widget aren't UserInputService's either)
        let in_widget = self.plugin_ui.slots.iter().any(|s| s.focused);
        for (k, down) in keys {
            let Some(name) = key_name(k) else { continue };
            if down {
                self.plugin_ui.keys_down.insert(name.to_string());
            } else {
                self.plugin_ui.keys_down.remove(name);
            }
            // The PluginMouse's KeyDown before UserInputService's InputBegan.
            if active && !text_box && !in_widget {
                let ch = name.to_lowercase();
                if ch.len() == 1 {
                    self.plugin_ui
                        .queue
                        .push(In::Mouse(if down { "KeyDown" } else { "KeyUp" }, Some(ch)));
                }
            }
            if in_widget {
                continue;
            }
            self.plugin_ui.queue.push(In::Uis {
                state: if down { "Begin" } else { "End" },
                uit: "Keyboard",
                key: name.to_string(),
                pos: [0.0; 3],
                delta: [0.0; 2],
                gp: false,
            });
        }
    }

    /// Toolbox instances dragged into the 3D view while a plugin holds the
    /// mouse: its PluginMouse.DragEnter(instances), before any Button1Down.
    /// (The edit VM's: the 3D view takes no plugin input during Play.)
    pub fn plugin_mouse_drag_enter(&mut self, refs: Vec<Ref>) {
        if self.game.play.is_none() && self.plugins.active_plugin().is_some() && !refs.is_empty() {
            self.plugin_ui.q(Where::Edit).push(In::MouseDragEnter(refs));
        }
    }

    /// A plugin drag (plugin:StartDrag): the widget under the pointer gets
    /// PluginDragEntered / Moved / Left, the release over one Dropped
    /// (F58: no Left on a drop). Call once a frame after the panels drew.
    pub fn plugin_drag_tick(&mut self, ctx: &egui::Context) {
        // Explorer drags (routed as the widgets draw): Escape drops egui's
        // payload with the button still held; that drag is over until the
        // release (the widget it was over heard PluginDragLeft).
        let xd = crate::explorer::explorer_drag_active(ctx);
        let down = ctx.input(|i| i.pointer.primary_down());
        if self.plugin_ui.xdrag_live && !xd && down {
            self.plugin_ui.xdrag_dead = true;
        }
        if !down {
            self.plugin_ui.xdrag_dead = false;
        }
        if !xd {
            self.plugin_ui.xdrag_over = None;
        }
        self.plugin_ui.xdrag_live = xd;
        if !W::drag_active() {
            self.plugin_ui.drag_over = None;
            return;
        }
        let p = ctx.input(|i| i.pointer.latest_pos());
        let frame = self.plugin_ui.frame;
        let over = p.and_then(|p| {
            self.plugin_ui
                .slots
                .iter()
                .find(|s| s.drawn == frame && s.rect.is_some_and(|r| r.contains(p)) && s.r.is_some())
                .map(|s| s.r)
        });
        let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        let released = ctx.input(|i| i.pointer.any_released()) || !ctx.input(|i| i.pointer.primary_down());
        // (each widget hears it in its own VM; the drag itself is the one
        // of the DataModel in front, whose widgets are shown)
        let front = self.plugin_front();
        let at_of = |ui: &PluginUi, r: Ref| ui.slots.iter().find(|s| s.r == r).map(|s| s.at).unwrap_or(front);
        let mut out: Vec<(Where, In)> = Vec::new();
        if over != self.plugin_ui.drag_over {
            if let Some(old) = self.plugin_ui.drag_over {
                if !released {
                    out.push((at_of(&self.plugin_ui, old), In::Drag(old, "PluginDragLeft")));
                }
            }
            if let Some(new) = over {
                // (Studio: a PluginDragMoved at once, at the same point:
                // captures/ui.txt F58, captures/xdrag.txt)
                out.push((at_of(&self.plugin_ui, new), In::Drag(new, "PluginDragEntered")));
                out.push((at_of(&self.plugin_ui, new), In::Drag(new, "PluginDragMoved")));
            }
            self.plugin_ui.drag_over = over;
        } else if moved {
            if let Some(o) = over {
                out.push((at_of(&self.plugin_ui, o), In::Drag(o, "PluginDragMoved")));
            }
        }
        if released {
            if let Some(o) = over {
                out.push((at_of(&self.plugin_ui, o), In::Drag(o, "PluginDragDropped")));
            }
            out.push((front, In::DragEnd));
            self.plugin_ui.drag_over = None;
        }
        for (at, ev) in out {
            self.plugin_ui.q(at).push(ev);
        }
        ctx.request_repaint();
    }

    /// Prepares the per-frame UI/input flush payload so `Editor::plugin_tick`
    /// can apply it inside the same VM enter/leave pass as `PluginHost::tick`.
    pub(super) fn prepare_plugin_ui_flush(&mut self) -> PluginUiFlush {
        self.plugin_ui.frame += 1;
        let frame = self.plugin_ui.frame;
        let has_edit_slots = self
            .plugin_ui
            .slots
            .iter()
            .any(|s| s.at == Where::Edit && (s.drawn + 2 >= frame || s.open || s.enabled) && !s.gui.layout.is_empty());
        // Layout of every GUI drawn in the edit viewport (world SurfaceGui /
        // BillboardGui and StarterGui from `game.gui`), overlaid with CoreGui
        // and open plugin dock widgets.
        let world = self.game.play.is_none() && !self.game.gui.layout.is_empty();
        let layout = if !has_edit_slots && !world {
            if self.plugin_ui.core.layout != self.plugin_ui.layout_sent {
                let l = self.plugin_ui.core.layout.clone();
                self.plugin_ui.layout_sent = l.clone();
                Some(l)
            } else {
                None
            }
        } else {
            let mut l: HashMap<Ref, GuiBox> = if world { self.game.gui.layout.clone() } else { HashMap::new() };
            l.extend(self.plugin_ui.core.layout.iter().map(|(k, v)| (*k, *v)));
            for s in self.plugin_ui.slots.iter().filter(|s| s.at == Where::Edit) {
                if s.drawn + 2 >= frame || s.open || s.enabled {
                    l.extend(s.gui.layout.iter().map(|(k, v)| (*k, *v)));
                }
            }
            if l != self.plugin_ui.layout_sent {
                self.plugin_ui.layout_sent = l.clone();
                Some(l)
            } else {
                None
            }
        };
        let queue = std::mem::take(&mut self.plugin_ui.queue);
        let view = self.plugin_ui.view;
        let active_mouse = self.plugins.active_mouse();
        let keys = if self.plugin_ui.keys_down != self.plugin_ui.keys_sent {
            let k = self.plugin_ui.keys_down.clone();
            self.plugin_ui.keys_sent = k.clone();
            Some(k)
        } else {
            None
        };
        // The editor's view → workspace.CurrentCamera (not while Play shows
        // its own camera).
        let cam_now = (self.game.play.is_none()).then(|| {
            let c = &self.game.cam;
            let rot = glam::Mat3::from_cols(c.right(), c.up(), -c.forward());
            (rustudio_core::math::Xform { pos: c.pos, rot }, c.focus, c.fov_deg)
        });
        let cam_sent = self.plugin_ui.cam_sent;
        let dt = self.plugin_ui.dt;
        let vp_rect = Some(self.game.viewport_rect).filter(|r| r.is_finite() && r.is_positive());
        PluginUiFlush {
            layout,
            queue,
            view,
            active_mouse,
            keys,
            cam_now,
            cam_sent,
            dt,
            vp_rect,
        }
    }

    pub(super) fn finish_plugin_ui_flush(&mut self, cam_sent: Option<(rustudio_core::math::Xform, glam::Vec3, f32)>) {
        self.plugin_ui.cam_sent = cam_sent;
        if self.game.play.is_some() {
            self.plugin_play_flush();
        }
    }

    /// The Play copies' widgets: their queued input and their panels'
    /// layout reach the server's and the client's VM (each with its own
    /// DataModel entered, on its GUI channel).
    fn plugin_play_flush(&mut self) {
        let frame = self.plugin_ui.frame;
        let dt = self.plugin_ui.dt;
        for side in self.game.play_sides() {
            let at = Where::Play(side);
            let ix = side_ix(side);
            let mut layout: HashMap<Ref, GuiBox> = HashMap::new();
            let mut live = false;
            for s in self.plugin_ui.slots.iter().filter(|s| s.at == at) {
                live |= s.r.is_some();
                if s.drawn + 2 >= frame || s.open || s.enabled {
                    layout.extend(s.gui.layout.iter().map(|(k, v)| (*k, *v)));
                }
            }
            let queue = std::mem::take(&mut self.plugin_ui.play_q[ix]);
            let changed = layout != self.plugin_ui.play_sent[ix];
            // (the server's glides step here: its game has no GUI to step them)
            if queue.is_empty() && !changed && !(live && side == Side::Server) {
                continue;
            }
            let old = if changed {
                std::mem::replace(&mut self.plugin_ui.play_sent[ix], layout.clone())
            } else {
                HashMap::new()
            };
            if changed {
                let mut g = play_laid();
                let set = g.get_or_insert_with(HashSet::new);
                for k in old.keys() {
                    set.remove(k);
                }
                set.extend(layout.keys().copied());
            }
            self.game.queue_play_side(side, move |rt, _, _| {
                let _ch = rustudio_engine::gui::share_channel(channel(at));
                let lua = &rt.lua;
                if changed {
                    if let Ok(mut w) = rustudio_engine::runtime::world(lua) {
                        for k in old.keys().filter(|k| !layout.contains_key(k)) {
                            w.input.gui_layout.remove(k);
                        }
                        w.input.gui_layout.extend(layout.iter().map(|(k, v)| (*k, *v)));
                    }
                    let _ = rustudio_engine::runtime::ext::fire_layout_changes(lua);
                }
                let gui_service = rustudio_engine::runtime::world(lua).ok().and_then(|w| {
                    let d = w.d();
                    d.find_child(d.dom.root_ref(), "GuiService")
                });
                fire_inputs(rt, queue, None, gui_service);
                if side == Side::Server {
                    crate::play::game::step_smooth_scrolls(rt, dt);
                    rt.spawn_global("__rs_plugin_gui_step", vec![]);
                    rt.dispatch();
                }
            });
        }
    }

    /// A plugin wrote workspace.CurrentCamera's CFrame / Focus /
    /// FieldOfView: the editor's view follows (Studio moves its camera).
    fn plugin_camera_follow(&mut self) {
        let Some(sent) = self.plugin_ui.cam_sent else { return };
        if self.game.play.is_some() {
            return;
        }
        let Some(now) = read_camera(&self.game.doc) else { return };
        if same_cam(&sent, &now) {
            return;
        }
        self.game.cam.pos = now.0.pos;
        self.game.cam.set_rotation(now.0.rot);
        self.game.cam.focus = now.1;
        if now.2.is_finite() && now.2 > 0.0 {
            self.game.cam.fov_deg = now.2;
        }
        self.plugin_ui.cam_sent = Some(now);
    }
}

pub(super) struct PluginUiFlush {
    layout: Option<HashMap<Ref, GuiBox>>,
    queue: Vec<In>,
    view: ViewIn,
    active_mouse: Option<Ref>,
    keys: Option<HashSet<String>>,
    cam_now: Option<(rustudio_core::math::Xform, glam::Vec3, f32)>,
    pub(super) cam_sent: Option<(rustudio_core::math::Xform, glam::Vec3, f32)>,
    dt: f32,
    vp_rect: Option<Rect>,
}

pub(super) fn apply_plugin_ui_flush(
    rt: &rustudio_engine::runtime::Runtime,
    flush: PluginUiFlush,
    cam_sent: &mut Option<(rustudio_core::math::Xform, glam::Vec3, f32)>,
) {
    let lua = &rt.lua;
    let layout_changed = flush.layout.is_some();
    let had_keys = flush.keys.is_some();
    let had_queue = !flush.queue.is_empty();
    let view = flush.view;
    let mut mouse_changed = false;
    if let Ok(mut w) = rustudio_engine::runtime::world(lua) {
        if let Some(rect) = view.rect.or(flush.vp_rect) {
            w.input.view_size = [rect.width(), rect.height()];
        }
        let next_xy = view.xy.unwrap_or([-1.0, -1.0]);
        mouse_changed = w.input.mouse_xy != next_xy;
        w.input.mouse_xy = next_xy;
        w.input.mouse_ray = view.ray;
        w.input.mouse_target = view.hit.map(|h| h.0);
        w.input.mouse_hit = view.hit.map(|h| rustudio_core::math::Xform {
            pos: h.1,
            rot: glam::Mat3::IDENTITY,
        });
        if view.hit.is_none() && view.xy.is_some() {
            w.input.mouse_hit = Some(rustudio_core::math::Xform {
                pos: view.ray.0 + view.ray.1 * 10000.0,
                rot: glam::Mat3::IDENTITY,
            });
        }
        if let Some(layout) = flush.layout {
            w.input.gui_layout = layout;
        }
        if let Some(keys) = flush.keys {
            w.keys_down = keys;
        }
    }
    crate::plugin_host::api::mouse::set_pointer_in_view(lua, view.xy.is_some());
    if let Some(now) = flush.cam_now {
        if cam_sent.is_none_or(|s| !same_cam(&s, &now)) {
            write_camera(rt, &now);
            *cam_sent = Some(now);
        }
    }
    if layout_changed {
        let _ = rustudio_engine::runtime::ext::fire_layout_changes(lua);
    }
    if had_queue {
        let gui_service = rustudio_engine::runtime::world(lua).ok().and_then(|w| {
            let d = w.d();
            d.find_child(d.dom.root_ref(), "GuiService")
        });
        fire_inputs(rt, flush.queue, flush.active_mouse, gui_service);
    }
    crate::play::game::step_smooth_scrolls(rt, flush.dt);
    if had_queue || had_keys || mouse_changed {
        rt.spawn_global("__rs_input_frame_end", vec![Value::Number(0.0)]);
    }
}

fn camera_of(d: &Document) -> Option<Ref> {
    let ws = d.workspace()?;
    match d.prop(ws, "CurrentCamera") {
        Some(Variant::Ref(r)) if d.exists(r) => Some(r),
        _ => None,
    }
}

fn read_camera(d: &Document) -> Option<(rustudio_core::math::Xform, glam::Vec3, f32)> {
    let c = camera_of(d)?;
    let xf = match d.prop(c, "CFrame")? {
        Variant::CFrame(cf) => rustudio_core::math::Xform::from_cframe(&cf),
        _ => return None,
    };
    let focus = match d.prop(c, "Focus") {
        Some(Variant::CFrame(f)) => rustudio_core::math::v3(f.position),
        _ => xf.pos - xf.rot.z_axis * 20.0,
    };
    let fov = match d.prop(c, "FieldOfView") {
        Some(Variant::Float32(f)) => f,
        Some(Variant::Float64(f)) => f as f32,
        _ => 70.0,
    };
    Some((xf, focus, fov))
}

fn same_cam(a: &(rustudio_core::math::Xform, glam::Vec3, f32), b: &(rustudio_core::math::Xform, glam::Vec3, f32)) -> bool {
    a.0.pos.distance(b.0.pos) < 1e-3
        && (a.0.rot.x_axis - b.0.rot.x_axis).length() < 1e-4
        && (a.0.rot.y_axis - b.0.rot.y_axis).length() < 1e-4
        && a.1.distance(b.1) < 1e-3
        && (a.2 - b.2).abs() < 1e-3
}

/// The editor's view into the edit VM's Camera (its properties change as
/// when Studio's camera moves: Changed fires; the place stays saved).
fn write_camera(rt: &rustudio_engine::runtime::Runtime, now: &(rustudio_core::math::Xform, glam::Vec3, f32)) {
    let Ok(mut w) = rustudio_engine::runtime::world(&rt.lua) else { return };
    let Some(c) = camera_of(w.d()) else { return };
    let old = read_camera(w.d());
    let focus = rustudio_core::math::Xform {
        pos: now.1,
        rot: glam::Mat3::IDENTITY,
    };
    let mut changed: Vec<&str> = Vec::new();
    if let Some(i) = w.dm().dom.get_by_ref_mut(c) {
        if old.is_none_or(|o| {
            o.0.pos.distance(now.0.pos) > 1e-4
                || (o.0.rot.x_axis - now.0.rot.x_axis).length() > 1e-5
                || (o.0.rot.y_axis - now.0.rot.y_axis).length() > 1e-5
        }) {
            i.properties.insert("CFrame".into(), Variant::CFrame(now.0.to_cframe()));
            changed.push("CFrame");
        }
        if old.is_none_or(|o| o.1.distance(now.1) > 1e-4) {
            i.properties.insert("Focus".into(), Variant::CFrame(focus.to_cframe()));
            changed.push("Focus");
        }
        if old.is_none_or(|o| (o.2 - now.2).abs() > 1e-4) {
            i.properties.insert("FieldOfView".into(), Variant::Float32(now.2));
            changed.push("FieldOfView");
        }
    }
    for name in changed {
        if let Ok(v) = rt.lua.create_string(name) {
            w.fire(c, "Changed", vec![Value::String(v)]);
        }
        w.fire(c, &format!("Changed:{name}"), vec![]);
    }
}

/// Fires queued input in the entered edit VM, in order.
fn fire_inputs(rt: &rustudio_engine::runtime::Runtime, queue: Vec<In>, active_mouse: Option<Ref>, gui_service: Option<Ref>) {
    let lua = &rt.lua;
    for ev in queue {
        match ev {
            In::Gui(evs) => crate::play::game::fire_gui_events(rt, evs, gui_service),
            In::Uis {
                state,
                uit,
                key,
                pos,
                delta,
                gp,
            } => {
                let s = |x: &str| Value::String(lua.create_string(x).expect("lua string"));
                rt.spawn_global(
                    "__rs_input_event",
                    vec![
                        s(state),
                        s(uit),
                        s(&key),
                        Value::Number(pos[0] as f64),
                        Value::Number(pos[1] as f64),
                        Value::Number(pos[2] as f64),
                        Value::Number(delta[0] as f64),
                        Value::Number(delta[1] as f64),
                        Value::Boolean(gp),
                    ],
                );
            }
            In::Mouse(name, key) => {
                if let Some(m) = active_mouse {
                    let args = match key {
                        Some(k) => vec![Value::String(lua.create_string(&k).expect("lua string"))],
                        None => vec![],
                    };
                    rt.fire(m, name, args);
                }
            }
            In::Close(r) => W::close(rt, r),
            In::Show(r) => rt.set(r, "Enabled", Value::Boolean(true)),
            In::Focus(r, on) => rt.fire(r, if on { "WindowFocused" } else { "WindowFocusReleased" }, vec![]),
            In::Drag(r, name) => W::drag_fire(rt, r, name),
            In::XDrag(r, name) => W::explorer_drag_fire(rt, r, name),
            In::DragEnd => W::drag_end(lua),
            In::MouseDragEnter(refs) => {
                if let Some(m) = active_mouse {
                    let alive: Vec<Ref> = match rt.world_mut() {
                        Some(mut w) => refs.into_iter().filter(|r| w.dm().dom.get_by_ref(*r).is_some()).collect(),
                        None => Vec::new(),
                    };
                    let list = lua.create_table().and_then(|t| {
                        for r in &alive {
                            t.push(rustudio_engine::runtime::inst_value(lua, *r)?)?;
                        }
                        Ok(t)
                    });
                    if let Ok(t) = list {
                        rt.fire(m, "DragEnter", vec![Value::Table(t)]);
                    }
                }
            }
            In::ViewportHover(entered) => {
                let svc = rt.world_mut().and_then(|w| {
                    let d = w.d();
                    d.find_child(d.dom.root_ref(), "MouseService")
                });
                if let Some(ms) = svc {
                    rt.fire(
                        ms,
                        if entered { "MouseEnterStudioViewport" } else { "MouseLeaveStudioViewport" },
                        vec![],
                    );
                }
            }
        }
        rt.dispatch();
    }
}

/// What a widget's GUI hears of the pointer this frame (explorer_drag_widget).
enum XGui {
    /// The normal pointer input.
    Normal,
    /// An Explorer drag over it: hover and moves at this point, no button.
    Over(Pos2),
    /// An Explorer drag let go over it at this point.
    Drop(Pos2),
    /// Nothing (a drag elsewhere, or the stale hover kept).
    Skip,
}

/// Explorer instances dragged onto plugin widget `r` (drawn at `rect`), as
/// Studio 0.747 does it (checks/plugins/ref/captures/xdrag.txt, probe
/// checks/plugins/ref/xdrag/RefXD_explorer_drag.lua, real pointer drags):
/// * entering: PluginDragEntered and at once PluginDragMoved at the same
///   point, then the GUI's MouseEnter / InputBegan (MouseMovement); each
///   move a PluginDragMoved, GUI moves too;
/// * leaving: PluginDragLeft; the GUI gets no MouseLeave and keeps its hover
///   until the pointer is over it again with no drag (`hover_stale`);
/// * let go over it: PluginDragDropped (no Left), WindowFocused, then the
///   top GUI object under the pointer hears MouseButton1Up (a button) and
///   InputEnded (MouseButton1); no MouseButton1Down, Click or Activated;
/// * Escape while over it: PluginDragLeft and the drag is over: no more
///   events, nothing at the release;
/// * the event: a plain table {Sender "Explorer", MimeType "roblox/selection",
///   Data "", MouseIcon "", DragIcon "", HotSpot Vector2(0, 0)}, the same for
///   one or several instances: the dragged rows are the Selection
///   (explorer.rs selects an unselected row as its drag starts).
/// Drag events go into `q` before the GUI's events of the frame.
fn explorer_drag_widget(
    ctx: &egui::Context,
    rect: Rect,
    r: Ref,
    xd: bool,
    over: &mut Option<Ref>,
    dead: bool,
    slot: &mut Slot,
    q: &mut Vec<In>,
) -> XGui {
    let p = ctx.input(|i| i.pointer.latest_pos());
    let inside = p.filter(|p| rect.contains(*p));
    let released = ctx.input(|i| i.pointer.any_released());
    let moved = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
    if !xd {
        if *over == Some(r) {
            // The payload went with the button held: Escape.
            q.push(In::XDrag(r, "PluginDragLeft"));
            *over = None;
            slot.hover_stale = slot.gui.has_hover();
            return XGui::Skip;
        }
        if dead || slot.hover_stale && inside.is_none() {
            slot.hover_stale |= slot.gui.has_hover();
            return XGui::Skip;
        }
        slot.hover_stale = false;
        return XGui::Normal;
    }
    let Some(p) = inside else {
        if *over == Some(r) {
            q.push(In::XDrag(r, "PluginDragLeft"));
            *over = None;
        }
        slot.hover_stale |= slot.gui.has_hover();
        return XGui::Skip;
    };
    slot.hover_stale = false;
    if *over != Some(r) {
        q.push(In::XDrag(r, "PluginDragEntered"));
        q.push(In::XDrag(r, "PluginDragMoved"));
        *over = Some(r);
    } else if moved {
        q.push(In::XDrag(r, "PluginDragMoved"));
    }
    if !released {
        return XGui::Over(p);
    }
    q.push(In::XDrag(r, "PluginDragDropped"));
    *over = None;
    if !slot.focused {
        slot.focused = true;
        q.push(In::Focus(r, true));
    }
    XGui::Drop(p)
}

/// UserInputService's InputBegan / InputEnded for mouse buttons pressed
/// this frame (gameProcessed: over GUI).
fn push_buttons(ctx: &egui::Context, q: &mut Vec<In>, gp: bool, xy: [f32; 2]) {
    let presses: Vec<(egui::PointerButton, bool)> = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::PointerButton { button, pressed, .. } => Some((*button, *pressed)),
                _ => None,
            })
            .collect()
    });
    for (b, down) in presses {
        let uit = match b {
            egui::PointerButton::Primary => "MouseButton1",
            egui::PointerButton::Secondary => "MouseButton2",
            egui::PointerButton::Middle => "MouseButton3",
            _ => continue,
        };
        q.push(In::Uis {
            state: if down { "Begin" } else { "End" },
            uit,
            key: "Unknown".into(),
            pos: [xy[0], xy[1], 0.0],
            delta: [0.0; 2],
            gp,
        });
    }
}

/// The editor is closing: Studio saves the widget layout now.
pub fn on_exit() {
    save_layout();
}

/// Sets where a plain interactive run keeps the widget layout.
pub fn set_persist_dir(dir: Option<PathBuf>) {
    if let Ok(mut g) = PERSIST.lock() {
        *g = dir.map(|d| d.join("plugin_widgets.json"));
    }
}

impl Editor {
    /// The nearest part along a ray, locked ones too (Mouse.Target: Studio
    /// reports the locked Baseplate, F57).
    fn plugin_pick(&self, ray: &rustudio_core::math::Ray) -> Option<(Ref, f32)> {
        let mut best: Option<(f32, Ref)> = None;
        for (sp, b) in self.game.scene.parts.iter().zip(&self.game.scene.bounds) {
            let oc = b.truncate() - ray.origin;
            let along = oc.dot(ray.dir);
            if oc.length_squared() - along * along > b.w * b.w || along < -b.w {
                continue;
            }
            if let Some(t) = ray.hit_obb(&sp.xf, sp.size) {
                if t > 0.0 && best.is_none_or(|(bt, _)| t < bt) {
                    best = Some((t, sp.r));
                }
            }
        }
        best.map(|(t, r)| (r, t))
    }
}

#[allow(dead_code)]
fn _unused(_: Pos2) {}

// ------------------------------------------------------------ harness

impl Editor {
    /// Where widget `who` (`<plugin Name>/<widget id>`) of the DataModel in
    /// front was drawn last frame (the edit one's, or Play's shown side's).
    pub fn plugin_widget_rect(&self, who: &str) -> Option<Rect> {
        let key = format!("{}_{}", context_prefix(self.plugin_front()), who.replacen('/', "_", 1));
        self.plugin_ui.slots.iter().find(|s| s.key == key).and_then(|s| s.rect)
    }

    /// `--action plugin-widget:<plugin>/<widget id>:CMD@S` (CMD: show, hide
    /// (the panel's ×), click:X,Y / rclick:X,Y (widget pixels), move:X,Y,
    /// type:TEXT, shot:FILE (the widget's content as a PNG)). `<plugin>` is
    /// the Plugin's Name (`user_probe.lua`, `cloud_123`). False when `name`
    /// isn't one.
    pub fn plugin_widget_action(&mut self, name: &str, raw: &mut egui::RawInput) -> bool {
        // plugin-reload:NAME: the plugin reloads (its widgets close and come back).
        if let Some(pname) = name.strip_prefix("plugin-reload:") {
            match self.plugins.loaded().into_iter().find(|p| p.name == pname) {
                Some(p) => self.plugins.reload(p.id),
                None => self.push_log(crate::app::Level::Warn, format!("--action {name}: no plugin {pname}")),
            }
            return true;
        }
        // plugin-core:press:PATH / plugin-core:hover:PATH: a click on (the
        // pointer onto) the CoreGui GuiObject at PATH (`ScreenGui.Frame.Button`),
        // where it was laid out over the edit view last frame.
        if let Some(rest) = name.strip_prefix("plugin-core:") {
            let (verb, path) = rest.split_once(':').unwrap_or((rest, ""));
            let Some(p) = self.gui_press_point(None, path) else {
                self.push_log(crate::app::Level::Warn, format!("--action {name}: no laid-out CoreGui object {path}"));
                return true;
            };
            push_pointer(raw, p, verb);
            return true;
        }
        if !name.starts_with("plugin-widget:") {
            return false;
        }
        self.try_plugin_widget_action(name, raw, true);
        true
    }

    fn try_plugin_widget_action(&mut self, name: &str, raw: &mut egui::RawInput, wake_deferred: bool) -> bool {
        let Some(rest) = name.strip_prefix("plugin-widget:") else {
            return false;
        };
        let slash = rest.find('/').unwrap_or(0);
        let Some(colon) = rest[slash..].find(':').map(|c| c + slash) else {
            eprintln!("--action {name}: want plugin-widget:PLUGIN/WIDGET:CMD");
            return true;
        };
        let (who, cmd) = (&rest[..colon], &rest[colon + 1..]);
        // Resolve within the DataModel in front, including Title/Name aliases.
        let at = self.plugin_front();
        let mut key = format!("{}_{}", context_prefix(at), who.replacen('/', "_", 1));
        if !self.plugin_ui.slots.iter().any(|s| s.key == key) {
            if let Some((pname, wname)) = who.split_once('/') {
                let prefix = format!("{}_{}_", context_prefix(at), pname);
                if let Some(doc) = self.plugin_context_doc(at) {
                    // SAFETY: the context document stays put during this lookup.
                    let doc = unsafe { &*doc };
                    if let Some(s) = self.plugin_ui.slots.iter().find(|s| {
                        s.at == at && s.key.starts_with(&prefix) && doc.exists(s.r)
                            && (str_prop(doc, s.r, "Title") == wname || str_prop(doc, s.r, "Name") == wname)
                    }) {
                        key = s.key.clone();
                    }
                }
            }
        }
        let Some(slot) = self.plugin_ui.slots.iter().find(|s| s.key == key) else {
            if wake_deferred && at == Where::Edit {
                if let Some((pname, _)) = who.split_once('/') {
                    if self.plugins.wake_plugin_by_name(pname).is_some() {
                        self.plugin_ui.pending_widget_actions.push((name.to_string(), 20));
                        return false;
                    }
                }
            }
            if wake_deferred {
                self.push_log(crate::app::Level::Warn, format!("--action {name}: no plugin widget {key}"));
            }
            return false;
        };
        let (r, rect, at) = (slot.r, slot.rect, slot.at);
        let (verb, arg) = cmd.split_once(':').unwrap_or((cmd, ""));
        let xy = || -> Option<egui::Pos2> {
            let (x, y) = arg.split_once(',')?;
            let y = y.split(':').next()?;
            Some(rect?.min + egui::vec2(x.trim().parse().ok()?, y.trim().parse().ok()?))
        };
        match verb {
            "show" => self.plugin_ui.q(at).push(In::Show(r)),
            // popout: into its own OS window (the tab's pop-out button)
            "popout" => {
                if let Some(i) = self.plugin_ui.slots.iter().position(|s| s.key == key) {
                    self.ui.dock_moves.push(crate::panels::DockMove::PopOut(Tab::Plugin(i as u32)));
                }
            }
            "hide" => self.plugin_ui.q(at).push(In::Close(r)),
            "type" => raw.events.push(egui::Event::Text(arg.to_string())),
            // wheel:X,Y:N — N notches (positive = forward) at X,Y.
            "wheel" => {
                let Some(p) = xy() else { return true };
                let n: f32 = arg.rsplit(':').next().and_then(|n| n.parse().ok()).unwrap_or(1.0);
                raw.events.push(egui::Event::PointerMoved(p));
                raw.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, n),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                });
            }
            "shot" => {
                self.plugin_ui.shots.push((key, PathBuf::from(arg)));
            }
            // press:PATH / hover:PATH: the GuiObject at PATH below the widget
            // (`Frame.Button`), at the middle of where it was laid out.
            "press" | "hover" => match self.gui_press_point(Some(&key), arg) {
                Some(p) => push_pointer(raw, p, verb),
                None => self.push_log(
                    crate::app::Level::Warn,
                    format!("--action {name}: no laid-out object {arg} in widget {key}"),
                ),
            },
            "move" | "click" | "rclick" | "down" | "up" => {
                let Some(p) = xy() else {
                    self.push_log(
                        crate::app::Level::Warn,
                        format!("--action {name}: widget {key} isn't drawn (or bad X,Y)"),
                    );
                    return true;
                };
                raw.events.push(egui::Event::PointerMoved(p));
                let button = if verb == "rclick" {
                    egui::PointerButton::Secondary
                } else {
                    egui::PointerButton::Primary
                };
                let presses: &[bool] = match verb {
                    "move" => &[],
                    "down" => &[true],
                    "up" => &[false],
                    _ => &[true, false],
                };
                for &pressed in presses {
                    raw.events.push(egui::Event::PointerButton {
                        pos: p,
                        button,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
            _ => eprintln!("--action {name}: unknown widget command {verb}"),
        }
        true
    }

    /// The shots `plugin_widget_action` asked for: the frame's picture,
    /// cut to each widget's panel content. Call once a frame.
    pub fn plugin_widget_shots(&mut self, ctx: &egui::Context) {
        if self.plugin_ui.shots.is_empty() {
            return;
        }
        if !self.plugin_ui.shot_asked {
            self.plugin_ui.shot_asked = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(PluginShot)));
            ctx.request_repaint();
            return;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, user_data, .. }
                    if user_data.data.as_ref().is_some_and(|d| d.downcast_ref::<PluginShot>().is_some()) =>
                {
                    Some(image.clone())
                }
                _ => None,
            })
        });
        let Some(image) = image else {
            ctx.request_repaint();
            return;
        };
        self.plugin_ui.shot_asked = false;
        let ppp = ctx.pixels_per_point();
        for (key, path) in std::mem::take(&mut self.plugin_ui.shots) {
            let Some(r) = self.plugin_ui.slots.iter().find(|s| s.key == key).and_then(|s| s.rect) else {
                self.push_log(crate::app::Level::Warn, format!("plugin widget shot: {key} isn't drawn"));
                continue;
            };
            let crop = crop(&image, r, ppp);
            match crate::app::save_png(&crop, &path) {
                Ok(()) => {
                    let p = std::path::absolute(&path).unwrap_or(path);
                    let line = format!("[shot] {}", p.display());
                    println!("{line}");
                    self.harness.run.shot_lines.push(line);
                }
                Err(e) => eprintln!("plugin widget shot {}: {e}", path.display()),
            }
        }
    }

    /// `--no-ui`: there is no dock, so each open plugin widget is drawn as a
    /// fixed panel over the view at Studio's size for its dock state (a
    /// Float's client FloatingXSize − 2 × FloatingYSize − 24, docked: the
    /// sizes `initial_size` gives), so plugin GUIs lay out (AbsoluteSize /
    /// AbsolutePosition), draw and take the harness's clicks headless.
    /// Docked Left panels go to the left edge, Right to the right edge,
    /// Bottom / Top along those edges, Floats near the middle.
    pub fn plugin_noui_widgets(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        let mut at: HashMap<&'static str, f32> = HashMap::new();
        for i in 0..self.plugin_ui.slots.len() {
            let s = &mut self.plugin_ui.slots[i];
            if s.open != s.want {
                s.open = s.want;
                s.asked = None;
            }
            if !s.want || s.r.is_none() {
                continue;
            }
            let state = s.state.clone();
            let float = reg().slots.get(i).map(|r| r.float).unwrap_or([0.0, 0.0]);
            let [w, h] = initial_size(&state, float[0] as f64, float[1] as f64);
            let size = egui::vec2(w.min(screen.width()), h.min(screen.height()));
            let side: &'static str = match state.as_str() {
                "Left" => "Left",
                "Right" => "Right",
                "Top" => "Top",
                "Bottom" => "Bottom",
                _ => "Float",
            };
            let off = at.entry(side).or_insert(0.0);
            let min = match side {
                "Left" => egui::pos2(screen.min.x, screen.min.y + *off),
                "Right" => egui::pos2(screen.max.x - size.x, screen.min.y + *off),
                "Top" => egui::pos2(screen.min.x, screen.min.y + *off),
                "Bottom" => egui::pos2(screen.min.x, screen.max.y - size.y - *off),
                _ => egui::pos2(screen.center().x - size.x / 2.0 + *off, screen.center().y - size.y / 2.0 + *off),
            };
            *off += if side == "Float" { 24.0 } else { size.y };
            let rect = egui::Rect::from_min_size(min, size);
            egui::Area::new(egui::Id::new(("plugin-noui-widget", i)))
                .fixed_pos(rect.min)
                .order(egui::Order::Middle)
                .show(ctx, |ui| {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    child.set_clip_rect(rect);
                    child.painter().rect_filled(rect, 0.0, rustudio_engine::theme::BG);
                    self.plugin_widget_ui(&mut child, i as u32);
                });
        }
    }

    /// The GuiObject at `path` below `root`: names joined by `.`, each a
    /// child's Name (the first match), else the first descendant so named;
    /// `@Text` is the first descendant whose Text is that.
    fn gui_at_path(doc: &Document, root: Ref, path: &str) -> Option<Ref> {
        let mut at = root;
        for seg in path.split('.').filter(|s| !s.is_empty()) {
            // `@Text`: the first descendant showing that Text (generic names, as Argon's)
            if let Some(text) = seg.strip_prefix('@') {
                at = doc
                    .dom
                    .descendants_of(at)
                    .find(|i| matches!(i.properties.get(&rbx_dom_weak::ustr("Text")), Some(Variant::String(t)) if t == text))
                    .map(|i| i.referent())?;
                continue;
            }
            let inst = doc.dom.get_by_ref(at)?;
            at = match inst
                .children()
                .iter()
                .copied()
                .find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == seg))
            {
                Some(c) => c,
                None => doc.find_named(at, seg)?,
            };
        }
        Some(at)
    }

    /// `press:PATH` for the harness: the screen point at the middle of the
    /// laid-out GuiObject `PATH` in widget slot `key`, or in CoreGui (key
    /// None, PATH from a CoreGui child), as last drawn.
    fn gui_press_point(&mut self, key: Option<&str>, path: &str) -> Option<egui::Pos2> {
        let doc: *mut Document = edit_doc(self);
        // SAFETY: the edit document stays put while this runs.
        let doc = unsafe { &*doc };
        let (root, gui, origin) = match key {
            Some(k) => {
                let s = self.plugin_ui.slots.iter().find(|s| s.key == k)?;
                (s.r, &s.gui, s.rect?.min)
            }
            None => {
                let core = doc.find_child(doc.dom.root_ref(), "CoreGui")?;
                (core, &self.plugin_ui.core, self.plugin_ui.view.rect?.min)
            }
        };
        let r = Self::gui_at_path(doc, root, path)?;
        let b = gui.layout.get(&r)?;
        Some(origin + egui::vec2(b.pos[0] + b.size[0] / 2.0, b.pos[1] + b.size[1] / 2.0))
    }

    /// Every drawn plugin widget's content rect, by key (screenshots).
    pub fn plugin_widget_rects(&self) -> Vec<(String, Rect)> {
        self.plugin_ui
            .slots
            .iter()
            .filter(|s| s.open)
            .filter_map(|s| Some((s.key.clone(), s.rect?)))
            .collect()
    }
}

/// The harness's pointer onto `p`, and for "press" a left click there.
fn push_pointer(raw: &mut egui::RawInput, p: egui::Pos2, verb: &str) {
    raw.events.push(egui::Event::PointerMoved(p));
    if verb == "press" {
        for pressed in [true, false] {
            raw.events.push(egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }
}

#[derive(Clone, Copy)]
struct PluginShot;

/// `image` cut to `r` (logical pixels).
pub fn crop(image: &egui::ColorImage, r: Rect, ppp: f32) -> egui::ColorImage {
    let (x0, y0) = (
        ((r.min.x * ppp).round() as usize).min(image.size[0]),
        ((r.min.y * ppp).round() as usize).min(image.size[1]),
    );
    let (w, h) = (
        ((r.width() * ppp).round() as usize).min(image.size[0] - x0),
        ((r.height() * ppp).round() as usize).min(image.size[1] - y0),
    );
    let pixels = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| image.pixels[(y0 + y) * image.size[0] + x0 + x])
        .collect();
    egui::ColorImage::new([w, h], pixels)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rbx_dom_weak::{InstanceBuilder, WeakDom};

    use super::*;
    use crate::plugin_host::{PluginHost, PluginSource};

    fn host_with(src: &str) -> (PluginHost, Document) {
        // (the plugin tests' one folder: RUSTUDIO_PLUGIN_DATA is process-wide)
        crate::plugin_host::tests::temp_data();
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let root = dom.root_ref();
        dom.insert(root, InstanceBuilder::new("Script").with_name("Main").with_property("Source", src.to_string()));
        let mut h = PluginHost::default();
        h.load(PluginSource::Tree { name: "user_WTest.rbxmx".into(), asset_id: None, dom: Arc::new(dom) }).unwrap();
        (h, Document::new_place())
    }

    fn run(h: &mut PluginHost, doc: &mut Document, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..n {
            h.tick(doc, 1.0 / 60.0, true, false);
            out.extend(h.take_log().into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()));
        }
        out
    }

    #[test]
    fn float_client_follows_studio() {
        assert_eq!(float_client(320.0, 240.0), [318.0, 216.0]);
        assert_eq!(float_client(260.0, 180.0), [258.0, 156.0]);
        assert_eq!(float_client(150.0, 150.0), [148.0, 126.0]);
        assert_eq!(float_client(0.0, 0.0), [248.0, 326.0]);
        assert_eq!(float_client(200.0, 100.0), [198.0, 100.0]);
    }

    #[test]
    fn play_widget_keys_and_boxes() {
        assert_eq!(context_prefix(Where::Edit), "edit");
        assert_eq!(context_prefix(Where::Play(Side::Server)), "PlayServer");
        assert_eq!(context_prefix(Where::Play(Side::Client)), "PlayClient");
        // Each DataModel's widgets on their own GUI channel (the client's
        // shares the game's: one DataModel, one focused TextBox).
        assert_eq!(channel(Where::Edit), 1);
        assert_eq!(channel(Where::Play(Side::Client)), 0);
        assert_eq!(channel(Where::Play(Side::Server)), 2);
        // The client's game GUI laid out anew keeps a Play widget's box.
        let (w, game) = (Ref::new(), Ref::new());
        note_play_laid(w);
        let b = GuiBox { size: [10.0, 20.0], ..Default::default() };
        let old: HashMap<Ref, GuiBox> = [(w, b), (game, b)].into_iter().collect();
        let mut new: HashMap<Ref, GuiBox> = HashMap::new();
        keep_play_layout(&old, &mut new);
        assert!(new.contains_key(&w) && !new.contains_key(&game));
    }

    #[test]
    fn dock_states_place_as_studio() {
        use crate::panels::{plugin_placement, Spot};
        assert_eq!(plugin_placement("plugin:a", "Left").spot, Spot::Left);
        assert_eq!(plugin_placement("plugin:a", "Right").spot, Spot::Right);
        assert_eq!(plugin_placement("plugin:a", "Top").spot, Spot::Top);
        assert_eq!(plugin_placement("plugin:a", "Bottom").spot, Spot::BottomFull);
        assert!(plugin_placement("plugin:a", "Float").floats);
    }

    #[test]
    fn widgets_yield_size_close_and_drag() {
        let (mut h, mut doc) = host_with(
            r#"
            local f = plugin:CreateDockWidgetPluginGui("F", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Float, true, false, 320, 240, 10, 10))
            print("[T] float", f.AbsoluteSize.X, f.AbsoluteSize.Y, f.Enabled)
            local h = plugin:CreateDockWidgetPluginGui("H", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Left, false, false, 0, 0))
            print("[T] hidden", h.AbsoluteSize.X, h.AbsoluteSize.Y)
            print("[T] rel", f:GetRelativeMousePosition())
            f:GetPropertyChangedSignal("Enabled"):Connect(function() print("[T] enabled", f.Enabled) end)
            h:BindToClose(function() print("[T] bound close") end)
            f.PluginDragDropped:Connect(function(d) print("[T] dropped", d.Sender, d.Data, typeof(d)) end)
            print("[T] drag", pcall(function() return plugin:StartDrag({Sender = "S", Data = "x"}) end))
            "#,
        );
        let out = run(&mut h, &mut doc, 8);
        assert!(out.iter().any(|l| l == "[T] float 318 216 true"), "{out:#?}");
        assert!(out.iter().any(|l| l == "[T] hidden 0 0"), "{out:#?}");
        assert!(out.iter().any(|l| l == "[T] rel 0, 0"), "{out:#?}");
        assert!(out.iter().any(|l| l == "[T] drag true"), "{out:#?}");
        assert!(W::drag_active());
        let items = h.ui_items();
        let get = |id: &str| {
            items.iter().find(|u| matches!(&u.kind, UiKind::DockWidget { id: i, .. } if i == id)).map(|u| u.r).unwrap()
        };
        let (f, hid) = (get("F"), get("H"));
        h.with_edit(&mut doc, |rt| {
            W::close(rt, f);
            W::close(rt, hid);
            W::drag_fire(rt, f, "PluginDragDropped");
            W::drag_end(&rt.lua);
        });
        let out: Vec<String> = h.take_log().into_iter().map(|(_, t)| t.split("  —").next().unwrap_or("").to_string()).collect();
        assert!(out.iter().any(|l| l == "[T] enabled false"), "{out:#?}");
        assert!(out.iter().any(|l| l == "[T] bound close"), "{out:#?}");
        assert!(out.iter().any(|l| l == "[T] dropped S x table"), "{out:#?}");
        assert!(!W::drag_active());
    }

    /// Argon's settings widget: its BindToClose callback destroys the widget
    /// and clears its state; a second × on the destroyed widget's panel must
    /// not run the callback again.
    #[test]
    fn a_destroyed_widget_ignores_close() {
        let (mut h, mut doc) = host_with(
            r#"
            local w = plugin:CreateDockWidgetPluginGui("S", DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Float, true, false, 200, 200))
            local held = w
            w:BindToClose(function()
                print("[T] closing")
                held:Destroy()
                held = nil
            end)
            "#,
        );
        run(&mut h, &mut doc, 4);
        let items = h.ui_items();
        let w = items.iter().find(|u| matches!(&u.kind, UiKind::DockWidget { id, .. } if id == "S")).map(|u| u.r).unwrap();
        h.with_edit(&mut doc, |rt| {
            W::close(rt, w);
            W::close(rt, w);
        });
        let log = h.take_log();
        let closing = log.iter().filter(|(_, t)| t.starts_with("[T] closing")).count();
        assert_eq!(closing, 1, "{log:#?}");
        assert!(!log.iter().any(|(_, t)| t.contains("attempt to index nil")), "{log:#?}");
    }

    #[test]
    fn create_dock_widget_yields_a_frame() {
        let (mut h, mut doc) = host_with(
            r#"
            local n = 0
            game:GetService("RunService").Heartbeat:Connect(function() n += 1 end)
            local w = plugin:CreateDockWidgetPluginGui("Y", DockWidgetPluginGuiInfo.new())
            print("[T] after", n > 0, w.ClassName)
            "#,
        );
        let out = run(&mut h, &mut doc, 4);
        assert!(out.iter().any(|l| l == "[T] after true DockWidgetPluginGui"), "{out:#?}");
    }
}
