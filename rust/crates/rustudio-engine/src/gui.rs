//! GUI drawing and input: ScreenGui, SurfaceGui and BillboardGui with their
//! GuiObjects (Frame, TextLabel/Button/Box, ImageLabel/Button, ScrollingFrame,
//! CanvasGroup, ViewportFrame) and modifiers (UIListLayout, UIGridLayout,
//! UIPadding, UIScale, UIAspectRatioConstraint, UISizeConstraint,
//! UITextSizeConstraint, UICorner, UIGradient, UIStroke), following the public
//! Roblox docs and measured against Roblox Studio (offline Studio; see docs/MECHANICS.md).
//!
//! Every GUI is laid out in its own canvas (pixels), turned into egui shapes,
//! tessellated, clipped in canvas space, and only then mapped to the screen:
//! straight for ScreenGuis, through the 3D camera for SurfaceGuis (so a
//! tilted panel is drawn in perspective) and BillboardGuis. Fonts are Studio's
//! own font files from the content folder (fonts/families/*.json).
//!
//! gui_walk.rs lays out and draws one GUI; gui_input.rs places GUIs on the
//! screen and handles the mouse and keyboard.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::epaint::{Mesh, Vertex};
use egui::{Color32, FontFamily, Pos2, Rect, TextureHandle};
use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant};

use rustudio_core::assets::{AssetStore, Fetch};
use rustudio_render::camera::Camera;
use rustudio_core::doc::{self, Document};
use rustudio_core::mech;
use rustudio_render::scene::{enum_u32, f32_of};

#[path = "gui_input.rs"]
mod input;
#[path = "gui_message.rs"]
mod message;
#[path = "gui_walk.rs"]
mod walk;
pub(crate) use walk::padded as read_padding;
#[path = "gui_fx.rs"]
mod fx;
#[path = "gui_adorn.rs"]
mod adorn;
#[path = "gui_textbox.rs"]
pub mod textbox;

/// Where a GuiObject ended up (the values scripts read as AbsolutePosition /
/// AbsoluteSize / AbsoluteRotation), plus its text's size (TextBounds).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GuiBox {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub rot: f32,
    pub text: Option<[f32; 2]>,
    /// Whether a text object's full layout fits inside its padded box (TextFits).
    pub text_fits: bool,
    /// A ScrollingFrame's canvas (CanvasSize / AutomaticCanvasSize), pixels;
    /// for a UIGridLayout, its AbsoluteCellCount.
    pub canvas: Option<[f32; 2]>,
    /// A ScrollingFrame's AbsoluteWindowSize, pixels;
    /// for a UIGridLayout, its AbsoluteCellSize.
    pub window: Option<[f32; 2]>,
    /// How that canvas was made, so the scroll range can be worked out again
    /// from CanvasSize before the next layout (a glide re-reads it each frame).
    pub canvas_fit: Option<CanvasFit>,
}

/// A ScrollingFrame's layout facts behind `GuiBox::canvas`: the canvas is
/// max(CanvasSize.Scale x parent_content + Offset x scale, floor).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CanvasFit {
    /// The parent's content size that CanvasSize.Scale multiplies.
    pub basis: [f32; 2],
    /// The least canvas (frame less active scrollbar insets, or AutomaticCanvasSize content).
    pub floor: [f32; 2],
    /// The UI scale CanvasSize offsets were multiplied by.
    pub scale: f32,
}

impl GuiBox {
    /// The canvas a ScrollingFrame would get now with CanvasSize `cs`
    /// (scale x, offset x, scale y, offset y), at its last laid-out size.
    pub fn canvas_for(&self, cs: [f32; 4]) -> Option<[f32; 2]> {
        let f = self.canvas_fit?;
        Some([
            (cs[0] * f.basis[0] + cs[1] * f.scale).max(f.floor[0]),
            (cs[2] * f.basis[1] + cs[3] * f.scale).max(f.floor[1]),
        ])
    }
}

/// What the viewport's GUI and the client's scripts tell each other between
/// frames (crates/rustudio-engine/src/domains/gui_input.rs): which TextBox has the keyboard, what
/// is hovered / pressed (GuiObject.GuiState), which images have loaded
/// (ImageLabel.IsLoaded), and the focus changes each side asked for. One
/// play client per process, so one of these.
pub(crate) fn fallback_topbar_inset(width: f32, chat: bool) -> crate::ltypes::LRect {
    crate::ltypes::LRect {
        min: glam::Vec2::new(mech::gui_topbar_left(chat).min(width), 0.0),
        max: glam::Vec2::new(width, mech::GUI_TOP_INSET),
    }
}

#[derive(Default)]
pub struct GuiShare {
    /// The TextBox typing goes to (both sides keep this up to date).
    pub focus: Option<Ref>,
    /// Scripts' CaptureFocus (Some) / ReleaseFocus (None) for the viewport.
    pub focus_req: Vec<Option<Ref>>,
    /// The player's clicks / Enter / Esc changing focus, for the scripts to
    /// fire Focused / FocusLost in Studio's order: (box, gained, enter
    /// pressed, the mouse button and GUI position that caused it).
    pub focus_events: Vec<(Ref, bool, bool, Option<(u8, f32, f32)>)>,
    /// Live 1-based CursorPosition / SelectionStart mirrored between UI and Luau.
    pub tb_cursor: HashMap<Ref, i32>,
    pub tb_sel: HashMap<Ref, i32>,
    /// Script writes to TextBox.CursorPosition / SelectionStart for the UI to pick up.
    pub tb_script_sel: Vec<(Ref, Option<i32>, Option<i32>)>,
    /// Initial 1-based caret position from a mouse click that focused a TextBox.
    pub tb_focus_click: Option<(Ref, usize)>,
    pub hovered: HashSet<Ref>,
    pub pressed: Option<Ref>,
    /// Image content ids decoded for drawing.
    pub loaded: HashSet<String>,
    /// Mouse button presses / releases over the view this frame, in order:
    /// (button, down, a GUI object took it).
    pub mouse_edges: Vec<(u8, bool, bool)>,
    /// Mouse buttons held over the view (1, 2, 3).
    pub buttons: [bool; 3],
    /// Mouse drags of Draggable objects / UIDragDetectors for the scripts:
    /// (object, 0 begin / 1 move / 2 end, GUI x, y).
    pub drags: Vec<(Ref, u8, f32, f32)>,
    /// The 3D view's texture and where it was drawn this frame (UIBlur).
    pub scene: Option<(egui::TextureId, Rect)>,
    /// ParticleEmitter2D:Emit(n) / Clear() from the scripts (n = 0: clear).
    pub p2d: Vec<(Ref, u32)>,
    /// GuiService's inspect menu (game_menu.rs draws it): None closed.
    pub inspect: Option<InspectView>,
    /// The player closed it (back arrow / Esc) since the scripts last looked.
    pub inspect_closed: bool,
    /// UIPageLayout scroll: (page it leaves, page it goes to, tween alpha, direction -1/0/+1).
    pub pages: HashMap<Ref, (Ref, Ref, f32, i8)>,
    /// Bumped whenever `pages` changes so cached ScreenGuis re-walk.
    pub pages_rev: u64,
    /// Mouse-wheel page turns queued by the viewport UI for `UIPageLayout`s
    /// with `ScrollWheelInputEnabled`: `(layout_ref, delta -1/+1)`.
    pub page_scrolls: Vec<(Ref, i8)>,
    /// A handle adornment is hovered or held (the scripts' hit test).
    pub adorn_hot: bool,
    /// ... and it is a Handles / ArcHandles: the press is processed
    /// (UserInputService gameProcessed, no PluginMouse Button1Down: Studio,
    /// checks/plugins/widgets/captures/studio-core.txt). A HandleAdornment
    /// (Model Reflect's plane marker under the cursor) leaves it unprocessed.
    pub adorn_handles: bool,
    /// A CoreGui menu is open (`GuiService:SetMenuIsOpen` / inspect menu).
    pub core_menu_open: bool,
    /// Keyboard / gamepad selection cursor is visible (hidden on mouse input).
    pub selection_visible: bool,
    /// `UserGameSettings.UiNavigationKeyBindEnabled == false` disables `\` toggle.
    pub ui_nav_toggle_disabled: bool,
    /// Pending window fullscreen change requested by `UserGameSettings.Fullscreen`.
    pub fullscreen_req: Option<bool>,
    /// Pending MicroProfiler toggle requested by `UserGameSettings.OnScreenProfilerEnabled`.
    pub microprofiler_req: Option<bool>,
    /// Client `UserGameSettings.SavedQualityLevel` (0 = Automatic, 1..=10 = Manual).
    pub saved_quality: Option<u8>,
}

/// What the inspect menu shows: "<name>'s Avatar", the Robux total of
/// what's for sale, and the worn items (name, price or None off sale);
/// `loading` until the lookups come back.
#[derive(Clone, Debug, Default)]
pub struct InspectView {
    pub title: String,
    pub robux: i64,
    pub items: Vec<(String, Option<i64>)>,
    pub loading: bool,
    pub scroll: f32,
}

/// Three GUI channels: 0 the game's (the Play client and the viewport's
/// GUIs, and the Play client's plugin widgets: one DataModel, one focused
/// TextBox), 1 the edit DataModel's plugins (their dock widgets and
/// CoreGui, src/plugin_host/widgets_ui.rs), 2 the Play server's plugin
/// widgets. `share()` reads the channel this thread selected with
/// `share_channel`; the plugin host selects 1 while the edit VM runs and
/// while plugin GUIs are drawn.
pub fn share() -> std::sync::MutexGuard<'static, GuiShare> {
    static S: std::sync::OnceLock<[std::sync::Mutex<GuiShare>; 3]> = std::sync::OnceLock::new();
    let ch = SHARE_CHANNEL.with(|c| c.get()) as usize;
    S.get_or_init(Default::default)[ch.min(2)].lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    static SHARE_CHANNEL: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// Selects this thread's GUI channel (see `share`) until the guard drops.
pub fn share_channel(ch: u8) -> ShareChannel {
    ShareChannel(SHARE_CHANNEL.with(|c| c.replace(ch)))
}

pub struct ShareChannel(u8);

impl Drop for ShareChannel {
    fn drop(&mut self) {
        let was = self.0;
        SHARE_CHANNEL.with(|c| c.set(was));
    }
}

/// Something for the game to hear about (fired on the client).
#[derive(Debug, Clone)]
pub enum GuiEvent {
    /// Instance, event name, arguments.
    Fire(Ref, &'static str, Vec<GuiArg>),
    /// A TextBox's text typed by the player.
    SetText(Ref, String),
    /// A TextBox edit/navigation commit with Roblox's exact signal ordering
    /// (all fields updated first, then CursorPosition -> SelectionStart -> Text).
    TextBoxUpdate {
        r: Ref,
        text: Option<String>,
        cursor: Option<i32>,
        selection: Option<i32>,
    },
    /// GuiService.SelectedObject moved by keyboard navigation.
    Select(Option<Ref>),
    /// GuiService.SelectedCoreObject moved by keyboard navigation.
    SelectCore(Option<Ref>),
    /// GuiObject.GuiState may have changed (hover / press / focus transition).
    GuiStateChanged(Ref),
    /// ScrollingFrame.CanvasPosition moved by the wheel (pixels).
    Scroll(Ref, [f32; 2]),
}

#[derive(Debug, Clone)]
pub enum GuiArg {
    /// An InputObject: mouse button (1/2, 0 = none), began, position.
    Input {
        button: u8,
        began: bool,
        x: f32,
        y: f32,
    },
    #[allow(dead_code)]
    Bool(bool),
    Num(f64),
}

/// Cached layout of a drawn `TextBox` so pointer clicks, double-clicks, and drags
/// can hit-test exact glyph positions on the UI thread.
#[derive(Clone)]
pub(super) struct TextBoxLayout {
    pub galley: Arc<egui::Galley>,
    /// Text origin in canvas space (already includes horizontal scroll `-scroll_x`).
    pub text_origin: Pos2,
    /// Screen origin of the containing `ScreenGui` (None for world GUIs).
    pub screen_origin: Option<Pos2>,
    aff: walk::Aff,
    pub is_placeholder: bool,
    pub text: String,
}

/// An interactive object, in screen space, in draw order.
#[derive(Debug, Clone)]
struct Hit {
    r: Ref,
    quad: [Pos2; 4],
    clip: Option<[Pos2; 4]>,
    button: bool,
    textbox: bool,
    /// Sinks clicks (Active, buttons, text boxes).
    sink: bool,
    scrolling: bool,
    page_layout: Option<Ref>,
    drag: bool,
    painted: bool,
}

enum Img {
    Ready(TextureHandle, [usize; 2]),
    Failed,
}

/// An image's pixels and its mip levels (`half_image` each), or None when
/// it doesn't decode.
type DecodedImage = (String, Option<(egui::ColorImage, Vec<egui::ColorImage>)>);

fn decode_gui_image(bytes: &[u8]) -> Option<(egui::ColorImage, Vec<egui::ColorImage>)> {
    let img = rustudio_core::texture::decode(bytes).ok()?;
    let size = [img.width as usize, img.height as usize];
    let base = egui::ColorImage::from_rgba_unmultiplied(size, &img.pixels);
    let mut levels = Vec::new();
    let mut cur = base.clone();
    while levels.len() < 8 && (cur.size[0] >= 8 || cur.size[1] >= 8) {
        cur = half_image(&cur);
        levels.push(cur.clone());
    }
    Some((base, levels))
}

/// Decodes an image for 3D effects (particles, sparkles, beams, trails) as
/// straight (unpremultiplied) RGBA8. Studio's ParticleCustomFS,
/// ParticleCurveCrazySparklesFS and TrailFSLight sample unpremultiplied texels
/// and premultiply (or split RGB/A) in the fragment shader after filtering.
fn decode_fx_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = rustudio_core::texture::decode(bytes).ok()?;
    let size = [img.width as usize, img.height as usize];
    // `Color32::from_rgba_premultiplied` stores the four bytes untouched so
    // wgpu uploads and mipmaps straight RGBA8 without egui premultiplication.
    let pixels = img
        .pixels
        .chunks_exact(4)
        .map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    Some(egui::ColorImage {
        size,
        source_size: egui::vec2(size[0] as f32, size[1] as f32),
        pixels,
    })
}

/// Runs `job` on the GUI image decode threads (two, started on first use).
fn decode_off_thread(job: Box<dyn FnOnce() + Send>) {
    use std::sync::{mpsc, Mutex, OnceLock};
    type Job = Box<dyn FnOnce() + Send>;
    static POOL: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..2 {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name(format!("gui-decode-{i}")).spawn(move || loop {
                let job = rx.lock().ok().and_then(|r| r.recv().ok());
                let Some(job) = job else { return };
                let _p = rustudio_core::profiler::scope("gui_image_decode", rustudio_core::profiler::Cat::Assets);
                job();
            });
        }
        Mutex::new(tx)
    });
    if let Ok(tx) = pool.lock() {
        let _ = tx.send(job);
    }
}

/// GUI state kept between frames (textures, fonts, hover, focus).
/// Seconds clock for TextBox blink, double-click, and key-repeat timing:
/// follows simulated time under `--fixed-dt`, otherwise `egui`'s monotonic input time.
pub(super) fn ui_time(ctx: &egui::Context) -> f64 {
    rustudio_core::sim_clock::seconds().unwrap_or_else(|| ctx.input(|i| i.time))
}

#[derive(Default)]
pub struct Gui {
    /// Where the game's mouse is when it isn't the pointer: the middle of
    /// the view while LockCenter holds the cursor (mouse_lock::game_pointer).
    pub pointer_override: Option<Pos2>,
    /// The inset input positions are measured below (InputObject.Position,
    /// MouseEnter's x, y): None, the game's top bar (GUI_TOP_INSET); a
    /// plugin widget's or the edit viewport's CoreGui: 0.
    pub input_inset: Option<f32>,
    /// Whether Roblox's chat button is on the top bar (the Chat CoreGui):
    /// where the bar's free area — GuiService.TopbarInset, and a ScreenGui
    /// with ScreenInsets TopbarSafeInsets — starts (`mech::gui_topbar_left`).
    /// Set by the view before each draw; None reads as on (Roblox's default).
    pub topbar_chat: Option<bool>,
    /// Free rectangle published by the client CoreScripts, in viewport pixels.
    pub topbar_inset: Option<crate::ltypes::LRect>,
    /// The mobile editor supplies the Play top bar and its touch targets.
    pub mobile_play_controls: bool,
    /// SurfaceGui / BillboardGui triangles (AlwaysOnTop false) for the
    /// renderer's depth-tested pass, by egui texture (round 23).
    pub world_batches: Vec<rustudio_render::world_gui::GuiBatch>,
    /// ViewportFrames (drawn by the renderer, vf_render.rs): each one's
    /// render size from its last walk, the texture the renderer drew it
    /// into (`set_vf_textures`), the one that walk composited, and the
    /// frames drawn this frame (walked or from a kept GUI).
    vf_size: HashMap<Ref, [u32; 2]>,
    vf_tex: HashMap<Ref, (egui::TextureId, [u32; 2])>,
    vf_walked: HashMap<Ref, Option<egui::TextureId>>,
    vf_live: HashSet<Ref>,
    images: HashMap<String, Img>,
    /// Images being decoded off the main thread (`texture`), the channel
    /// their pixels come back on, and decoded ones waiting for their upload.
    images_decoding: HashSet<String>,
    image_decode: Option<(std::sync::mpsc::Sender<DecodedImage>, std::sync::mpsc::Receiver<DecodedImage>)>,
    images_ready: std::collections::VecDeque<DecodedImage>,
    /// Straight (unpremultiplied) RGBA textures for 3D effects (`fx_texture`).
    fx_images: HashMap<String, Option<TextureHandle>>,
    fx_images_decoding: HashSet<String>,
    fx_image_decode: Option<(
        std::sync::mpsc::Sender<(String, Option<egui::ColorImage>)>,
        std::sync::mpsc::Receiver<(String, Option<egui::ColorImage>)>,
    )>,
    /// EditableImage textures (runtime/editable.rs) by id without the
    /// generation: the generation shown (updated in place when it moves on).
    editable_gens: HashMap<String, u64>,
    /// Smaller copies of each GUI image (halving, premultiplied box filter):
    /// egui-wgpu makes no mipmaps, Roblox samples GUI images with them, so a
    /// 256 px icon drawn at 48 px was point-sampled (Laser Tag's ammo and
    /// hotbar icons). `mip` picks one by the drawn minification.
    mips: HashMap<String, Vec<egui::TextureHandle>>,
    /// Font file → egui family name, once added to egui.
    fonts: HashMap<String, String>,
    fonts_failed: HashSet<String>,
    /// The document `preload_faces` last ran for.
    faces_preloaded: Option<u64>,
    /// egui font size per Roblox TextSize, by family.
    em_ratio: HashMap<String, f32>,
    /// TextScaled sizes and reusable measurements of their size probes.
    text_fit: walk::TextFitCache,
    hits: Vec<Hit>,
    /// Layout of everything drawn last frame. Kept from frame to frame: a
    /// GUI walked again replaces its own entries (`layout_from`), one no
    /// longer drawn takes them away; `layout_changed` tells the game.
    pub layout: HashMap<Ref, GuiBox>,
    layout_from: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    layout_roots: Vec<Ref>,
    layout_changed: bool,
    hovered: HashSet<Ref>,
    pressed: Option<(Ref, u8)>,
    /// Whether the pointer was over a sinking GuiObject on the last `input` call.
    last_over_sink: bool,
    /// Whether the mouse wheel was over a sinking / scrolling GuiObject on the last `input` call.
    last_wheel_sunk: bool,
    /// The object being dragged with the left button.
    dragging: Option<Ref>,
    /// ParticleEmitter2D particles (gui_fx.rs).
    p2d: HashMap<Ref, fx::Emitter>,
    p2d_clock: Option<std::time::Instant>,
    p2d_dt: f32,
    pub focus: Option<Ref>,
    /// Live editing cursor (1-based UTF-8 byte offset, or -1 unfocused) & selection anchor.
    tb_cursor: i32,
    tb_sel: i32,
    /// Horizontal scroll offset (canvas px) per unwrapped TextBox.
    tb_scroll: HashMap<Ref, f32>,
    /// Last laid-out glyph geometry per drawn TextBox (for mouse click/drag hit-testing).
    tb_layouts: HashMap<Ref, TextBoxLayout>,
    /// TextBox currently being drag-selected with the left mouse button.
    tb_selecting: Option<Ref>,
    /// Last click inside a TextBox for double-click word selection (`egui` input time in seconds).
    tb_last_click: Option<(Ref, f64, Pos2)>,
    /// Caret blink anchor in `egui` input time (reset on every edit/navigation).
    tb_blink: f64,
    /// Internal clipboard mirror (synced alongside OS clipboard via `ctx.copy_text`,
    /// also enables headless Ctrl+C/X/V).
    tb_clipboard: String,
    /// Held navigation/delete key for synthetic key repeat when OS repeats aren't sent
    /// (e.g. headless harness holds). `(key, mods, pressed_at, last_repeat, saw_os_repeat)`.
    tb_held_key: Option<(egui::Key, egui::Modifiers, f64, f64, bool)>,
    /// The character of the last key pressed and when (gui_input.rs
    /// `keyboard`): a script's CaptureFocus from that key's InputBegan must
    /// not receive its text, which can arrive a frame or two later (IME).
    last_key_char: Option<(String, std::time::Instant)>,
    /// That character, to drop once if it arrives right after the focus.
    swallow: Option<(String, std::time::Instant)>,
    pub events: Vec<GuiEvent>,
    /// The last button clicked (the self-test reads it).
    pub last_click: Option<Ref>,
    /// Shapes drawn last frame (the self-test counts them).
    pub drawn: usize,
    /// SurfaceGuis / BillboardGuis under Workspace, in tree order (kept by
    /// `sync_roots`), for the document `roots_doc`.
    world_roots: Vec<Ref>,
    world_roots_set: HashSet<Ref>,
    /// Handle adornments under Workspace (gui_adorn.rs).
    adorn_roots: HashSet<Ref>,
    /// Adornments under the LocalPlayer's PlayerGui (`adorn_pg_of`), kept
    /// from the journal; their tree order once asked for (None: stale).
    adorn_pg: HashSet<Ref>,
    adorn_pg_of: Option<Ref>,
    adorn_pg_order: Option<Vec<Ref>>,
    roots_doc: u64,
    /// face_file per (family, weight, italic), with when it was looked up.
    faces: HashMap<(String, u32, bool), (Option<String>, std::time::Instant)>,
    /// Each GUI root's layout from its last walk (for frames it is skipped).
    root_boxes: rustc_hash::FxHashMap<Ref, Vec<(Ref, GuiBox)>>,
    /// ScreenGuis drawn as they were last frame (gui_input.rs `RootCache`).
    cache: rustc_hash::FxHashMap<Ref, input::RootCache>,
    /// Model billboard anchors, invalidated through the document journal.
    billboard_model_centers: HashMap<Ref, Option<Vec3>>,
    /// Workspace SurfaceGuis' own placement properties (gui_input.rs
    /// `SurfaceSpec`), invalidated through the document journal.
    surface_specs: rustc_hash::FxHashMap<Ref, input::SurfaceSpec>,
    /// Each placed SurfaceSpec's corners for its host part's last CFrame and
    /// Size (gui_input.rs `SurfaceGeo`): recomputed when those change.
    surface_geo: rustc_hash::FxHashMap<Ref, Option<input::SurfaceGeo>>,
    /// Each drawn root's own ZIndexBehavior and ClipsDescendants (gui_input.rs
    /// `RootProps`), dropped with `surface_specs` by the journal.
    root_props: rustc_hash::FxHashMap<Ref, input::RootProps>,
    /// Reads from archived canvases can outlive the active walk's ownership.
    billboard_archive_owner: rustc_hash::FxHashMap<Ref, Ref>,
    /// Recent exact canvas layouts for each billboard.
    billboard_canvases: rustc_hash::FxHashMap<Ref, input::BillboardArchive>,
    /// What a cached ScreenGui's walk read → that ScreenGui: instances whose
    /// properties it read (`cache_owner`) and whose children it listed
    /// (`cache_kids`); per ScreenGui, both lists, to drop them again. A
    /// hidden menu's own changes (Place E animates its hidden Index menu every
    /// frame) touch nothing a walk read, so they leave the cache alone.
    /// (`cache_owner`: read instance → its GUI and the object whose walk
    /// read it.)
    cache_owner: rustc_hash::FxHashMap<Ref, (Ref, Ref, u32)>,
    /// Bumped per `cache_own`: which walk last read an instance.
    own_gen: u32,
    cache_kids: rustc_hash::FxHashMap<Ref, Ref>,
    cache_owned: rustc_hash::FxHashMap<Ref, (Vec<(Ref, Ref)>, Vec<Ref>)>,
    /// The current walk's reads (see `cache_owner`): (instance, reader).
    walk_reads: Vec<(Ref, Ref)>,
    walk_kids: Vec<Ref>,
    /// Each drawn GuiObject's output from its last walk (gui_walk.rs
    /// `Memo`), replayed while its inputs are the same and no mark touched
    /// it or anything under it: Studio's per-node LayoutDirty /
    /// ViewportDirty bits.
    memo: rustc_hash::FxHashMap<Ref, walk::Memo>,
    memo_frames: u32,
    /// RUSTUDIO_GUI_CACHE_VERIFY's second walk: no memo is used.
    memo_off: bool,
    /// This frame's objects drawn from memos / walked (bench).
    memo_hits: u64,
    memo_walks: u64,
    /// What the cached outputs were drawn with besides the document.
    cache_with: input::CacheWith,
    /// Set while walking when something drawn depends on more than the
    /// document (an image still loading, a caret, particles, the 3D view
    /// behind a UIBlur…): that GUI is walked again next frame.
    no_cache: Option<&'static str>,
    /// RUSTUDIO_GUI_CACHE_WHY: the mark that dropped a kept GUI.
    cache_why: HashMap<Ref, String>,
    /// ViewportFrames the current walk drew.
    walk_vfs: Vec<Ref>,
    messages: message::LegacyMessages,
    /// Last `GuiShare::pages_rev` seen by `cache_check`, and the active
    /// `UIPageLayout` refs from that revision.
    pages_rev: u64,
    last_pages: Vec<Ref>,
}

/// Everything a draw needs from the editor.
pub struct DrawIn<'a> {
    pub render_state: Option<&'a egui_wgpu::RenderState>,
    pub doc: &'a Document,
    pub cam: &'a Camera,
    pub rect: Rect,
    pub store: &'a mut AssetStore,
    pub doc_dir: Option<&'a std::path::Path>,
    /// Play (the LocalPlayer's PlayerGui) or edit (StarterGui).
    pub playing: bool,
    /// Fixed-step replays resolve layout synchronously so measured bounds
    /// and captured frames don't depend on the host's clock.
    pub fixed_step: bool,
    /// Which GUIs: the view's (above), or the edit DataModel's plugin GUIs.
    pub roots: Roots,
}

/// The GUIs a `Gui` draws.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Roots {
    /// The game's: PlayerGui / StarterGui ScreenGuis, Workspace's
    /// SurfaceGuis / BillboardGuis, adornments while playing.
    #[default]
    View,
    /// What plugins put in CoreGui (ScreenGuis, BillboardGuis /
    /// SurfaceGuis, handle adornments), over the edit viewport.
    CoreGui,
    /// One DockWidgetPluginGui filling the rect (its panel's content).
    Widget(Ref),
}

// ------------------------------------------------------------------ helpers

fn udim2(d: &Document, r: Ref, name: &str) -> [f32; 4] {
    match d.prop(r, name) {
        Some(Variant::UDim2(u)) => [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32],
        _ => [0.0; 4],
    }
}

fn udim(d: &Document, r: Ref, name: &str) -> [f32; 2] {
    match d.prop(r, name) {
        Some(Variant::UDim(u)) => [u.scale, u.offset as f32],
        _ => [0.0; 2],
    }
}

fn vec2(d: &Document, r: Ref, name: &str, default: [f32; 2]) -> [f32; 2] {
    match d.prop(r, name) {
        Some(Variant::Vector2(v)) => [v.x, v.y],
        _ => default,
    }
}

fn color(d: &Document, r: Ref, name: &str, default: [f32; 3]) -> [f32; 3] {
    match d.prop(r, name) {
        Some(Variant::Color3(c)) => [c.r, c.g, c.b],
        Some(Variant::Color3uint8(c)) => {
            [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0]
        }
        _ => default,
    }
}

fn num(d: &Document, r: Ref, name: &str, default: f32) -> f32 {
    f32_of(d.prop(r, name)).unwrap_or(default)
}

fn boolp(d: &Document, r: Ref, name: &str, default: bool) -> bool {
    match d.prop(r, name) {
        Some(Variant::Bool(b)) => b,
        _ => default,
    }
}

fn text_prop(d: &Document, r: Ref, name: &str) -> String {
    match d.prop(r, name) {
        Some(Variant::String(s)) => s,
        Some(Variant::ContentId(c)) => c.as_str().to_string(),
        Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

fn rgba(c: [f32; 3], alpha: f32) -> Color32 {
    let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(to(c[0]), to(c[1]), to(c[2]), to(alpha))
}

fn children(d: &Document, r: Ref) -> Vec<Ref> {
    d.dom
        .get_by_ref(r)
        .map(|i| i.children().to_vec())
        .unwrap_or_default()
}

/// The first enabled child of a class (UI modifiers).
fn child_of_class(d: &Document, r: Ref, class: &str) -> Option<Ref> {
    d.dom.get_by_ref(r)?.children().iter().copied()
        .find(|c| d.class_of(*c).is_some_and(|k| k == class) && boolp(d, *c, "Enabled", true))
}

fn name_of(d: &Document, r: Ref) -> String {
    d.dom
        .get_by_ref(r)
        .map(|i| i.name.clone())
        .unwrap_or_default()
}

/// Gradient colour and transparency at t (0..1) along a UIGradient.
fn sample_gradient(d: &Document, g: Ref, t: f32) -> ([f32; 3], f32) {
    let t = t.clamp(0.0, 1.0);
    let c = match d.prop(g, "Color") {
        Some(Variant::ColorSequence(seq)) if !seq.keypoints.is_empty() => {
            let k = &seq.keypoints;
            let mut out = [k[0].color.r, k[0].color.g, k[0].color.b];
            for w in k.windows(2) {
                if t >= w[0].time && t <= w[1].time {
                    let f = if w[1].time > w[0].time {
                        (t - w[0].time) / (w[1].time - w[0].time)
                    } else {
                        0.0
                    };
                    out = [
                        w[0].color.r + (w[1].color.r - w[0].color.r) * f,
                        w[0].color.g + (w[1].color.g - w[0].color.g) * f,
                        w[0].color.b + (w[1].color.b - w[0].color.b) * f,
                    ];
                }
            }
            if t > k[k.len() - 1].time {
                let l = &k[k.len() - 1].color;
                out = [l.r, l.g, l.b];
            }
            out
        }
        _ => [1.0, 1.0, 1.0],
    };
    let a = match d.prop(g, "Transparency") {
        Some(Variant::NumberSequence(seq)) if !seq.keypoints.is_empty() => {
            let k = &seq.keypoints;
            let mut out = k[0].value;
            for w in k.windows(2) {
                if t >= w[0].time && t <= w[1].time {
                    let f = if w[1].time > w[0].time {
                        (t - w[0].time) / (w[1].time - w[0].time)
                    } else {
                        0.0
                    };
                    out = w[0].value + (w[1].value - w[0].value) * f;
                }
            }
            if t > k[k.len() - 1].time {
                out = k[k.len() - 1].value;
            }
            out
        }
        _ => 0.0,
    };
    (c, a)
}

/// A UIGradient over a rect, owned (sampled into a table), for text glyphs
/// and stroke paths whose colour callbacks outlive the walk's borrow.
#[derive(Clone)]
struct GradLut {
    lut: Arc<Vec<([f32; 3], f32)>>,
    rect: Rect,
    dir: egui::Vec2,
    offset: [f32; 2],
}

impl GradLut {
    const STEPS: usize = 256;
    fn new(d: &Document, g: Ref, rect: Rect) -> GradLut {
        let rot = num(d, g, "Rotation", 0.0).to_radians();
        GradLut {
            lut: Arc::new((0..Self::STEPS).map(|i| sample_gradient(d, g, i as f32 / (Self::STEPS - 1) as f32)).collect()),
            rect,
            dir: egui::vec2(rot.cos(), rot.sin()),
            offset: vec2(d, g, "Offset", [0.0, 0.0]),
        }
    }
    fn at(&self, p: Pos2) -> ([f32; 3], f32) {
        let u = (p.x - self.rect.center().x) / self.rect.width().max(1e-3) - self.offset[0];
        let v = (p.y - self.rect.center().y) / self.rect.height().max(1e-3) - self.offset[1];
        let t = (u * self.dir.x + v * self.dir.y + 0.5).clamp(0.0, 1.0);
        self.lut[(t * (Self::STEPS - 1) as f32).round() as usize]
    }
    /// A premultiplied colour times the gradient at p.
    fn tint(&self, p: Pos2, c: Color32) -> Color32 {
        let (g, t) = self.at(p);
        let k = 1.0 - t;
        let m = |x: u8, f: f32| (x as f32 * f * k).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_premultiplied(m(c.r(), g[0]), m(c.g(), g[1]), m(c.b(), g[2]), m(c.a(), 1.0))
    }
}

/// A copy of a laid-out galley drawn at `pos` with every vertex colour
/// replaced by `f(screen position, colour)` (UIGradient over text).
fn recolor_galley(
    galley: &Arc<egui::Galley>,
    pos: Pos2,
    color_glyph_override: Option<Color32>,
    f: impl Fn(Pos2, Color32) -> Color32,
) -> Arc<egui::Galley> {
    let mut g = (**galley).clone();
    for pr in &mut g.rows {
        let off = pos.to_vec2() + pr.pos.to_vec2();
        let row = Arc::make_mut(&mut pr.row);
        let color_starts = &row.visuals.color_glyph_vertex_starts;
        for (vi, v) in row.visuals.mesh.vertices.iter_mut().enumerate() {
            let is_color = !color_starts.is_empty()
                && color_starts
                    .iter()
                    .any(|&s| vi >= s as usize && vi < s as usize + 4);
            if is_color {
                if let Some(ov) = color_glyph_override {
                    v.color = ov;
                }
            } else {
                v.color = f(v.pos + off, v.color);
            }
        }
    }
    Arc::new(g)
}

/// A UIGradient over a rect: point → (colour, transparency).
struct Grad<'a> {
    d: &'a Document,
    g: Ref,
    rect: Rect,
    dir: egui::Vec2,
    offset: [f32; 2],
}

impl Grad<'_> {
    fn new(d: &Document, g: Ref, rect: Rect) -> Grad<'_> {
        let rot = num(d, g, "Rotation", 0.0).to_radians();
        Grad {
            d,
            g,
            rect,
            dir: egui::vec2(rot.cos(), rot.sin()),
            offset: vec2(d, g, "Offset", [0.0, 0.0]),
        }
    }
    fn at(&self, p: Pos2) -> ([f32; 3], f32) {
        // Position in the unit square around the centre, minus Offset, along the direction.
        let u = (p.x - self.rect.center().x) / self.rect.width().max(1e-3) - self.offset[0];
        let v = (p.y - self.rect.center().y) / self.rect.height().max(1e-3) - self.offset[1];
        sample_gradient(self.d, self.g, u * self.dir.x + v * self.dir.y + 0.5)
    }
}

/// Quarter-circle unit `(cos, sin)` samples from `0°` to `90°` inclusive (`n + 1` points),
/// cached once per segment count so rounded outlines do zero trig in the hot path.
fn quarter_arc(n: usize) -> &'static [(f32, f32)] {
    use std::sync::OnceLock;
    fn build(n: usize) -> Vec<(f32, f32)> {
        (0..=n)
            .map(|k| {
                if k == 0 {
                    (1.0, 0.0)
                } else if k == n {
                    (0.0, 1.0)
                } else {
                    let a = std::f32::consts::FRAC_PI_2 * k as f32 / n as f32;
                    (a.cos(), a.sin())
                }
            })
            .collect()
    }
    match n {
        4 => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(4)) }
        8 => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(8)) }
        12 => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(12)) }
        16 => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(16)) }
        24 => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(24)) }
        _ => { static T: OnceLock<Vec<(f32, f32)>> = OnceLock::new(); T.get_or_init(|| build(32)) }
    }
}

/// Number of arc segments per quadrant for corner radius `r` (keeps chord sagitta < ~0.12 px).
fn corner_segments(r: f32) -> usize {
    if r <= 2.0 {
        4
    } else if r <= 6.0 {
        8
    } else if r <= 16.0 {
        12
    } else if r <= 36.0 {
        16
    } else if r <= 80.0 {
        24
    } else {
        32
    }
}

/// A rounded rectangle's deduplicated clockwise boundary with exact outward normals.
/// For sharp corners (`r < 0.5`), normals are miter offsets `(±1, ±1)` so `±half * normal`
/// shifts each axis-aligned edge orthogonally by `±half`.
fn rounded_path(rect: Rect, radius: f32) -> Vec<(Pos2, egui::Vec2)> {
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
    if r < 0.5 {
        return vec![
            (rect.left_top(), egui::vec2(-1.0, -1.0)),
            (rect.right_top(), egui::vec2(1.0, -1.0)),
            (rect.right_bottom(), egui::vec2(1.0, 1.0)),
            (rect.left_bottom(), egui::vec2(-1.0, 1.0)),
        ];
    }
    let n = corner_segments(r);
    let arc = quarter_arc(n);
    // Avoid duplicated seam points when opposite corners meet (circles and pills).
    let flat_x = rect.width() > 2.0 * r + 1e-3;
    let flat_y = rect.height() > 2.0 * r + 1e-3;
    let mut pts = Vec::with_capacity(4 * (n + 1));
    let (l, t, ri, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());

    // Q0: top-left, 180° -> 270° (normal: (-c, -s))
    let c0 = Pos2::new(l + r, t + r);
    let end0 = if flat_x { n + 1 } else { n };
    for &(co, si) in &arc[..end0] {
        let normal = egui::vec2(-co, -si);
        pts.push((c0 + normal * r, normal));
    }
    // Q1: top-right, 270° -> 360° (normal: (s, -c))
    let c1 = Pos2::new(ri - r, t + r);
    let end1 = if flat_y { n + 1 } else { n };
    for &(co, si) in &arc[..end1] {
        let normal = egui::vec2(si, -co);
        pts.push((c1 + normal * r, normal));
    }
    // Q2: bottom-right, 0° -> 90° (normal: (c, s))
    let c2 = Pos2::new(ri - r, b - r);
    let end2 = if flat_x { n + 1 } else { n };
    for &(co, si) in &arc[..end2] {
        let normal = egui::vec2(co, si);
        pts.push((c2 + normal * r, normal));
    }
    // Q3: bottom-left, 90° -> 180° (normal: (-s, c))
    let c3 = Pos2::new(l + r, b - r);
    let end3 = if flat_y { n + 1 } else { n };
    for &(co, si) in &arc[..end3] {
        let normal = egui::vec2(-si, co);
        pts.push((c3 + normal * r, normal));
    }
    pts
}

/// A filled convex polygon as a mesh, coloured per point (no feathering; used for
/// axis-aligned sharp rectangles and borders).
fn fill_mesh(pts: &[Pos2], col: &dyn Fn(Pos2) -> Color32) -> Mesh {
    let mut m = Mesh::default();
    if pts.len() < 3 {
        return m;
    }
    let c =
        (pts.iter().fold(egui::Vec2::ZERO, |a, p| a + p.to_vec2()) / pts.len() as f32).to_pos2();
    m.vertices.push(Vertex {
        pos: c,
        uv: egui::epaint::WHITE_UV,
        color: col(c),
    });
    for p in pts {
        m.vertices.push(Vertex {
            pos: *p,
            uv: egui::epaint::WHITE_UV,
            color: col(*p),
        });
    }
    let n = pts.len() as u32;
    for i in 0..n {
        m.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n]);
    }
    m
}

/// Filled rounded rectangle with 1-pixel anti-aliased feathering along its boundary.
/// When `uv` is `Some`, maps vertex UVs inside that sub-rect (clamped to `[0, 1]`).
fn fill_rounded_aa_mesh(
    rect: Rect,
    radius: f32,
    feather: f32,
    uv: Option<Rect>,
    tex: egui::TextureId,
    col: &dyn Fn(Pos2) -> Color32,
    center_vertex: bool,
) -> Mesh {
    let mut m = Mesh::with_texture(tex);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return m;
    }
    let path = rounded_path(rect, radius);
    let n = path.len();
    if n < 3 {
        return m;
    }
    let uv_at = |p: Pos2| match uv {
        Some(u) => {
            let fx = ((p.x - rect.left()) / rect.width().max(1e-6)).clamp(0.0, 1.0);
            let fy = ((p.y - rect.top()) / rect.height().max(1e-6)).clamp(0.0, 1.0);
            Pos2::new(u.left() + u.width() * fx, u.top() + u.height() * fy)
        }
        None => egui::epaint::WHITE_UV,
    };
    if feather <= 0.0 {
        if center_vertex {
            let c = rect.center();
            m.vertices.push(Vertex { pos: c, uv: uv_at(c), color: col(c) });
            for &(p, _) in &path {
                m.vertices.push(Vertex { pos: p, uv: uv_at(p), color: col(p) });
            }
            let nu = n as u32;
            for i in 0..nu {
                m.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % nu]);
            }
        } else {
            for &(p, _) in &path {
                m.vertices.push(Vertex { pos: p, uv: uv_at(p), color: col(p) });
            }
            for i in 2..n as u32 {
                m.indices.extend_from_slice(&[0, i - 1, i]);
            }
        }
        return m;
    }

    let half = 0.5 * feather;
    let in_d = half.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let has_center = center_vertex;
    m.vertices.reserve(2 * n + usize::from(has_center));
    m.indices.reserve(3 * (n * 3));

    if has_center {
        let c = rect.center();
        m.vertices.push(Vertex { pos: c, uv: uv_at(c), color: col(c) });
    }
    let base = m.vertices.len() as u32;
    for &(p, normal) in &path {
        let p_in = p - normal * in_d;
        let p_out = p + normal * half;
        m.vertices.push(Vertex { pos: p_in, uv: uv_at(p_in), color: col(p_in) });
        m.vertices.push(Vertex { pos: p_out, uv: uv_at(p_out), color: Color32::TRANSPARENT });
    }

    let nu = n as u32;
    if has_center {
        for i in 0..nu {
            let next = (i + 1) % nu;
            m.indices.extend_from_slice(&[0, base + i * 2, base + next * 2]);
        }
    } else {
        for i in 2..nu {
            m.indices.extend_from_slice(&[base, base + (i - 1) * 2, base + i * 2]);
        }
    }
    // Anti-aliasing outer fringe (inner opaque -> outer transparent).
    for i in 0..nu {
        let next = (i + 1) % nu;
        let i_in = base + i * 2;
        let i_out = i_in + 1;
        let n_in = base + next * 2;
        let n_out = n_in + 1;
        m.indices.extend_from_slice(&[i_in, i_out, n_out, i_in, n_out, n_in]);
    }
    m
}

/// Anti-aliased closed stroke around `rect` (outside by `thickness`, matching Roblox's
/// `UIStroke` Border mode) with concentric rounded corners and zero miter spikes.
fn stroke_rounded_aa_mesh(
    rect: Rect,
    corner: Option<f32>,
    thickness: f32,
    feather: f32,
    col: &dyn Fn(Pos2) -> Color32,
) -> Mesh {
    let mut m = Mesh::default();
    if thickness <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return m;
    }
    let clamped_r = corner
        .map(|c| c.min(rect.width() * 0.5).min(rect.height() * 0.5).max(0.0))
        .unwrap_or(0.0);
    let mid_rect = rect.expand(thickness * 0.5);
    let mid_r = if clamped_r >= 0.5 { clamped_r + thickness * 0.5 } else { 0.0 };
    let path = rounded_path(mid_rect, mid_r);
    let n = path.len();
    if n < 3 {
        return m;
    }
    let nu = n as u32;
    let half_th = thickness * 0.5;
    let half_f = 0.5 * feather.max(0.0);

    if feather > 0.0 && thickness <= feather {
        // Sub-pixel thin stroke: fade opacity linearly instead of collapsing geometry.
        let opacity = (thickness / feather).clamp(0.0, 1.0);
        m.vertices.reserve(3 * n);
        m.indices.reserve(12 * n);
        for &(p, normal) in &path {
            let p_in = p - normal * feather;
            let p_out = p + normal * feather;
            let c = col(p).gamma_multiply(opacity);
            m.vertices.push(Vertex { pos: p_in, uv: egui::epaint::WHITE_UV, color: Color32::TRANSPARENT });
            m.vertices.push(Vertex { pos: p, uv: egui::epaint::WHITE_UV, color: c });
            m.vertices.push(Vertex { pos: p_out, uv: egui::epaint::WHITE_UV, color: Color32::TRANSPARENT });
        }
        for i in 0..nu {
            let next = (i + 1) % nu;
            let a = i * 3;
            let b = next * 3;
            m.indices.extend_from_slice(&[
                a, a + 1, b + 1, a, b + 1, b,
                a + 1, a + 2, b + 2, a + 1, b + 2, b + 1,
            ]);
        }
    } else if feather > 0.0 {
        // 4 concentric rings: inner AA -> inner solid -> outer solid -> outer AA.
        m.vertices.reserve(4 * n);
        m.indices.reserve(18 * n);
        for &(p, normal) in &path {
            let p0 = p - normal * (half_th + half_f);
            let p1 = p - normal * (half_th - half_f);
            let p2 = p + normal * (half_th - half_f);
            let p3 = p + normal * (half_th + half_f);
            m.vertices.push(Vertex { pos: p0, uv: egui::epaint::WHITE_UV, color: Color32::TRANSPARENT });
            m.vertices.push(Vertex { pos: p1, uv: egui::epaint::WHITE_UV, color: col(p1) });
            m.vertices.push(Vertex { pos: p2, uv: egui::epaint::WHITE_UV, color: col(p2) });
            m.vertices.push(Vertex { pos: p3, uv: egui::epaint::WHITE_UV, color: Color32::TRANSPARENT });
        }
        for i in 0..nu {
            let next = (i + 1) % nu;
            let a = i * 4;
            let b = next * 4;
            m.indices.extend_from_slice(&[
                a, a + 1, b + 1, a, b + 1, b,
                a + 1, a + 2, b + 2, a + 1, b + 2, b + 1,
                a + 2, a + 3, b + 3, a + 2, b + 3, b + 2,
            ]);
        }
    } else {
        m.vertices.reserve(2 * n);
        m.indices.reserve(6 * n);
        for &(p, normal) in &path {
            let p_in = p - normal * half_th;
            let p_out = p + normal * half_th;
            m.vertices.push(Vertex { pos: p_in, uv: egui::epaint::WHITE_UV, color: col(p_in) });
            m.vertices.push(Vertex { pos: p_out, uv: egui::epaint::WHITE_UV, color: col(p_out) });
        }
        for i in 0..nu {
            let next = (i + 1) % nu;
            let a = i * 2;
            let b = next * 2;
            m.indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        }
    }
    m
}

/// A textured quad with smooth anti-aliased rounded corners (an image under a UICorner).
fn rounded_quad_mesh(
    rect: Rect,
    uv: Rect,
    tex: egui::TextureId,
    col: &dyn Fn(Pos2) -> Color32,
    radius: f32,
    feather: f32,
) -> Mesh {
    fill_rounded_aa_mesh(rect, radius, feather, Some(uv), tex, col, true)
}

fn quad_mesh(
    rect: Rect,
    uv: Rect,
    tex: egui::TextureId,
    col: &dyn Fn(Pos2) -> Color32,
    steps: usize,
) -> Mesh {
    let mut m = Mesh::with_texture(tex);
    let n = steps.max(1);
    for j in 0..=n {
        for i in 0..=n {
            let (fx, fy) = (i as f32 / n as f32, j as f32 / n as f32);
            let p = Pos2::new(
                rect.left() + rect.width() * fx,
                rect.top() + rect.height() * fy,
            );
            let t = Pos2::new(uv.left() + uv.width() * fx, uv.top() + uv.height() * fy);
            m.vertices.push(Vertex {
                pos: p,
                uv: t,
                color: col(p),
            });
        }
    }
    let w = (n + 1) as u32;
    for j in 0..n as u32 {
        for i in 0..n as u32 {
            let a = j * w + i;
            m.indices
                .extend_from_slice(&[a, a + 1, a + w, a + 1, a + w + 1, a + w]);
        }
    }
    m
}

fn lerp_vertex(a: &Vertex, b: &Vertex, t: f32) -> Vertex {
    let c = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Vertex {
        pos: a.pos + (b.pos - a.pos) * t,
        uv: a.uv + (b.uv - a.uv) * t,
        color: Color32::from_rgba_premultiplied(
            c(a.color.r(), b.color.r()),
            c(a.color.g(), b.color.g()),
            c(a.color.b(), b.color.b()),
            c(a.color.a(), b.color.a()),
        ),
    }
}

/// Clips a mesh's triangles to a rect (Sutherland–Hodgman).
fn clip_mesh(m: Mesh, clip: Rect) -> Mesh {
    if m.vertices.iter().all(|v| clip.contains(v.pos)) {
        return m;
    }
    let mut out = Mesh::with_texture(m.texture_id);
    for tri in m.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (
            m.vertices.get(tri[0] as usize),
            m.vertices.get(tri[1] as usize),
            m.vertices.get(tri[2] as usize),
        ) else { continue };
        // Each of the four planes adds at most one vertex to a convex polygon.
        let mut poly = [Vertex::default(); 7];
        poly[..3].copy_from_slice(&[*a, *b, *c]);
        let mut len = 3;
        for (axis, val, ge) in [
            (0, clip.left(), true),
            (0, clip.right(), false),
            (1, clip.top(), true),
            (1, clip.bottom(), false),
        ] {
            if len == 0 {
                break;
            }
            let get = |v: &Vertex| if axis == 0 { v.pos.x } else { v.pos.y };
            let ins = |v: &Vertex| if ge { get(v) >= val } else { get(v) <= val };
            let mut next = [Vertex::default(); 7];
            let mut count = 0;
            for k in 0..len {
                let a = &poly[k];
                let b = &poly[(k + 1) % len];
                if ins(a) {
                    next[count] = *a;
                    count += 1;
                }
                if ins(a) != ins(b) {
                    next[count] = lerp_vertex(a, b, (val - get(a)) / (get(b) - get(a)));
                    count += 1;
                }
            }
            poly = next;
            len = count;
        }
        if len >= 3 {
            let base = out.vertices.len() as u32;
            let n = len as u32;
            out.vertices.extend_from_slice(&poly[..len]);
            for k in 1..n - 1 {
                out.indices
                    .extend_from_slice(&[base, base + k, base + k + 1]);
            }
        }
    }
    out
}

/// Splits a SurfaceGui mesh (canvas space) where perspective would bend
/// its colours or texture: `screen` gives a canvas point's screen position
/// and clip-space w. Projection keeps straight edges straight, so a triangle
/// with one colour and one uv is exact as it is; any other is split until
/// screen-space interpolation is within `err` pixels of perspective-correct
/// (about edge × (w_max / w_min − 1) / 4), at most 5 times. Pieces that
/// fall wholly past one edge of `view` (screen) are dropped. A mesh whose
/// whole screen extent passes that test (no triangle in it can fail it)
/// comes back as it is.
///
/// The mesh is borrowed back when it needs no splitting; `projected` then
/// holds each vertex's screen position (as `screen` gives it), else it is
/// left empty.
fn subdivide<'a>(
    m: &'a Mesh,
    err: f32,
    view: Rect,
    screen: &dyn Fn(Pos2) -> Option<(Pos2, f32)>,
    projected: &mut Vec<Pos2>,
) -> std::borrow::Cow<'a, Mesh> {
    use std::borrow::Cow;
    projected.clear();
    {
        let (mut b, mut wlo, mut whi) = (Rect::NOTHING, f32::INFINITY, 0.0f32);
        let mut known = true;
        for v in &m.vertices {
            match screen(v.pos) {
                Some((p, w)) => {
                    projected.push(p);
                    b.extend_with(p);
                    wlo = wlo.min(w);
                    whi = whi.max(w);
                }
                None => {
                    known = false;
                    break;
                }
            }
        }
        // (Behind the camera in part: the caller drops the mesh anyway.)
        if !known {
            projected.clear();
            return Cow::Borrowed(m);
        }
        let screen = |p: Pos2| screen(p).map(|(p, w)| (p, w, ()));
        match subdivide_known(m, err, view, screen, b, wlo, whi, &mut Vec::new()) {
            None => Cow::Borrowed(m),
            Some(out) => {
                projected.clear();
                Cow::Owned(out)
            }
        }
    }
}

/// `subdivide` once every vertex is known to be in front of the camera,
/// with the screen bounds `b` of their projections and their least and
/// greatest clip-space w: None when the mesh needs no splitting. `screen`
/// may give something more with each projection (a world point): the
/// split mesh's vertices then get theirs in `seen`, in order (None: one
/// behind the camera).
#[allow(clippy::too_many_arguments)]
fn subdivide_known<X: Copy>(
    m: &Mesh,
    err: f32,
    view: Rect,
    screen: impl Fn(Pos2) -> Option<(Pos2, f32, X)>,
    b: Rect,
    wlo: f32,
    whi: f32,
    seen: &mut Vec<Option<(Pos2, X)>>,
) -> Option<Mesh> {
    seen.clear();
    if m.vertices.is_empty() || b.size().length() * (whi / wlo - 1.0) * 0.25 <= err {
        return None;
    }
    let flat = |a: &Vertex, b: &Vertex, c: &Vertex| {
        a.uv == b.uv && a.uv == c.uv && a.color == b.color && a.color == c.color
    };
    let tri_flat = |t: &[u32]| match (
        m.vertices.get(t[0] as usize),
        m.vertices.get(t[1] as usize),
        m.vertices.get(t[2] as usize),
    ) {
        (Some(a), Some(b), Some(c)) => flat(a, b, c),
        _ => true,
    };
    if m.indices.chunks_exact(3).all(tri_flat) {
        return None;
    }
    let mut out = Mesh::with_texture(m.texture_id);
    let mut stack: Vec<([Vertex; 3], u8)> = Vec::new();
    for tri in m.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (
            m.vertices.get(tri[0] as usize),
            m.vertices.get(tri[1] as usize),
            m.vertices.get(tri[2] as usize),
        ) else {
            continue;
        };
        stack.push(([*a, *b, *c], 0));
    }
    while let Some(([a, b, c], depth)) = stack.pop() {
        let on_screen = (screen(a.pos), screen(b.pos), screen(c.pos));
        if let (Some((pa, _, _)), Some((pb, _, _)), Some((pc, _, _))) = on_screen {
            let past = |f: &dyn Fn(Pos2) -> bool| f(pa) && f(pb) && f(pc);
            if past(&|p| p.x < view.min.x)
                || past(&|p| p.x > view.max.x)
                || past(&|p| p.y < view.min.y)
                || past(&|p| p.y > view.max.y)
            {
                continue;
            }
        }
        let done = depth >= 5
            || flat(&a, &b, &c)
            || match on_screen {
                (Some((pa, wa, _)), Some((pb, wb, _)), Some((pc, wc, _))) => {
                    let long = (pa - pb).length().max((pb - pc).length()).max((pc - pa).length());
                    let ratio = wa.max(wb).max(wc) / wa.min(wb).min(wc);
                    long * (ratio - 1.0) * 0.25 <= err
                }
                // Behind the camera: the caller drops the mesh anyway.
                _ => true,
            };
        if done {
            let base = out.vertices.len() as u32;
            out.vertices.extend_from_slice(&[a, b, c]);
            out.indices.extend_from_slice(&[base, base + 1, base + 2]);
            let (sa, sb, sc) = on_screen;
            seen.extend([sa, sb, sc].map(|s| s.map(|(p, _, x)| (p, x))));
            continue;
        }
        let (ab, bc, ca) = (
            lerp_vertex(&a, &b, 0.5),
            lerp_vertex(&b, &c, 0.5),
            lerp_vertex(&c, &a, 0.5),
        );
        for t in [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]] {
            stack.push((t, depth + 1));
        }
    }
    Some(out)
}

fn point_in_quad(q: &[Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let a = q[i];
        let b = q[(i + 1) % 4];
        let c = (b - a).x * (p - a).y - (b - a).y * (p - a).x;
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    // A point in line with every edge only happens for a quad with no area
    // (a zero-size GuiObject, such as legacy chat's MessageMode button),
    // which takes no input in Roblox.
    sign != 0.0
}

// ------------------------------------------------------------------ fonts

/// Enum.Font → (family file, weight, italic), as Font.fromEnum maps them
/// (Gotham is Montserrat in current Studio's content folder).
fn legacy_font(name: &str) -> (String, u32, bool) {
    let f = |fam: &str, w: u32, i: bool| (format!("rbxasset://fonts/families/{fam}.json"), w, i);
    match name {
        "Legacy" => f("LegacyArial", 400, false),
        "Arial" => f("Arimo", 400, false),
        "ArialBold" | "ArimoBold" => f("Arimo", 700, false),
        "SourceSans" => f("SourceSansPro", 400, false),
        "SourceSansBold" => f("SourceSansPro", 700, false),
        "SourceSansSemibold" => f("SourceSansPro", 600, false),
        "SourceSansLight" => f("SourceSansPro", 300, false),
        "SourceSansItalic" => f("SourceSansPro", 400, true),
        "Bodoni" => f("AccanthisADFStd", 400, false),
        "Garamond" => f("Guru", 400, false),
        "Cartoon" => f("ComicNeueAngular", 400, false),
        "Code" => f("Inconsolata", 400, false),
        "Highway" => f("HighwayGothic", 400, false),
        "SciFi" => f("Zekton", 400, false),
        "Arcade" => f("PressStart2P", 400, false),
        "Fantasy" => f("Balthazar", 400, false),
        "Antique" => f("RomanAntique", 400, false),
        "Gotham" => f("Montserrat", 400, false),
        "GothamMedium" | "GothamSemibold" => f("Montserrat", 500, false),
        "GothamBold" => f("Montserrat", 700, false),
        "GothamBlack" => f("Montserrat", 900, false),
        "BuilderSans" => f("BuilderSans", 400, false),
        "BuilderSansMedium" => f("BuilderSans", 500, false),
        "BuilderSansBold" => f("BuilderSans", 700, false),
        "BuilderSansExtraBold" => f("BuilderSans", 800, false),
        other => f(other, 400, false),
    }
}

fn weight_of(w: rbx_dom_weak::types::FontWeight) -> u32 {
    use rbx_dom_weak::types::FontWeight as W;
    match w {
        W::Thin => 100,
        W::ExtraLight => 200,
        W::Light => 300,
        W::Regular => 400,
        W::Medium => 500,
        W::SemiBold => 600,
        W::Bold => 700,
        W::ExtraBold => 800,
        W::Heavy => 900,
        _ => 400,
    }
}

/// How much bigger than its TextSize Studio draws the Legacy font (Arimo
/// from fonts/families/LegacyArial.json): a default TextLabel (Font Legacy,
/// TextSize 8) draws "row 1" 26 x 8 ink pixels in Studio's own capture
/// checks/plugins/widgets/captures/studio-wleft.png, Arimo at TextSize 12.
pub(crate) fn legacy_text_scale(font: &(String, u32, bool)) -> f32 {
    if font.0.ends_with("/LegacyArial.json") {
        rustudio_core::mech::GUI_LEGACY_TEXT_SCALE
    } else {
        1.0
    }
}

/// A text object's font: family file, weight, italic. FontFace wins when set;
/// a script that set the legacy Font enum gets that one.
fn font_of(d: &Document, r: Ref) -> (String, u32, bool) {
    let inst = d.dom.get_by_ref(r);
    let has = |n: &str| inst.is_some_and(|i| i.properties.contains_key(&rbx_dom_weak::ustr(n)));
    let face = || match d.prop(r, "FontFace") {
        Some(Variant::Font(f)) => Some((
            // Studio writes Gotham (Font.Gotham*) as GothamSSm.json, a family
            // its content doesn't have; it draws Montserrat for it, as the
            // legacy Gotham fonts here (`legacy_font`).
            if f.family.ends_with("/GothamSSm.json") {
                "rbxasset://fonts/families/Montserrat.json".to_string()
            } else {
                f.family.clone()
            },
            weight_of(f.weight),
            matches!(f.style, rbx_dom_weak::types::FontStyle::Italic),
        )),
        _ => None,
    };
    let legacy = || {
        let v = enum_u32(d.prop(r, "Font"))?;
        let name = doc::db()
            .enums
            .get("Font")?
            .items
            .iter()
            .find(|(_, val)| **val == v)
            .map(|(n, _)| n.to_string())?;
        Some(legacy_font(&name))
    };
    if has("FontFace") {
        if let Some(f) = face() {
            return f;
        }
    }
    if has("Font") {
        if let Some(f) = legacy() {
            return f;
        }
    }
    face()
        .or_else(legacy)
        .unwrap_or_else(|| legacy_font("SourceSans"))
}

/// The face file for a family/weight/style: the nearest weight (same style
/// first) whose file is here or is a cloud asset still to download (Laser
/// Tag's Builder Mono, rbxassetid://16658246179, has only cloud faces: they
/// were never fetched, so its timer drew in the default font). `failed`
/// faces (download refused) are skipped for the nearest one here.
fn face_file(
    store: &mut AssetStore,
    family: &str,
    weight: u32,
    italic: bool,
    failed: &HashSet<String>,
) -> Option<String> {
    static FAMILIES: std::sync::Mutex<Option<HashMap<String, Vec<(u32, bool, String)>>>> =
        std::sync::Mutex::new(None);
    let mut g = FAMILIES.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if !map.contains_key(family) {
        let faces = match store.content(family, None) {
            Fetch::Ready(b) => serde_json::from_slice::<serde_json::Value>(&b)
                .ok()
                .and_then(|j| {
                    Some(
                        j.get("faces")?
                            .as_array()?
                            .iter()
                            .filter_map(|f| {
                                Some((
                                    f.get("weight")?.as_u64()? as u32,
                                    f.get("style")?.as_str()? == "italic",
                                    f.get("assetId")?.as_str()?.to_string(),
                                ))
                            })
                            .collect::<Vec<_>>(),
                    )
                }),
            _ => None,
        }
        .unwrap_or_default();
        map.insert(family.to_string(), faces);
    }
    let available = |id: &str| -> bool {
        if failed.contains(id) {
            false
        } else if id.starts_with("rbxasset://") {
            rustudio_core::assets::resolve(id, None).is_some_and(|p| p.is_file())
        } else {
            // here, or fetched by `family` (Pending until it arrives)
            rustudio_core::assets::asset_ref(id).is_some()
        }
    };
    let mut best: Option<(u32, &String)> = None;
    for (w, it, id) in map.get(family)? {
        if !available(id) {
            continue;
        }
        let cost = w.abs_diff(weight) + if *it != italic { 1000 } else { 0 };
        if best.is_none_or(|(c, _)| cost < c) {
            best = Some((cost, id));
        }
    }
    best.map(|(_, id)| id.clone())
}

/// Fonts for glyphs a game font lacks, matching Roblox Studio's fallback order:
/// Builder Sans & Source Sans Pro (for display fonts lacking Latin/symbols),
/// RobloxEmoji (Robux/Verified/Premium PUA), TwemojiMozilla (colour emoji,
/// before Arimo/CJK so emoji render in colour), Arimo (Hebrew/Cyrillic/Greek),
/// Studio's Noto UI script faces (Arabic, Bengali, Devanagari, Georgian,
/// Khmer, Myanmar, Sinhala, Thai), Studio's PlatformContent NotoSansCJKjp
/// (Chinese/Japanese/Korean), then system symbol fonts as a last resort.
pub(crate) fn fallback_fonts() -> &'static [&'static [u8]] {
    static FONTS: std::sync::OnceLock<Vec<&'static [u8]>> = std::sync::OnceLock::new();
    FONTS.get_or_init(|| {
        let content = rustudio_core::assets::content_dir().join("fonts");
        let mut files: Vec<std::path::PathBuf> = [
            "BuilderSans-Regular.otf",
            "SourceSansPro-Regular.ttf",
            "RobloxEmoji.ttf",
            "TwemojiMozilla.ttf",
            "Arimo-Regular.ttf",
        ]
        .iter()
        .map(|f| content.join(f))
        .collect();
        let mut scripts: Vec<std::path::PathBuf> = std::fs::read_dir(&content)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("NotoSans") || n.starts_with("NotoNaskh"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        scripts.sort();
        files.extend(scripts);
        if let Some(studio) = rustudio_core::default_scripts::studio_dir()
            .or_else(|| rustudio_core::assets::content_dir().parent().map(|p| p.to_path_buf()))
        {
            let cjk = studio.join("PlatformContent/pc/fonts/NotoSansCJKjp-Regular.otf");
            if cjk.is_file() {
                files.push(cjk);
            }
        }
        files.extend(rustudio_core::platform::fallback_font_files().iter().map(std::path::PathBuf::from));
        files
            .iter()
            .filter_map(|p| std::fs::read(p).ok())
            .map(|b| &*Box::leak(b.into_boxed_slice()))
            .collect()
    })
}

/// Shared `FontData` for a fallback font with Roblox Studio's per-face
/// line-height normalization enabled.
pub(crate) fn fallback_font_data(bytes: &'static [u8]) -> egui::FontData {
    let mut tweak = egui::epaint::text::FontTweak::default();
    tweak.normalize_line_height = true;
    egui::FontData::from_static(bytes).tweak(tweak)
}

impl Gui {
    /// Faces that weren't there, looked for again once a second: true when
    /// one has turned up (what was drawn without it must be walked again).
    /// A GUI with text in a missing face (the test place's 200 NPC name tags)
    /// was walked every frame so that `family` would ask again.
    pub(crate) fn retry_missing_faces(&mut self, store: &mut AssetStore) -> bool {
        let due: Vec<(String, u32, bool)> = self
            .faces
            .iter()
            .filter(|(k, (p, at))| p.is_none() && at.elapsed().as_secs_f32() >= 1.0 && store.allows(&k.0))
            .map(|(k, _)| k.clone())
            .collect();
        let mut found = false;
        for key in due {
            let p = face_file(store, &key.0, key.1, key.2, &self.fonts_failed);
            found |= p.is_some();
            self.faces.insert(key, (p, std::time::Instant::now()));
        }
        found
    }

    /// Adds every face the document's text objects name, once per
    /// document, so egui rebuilds its glyph atlas once for them instead of
    /// once per face as GUIs first show text in it (each rebuild re-walks
    /// every kept GUI: a game added faces 3–4 times during a round).
    pub(crate) fn preload_faces(&mut self, ctx: &egui::Context, store: &mut AssetStore, d: &Document) {
        if self.faces_preloaded == Some(d.id) {
            return;
        }
        self.faces_preloaded = Some(d.id);
        let _p = rustudio_core::bench::scope("gui_preload_faces");
        let mut seen = HashSet::new();
        let texts: Vec<Ref> = d
            .dom
            .descendants()
            .filter(|i| matches!(i.class.as_str(), "TextLabel" | "TextButton" | "TextBox"))
            .map(|i| i.referent())
            .collect();
        for r in texts {
            let font = font_of(d, r);
            if seen.insert(font.clone()) {
                self.family(ctx, store, &font);
            }
        }
        self.no_cache = None;
    }

    /// The egui family for a font (Studio's file for the nearest available
    /// face); egui's default font until the file has been added (next frame).
    fn family(
        &mut self,
        ctx: &egui::Context,
        store: &mut AssetStore,
        font: &(String, u32, bool),
    ) -> FontFamily {
        // face_file stats every face's file: once per face, not per text
        // object per frame (60 µs each, most of Place B's GUI walk). A face
        // not here yet is looked for again a second later.
        if !store.allows(&font.0) {
            return FontFamily::Proportional;
        }
        let key = font.clone();
        let path = match self.faces.get(&key) {
            Some((p, at)) if p.is_some() || at.elapsed().as_secs_f32() < 1.0 => p.clone(),
            _ => {
                let p = face_file(store, &font.0, font.1, font.2, &self.fonts_failed);
                self.faces.insert(key, (p.clone(), std::time::Instant::now()));
                p
            }
        };
        let Some(path) = path else {
            // (looked for again in a second, by `retry_missing_faces`: GUIs
            // drawn without it are kept until it turns up)
            return FontFamily::Proportional;
        };
        if !store.allows(&path) {
            return FontFamily::Proportional;
        }
        if let Some(name) = self.fonts.get(&path) {
            let fam = FontFamily::Name(name.clone().into());
            let ready = ctx.fonts(|f| f.definitions().families.contains_key(&fam));
            if !ready { self.no_cache = Some("font not ready"); }
            return if ready { fam } else { FontFamily::Proportional };
        }
        if self.fonts_failed.contains(&path) {
            return FontFamily::Proportional;
        }
        let bytes = match store.content(&path, None) {
            Fetch::Ready(b) => b,
            Fetch::Pending => {
                self.no_cache = Some("font loading");
                return FontFamily::Proportional;
            }
            Fetch::Unavailable(_) => {
                // (the next lookup takes the nearest face that is here)
                self.faces.retain(|_, (p, _)| p.as_deref() != Some(path.as_str()));
                self.fonts_failed.insert(path);
                return FontFamily::Proportional;
            }
        };
        let name = format!("rbx:{path}");
        register_font_ligatures(&name, &bytes);
        let family = FontFamily::Name(name.clone().into());
        ctx.add_font(egui::epaint::text::FontInsert::new(
            &name,
            egui::FontData::from_owned(bytes.to_vec()),
            vec![egui::epaint::text::InsertFontFamily {
                family: family.clone(),
                priority: egui::epaint::text::FontPriority::Highest,
            }],
        ));
        // Glyphs the face lacks ("▶" in Sarpanch, emoji, CJK, Arabic, ...)
        // come from the shared fallback fonts after it, as Roblox's text falls
        // back. Sharing the fallback FontFace keys across families avoids
        // re-parsing large fonts (e.g. 16 MB NotoSansCJKjp, 1.3 MB Twemoji) per family.
        for (i, bytes) in fallback_fonts().iter().enumerate() {
            ctx.add_font(egui::epaint::text::FontInsert::new(
                &format!("rbx:fallback{i}"),
                fallback_font_data(bytes),
                vec![egui::epaint::text::InsertFontFamily {
                    family: family.clone(),
                    priority: egui::epaint::text::FontPriority::Lowest,
                }],
            ));
        }
        self.fonts.insert(path, name);
        // (the face is used from the next frame)
        self.no_cache = Some("font added");
        FontFamily::Proportional
    }

    /// An egui font for a legacy Font enum name at a Roblox TextSize (the
    /// em mapping of gui_walk.rs `em_per_size`), for text the engine draws
    /// itself (the Backpack hotbar).
    fn core_font(
        &mut self,
        ctx: &egui::Context,
        store: &mut AssetStore,
        legacy: &str,
        text_size: f32,
    ) -> egui::FontId {
        let family = self.family(ctx, store, &legacy_font(legacy));
        let key = format!("{family:?}");
        let k = match self.em_ratio.get(&key) {
            Some(k) => *k,
            None => {
                let line = ctx.fonts_mut(|f| f.row_height(&egui::FontId::new(100.0, family.clone()))) / 100.0;
                let k = rustudio_core::mech::GUI_FONT_EM_PER_TEXT_SIZE / line.max(0.5);
                if matches!(family, FontFamily::Name(_)) {
                    self.em_ratio.insert(key, k);
                }
                k
            }
        };
        egui::FontId::new(text_size * k, family)
    }

    /// The Backpack hotbar as Roblox's CoreScript draws it (Studio 0.744
    /// ExtraContent/scripts/CoreScripts/Modules/BackpackScript.lua): ICON_SIZE
    /// 60 slots ICON_BUFFER 5 apart, centred in a 70 px bar on the bottom
    /// edge; slot BACKGROUND_COLOR 31,31,31 at slotFadeLocked 0.5; the
    /// TextureId image at 80 % (10 % in); the tool's name (SourceSans 14,
    /// wrapped) only when TextureId is empty; the slot number (SourceSans 14
    /// in a 15 % box at the top left); the equipped slot's four edge frames,
    /// SLOT_EQUIP_THICKNESS 0.1 of the slot, SLOT_EQUIP_COLOR 90,142,233, over
    /// the number. Slots are (name, TextureId, equipped); returns their rects.
    pub fn draw_hotbar(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        store: &mut AssetStore,
        doc_dir: Option<&std::path::Path>,
        view: Rect,
        slots: &[(String, String, bool)],
    ) -> Vec<Rect> {
        const ICON: f32 = 60.0;
        const BUFFER: f32 = 5.0;
        let n = slots.len() as f32;
        let bar_w = BUFFER + n * (ICON + BUFFER);
        let bar = Rect::from_min_size(
            Pos2::new(view.center().x - bar_w / 2.0, view.max.y - (ICON + 2.0 * BUFFER)),
            egui::vec2(bar_w, ICON + 2.0 * BUFFER),
        );
        let mut rects = Vec::with_capacity(slots.len());
        for (i, (name, texture, equipped)) in slots.iter().enumerate() {
            // slot:Readjust: centered - ICON/2 + (ICON+BUFFER) * (index - (total/2 + 0.5))
            let factor = (i + 1) as f32 - (n / 2.0 + 0.5);
            let x = bar.min.x + bar_w / 2.0 - ICON / 2.0 + (ICON + BUFFER) * factor;
            let r = Rect::from_min_size(Pos2::new(x, bar.min.y + BUFFER), egui::vec2(ICON, ICON));
            rects.push(r);
            painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(31, 31, 31, 128));
            let icon = (!texture.is_empty())
                .then(|| self.texture(ctx, store, doc_dir, texture))
                .flatten();
            if let Some((tex, [tw, th])) = icon {
                let minify = (tw as f32 / (ICON * 0.8)).min(th as f32 / (ICON * 0.8)) * ctx.pixels_per_point().recip();
                painter.image(
                    self.mip(texture, tex, minify),
                    Rect::from_min_size(r.min + egui::vec2(ICON * 0.1, ICON * 0.1), egui::vec2(ICON * 0.8, ICON * 0.8)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else if texture.is_empty() {
                let font = self.core_font(ctx, store, "SourceSans", 14.0);
                let inner = r.shrink(1.0);
                let galley = ctx.fonts_mut(|f| f.layout(name.clone(), font, Color32::WHITE, inner.width()));
                let at = inner.center() - galley.size() / 2.0;
                painter.galley(at, galley, Color32::WHITE);
            }
            if i < 9 {
                let font = self.core_font(ctx, store, "SourceSans", 14.0);
                let b = Rect::from_min_size(r.min, egui::vec2(ICON * 0.15, ICON * 0.15));
                painter.text(b.center(), egui::Align2::CENTER_CENTER, format!("{}", i + 1), font, Color32::WHITE);
            }
            if *equipped {
                let t = ICON * 0.1;
                let c = Color32::from_rgb(90, 142, 233);
                for e in [
                    Rect::from_min_size(r.min, egui::vec2(t, ICON)),
                    Rect::from_min_size(r.min + egui::vec2(t, 0.0), egui::vec2(ICON - 2.0 * t, t)),
                    Rect::from_min_size(r.min + egui::vec2(ICON - t, 0.0), egui::vec2(t, ICON)),
                    Rect::from_min_size(r.min + egui::vec2(t, ICON - t), egui::vec2(ICON - 2.0 * t, t)),
                ] {
                    painter.rect_filled(e, 0.0, c);
                }
            }
        }
        rects
    }

    /// A game's mouse cursor image (UserInputService.MouseIcon / Mouse.Icon)
    /// drawn centred on the pointer at its own pixel size, as Roblox does.
    /// False while the image isn't loaded (the system cursor stays then).
    pub fn draw_cursor(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        store: &mut AssetStore,
        doc_dir: Option<&std::path::Path>,
        id: &str,
        at: Pos2,
    ) -> bool {
        let Some((tex, size)) = self.texture(ctx, store, doc_dir, id) else {
            return false;
        };
        let s = egui::vec2(size[0] as f32, size[1] as f32);
        painter.image(
            tex,
            crate::mouse_lock::icon_rect(at, s, ctx.pixels_per_point()),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        true
    }

    /// Whether an image went through `texture` (GUI images and, since
    /// round 26, effect textures): loaded, failed, or never asked for (None).
    pub fn image_state(&self, id: &str) -> Option<Result<(), String>> {
        match self.images.get(id)? {
            Img::Ready(..) => Some(Ok(())),
            Img::Failed => Some(Err("not decoded or unavailable".into())),
        }
    }

    /// An image as an egui texture (and its pixel size), loaded on first use.
    pub fn texture(
        &mut self,
        ctx: &egui::Context,
        store: &mut AssetStore,
        doc_dir: Option<&std::path::Path>,
        id: &str,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        if let Some((base, gen)) = rustudio_core::editable_source::image_key(id) {
            // (EditableImage pixels change without a document mark)
            self.no_cache = Some("editable image");
            if self.editable_gens.get(base).is_some_and(|h| *h >= gen) {
                if let Some(Img::Ready(h, s)) = self.images.get(base) {
                    return Some((h.id(), *s));
                }
            }
            let (now, img) = rustudio_core::editable_source::image_pixels(id)?;
            let size = [img.width as usize, img.height as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &img.pixels);
            let opts = egui::TextureOptions::NEAREST;
            let out = match self.images.get_mut(base) {
                Some(Img::Ready(h, s)) => {
                    h.set(color, opts);
                    *s = size;
                    (h.id(), size)
                }
                _ => {
                    let h = ctx.load_texture(format!("gui:{base}"), color, opts);
                    let out = (h.id(), size);
                    self.images.insert(base.to_string(), Img::Ready(h, size));
                    out
                }
            };
            self.editable_gens.insert(base.to_string(), now.max(gen));
            return Some(out);
        }
        if !store.allows(id) {
            return None;
        }
        if self.images_decoding.contains(id) {
            self.poll_images(ctx);
        }
        if let Some(i) = self.images.get(id) {
            return match i {
                Img::Ready(h, s) => Some((h.id(), *s)),
                Img::Failed => None,
            };
        }
        if self.images_decoding.contains(id) {
            self.no_cache = Some("image loading");
            return None;
        }
        let _load = rustudio_core::bench::scope("gui_image_load");
        let mut bytes = match store.content(id, doc_dir) {
            Fetch::Ready(b) => b,
            Fetch::Pending => {
                self.no_cache = Some("image loading");
                return None;
            }
            Fetch::Unavailable(_) => {
                self.images.insert(id.to_string(), Img::Failed);
                return None;
            }
        };
        // A Decal asset (XML) points at its image.
        if bytes.starts_with(b"<roblox") {
            let inner = Document::decode(&bytes).ok().and_then(|dom| {
                dom.descendants().find_map(|i| {
                    match i.properties.get(&rbx_dom_weak::ustr("Texture")) {
                        Some(Variant::ContentId(c)) => Some(c.as_str().to_string()),
                        Some(Variant::Content(c)) => c.as_uri().map(|s| s.to_string()),
                        _ => None,
                    }
                })
            });
            match inner.map(|i| store.content(&i, doc_dir)) {
                Some(Fetch::Ready(b)) => bytes = b,
                Some(Fetch::Pending) => {
                    self.no_cache = Some("decal image loading");
                    return None;
                }
                _ => {
                    self.images.insert(id.to_string(), Img::Failed);
                    return None;
                }
            }
        }
        // Fixed-step runs decode here, so captures don't depend on the
        // host's timing; otherwise the decode and mip levels are made on
        // the decode threads and the image shows from the frame they land
        // (as while it downloads).
        if rustudio_core::sim_clock::on() {
            let decoded = decode_gui_image(&bytes);
            return self.finish_image(ctx, id, decoded);
        }
        let (tx, _) = self.image_decode.get_or_insert_with(std::sync::mpsc::channel);
        let tx = tx.clone();
        let repaint = ctx.clone();
        let name = id.to_string();
        decode_off_thread(Box::new(move || {
            let decoded = decode_gui_image(&bytes);
            let _ = tx.send((name, decoded));
            repaint.request_repaint();
        }));
        self.images_decoding.insert(id.to_string());
        self.no_cache = Some("image loading");
        None
    }

    /// Makes the textures of images decoded off the main thread, at most
    /// about 8 MB of new texels a frame (the rest the next frames: one image
    /// at least).
    fn poll_images(&mut self, ctx: &egui::Context) {
        if let Some((_, rx)) = &self.image_decode {
            self.images_ready.extend(rx.try_iter());
        }
        let mut budget: usize = 8 << 20;
        let mut first = true;
        while let Some((_, decoded)) = self.images_ready.front() {
            let bytes = decoded.as_ref().map_or(0, |(base, _)| base.pixels.len() * 4 * 4 / 3);
            if !first && bytes > budget {
                ctx.request_repaint();
                break;
            }
            first = false;
            budget = budget.saturating_sub(bytes);
            let (id, decoded) = self.images_ready.pop_front().expect("front");
            self.images_decoding.remove(&id);
            self.finish_image(ctx, &id, decoded);
        }
    }

    /// An image's textures (mip levels first, then the image, as they were
    /// always made) from its decoded pixels.
    fn finish_image(
        &mut self,
        ctx: &egui::Context,
        id: &str,
        decoded: Option<(egui::ColorImage, Vec<egui::ColorImage>)>,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        let Some((base, levels)) = decoded else {
            self.images.insert(id.to_string(), Img::Failed);
            return None;
        };
        let levels = levels
            .into_iter()
            .enumerate()
            .map(|(i, level)| ctx.load_texture(format!("gui:{id}#{}", i + 1), level, egui::TextureOptions::LINEAR))
            .collect();
        self.mips.insert(id.to_string(), levels);
        let size = base.size;
        let h = ctx.load_texture(format!("gui:{id}"), base, egui::TextureOptions::LINEAR);
        let out = (h.id(), size);
        self.images.insert(id.to_string(), Img::Ready(h, size));
        share().loaded.insert(id.to_string());
        Some(out)
    }

    /// Straight (unpremultiplied) RGBA texture for 3D effects (ParticleEmitter,
    /// Sparkles, Beam, Trail). Kept separate from `texture` so egui's
    /// premultiplied GUI textures stay untouched while 3D effect shaders get
    /// exact unpremultiplied RGB and A channels for filtering and mipmapping.
    pub fn fx_texture(
        &mut self,
        ctx: &egui::Context,
        store: &mut AssetStore,
        doc_dir: Option<&std::path::Path>,
        id: &str,
    ) -> Option<egui::TextureId> {
        if let Some((_, rx)) = &self.fx_image_decode {
            let ready: Vec<_> = rx.try_iter().collect();
            for (name, decoded) in ready {
                self.fx_images_decoding.remove(&name);
                let handle = decoded.map(|img| ctx.load_texture(format!("fx:{name}"), img, egui::TextureOptions::LINEAR));
                self.fx_images.insert(name, handle);
            }
        }
        if !store.allows(id) {
            return None;
        }
        if let Some(entry) = self.fx_images.get(id) {
            return entry.as_ref().map(|h| h.id());
        }
        if self.fx_images_decoding.contains(id) {
            return None;
        }
        let mut bytes = match store.content(id, doc_dir) {
            Fetch::Ready(b) => b,
            Fetch::Pending => return None,
            Fetch::Unavailable(_) => {
                self.fx_images.insert(id.to_string(), None);
                return None;
            }
        };
        if bytes.starts_with(b"<roblox") {
            let inner = Document::decode(&bytes).ok().and_then(|dom| {
                dom.descendants().find_map(|i| {
                    match i.properties.get(&rbx_dom_weak::ustr("Texture")) {
                        Some(Variant::ContentId(c)) => Some(c.as_str().to_string()),
                        Some(Variant::Content(c)) => c.as_uri().map(|s| s.to_string()),
                        _ => None,
                    }
                })
            });
            match inner.map(|i| store.content(&i, doc_dir)) {
                Some(Fetch::Ready(b)) => bytes = b,
                Some(Fetch::Pending) => return None,
                _ => {
                    self.fx_images.insert(id.to_string(), None);
                    return None;
                }
            }
        }
        if rustudio_core::sim_clock::on() {
            let handle = decode_fx_image(&bytes).map(|img| ctx.load_texture(format!("fx:{id}"), img, egui::TextureOptions::LINEAR));
            let tid = handle.as_ref().map(|h| h.id());
            self.fx_images.insert(id.to_string(), handle);
            return tid;
        }
        let (tx, _) = self.fx_image_decode.get_or_insert_with(std::sync::mpsc::channel);
        let tx = tx.clone();
        let repaint = ctx.clone();
        let name = id.to_string();
        decode_off_thread(Box::new(move || {
            let decoded = decode_fx_image(&bytes);
            let _ = tx.send((name, decoded));
            repaint.request_repaint();
        }));
        self.fx_images_decoding.insert(id.to_string());
        None
    }

    /// The texture to draw image `id` with when `minify` texels of its level
    /// 0 fall on each drawn pixel: level floor(log2(minify)), or `base`.
    pub(crate) fn mip(&self, id: &str, base: egui::TextureId, minify: f32) -> egui::TextureId {
        if !(minify >= 2.0) {
            return base;
        }
        let level = minify.log2().floor() as usize;
        match self.mips.get(id) {
            Some(l) if !l.is_empty() => l[level.min(l.len()) - 1].id(),
            _ => base,
        }
    }

}

impl Gui {
    /// The ViewportFrames drawn this frame (their parts make up the
    /// viewport's ViewportFrame scene).
    pub fn viewport_frames(&self) -> &HashSet<Ref> {
        &self.vf_live
    }

    /// The last laid-out render size of a ViewportFrame, if known.
    pub fn vf_size_of(&self, r: Ref) -> Option<[u32; 2]> {
        self.vf_size.get(&r).copied()
    }

    /// What each ViewportFrame drawn this frame shows, read now: its size
    /// from the last walk, its CurrentCamera, Ambient, LightColor and
    /// LightDirection. Frames without a camera aren't drawn.
    pub fn vf_requests(&self, d: &Document) -> HashMap<Ref, rustudio_render::render::vf::Request> {
        let mut out = HashMap::new();
        for &r in &self.vf_live {
            let Some(&size) = self.vf_size.get(&r) else { continue };
            if let Some(req) = vf_request_for(d, r, size) {
                out.insert(r, req);
            }
        }
        out
    }

    /// The textures the renderer drew the ViewportFrames into.
    pub fn set_vf_textures(&mut self, tex: impl Fn(Ref) -> Option<(egui::TextureId, [u32; 2])>) {
        self.vf_tex.clear();
        for &r in &self.vf_live {
            if let Some(t) = tex(r) {
                self.vf_tex.insert(r, t);
            }
        }
        let live = &self.vf_live;
        self.vf_size.retain(|r, _| live.contains(r));
        self.vf_walked.retain(|r, _| live.contains(r));
    }
}

/// Builds a `vf::Request` for a `ViewportFrame` at `size` (shared by live GUI
/// draws and `ViewportFrame:CaptureSnapshotAsync`). Returns `None` if it has no
/// valid `CurrentCamera`.
pub fn vf_request_for(d: &Document, r: Ref, size: [u32; 2]) -> Option<rustudio_render::render::vf::Request> {
    if !d.exists(r) {
        return None;
    }
    let cam = match d.prop(r, "CurrentCamera") {
        Some(Variant::Ref(c)) if d.exists(c) => c,
        _ => return None,
    };
    let xf = rustudio_render::scene::xform_of(d, cam)?;
    let fov = f32_of(d.prop(cam, "FieldOfView")).unwrap_or(70.0).to_radians();
    let ambient = color(d, r, "Ambient", [200.0 / 255.0; 3]);
    let light = color(d, r, "LightColor", [140.0 / 255.0; 3]);
    let mut ldir = rustudio_render::scene::vec3_of(d.prop(r, "LightDirection")).unwrap_or(Vec3::splat(-1.0));
    let length2 = ldir.length_squared();
    let tolerance = if (length2.abs() + 1.0).is_infinite() { 1.0e-5 } else { (length2.abs() + 1.0) * 1.0e-5 };
    if !(tolerance < length2.abs()) {
        ldir = Vec3::splat(-1.0);
    }
    let bg = color(d, r, "BackgroundColor3", [1.0; 3]);
    let bt = f32_of(d.prop(r, "BackgroundTransparency")).unwrap_or(0.0).clamp(0.0, 1.0);
    Some(rustudio_render::render::vf::Request {
        size: [size[0].clamp(1, 4096), size[1].clamp(1, 4096)],
        cam: xf,
        fov_y: fov,
        ambient,
        light,
        light_dir: ldir.normalize_or_zero(),
        background: [bg[0], bg[1], bg[2], 1.0 - bt],
    })
}

/// Picks a snapshot render size for a `ViewportFrame` (laid-out size if given,
/// else its `Size` UDim2 offset, else 128×128).
pub fn vf_snapshot_size(d: &Document, r: Ref, laid_out: Option<[u32; 2]>) -> [u32; 2] {
    if let Some(s) = laid_out {
        if s[0] > 0 && s[1] > 0 {
            return [s[0].clamp(1, 512), s[1].clamp(1, 512)];
        }
    }
    if let Some(Variant::UDim2(u)) = d.prop(r, "Size") {
        let w = if u.x.offset > 0 { u.x.offset as u32 } else { 128 };
        let h = if u.y.offset > 0 { u.y.offset as u32 } else { 128 };
        return [w.clamp(1, 512), h.clamp(1, 512)];
    }
    [128, 128]
}

#[derive(Default)]
struct VfSnapQueue {
    next_id: u64,
    pending: Vec<(u64, Ref)>,
    done: HashMap<u64, String>,
}

fn vf_snaps() -> std::sync::MutexGuard<'static, VfSnapQueue> {
    static Q: std::sync::OnceLock<std::sync::Mutex<VfSnapQueue>> = std::sync::OnceLock::new();
    Q.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Queues a `ViewportFrame:CaptureSnapshotAsync` request; returns its poll id.
pub fn vf_snap_request(vf: Ref) -> u64 {
    let mut q = vf_snaps();
    q.next_id += 1;
    let id = q.next_id;
    q.pending.push((id, vf));
    id
}

/// Pending snapshot requests to render and read back this frame.
pub fn vf_snap_pending() -> Vec<(u64, Ref)> {
    vf_snaps().pending.clone()
}

/// Marks a snapshot request finished with its `rbxtemp://…` content id (or `""`).
pub fn vf_snap_complete(id: u64, content_id: String) {
    let mut q = vf_snaps();
    q.pending.retain(|(i, _)| *i != id);
    q.done.insert(id, content_id);
}

/// Polls a snapshot request (returns `Some(content_id)` once complete).
pub fn vf_snap_poll(id: u64) -> Option<String> {
    vf_snaps().done.remove(&id)
}

/// An image at half size (odd edges keep their last texel), each texel the
/// mean of up to four premultiplied texels.
fn half_image(src: &egui::ColorImage) -> egui::ColorImage {
    let [w, h] = src.size;
    let (nw, nh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut out = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (sx, sy) = (x * 2 + dx, y * 2 + dy);
                if sx < w && sy < h {
                    let c = src.pixels[sy * w + sx];
                    for (a, v) in acc.iter_mut().zip(c.to_array()) {
                        *a += v as u32;
                    }
                    n += 1;
                }
            }
            let v = |i: usize| ((acc[i] + n / 2) / n) as u8;
            out.push(Color32::from_rgba_premultiplied(v(0), v(1), v(2), v(3)));
        }
    }
    egui::ColorImage::new([nw, nh], out)
}

// ------------------------------------------------------------------ rich text

/// One run of rich text.
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<[f32; 3]>,
    size: Option<f32>,
    transparency: Option<f32>,
    stroke_transparency: Option<f32>,
}

fn parse_color(s: &str) -> Option<[f32; 3]> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let v = u32::from_str_radix(h, 16).ok()?;
        return Some([
            ((v >> 16) & 255) as f32 / 255.0,
            ((v >> 8) & 255) as f32 / 255.0,
            (v & 255) as f32 / 255.0,
        ]);
    }
    let inner = s.strip_prefix("rgb(").and_then(|x| x.strip_suffix(')'))?;
    let v: Vec<f32> = inner
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    (v.len() == 3).then(|| [v[0] / 255.0, v[1] / 255.0, v[2] / 255.0])
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let i = tag.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    let q = rest.chars().next()?;
    if q == '"' || q == '\'' {
        let end = rest[1..].find(q)?;
        Some(rest[1..1 + end].to_string())
    } else {
        Some(
            rest.split_whitespace()
                .next()?
                .trim_end_matches('>')
                .to_string(),
        )
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Roblox RichText: <b> <i> <u> <s> <font color size transparency weight>
/// <br/> <uppercase>/<uc> <smallcaps>/<sc> (as uppercase) and the XML escapes.
fn rich_runs(src: &str) -> Vec<Run> {
    #[derive(Clone, Default)]
    struct St {
        b: bool,
        i: bool,
        u: bool,
        s: bool,
        upper: bool,
        color: Option<[f32; 3]>,
        size: Option<f32>,
        tr: Option<f32>,
        stroke_tr: Option<f32>,
    }
    let mut stack: Vec<(String, St)> = Vec::new();
    let mut st = St::default();
    let mut out = Vec::new();
    let mut rest = src;
    let push = |out: &mut Vec<Run>, text: &str, st: &St| {
        if text.is_empty() {
            return;
        }
        let t = unescape(text);
        out.push(Run {
            text: if st.upper { t.to_uppercase() } else { t },
            bold: st.b,
            italic: st.i,
            underline: st.u,
            strike: st.s,
            color: st.color,
            size: st.size,
            transparency: st.tr,
            stroke_transparency: st.stroke_tr,
        });
    };
    while let Some(lt) = rest.find('<') {
        push(&mut out, &rest[..lt], &st);
        let Some(gt) = rest[lt..].find('>') else {
            push(&mut out, &rest[lt..], &st);
            return out;
        };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let lower = tag.trim().to_ascii_lowercase();
        if lower == "br" || lower == "br/" || lower == "br /" {
            push(&mut out, "\n", &st);
            continue;
        }
        if let Some(name) = lower.strip_prefix('/') {
            if let Some(pos) = stack.iter().rposition(|(n, _)| n == name.trim()) {
                st = stack[pos].1.clone();
                stack.truncate(pos);
            }
            continue;
        }
        let name = lower
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        let prev = st.clone();
        match name.as_str() {
            "b" => st.b = true,
            "i" => st.i = true,
            "u" => st.u = true,
            "s" => st.s = true,
            "uppercase" | "uc" | "smallcaps" | "sc" => st.upper = true,
            "font" => {
                if let Some(c) = attr(tag, "color").and_then(|c| parse_color(&c)) {
                    st.color = Some(c);
                }
                if let Some(s) = attr(tag, "size").and_then(|s| s.parse().ok()) {
                    st.size = Some(s);
                }
                if let Some(t) = attr(tag, "transparency").and_then(|s| s.parse().ok()) {
                    st.tr = Some(t);
                }
                if attr(tag, "weight").is_some_and(|w| {
                    w.eq_ignore_ascii_case("bold") || w.parse::<u32>().is_ok_and(|n| n >= 600)
                }) {
                    st.b = true;
                }
            }
            "stroke" => {
                if let Some(t) = attr(tag, "transparency").and_then(|s| s.parse().ok()) {
                    st.stroke_tr = Some(t);
                }
            }
            "mark" | "comment" => {}
            _ => {
                // Not a tag Roblox knows: shown as text.
                push(&mut out, &format!("<{tag}>"), &st);
                continue;
            }
        }
        if !tag.trim_end().ends_with('/') {
            stack.push((name, prev));
        }
    }
    push(&mut out, rest, &st);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GUI images get halved copies with premultiplied means (odd edges kept).
    #[test]
    fn half_image_box_filter() {
        let px = |a: u8| Color32::from_rgba_premultiplied(a, a, a, a);
        let img = egui::ColorImage::new([3, 2], vec![px(0), px(255), px(10), px(255), px(0), px(30)]);
        let h = half_image(&img);
        assert_eq!(h.size, [2, 1]);
        assert_eq!(h.pixels[0], px(128));
        assert_eq!(h.pixels[1], px(20));
    }

    #[test]
    fn rich_text_runs() {
        let runs = rich_runs(
            "a <b>bold</b> <font color=\"#FF0000\" size=\"20\">red</font><br/>x &lt;y&gt;",
        );
        let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["a ", "bold", " ", "red", "\n", "x <y>"]);
        assert!(runs[1].bold);
        assert_eq!(runs[3].color, Some([1.0, 0.0, 0.0]));
        assert_eq!(runs[3].size, Some(20.0));
    }

    #[test]
    fn clip_keeps_inside() {
        let m = quad_mesh(
            Rect::from_min_max(Pos2::new(-10.0, -10.0), Pos2::new(10.0, 10.0)),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            egui::TextureId::default(),
            &|_| Color32::WHITE,
            1,
        );
        let c = clip_mesh(m, Rect::from_min_max(Pos2::ZERO, Pos2::new(5.0, 5.0)));
        assert!(c.vertices.iter().all(|v| v.pos.x >= -1e-4
            && v.pos.x <= 5.0001
            && v.pos.y >= -1e-4
            && v.pos.y <= 5.0001));
        assert!(!c.indices.is_empty());
    }

    #[test]
    fn point_in_turned_quad() {
        let q = [
            Pos2::new(0.0, 5.0),
            Pos2::new(5.0, 0.0),
            Pos2::new(10.0, 5.0),
            Pos2::new(5.0, 10.0),
        ];
        assert!(point_in_quad(&q, Pos2::new(5.0, 5.0)));
        assert!(!point_in_quad(&q, Pos2::new(0.5, 0.5)));
        // Keep edges and corners interactive, with either winding order.
        for q in [q, [q[3], q[2], q[1], q[0]]] {
            assert!(point_in_quad(&q, Pos2::new(2.5, 2.5)));
            assert!(point_in_quad(&q, q[0]));
            assert!(point_in_quad(&q, Pos2::new(5.0, 5.0)));
        }
    }

    #[test]
    fn rounded_path_deduplicates_circles_and_pills() {
        let circle = Rect::from_min_size(Pos2::ZERO, egui::vec2(44.0, 44.0));
        let pts = rounded_path(circle, 22.0);
        assert_eq!(pts.len(), 64); // 4 * 16 segments, zero duplicates
        for i in 0..pts.len() {
            let (p0, n0) = pts[i];
            let (p1, _) = pts[(i + 1) % pts.len()];
            assert!((p1 - p0).length() > 1e-3, "duplicate vertex at {i}");
            assert!((n0.length() - 1.0).abs() < 1e-4, "non-unit normal at {i}");
        }

        let pill = Rect::from_min_size(Pos2::ZERO, egui::vec2(96.0, 44.0));
        let p_pts = rounded_path(pill, 22.0);
        assert_eq!(p_pts.len(), 66); // 64 + 2 tangent endpoints along flat top/bottom
        for i in 0..p_pts.len() {
            let (p0, _) = p_pts[i];
            let (p1, _) = p_pts[(i + 1) % p_pts.len()];
            assert!((p1 - p0).length() > 1e-3, "duplicate pill vertex at {i}");
        }
    }

    #[test]
    fn rounded_aa_fill_and_stroke_have_feathered_edges_and_valid_indices() {
        let r = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(96.0, 44.0));
        let fill = fill_rounded_aa_mesh(
            r,
            22.0,
            1.0,
            None,
            egui::TextureId::default(),
            &|_| Color32::WHITE,
            false,
        );
        assert!(fill.is_valid());
        assert!(fill.vertices.iter().any(|v| v.color == Color32::TRANSPARENT));
        assert!(fill.vertices.iter().any(|v| v.color == Color32::WHITE));

        let stroke = stroke_rounded_aa_mesh(r, Some(22.0), 2.0, 1.0, &|_| Color32::WHITE);
        assert!(stroke.is_valid());
        let max_bound = r.expand(3.0);
        // No miter spikes: every vertex stays tightly within rect + thickness + feather.
        assert!(stroke.vertices.iter().all(|v| max_bound.contains(v.pos)));

        // Textured rounded quad clamps UVs to the requested sub-rect.
        let uv = Rect::from_min_max(Pos2::new(0.25, 0.25), Pos2::new(0.75, 0.75));
        let img = rounded_quad_mesh(r, uv, egui::TextureId::default(), &|_| Color32::WHITE, 12.0, 1.0);
        assert!(img.is_valid());
        assert!(img.vertices.iter().all(|v| {
            v.uv.x >= 0.2499 && v.uv.x <= 0.7501 && v.uv.y >= 0.2499 && v.uv.y <= 0.7501
        }));
    }

    #[test]
    fn point_in_zero_size_quad() {
        // A zero-size object (all four corners at one point) is hit nowhere.
        let p = Pos2::new(12.0, 147.3);
        assert!(!point_in_quad(&[p; 4], Pos2::new(480.0, 300.0)));
        assert!(!point_in_quad(&[p; 4], p));

        // A single collapsed dimension is also noninteractive, even on the
        // resulting line or its endpoints. Include rotated and reversed quads.
        for end in [
            Pos2::new(10.0, 0.0),
            Pos2::new(0.0, 10.0),
            Pos2::new(10.0, 10.0),
        ] {
            for q in [
                [Pos2::ZERO, end, end, Pos2::ZERO],
                [Pos2::ZERO, Pos2::ZERO, end, end],
            ] {
                for p in [
                    Pos2::ZERO,
                    end,
                    Pos2::new(end.x * 0.5, end.y * 0.5),
                    Pos2::new(20.0, 3.0),
                ] {
                    assert!(!point_in_quad(&q, p), "quad {q:?} accepted {p:?}");
                }
            }
        }
    }
}

#[cfg(test)]
mod calibrate {
    /// Prints egui's width for Roblox's measured strings (TextService:GetTextSize
    /// in Studio: "START GAME" Oswald 14 → 46×14, 18 → 58×18, 24 → 78×24, 32 → 104×32).
    #[test]
    fn oswald_width() {
        let path = rustudio_core::assets::content_dir().join("fonts/Oswald-Regular.ttf");
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let ctx = egui::Context::default();
        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert(
            "oswald".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        defs.families.insert(
            egui::FontFamily::Name("oswald".into()),
            vec!["oswald".into()],
        );
        ctx.set_fonts(defs);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for s in [14.0f32, 18.0, 24.0, 32.0] {
                let g = ui.ctx().fonts_mut(|f| {
                    f.layout_job(egui::text::LayoutJob::simple_singleline(
                        "START GAME".into(),
                        egui::FontId::new(s, egui::FontFamily::Name("oswald".into())),
                        egui::Color32::WHITE,
                    ))
                });
                println!("CAL oswald {s}: {:.2} x {:.2}", g.size().x, g.size().y);
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn fallback_color_emoji_and_bidi_metrics() {
        let ss_path = rustudio_core::assets::content_dir().join("fonts/SourceSansPro-Regular.ttf");
        let Ok(ss_bytes) = std::fs::read(&ss_path) else {
            return;
        };
        let ctx = egui::Context::default();
        let mut defs = egui::FontDefinitions::default();
        defs.font_data.insert(
            "ss".into(),
            std::sync::Arc::new(egui::FontData::from_owned(ss_bytes)),
        );
        let mut fam_list = vec!["ss".to_string()];
        for (i, bytes) in super::fallback_fonts().iter().enumerate() {
            let key = format!("rbx:fallback{i}");
            defs.font_data
                .insert(key.clone(), std::sync::Arc::new(super::fallback_font_data(bytes)));
            fam_list.push(key);
        }
        let fam = egui::FontFamily::Name("ss".into());
        defs.families.insert(fam.clone(), fam_list);
        ctx.set_fonts(defs);

        let mut output = ctx.run_ui(Default::default(), |ui| {
            let line = ui.ctx().fonts_mut(|f| f.row_height(&egui::FontId::new(100.0, fam.clone()))) / 100.0;
            let em = rustudio_core::mech::GUI_FONT_EM_PER_TEXT_SIZE / line.max(0.5);
            let font_id = egui::FontId::new(24.0 * em, fam.clone());

            // 1. Color emoji (COLR/CPAL) rasterize as color glyphs and match Studio's exact 21px width at 24px.
            for sample in ["🏆", "🛒", "🎟", "❤️", "👨‍👩‍👧‍👦", "🇺🇸", "1️⃣"] {
                let mut f = egui::TextFormat::simple(font_id.clone(), egui::Color32::from_rgb(255, 70, 70));
                f.studio_text_size = Some(24.0);
                f.line_height = Some(24.0);
                let mut job = egui::text::LayoutJob::default();
                job.append(sample, 0.0, f);
                job.round_output_to_gui = false;
                let g = ui.ctx().fonts_mut(|fo| fo.layout_job(job));
                let w = g.rect.width().ceil();
                assert_eq!(w, 21.0, "{sample:?} width={w} (Studio=21)");
                let row = &g.rows[0].row;
                assert!(
                    !row.visuals.color_glyph_vertex_starts.is_empty(),
                    "{sample:?} should render as a COLR color glyph"
                );
                // Color glyph vertex must be white (un-tinted), not red (255, 70, 70).
                let v0 = row.visuals.mesh.vertices[row.visuals.color_glyph_vertex_starts[0] as usize];
                assert_eq!(v0.color, egui::Color32::WHITE, "{sample:?} color glyph was tinted");
            }

            // 2. Hebrew + Latin RTL paragraph visually reorders "שלום עולם Hello 42" -> "Hello 42" on left.
            let mut f = egui::TextFormat::simple(font_id.clone(), egui::Color32::WHITE);
            f.studio_text_size = Some(24.0);
            f.line_height = Some(24.0);
            let mut job = egui::text::LayoutJob::default();
            job.append("שלום עולם Hello 42", 0.0, f);
            job.round_output_to_gui = false;
            let g = ui.ctx().fonts_mut(|fo| fo.layout_job(job));
            let first_chr = g.rows[0].row.glyphs.first().map(|gl| gl.chr);
            assert_eq!(first_chr, Some('H'), "RTL paragraph should place 'Hello 42' on the left");
        });
        output.textures_delta.clear();
    }
}

/// Faces the GUI walk has drawn text with (font -> egui family, em per
/// TextSize), and its egui context: what `text_bounds_now` measures with.
pub type FaceKey = (String, u32, bool);
static TEXT_CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();
static TEXT_FACES: std::sync::Mutex<Option<HashMap<FaceKey, (FontFamily, f32)>>> = std::sync::Mutex::new(None);
/// OpenType GSUB Type-4 ligatures (e.g. BuilderIcons `"tilt"` -> U+F200) per egui font family name.
static FONT_LIGATURES: std::sync::Mutex<Option<HashMap<String, HashMap<String, char>>>> = std::sync::Mutex::new(None);

fn parse_ttf_ligatures(data: &[u8]) -> Option<HashMap<String, char>> {
    let u16be = |off: usize| -> Option<u16> { Some(u16::from_be_bytes([*data.get(off)?, *data.get(off + 1)?])) };
    let i16be = |off: usize| -> Option<i16> { Some(i16::from_be_bytes([*data.get(off)?, *data.get(off + 1)?])) };
    let u32be = |off: usize| -> Option<u32> {
        Some(u32::from_be_bytes([*data.get(off)?, *data.get(off + 1)?, *data.get(off + 2)?, *data.get(off + 3)?]))
    };
    let num_tables = u16be(4)? as usize;
    let mut cmap_off = None;
    let mut gsub_off = None;
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let tag = data.get(rec..rec + 4)?;
        let off = u32be(rec + 8)? as usize;
        if tag == b"cmap" { cmap_off = Some(off); }
        else if tag == b"GSUB" { gsub_off = Some(off); }
    }
    let (c_off, g_off) = (cmap_off?, gsub_off?);
    // Parse cmap format 4 to build glyph_id -> char
    let n_sub = u16be(c_off + 2)? as usize;
    let mut g2c: HashMap<u16, char> = HashMap::new();
    for i in 0..n_sub {
        let sub_off = u32be(c_off + 4 + i * 8 + 4)? as usize;
        let so = c_off + sub_off;
        if u16be(so)? != 4 { continue; }
        let seg_count = (u16be(so + 6)? / 2) as usize;
        let end_base = so + 14;
        let start_base = end_base + seg_count * 2 + 2;
        let delta_base = start_base + seg_count * 2;
        let ro_base = delta_base + seg_count * 2;
        for s in 0..seg_count {
            let ec = u16be(end_base + s * 2)?;
            if ec == 0xFFFF { continue; }
            let sc = u16be(start_base + s * 2)?;
            let dl = i16be(delta_base + s * 2)? as i32;
            let ro = u16be(ro_base + s * 2)? as usize;
            for cp in sc..=ec {
                let gid = if ro == 0 {
                    ((cp as i32 + dl) & 0xFFFF) as u16
                } else {
                    let ptr = ro_base + s * 2 + ro + (cp - sc) as usize * 2;
                    let raw = u16be(ptr)?;
                    if raw == 0 { 0 } else { ((raw as i32 + dl) & 0xFFFF) as u16 }
                };
                if gid != 0 {
                    if let Some(ch) = char::from_u32(cp as u32) {
                        g2c.entry(gid).or_insert(ch);
                    }
                }
            }
        }
    }
    // Parse GSUB LookupList -> Type 4 (LigatureSubst)
    let ll_off = g_off + u16be(g_off + 8)? as usize;
    let n_lookups = u16be(ll_off)? as usize;
    let mut ligs: HashMap<String, char> = HashMap::new();
    for i in 0..n_lookups {
        let lo = ll_off + u16be(ll_off + 2 + i * 2)? as usize;
        let ltype = u16be(lo)?;
        if ltype != 4 { continue; }
        let n_st = u16be(lo + 4)? as usize;
        for j in 0..n_st {
            let so = lo + u16be(lo + 6 + j * 2)? as usize;
            let cov_off = so + u16be(so + 2)? as usize;
            let cov_fmt = u16be(cov_off)?;
            let mut cov = Vec::new();
            if cov_fmt == 1 {
                let n = u16be(cov_off + 2)? as usize;
                for k in 0..n { cov.push(u16be(cov_off + 4 + k * 2)?); }
            } else if cov_fmt == 2 {
                let n = u16be(cov_off + 2)? as usize;
                for k in 0..n {
                    let a = u16be(cov_off + 4 + k * 6)?;
                    let b = u16be(cov_off + 6 + k * 6)?;
                    for g in a..=b { cov.push(g); }
                }
            }
            let n_ls = u16be(so + 4)? as usize;
            for (ci, &first_gid) in cov.iter().take(n_ls).enumerate() {
                let lso = so + u16be(so + 6 + ci * 2)? as usize;
                let n_l = u16be(lso)? as usize;
                for li in 0..n_l {
                    let ligo = lso + u16be(lso + 2 + li * 2)? as usize;
                    let lig_gid = u16be(ligo)?;
                    let cc = u16be(ligo + 2)? as usize;
                    let Some(&lig_ch) = g2c.get(&lig_gid) else { continue };
                    let mut seq = String::with_capacity(cc);
                    let mut ok = true;
                    if let Some(&ch0) = g2c.get(&first_gid) {
                        seq.push(ch0);
                    } else {
                        ok = false;
                    }
                    for ci2 in 0..cc.saturating_sub(1) {
                        let g = u16be(ligo + 4 + ci2 * 2)?;
                        if let Some(&ch) = g2c.get(&g) { seq.push(ch); } else { ok = false; break; }
                    }
                    if ok && !seq.is_empty() {
                        ligs.insert(seq, lig_ch);
                    }
                }
            }
        }
    }
    (!ligs.is_empty()).then_some(ligs)
}

fn register_font_ligatures(family_name: &str, bytes: &[u8]) {
    let Some(ligs) = parse_ttf_ligatures(bytes) else { return };
    let mut guard = FONT_LIGATURES.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(family_name.to_string(), ligs);
}

pub(super) fn apply_font_ligatures<'a>(family: &FontFamily, text: &'a str) -> Cow<'a, str> {
    let FontFamily::Name(name) = family else { return Cow::Borrowed(text) };
    let guard = FONT_LIGATURES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ligs) = guard.as_ref().and_then(|m| m.get(name.as_ref())) else {
        return Cow::Borrowed(text);
    };
    if let Some(&ch) = ligs.get(text) {
        return Cow::Owned(ch.to_string());
    }
    Cow::Borrowed(text)
}

pub fn legacy_face_key(enum_name: &str) -> FaceKey {
    legacy_font(enum_name)
}

pub fn normalize_face_key(family: &str, weight: u32, italic: bool) -> FaceKey {
    let fam = if family.ends_with("/GothamSSm.json") {
        "rbxasset://fonts/families/Montserrat.json".to_string()
    } else {
        family.to_string()
    };
    (fam, weight, italic)
}

/// Standalone measure-only `Fonts` backed by Studio's local content fonts +
/// fallback chain, used by `measure_text_string` / `text_bounds_now` when a
/// requested face has not yet been registered in `TEXT_FACES` (or before the
/// first GUI frame).
struct StandaloneFonts {
    defs: egui::FontDefinitions,
    fonts: egui::epaint::text::Fonts,
    fallback_keys: Vec<String>,
    faces: HashMap<std::path::PathBuf, FontFamily>,
}

static STANDALONE_FONTS: std::sync::Mutex<Option<StandaloneFonts>> = std::sync::Mutex::new(None);

fn local_face_path(family_json: &str, weight: u32, italic: bool) -> Option<std::path::PathBuf> {
    let p = rustudio_core::assets::resolve(family_json, None)?;
    let j: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
    let mut best: Option<(u32, std::path::PathBuf)> = None;
    for f in j.get("faces")?.as_array()? {
        let id = f.get("assetId")?.as_str()?;
        if !id.starts_with("rbxasset://") {
            continue;
        }
        let Some(file) = rustudio_core::assets::resolve(id, None).filter(|p| p.is_file()) else {
            continue;
        };
        let w = f.get("weight")?.as_u64()? as u32;
        let it = f.get("style")?.as_str()? == "italic";
        let cost = w.abs_diff(weight) + if it != italic { 1000 } else { 0 };
        if best.as_ref().is_none_or(|(c, _)| cost < *c) {
            best = Some((cost, file));
        }
    }
    best.map(|(_, f)| f)
}

impl StandaloneFonts {
    fn new() -> Self {
        let mut defs = egui::FontDefinitions::empty();
        let mut fallback_keys = Vec::new();
        for (i, bytes) in fallback_fonts().iter().enumerate() {
            let key = format!("rbx:fallback{i}");
            defs.font_data.insert(key.clone(), std::sync::Arc::new(fallback_font_data(bytes)));
            fallback_keys.push(key);
        }
        let mut fonts = egui::epaint::text::Fonts::new(egui::epaint::text::TextOptions::default(), defs.clone());
        fonts.set_measure_only(true);
        Self { defs, fonts, fallback_keys, faces: HashMap::new() }
    }

    fn ensure_face(&mut self, font: &FaceKey) -> Option<FontFamily> {
        let path = local_face_path(&font.0, font.1, font.2)
            .or_else(|| local_face_path("rbxasset://fonts/families/SourceSansPro.json", 400, false))?;
        if let Some(fam) = self.faces.get(&path) {
            return Some(fam.clone());
        }
        let bytes = std::fs::read(&path).ok()?;
        let name = format!("rbx:local:{}", path.display());
        register_font_ligatures(&name, &bytes);
        let fam = FontFamily::Name(name.clone().into());
        self.defs.font_data.insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        let mut chain = Vec::with_capacity(1 + self.fallback_keys.len());
        chain.push(name);
        chain.extend(self.fallback_keys.iter().cloned());
        self.defs.families.insert(fam.clone(), chain);
        self.fonts = egui::epaint::text::Fonts::new(egui::epaint::text::TextOptions::default(), self.defs.clone());
        self.fonts.set_measure_only(true);
        self.faces.insert(path, fam.clone());
        Some(fam)
    }
}

fn build_measure_job(
    text: &str,
    ts: f32,
    line_height: f32,
    scale: f32,
    legacy_scale: f32,
    wrap: f32,
    rich: bool,
    font: &FaceKey,
    mut resolve_fam: impl FnMut(&FaceKey) -> (FontFamily, f32),
) -> (egui::text::LayoutJob, bool) {
    let mut job = egui::text::LayoutJob::default();
    let mut has_custom_size = false;
    if rich {
        let runs = rich_runs(text);
        let base = resolve_fam(font);
        let bold_face = font.1 >= 600;
        let italic_face = font.2;
        let bold_fam = if !bold_face && runs.iter().any(|r| r.bold && !r.italic) {
            Some(resolve_fam(&(font.0.clone(), 700, font.2)))
        } else {
            None
        };
        let italic_fam = if !italic_face && runs.iter().any(|r| r.italic && !r.bold) {
            Some(resolve_fam(&(font.0.clone(), font.1, true)))
        } else {
            None
        };
        let bold_italic_fam = if !(bold_face && italic_face) && runs.iter().any(|r| r.bold && r.italic) {
            Some(resolve_fam(&(font.0.clone(), 700, true)))
        } else {
            None
        };
        for run in &runs {
            if run.size.is_some() {
                has_custom_size = true;
            }
            let rs = run.size.map(|s| s * scale * legacy_scale).unwrap_or(ts).max(1.0);
            let (fam, em) = match (run.bold, run.italic) {
                (true, true) => bold_italic_fam
                    .as_ref()
                    .or(bold_fam.as_ref())
                    .or(italic_fam.as_ref())
                    .unwrap_or(&base),
                (true, false) => bold_fam.as_ref().unwrap_or(&base),
                (false, true) => italic_fam.as_ref().unwrap_or(&base),
                (false, false) => &base,
            };
            let shaped = apply_font_ligatures(fam, &run.text);
            let mut f = egui::TextFormat::simple(egui::FontId::new(rs * *em, fam.clone()), Color32::WHITE);
            f.studio_text_size = Some(rs);
            f.line_height = Some(rs * line_height);
            job.append(shaped.as_ref(), 0.0, f);
        }
    } else {
        let (fam, em) = resolve_fam(font);
        let shaped = apply_font_ligatures(&fam, text);
        let mut f = egui::TextFormat::simple(egui::FontId::new(ts * em, fam), Color32::WHITE);
        f.studio_text_size = Some(ts);
        f.line_height = Some(ts * line_height);
        job.append(shaped.as_ref(), 0.0, f);
    }
    job.wrap.max_width = wrap;
    job.keep_trailing_whitespace = true;
    job.round_output_to_gui = false;
    (job, has_custom_size)
}

/// Measures `text` for TextService:GetTextSize / GetTextBoundsAsync using the
/// same egui font faces and LayoutJob as the GUI walk.
pub fn measure_text_string(text: &str, text_size: f32, font: &FaceKey, frame_w: f32, rich: bool) -> [f32; 2] {
    let legacy_scale = legacy_text_scale(font);
    let ts = text_size.max(1.0) * legacy_scale;
    if text.is_empty() || (rich && content_text(text).is_empty()) {
        return [0.0, ts.ceil()];
    }
    let wrap = if frame_w > 0.0 && frame_w.is_finite() { frame_w } else { f32::INFINITY };
    // Prefer an exact face already registered in the GUI walk (covers cloud fonts).
    if let Some((ctx, base_fam, base_em)) = exact_text_face(font) {
        let (job, has_custom_size) = build_measure_job(
            text, ts, 1.0, 1.0, legacy_scale, wrap, rich, font,
            |fk| exact_text_face(fk).map(|(_, f, e)| (f, e)).unwrap_or_else(|| (base_fam.clone(), base_em)),
        );
        let g = ctx.fonts_mut(|fo| fo.layout_job(job));
        let h = if has_custom_size { g.rect.height().ceil() } else { walk::text_rows_height(g.rows.len(), ts, 1.0).ceil() };
        return [g.rect.width().ceil(), h];
    }
    // Load the exact Studio local font face on demand (also works before the first GUI frame).
    {
        let mut guard = STANDALONE_FONTS.lock().unwrap_or_else(|e| e.into_inner());
        let sf = guard.get_or_insert_with(StandaloneFonts::new);
        if let Some(base_fam) = sf.ensure_face(font) {
            let (job, has_custom_size) = build_measure_job(
                text, ts, 1.0, 1.0, legacy_scale, wrap, rich, font,
                |fk| (sf.ensure_face(fk).unwrap_or_else(|| base_fam.clone()), 1.0),
            );
            let g = sf.fonts.with_pixels_per_point(1.0).layout_job(job);
            let h = if has_custom_size { g.rect.height().ceil() } else { walk::text_rows_height(g.rows.len(), ts, 1.0).ceil() };
            return [g.rect.width().ceil(), h];
        }
    }
    if let Some((ctx, family, em)) = text_face(font) {
        let (job, has_custom_size) = build_measure_job(
            text, ts, 1.0, 1.0, legacy_scale, wrap, rich, font,
            |_| (family.clone(), em),
        );
        let g = ctx.fonts_mut(|fo| fo.layout_job(job));
        let h = if has_custom_size { g.rect.height().ceil() } else { walk::text_rows_height(g.rows.len(), ts, 1.0).ceil() };
        return [g.rect.width().ceil(), h];
    }
    // Pre-first-frame fallback (when no Studio content folder is present).
    let plain = if rich { Cow::Owned(content_text(text)) } else { Cow::Borrowed(text) };
    let char_w = ts * 0.52;
    let max_w = wrap;
    let mut best_w = 0.0f32;
    let mut lines = 0u32;
    for raw_line in plain.split('\n') {
        let w = raw_line.chars().count() as f32 * char_w;
        if w <= max_w {
            best_w = best_w.max(w);
            lines += 1;
        } else {
            best_w = best_w.max(max_w);
            lines += (w / max_w).ceil().max(1.0) as u32;
        }
    }
    [best_w.ceil(), (lines.max(1) as f32 * ts).ceil()]
}

pub(super) fn note_text_face(ctx: &egui::Context, font: &FaceKey, family: &FontFamily, em: f32) {
    let _ = TEXT_CTX.set(ctx.clone());
    let mut m = TEXT_FACES.lock().unwrap_or_else(|e| e.into_inner());
    let m = m.get_or_insert_with(HashMap::new);
    if *family != FontFamily::Proportional || !m.contains_key(font) {
        m.insert(font.clone(), (family.clone(), em));
    }
}

/// TextBounds of a text object that wasn't drawn with its current text or
/// box (`size`, its AbsoluteSize): Studio measures on the read, so a script
/// reading it right after the object appeared (the title templates'
/// EmitterSize scripts) gets the text's size, not a guess. Measured like the
/// walk does (TextScaled fitted to the box, wrapping, UITextSizeConstraint),
/// with the face the walk last used for this font, else the default face.
/// None before the GUI has drawn any text.
pub fn text_bounds_now(d: &Document, r: Ref, size: [f32; 2], ui_scale: f32) -> Option<[f32; 2]> {
    let ctx = TEXT_CTX.get()?;
    let font = font_of(d, r);
    let (family, em) = {
        let m = TEXT_FACES.lock().unwrap_or_else(|e| e.into_inner());
        let m = m.as_ref()?;
        match m.get(&font) {
            Some(f) => f.clone(),
            None => m
                .iter()
                .find(|(_, (f, _))| *f == FontFamily::Proportional)
                .or_else(|| m.iter().next())
                .map(|(_, f)| f.clone())?,
        }
    };
    let text = text_prop(d, r, "Text");
    let rich = boolp(d, r, "RichText", false);
    let wrapped = boolp(d, r, "TextWrapped", false);
    let scaled = boolp(d, r, "TextScaled", false);
    let line_height = num(d, r, "LineHeight", 1.0);
    let tsc = child_of_class(d, r, "UITextSizeConstraint");
    let legacy_scale = legacy_text_scale(&font);
    let mut ts = num(d, r, "TextSize", 14.0) * ui_scale * legacy_scale;
    if !scaled {
        if let Some(c) = tsc {
            ts = ts.clamp(num(d, c, "MinTextSize", 1.0) * ui_scale, num(d, c, "MaxTextSize", 100.0) * ui_scale);
        }
    }
    if text.is_empty() || (rich && content_text(&text).is_empty()) {
        return Some([0.0, ts]);
    }
    let layout = |ts: f32, wrap: bool| {
        let max_w = if wrap { size[0].max(1.0) } else { f32::INFINITY };
        let (job, has_custom_size) = build_measure_job(
            &text, ts, line_height, ui_scale, legacy_scale, max_w, rich, &font,
            |fk| exact_text_face(fk).map(|(_, f, e)| (f, e)).unwrap_or_else(|| (family.clone(), em)),
        );
        (ctx.fonts_mut(|fo| fo.layout_job(job)), has_custom_size)
    };
    if scaled {
        let max = tsc.map(|c| num(d, c, "MaxTextSize", 100.0)).unwrap_or(100.0).min(100.0) * ui_scale.max(1.0);
        let mut lo = 1i32;
        let mut hi = (max.floor().max(1.0) as i32) + 1;
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            let (g, _) = layout(mid as f32, true);
            let h = walk::text_rows_height(g.rows.len(), mid as f32, line_height);
            if !walk::has_mid_word_break(&g) && g.size().x <= size[0] + 0.5 && h <= size[1] + 0.5 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        ts = lo as f32;
        if let Some(c) = tsc {
            ts = ts.max(num(d, c, "MinTextSize", 1.0) * ui_scale);
        }
    }
    let (g, has_custom_size) = layout(ts, wrapped || scaled);
    let h = if has_custom_size && (line_height - 1.0).abs() < 1e-4 {
        g.rect.height().ceil()
    } else {
        walk::text_rows_height(g.rows.len(), ts, line_height)
    };
    Some([g.rect.width().ceil(), h])
}

/// Exact face registered by the GUI walk for `font`.
fn exact_text_face(font: &FaceKey) -> Option<(&'static egui::Context, FontFamily, f32)> {
    let ctx = TEXT_CTX.get()?;
    let m = TEXT_FACES.lock().unwrap_or_else(|e| e.into_inner());
    let (family, em) = m.as_ref()?.get(font)?.clone();
    Some((ctx, family, em))
}

/// The face the walk last drew `font` with (else its default face), and
/// the egui context it measures with: None before any text was drawn.
fn text_face(font: &FaceKey) -> Option<(&'static egui::Context, FontFamily, f32)> {
    let ctx = TEXT_CTX.get()?;
    let m = TEXT_FACES.lock().unwrap_or_else(|e| e.into_inner());
    let m = m.as_ref()?;
    let (family, em) = match m.get(font) {
        Some(f) => f.clone(),
        None => m
            .iter()
            .find(|(_, (f, _))| *f == FontFamily::Proportional)
            .or_else(|| m.iter().next())
            .map(|(_, f)| f.clone())?,
    };
    Some((ctx, family, em))
}

/// `Walk::measure_text` outside the walk, with the walk's faces.
struct FaceText;

impl walk::AutoText for FaceText {
    fn measure_text(&mut self, d: &Document, r: Ref, wrap: f32, scale: f32) -> egui::Vec2 {
        let mut text = text_prop(d, r, "Text");
        if text.is_empty() && d.class_of(r).is_some_and(|c| c == "TextBox") {
            text = text_prop(d, r, "PlaceholderText");
        }
        let rich = boolp(d, r, "RichText", false);
        let font = font_of(d, r);
        let legacy_scale = legacy_text_scale(&font);
        let mut size = num(d, r, "TextSize", 14.0) * scale * legacy_scale;
        if let Some(c) = child_of_class(d, r, "UITextSizeConstraint") {
            size = size.clamp(
                num(d, c, "MinTextSize", 1.0) * scale,
                num(d, c, "MaxTextSize", 100.0) * scale,
            );
        }
        if text.is_empty() || (rich && content_text(&text).is_empty()) {
            return egui::vec2(0.0, size);
        }
        let Some((ctx, family, em)) = text_face(&font) else { return egui::vec2(0.0, size) };
        let lh = num(d, r, "LineHeight", 1.0);
        let (job, has_custom_size) = build_measure_job(
            &text, size, lh, scale, legacy_scale, wrap.max(1.0), rich, &font,
            |fk| exact_text_face(fk).map(|(_, f, e)| (f, e)).unwrap_or_else(|| (family.clone(), em)),
        );
        let g = ctx.fonts_mut(|fo| fo.layout_job(job));
        let h = if has_custom_size && (lh - 1.0).abs() < 1e-4 {
            g.rect.height().ceil()
        } else {
            walk::text_rows_height(g.rows.len(), size, lh)
        };
        egui::vec2(g.rect.width().ceil(), h)
    }
}

/// Slots in a single list or grid, in coordinates relative to the parent content origin.
pub(crate) fn layout_slots(
    d: &Document,
    parent: Ref,
    size: [f32; 2],
    scale: f32,
) -> (HashMap<Ref, Rect>, Vec<(Ref, GuiBox)>) {
    let mut gui = Gui::default();
    let ctx = egui::Context::default();
    let cam = Camera::default();
    let mut store = AssetStore::default();
    let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0], size[1]));
    let mut input = DrawIn {
        render_state: None, doc: d, cam: &cam, rect, store: &mut store,
        doc_dir: None, playing: false, fixed_step: true, roots: Roots::View,
    };
    let mut walk = walk::Walk {
        gui: &mut gui, ctx: &ctx, input: &mut input, items: Vec::new(),
        hits: Vec::new(), seq: 0, boxes: Vec::new(), global_z: false,
        abs_offset: [0.0; 2], screen: None, cur: parent, rec: Vec::new(),
        replaying: 0, table_cells: HashMap::new(),
    };
    let slots = walk.place_children(parent, rect, scale, true);
    (slots, walk.boxes)
}

/// Grows the requested axes from a resolved box, including a grid cell.
pub(crate) fn grow_axes(d: &Document, r: Ref, size: [f32; 2], scale: f32, auto: u32, room: f32) -> [f32; 2] {
    let mut sizer = walk::Sizer { d, m: FaceText };
    let c = sizer.content_size_in(r, egui::vec2(size[0], size[1]), scale, auto, 0, room);
    [
        if auto & 1 != 0 { size[0].max(c.x) } else { size[0] },
        if auto & 2 != 0 { size[1].max(c.y) } else { size[1] },
    ]
}

/// An AutomaticSize object's `size` (from its Size in a `parent` content
/// box) grown to hold its content, for a script reading AbsoluteSize before
/// the GUI walk has laid it out (Studio's ProximityPrompt reads its
/// AutomaticSize X frame two RenderSteppeds after building it: with the Play
/// worker the walk's boxes reach scripts a step later, and it read 0).
/// The walk's rules (`walk::Sizer`) with the text faces available to scripts.
pub fn auto_grow_now(d: &Document, r: Ref, size: [f32; 2], parent: [f32; 2], position: [f32; 2], scale: f32) -> Option<[f32; 2]> {
    let auto = enum_u32(d.prop(r, "AutomaticSize")).unwrap_or(0);
    Some(grow_axes(d, r, size, scale, auto, parent[0] - position[0]))
}

/// Tests: lets reads measure text as if the walk had drawn some with
/// egui's default face.
#[cfg(test)]
pub(crate) fn prime_text_measure() {
    let ctx = TEXT_CTX.get_or_init(egui::Context::default).clone();
    ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
    let line = ctx.fonts_mut(|f| f.row_height(&egui::FontId::new(100.0, FontFamily::Proportional))) / 100.0;
    let em = rustudio_core::mech::GUI_FONT_EM_PER_TEXT_SIZE / line.max(0.5);
    let mut m = TEXT_FACES.lock().unwrap_or_else(|e| e.into_inner());
    m.get_or_insert_with(HashMap::new).entry((String::new(), 0, false)).or_insert((FontFamily::Proportional, em));
}

/// What RichText leaves of a string once its tags are gone (ContentText).
pub fn content_text(src: &str) -> String {
    rich_runs(src).into_iter().map(|r| r.text).collect()
}
