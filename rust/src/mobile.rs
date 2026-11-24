//! Touch screens and phone-sized GPUs.
//!
//! egui-winit turns *one* finger into a mouse; Studio's 3D view wants a lot
//! more (fly, look, orbit, walk, jump). Everything that only matters with a
//! finger lives here and is inert on desktop (see [`enabled`]):
//!
//! - [`process_input`] replaces egui's single-pointer emulation with a small
//!   touch router (called from `raw_input_hook`, before the keys are read):
//!   * Play: a floating stick on the left of the 3D view -> W/A/S/D, a jump
//!     button on the right -> Space, dragging anywhere else -> look (a
//!     virtual right-button drag sent directly to Play), a tap -> a click;
//!   * Edit: the view's mode buttons choose what one finger does in the 3D
//!     view (Select = left button, Orbit = Alt + left drag, Look = right
//!     drag); two fingers pinch to zoom and drag to pan (see
//!     [`pinch_notches`] / [`two_finger_pan`]); outside the 3D view a finger
//!     is the left button;
//! - [`hud`] draws the stick, the jump button and the mode / menu buttons;
//! - [`tune`] makes egui's widgets finger-sized;

use std::sync::{Mutex, OnceLock};

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, TouchPhase, Vec2};
use crate::game_view::TouchLookEvent;

/// What one finger does in the 3D view while editing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Select,
    Orbit,
    Look,
}

/// Touch routing on? Always on Android; `RUSTUDIO_TOUCH=1` forces it on desktop.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| cfg!(target_os = "android") || std::env::var_os("RUSTUDIO_TOUCH").is_some())
}

/// UI zoom (egui `zoom_factor`) the app starts with; `RUSTUDIO_UI_SCALE`.
pub fn ui_scale() -> f32 {
    std::env::var("RUSTUDIO_UI_SCALE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map(|v| v.clamp(0.5, 3.0))
        .unwrap_or(1.0)
}

/// Finger-sized widgets (called at the end of theme::apply).
pub fn tune(style: &mut egui::Style) {
    if !enabled() {
        return;
    }
    let sp = &mut style.spacing;
    sp.interact_size = egui::vec2(36.0, 30.0);
    sp.button_padding = egui::vec2(10.0, 5.0);
    sp.item_spacing = egui::vec2(8.0, 6.0);
    sp.icon_width = 20.0;
    sp.scroll.bar_width = 14.0;
    style.interaction.interact_radius = 10.0;
    style.interaction.resize_grab_radius_side = 14.0;
    style.interaction.resize_grab_radius_corner = 16.0;
}

const SLOP: f32 = 10.0;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Stick,
    Jump,
    Pointer,
    Look,
    Extra,
}

struct Touch {
    id: u64,
    role: Role,
    start: Pos2,
    pos: Pos2,
}

#[derive(Clone, Copy, PartialEq)]
enum Ptr {
    /// Finger down in the 3D view, not moved yet (a tap if it never does).
    Pending { button: PointerButton, alt: bool },
    /// A button is down on behalf of this finger.
    Held { button: PointerButton, alt: bool },
    PlayPending,
    PlayResume,
    PlayLook,
    /// A second finger arrived: this one is ignored until it lifts.
    Dead,
}

struct State {
    mode: ViewMode,
    frame: u64,
    view: Option<Rect>,
    view_frame: u64,
    excl: Vec<Rect>,
    touches: Vec<Touch>,
    ptr: Option<(u64, Ptr)>,
    clear_pointer: bool,
    playing: bool,
    look: Vec<TouchLookEvent>,
    stick_vec: Vec2,
    stick_origin: Option<Pos2>,
    jump: bool,
    keys: [bool; 5],
    queued: Vec<(Key, Modifiers)>,
    alt: bool,
    physical_modifiers: Modifiers,
}

impl State {
    const fn new() -> Self {
        Self {
            mode: ViewMode::Select,
            frame: 0,
            view: None,
            view_frame: 0,
            excl: Vec::new(),
            touches: Vec::new(),
            ptr: None,
            clear_pointer: false,
            playing: false,
            look: Vec::new(),
            stick_vec: Vec2::ZERO,
            stick_origin: None,
            jump: false,
            keys: [false; 5],
            queued: Vec::new(),
            alt: false,
            physical_modifiers: Modifiers::NONE,
        }
    }

    fn live_view(&self) -> Option<Rect> {
        self.view.filter(|_| self.frame.saturating_sub(self.view_frame) <= 3)
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The 3D view's rectangle this frame (called by viewport.rs).
pub fn note_view_rect(rect: Rect) {
    if !enabled() {
        return;
    }
    let mut s = st();
    s.view = rect.is_positive().then_some(rect);
    s.view_frame = s.frame;
}

/// Pinch as mouse-wheel notches (positive: zoom in).
pub fn pinch_notches(ctx: &egui::Context) -> f32 {
    ctx.input(|i| i.multi_touch().map(|m| m.zoom_delta.ln() * 10.0).unwrap_or(0.0))
}

pub fn play_pinch_notches(ctx: &egui::Context) -> f32 {
    if !st().play_pinch() {
        return 0.0;
    }
    pinch_notches(ctx)
}

/// Two-finger drag, in points (like a middle-button drag).
pub fn two_finger_pan(ctx: &egui::Context) -> Option<Vec2> {
    ctx.input(|i| i.multi_touch().map(|m| m.translation_delta)).filter(|d| *d != Vec2::ZERO)
}

fn jump_center(view: Rect) -> Pos2 {
    view.right_bottom() - Vec2::splat(jump_radius(view) + 16.0)
}

fn stick_radius(view: Rect) -> f32 {
    (view.height() * 0.18).clamp(32.0, 62.0)
}

fn jump_radius(view: Rect) -> f32 {
    (view.height() * 0.14).clamp(25.0, 42.0)
}

fn key_event(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    // The player-list tap uses Tab's physical key without moving egui focus.
    let logical = if key == Key::Tab { Key::F35 } else { key };
    Event::Key { key: logical, physical_key: Some(key), pressed, repeat: false, modifiers }
}

fn button_event(pos: Pos2, button: PointerButton, pressed: bool, alt: bool) -> Event {
    let modifiers = Modifiers { alt, ..Modifiers::NONE };
    Event::PointerButton { pos, button, pressed, modifiers }
}

impl State {
    fn play_pinch(&self) -> bool {
        self.touches.len() == 2 && self.touches.iter().all(|t| t.role == Role::Look)
    }

    fn classify(&self, pos: Pos2, playing: bool) -> Role {
        let ptr_free = self.ptr.is_none();
        if let Some(v) = self.live_view() {
            if playing && v.contains(pos) && !self.excl.iter().any(|r| r.contains(pos)) {
                if !self.jump && pos.distance(jump_center(v)) <= jump_radius(v) + 8.0 {
                    return Role::Jump;
                }
                let has_stick = self.touches.iter().any(|t| t.role == Role::Stick);
                if !has_stick && pos.x < v.left() + v.width() * 0.42 && pos.y > v.top() + v.height() * 0.30 {
                    return Role::Stick;
                }
                return Role::Look;
            }
        }
        if ptr_free {
            Role::Pointer
        } else {
            Role::Extra
        }
    }

    fn start_pointer(&mut self, id: u64, pos: Pos2, role: Role, blocked: bool, out: &mut Vec<Event>) {
        let in_view = self.live_view().is_some_and(|v| v.contains(pos)) && !self.excl.iter().any(|r| r.contains(pos));
        out.push(Event::PointerMoved(pos));
        if role == Role::Look {
            self.ptr = Some((id, Ptr::PlayPending));
            return;
        }
        let deferred = if !in_view || blocked {
            None
        } else {
            match self.mode {
                ViewMode::Select => None,
                ViewMode::Orbit => Some((PointerButton::Primary, true)),
                ViewMode::Look => Some((PointerButton::Secondary, false)),
            }
        };
        let state = match deferred {
            Some((button, alt)) => Ptr::Pending { button, alt },
            None => {
                out.push(button_event(pos, PointerButton::Primary, true, false));
                Ptr::Held { button: PointerButton::Primary, alt: false }
            }
        };
        self.ptr = Some((id, state));
    }

    fn look_motion(&mut self, delta: Vec2) {
        if delta == Vec2::ZERO {
            return;
        }
        if let Some(TouchLookEvent::Motion(previous)) = self.look.last_mut() {
            *previous += delta;
        } else {
            self.look.push(TouchLookEvent::Motion(delta));
        }
    }

    fn resume_play_look(&mut self) {
        if !matches!(self.ptr, None | Some((_, Ptr::Dead))) {
            return;
        }
        let mut fingers = self.touches.iter().filter(|t| t.role == Role::Look);
        if let Some(touch) = fingers.next() {
            if fingers.next().is_none() {
                self.ptr = Some((touch.id, Ptr::PlayResume));
            }
        }
    }

    fn move_pointer(&mut self, id: u64, start: Pos2, last: Pos2, pos: Pos2, out: &mut Vec<Event>) {
        let Some((pid, state)) = self.ptr else { return };
        if pid != id {
            return;
        }
        match state {
            Ptr::Pending { button, alt } => {
                if pos.distance(start) > SLOP {
                    out.push(Event::PointerMoved(pos));
                    out.push(button_event(pos, button, true, alt));
                    self.alt = alt;
                    self.ptr = Some((id, Ptr::Held { button, alt }));
                }
            }
            Ptr::Held { .. } => out.push(Event::PointerMoved(pos)),
            Ptr::PlayPending if pos.distance(start) > SLOP => {
                self.look.push(TouchLookEvent::Button(true));
                self.look_motion(pos - last);
                out.push(Event::PointerMoved(pos));
                self.ptr = Some((id, Ptr::PlayLook));
            }
            Ptr::PlayResume if pos != last => {
                self.look.push(TouchLookEvent::Button(true));
                self.look_motion(pos - last);
                out.push(Event::PointerMoved(pos));
                self.ptr = Some((id, Ptr::PlayLook));
            }
            Ptr::PlayLook => {
                self.look_motion(pos - last);
                out.push(Event::PointerMoved(pos));
            }
            Ptr::PlayPending | Ptr::PlayResume | Ptr::Dead => {}
        }
    }

    fn end_pointer(&mut self, id: u64, start: Pos2, last: Pos2, pos: Pos2, cancel: bool, out: &mut Vec<Event>) {
        let Some((pid, state)) = self.ptr else { return };
        if pid != id {
            return;
        }
        match state {
            Ptr::PlayPending if !cancel && pos.distance(start) > SLOP => {
                self.look.push(TouchLookEvent::Button(true));
                self.look_motion(pos - last);
                self.look.push(TouchLookEvent::Button(false));
                out.push(Event::PointerGone);
            }
            Ptr::Pending { .. } | Ptr::PlayPending if !cancel => {
                out.push(Event::PointerMoved(start));
                out.push(button_event(start, PointerButton::Primary, true, false));
                out.push(button_event(start, PointerButton::Primary, false, false));
                out.push(Event::PointerGone);
            }
            Ptr::Held { button, alt } => {
                out.push(Event::PointerMoved(pos));
                out.push(button_event(pos, button, false, alt));
                out.push(Event::PointerGone);
            }
            Ptr::PlayLook => {
                if !cancel {
                    self.look_motion(pos - last);
                }
                self.look.push(TouchLookEvent::Button(false));
                out.push(Event::PointerGone);
            }
            _ => out.push(Event::PointerGone),
        }
        self.alt = false;
        self.ptr = None;
    }

    /// A second finger on the view: stop acting as a pointer (pinch / pan take over).
    fn cancel_pointer(&mut self, last: Pos2, out: &mut Vec<Event>) {
        if let Some((id, state)) = self.ptr {
            if let Ptr::Held { button, alt } = state {
                out.push(button_event(last, button, false, alt));
                out.push(Event::PointerGone);
            }
            if state == Ptr::PlayLook {
                self.look.push(TouchLookEvent::Button(false));
                out.push(Event::PointerGone);
            }
            self.alt = false;
            self.ptr = Some((id, Ptr::Dead));
        }
    }

    fn reset_touches(&mut self, out: &mut Vec<Event>) {
        if let Some(last) = self.ptr.and_then(|(id, _)| self.touches.iter().find(|t| t.id == id).map(|t| t.pos)) {
            self.cancel_pointer(last, out);
        }
        self.ptr = None;
        self.touches.clear();
        self.stick_vec = Vec2::ZERO;
        self.stick_origin = None;
        self.jump = false;
    }

    fn on_touch(&mut self, id: u64, phase: TouchPhase, pos: Pos2, playing: bool, blocked: bool, out: &mut Vec<Event>) {
        match phase {
            TouchPhase::Start => {
                let role = self.classify(pos, playing && !blocked);
                self.touches.retain(|t| t.id != id);
                self.touches.push(Touch { id, role, start: pos, pos });
                match role {
                    Role::Jump => self.jump = true,
                    Role::Stick => {
                        self.stick_origin = Some(pos);
                        self.stick_vec = Vec2::ZERO;
                    }
                    Role::Pointer => self.start_pointer(id, pos, role, blocked, out),
                    Role::Look if self.ptr.is_none() => self.start_pointer(id, pos, role, blocked, out),
                    Role::Look | Role::Extra => {}
                }
                let second_finger = if playing {
                    role == Role::Look && self.touches.iter().filter(|t| t.role == Role::Look).count() >= 2
                        && matches!(self.ptr, Some((_, Ptr::PlayPending | Ptr::PlayResume | Ptr::PlayLook)))
                } else {
                    self.touches.iter().filter(|t| matches!(t.role, Role::Pointer | Role::Extra)).count() >= 2
                };
                if second_finger {
                    let last = self.ptr.and_then(|(id, _)| self.touches.iter().find(|t| t.id == id).map(|t| t.pos)).unwrap_or(pos);
                    self.cancel_pointer(last, out);
                }
            }
            TouchPhase::Move => {
                let radius = self.live_view().map_or(48.0, stick_radius);
                let Some(t) = self.touches.iter_mut().find(|t| t.id == id) else { return };
                let last = t.pos;
                t.pos = pos;
                let (role, start) = (t.role, t.start);
                match role {
                    Role::Stick => {
                        let o = self.stick_origin.unwrap_or(start);
                        let d = (pos - o) / radius;
                        self.stick_vec = if d.length() > 1.0 { d / d.length() } else { d };
                    }
                    Role::Pointer | Role::Look => self.move_pointer(id, start, last, pos, out),
                    _ => {}
                }
            }
            TouchPhase::End | TouchPhase::Cancel => {
                let Some(i) = self.touches.iter().position(|t| t.id == id) else { return };
                let t = self.touches.remove(i);
                match t.role {
                    Role::Jump => self.jump = false,
                    Role::Stick => {
                        self.stick_vec = Vec2::ZERO;
                        self.stick_origin = None;
                    }
                    Role::Pointer | Role::Look => self.end_pointer(id, t.start, t.pos, pos, phase == TouchPhase::Cancel, out),
                    Role::Extra => {}
                }
                if playing && !blocked {
                    self.resume_play_look();
                }
            }
        }
    }

    /// Stick and jump -> W A S D / Space key events (only the changes).
    fn sync_keys(&mut self, playing: bool, out: &mut Vec<Event>) {
        let v = if playing { self.stick_vec } else { Vec2::ZERO };
        let want = [
            v.y < -0.35,
            v.x < -0.35,
            v.y > 0.35,
            v.x > 0.35,
            playing && self.jump,
        ];
        const KEYS: [Key; 5] = [Key::W, Key::A, Key::S, Key::D, Key::Space];
        for i in 0..5 {
            if want[i] != self.keys[i] {
                self.keys[i] = want[i];
                out.push(key_event(KEYS[i], want[i], Modifiers::NONE));
            }
        }
    }
}

/// Rewrites this frame's input: egui-winit's one-finger mouse emulation is
/// replaced by the touch router. Call before reading keys.
pub fn process_input(
    raw: &mut egui::RawInput,
    playing: bool,
    blocked: bool,
    over_game_gui: impl Fn(Pos2) -> bool,
) {
    if !enabled() {
        return;
    }
    let mut s = st();
    let focused = raw.focused && !raw.events.iter().any(|e| matches!(e, Event::WindowFocused(false)));
    let blocked = blocked || !focused;
    s.frame += 1;
    let was_alt = s.alt;
    let clear_pointer = std::mem::take(&mut s.clear_pointer);
    for event in &raw.events {
        match event {
            Event::ModifiersChanged(modifiers) => s.physical_modifiers = *modifiers,
            Event::WindowFocused(false) => s.physical_modifiers = Modifiers::NONE,
            _ => {}
        }
    }
    let queued: Vec<_> = s.queued.drain(..).collect();
    for (k, m) in queued {
        raw.events.push(key_event(k, true, m));
        raw.events.push(key_event(k, false, m));
    }
    let touches: Vec<(u64, TouchPhase, Pos2)> = raw
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Touch { id, phase, pos, .. } => Some((id.0, *phase, *pos)),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    if !focused || s.playing != playing {
        s.reset_touches(&mut out);
        if !playing {
            s.look.clear();
        }
    }
    s.playing = playing;
    if blocked {
        if matches!(s.ptr, Some((_, Ptr::PlayPending | Ptr::PlayResume | Ptr::PlayLook))) {
            s.cancel_pointer(Pos2::ZERO, &mut out);
        }
        for touch in &mut s.touches {
            if touch.role == Role::Look {
                touch.role = Role::Extra;
            }
        }
        if matches!(s.ptr, Some((_, Ptr::Dead))) {
            s.ptr = None;
        }
    }
    if !touches.is_empty() || !s.touches.is_empty() {
        // The emulated mouse events for the same fingers: dropped.
        raw.events.retain(|e| !matches!(e, Event::PointerButton { .. } | Event::PointerMoved(_) | Event::PointerGone));
        if focused {
            for (id, phase, pos) in touches {
                let hits_gui = playing && phase == TouchPhase::Start && over_game_gui(pos);
                s.on_touch(id, phase, pos, playing, blocked || hits_gui, &mut out);
            }
        }
    }
    // Gui::input resolves the button using the current pointer position.
    // Keep the release position for its frame, then clear the touch cursor.
    s.clear_pointer = playing && defer_pointer_gone(&mut out);
    if clear_pointer && !raw.events.iter().chain(&out).any(|event| {
        matches!(event, Event::PointerMoved(_) | Event::PointerButton { .. } | Event::PointerGone)
    }) {
        out.push(Event::PointerGone);
    }
    s.sync_keys(playing && !blocked, &mut out);
    raw.events.extend(out);
    if s.alt || was_alt {
        let mut modifiers = s.physical_modifiers;
        modifiers.alt |= s.alt;
        raw.events.push(Event::ModifiersChanged(modifiers));
    }
}

fn defer_pointer_gone(events: &mut Vec<Event>) -> bool {
    if matches!(events.as_slice(), [.., Event::PointerButton { pressed: false, .. }, Event::PointerGone]) {
        events.pop();
        true
    } else {
        false
    }
}

/// Takes the ordered drag inputs for the client view.
pub fn take_play_look() -> Vec<TouchLookEvent> {
    std::mem::take(&mut st().look)
}

/// Per-frame touch UI plus (Android) the soft keyboard. Call once per frame.
pub fn frame(_ctx: &egui::Context) {
    #[cfg(target_os = "android")]
    {
        crate::android::sync_soft_keyboard(_ctx);
        crate::android_dialog::poll_async(_ctx);
    }
}

pub(crate) fn select_edit_mode() {
    st().mode = ViewMode::Select;
}

pub(crate) fn edit_camera_controls(ui: &mut egui::Ui) {
    let mode = st().mode;
    for (candidate, label) in [
        (ViewMode::Select, "Select"), (ViewMode::Orbit, "Orbit"), (ViewMode::Look, "Look"),
    ] {
        if ui.selectable_label(mode == candidate, label).clicked() {
            st().mode = candidate;
            ui.close();
        }
    }
}

pub(crate) fn hud(ctx: &egui::Context, game: &mut crate::game::Game) {
    if game.play.is_none() {
        st().excl.clear();
        return;
    }
    let (view, stick_vec, origin, jump) = {
        let s = st();
        (s.live_view(), s.stick_vec, s.stick_origin, s.jump)
    };
    let Some(view) = view else {
        st().excl.clear();
        return;
    };
    let playing = game.play_game_input();
    let menu_open = game.any_menu_open();
    let chat_open = game.play.as_ref().is_some_and(|s| s.game.chat_open);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("touch_hud")))
        .with_clip_rect(view);
    let mut excl = Vec::new();
    if playing && !menu_open && !chat_open {
        let radius = stick_radius(view);
        if let Some(base) = origin {
            let knob = base + stick_vec * radius;
            painter.circle_filled(base, radius, egui::Color32::from_black_alpha(78));
            painter.circle_stroke(base, radius, egui::Stroke::new(1.5, egui::Color32::from_white_alpha(96)));
            painter.line_segment([base, knob], egui::Stroke::new(4.0, egui::Color32::from_white_alpha(62)));
            painter.circle_filled(knob, radius * 0.34, egui::Color32::from_white_alpha(168));
        } else {
            let hint = view.left_bottom() + egui::vec2(radius + 18.0, -radius - 18.0);
            painter.circle_stroke(hint, radius * 0.32, egui::Stroke::new(1.5, egui::Color32::from_white_alpha(66)));
        }
        let jc = jump_center(view);
        let radius = jump_radius(view);
        painter.circle_filled(jc, radius, if jump { egui::Color32::from_white_alpha(160) } else { egui::Color32::from_black_alpha(126) });
        painter.circle_stroke(jc, radius, egui::Stroke::new(1.5, egui::Color32::from_white_alpha(148)));
        rustudio_engine::ti::paint(&painter, Rect::from_center_size(jc, Vec2::splat(radius)), "arrow-up", if jump { egui::Color32::BLACK } else { egui::Color32::WHITE });
    }
    let mut toggle_chat = false;
    let controls = egui::Area::new(egui::Id::new("touch_hud_controls"))
        .order(egui::Order::Foreground)
        .fixed_pos(view.left_top() + egui::vec2(10.0, 8.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let menu_icon = if game.account.signed_in() { HudIcon::Roblox } else { HudIcon::RuStudio };
                if playing && hud_button(ui, "Game Menu", menu_icon, menu_open) {
                    st().queued.push((Key::Escape, Modifiers::NONE));
                }
                if playing && !menu_open && game.play_chat_available()
                    && hud_button(ui, "Chat", HudIcon::Tabler("message-circle"), chat_open)
                {
                    toggle_chat = true;
                }
                if playing && !menu_open && !chat_open
                    && game.play.as_ref().is_some_and(|s| s.game.core_scripts_active)
                    && hud_button(ui, "Players", HudIcon::Tabler("users"), false)
                {
                    st().queued.push((Key::Tab, Modifiers::NONE));
                }
                if hud_button(ui, "Stop Playtest", HudIcon::Tabler("player-stop"), false) {
                    st().queued.push((Key::F5, Modifiers::SHIFT));
                }
            });
        });
    excl.push(controls.response.rect);
    st().excl = excl;
    if toggle_chat {
        if let Some(error) = game.run_luau_as(
            rustudio_engine::runtime::Side::Client,
            TOGGLE_CHAT,
            rustudio_engine::runtime::security::CORE,
        ) {
            game.error(format!("Chat: {error}"));
        }
        ctx.request_repaint();
    }
}

// Legacy and fallback chat register ChatActive. Some versions of
// ExperienceChat mount the panel without registering that Core callback.
const TOGGLE_CHAT: &str = r#"
local starter = game:GetService("StarterGui")
local core = game:GetService("CoreGui")
local experience = core:FindFirstChild("ExperienceChat", true)
local panel = experience and experience:FindFirstChild("appLayout", true)
if not panel and experience then
    local window = experience:FindFirstChild("chatWindow", true)
    if window and window.Parent:IsA("GuiObject") then panel = window.Parent end
end
local active = starter:GetCore("ChatActive")
if type(active) == "boolean" then
    starter:SetCore("ChatActive", not active)
elseif panel and panel:IsA("GuiObject") then
    active = panel.Visible
    panel.Visible = not active
else
    return
end
if active then
    local focused = game:GetService("UserInputService"):GetFocusedTextBox()
    if focused and focused:IsDescendantOf(core) then
        focused:ReleaseFocus()
    end
end
"#;

enum HudIcon {
    RuStudio,
    Roblox,
    Tabler(&'static str),
}

fn hud_button(ui: &mut egui::Ui, label: &str, icon: HudIcon, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(44.0), egui::Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let fill = if selected || response.hovered() { 212 } else { 164 };
    ui.painter().circle_filled(rect.center(), 22.0, egui::Color32::from_black_alpha(fill));
    let glyph = Rect::from_center_size(rect.center(), Vec2::splat(24.0));
    match icon {
        HudIcon::RuStudio => crate::icons::paint_logo(ui.painter(), glyph),
        HudIcon::Roblox => paint_roblox_mark(ui.painter(), glyph),
        HudIcon::Tabler(icon) => rustudio_engine::ti::paint(ui.painter(), glyph, icon, egui::Color32::WHITE),
    }
    response.on_hover_text(label).clicked()
}

/// The same tilted square and open center used by the game's top bar.
fn paint_roblox_mark(painter: &egui::Painter, rect: Rect) {
    let rotation = egui::emath::Rot2::from_angle(15.0_f32.to_radians());
    let corners = |radius| {
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .map(|(x, y)| rect.center() + rotation * egui::vec2(x * radius, y * radius))
    };
    let outer = corners(rect.width() * 0.375);
    let inner = corners(rect.width() * 0.125);
    let mut mesh = egui::Mesh::default();
    for point in outer.into_iter().chain(inner) {
        mesh.colored_vertex(point, egui::Color32::WHITE);
    }
    for i in 0..4 {
        let next = (i + 1) % 4;
        mesh.add_triangle(i, next, i + 4);
        mesh.add_triangle(next, next + 4, i + 4);
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_view() -> State {
        let mut state = State::new();
        state.view = Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 400.0)));
        state
    }

    #[test]
    fn play_gui_touch_keeps_the_release_for_gui_dispatch() {
        let mut state = state_with_view();
        let mut events = Vec::new();
        let pos = egui::pos2(725.0, 90.0);
        state.on_touch(1, TouchPhase::Start, pos, true, true, &mut events);
        state.on_touch(1, TouchPhase::End, pos, true, false, &mut events);
        assert!(state.look.is_empty());
        assert!(defer_pointer_gone(&mut events));
        assert!(matches!(events.last(), Some(Event::PointerButton { pressed: false, .. })));
    }

    #[test]
    fn play_drag_accumulates_motion_and_releases_outside_view() {
        let mut state = state_with_view();
        let mut events = Vec::new();
        for (phase, x, y) in [
            (TouchPhase::Start, 600.0, 180.0),
            (TouchPhase::Move, 604.0, 182.0),
            (TouchPhase::Move, 625.0, 192.0),
            (TouchPhase::Move, 790.0, 205.0),
            (TouchPhase::End, 810.0, 208.0),
        ] {
            state.on_touch(1, phase, egui::pos2(x, y), true, false, &mut events);
        }
        assert_eq!(state.look, [
            TouchLookEvent::Button(true),
            TouchLookEvent::Motion(egui::vec2(206.0, 26.0)),
            TouchLookEvent::Button(false),
        ]);
        assert!(!events.iter().any(|event| matches!(event, Event::PointerButton { .. })));
    }

    #[test]
    fn play_tap_stays_a_click_and_hud_taps_bypass_look() {
        for excluded in [false, true] {
            let mut state = state_with_view();
            let pos = egui::pos2(600.0, 150.0);
            if excluded {
                state.excl.push(Rect::from_center_size(pos, Vec2::splat(44.0)));
            }
            let mut events = Vec::new();
            state.on_touch(1, TouchPhase::Start, pos, true, false, &mut events);
            state.on_touch(1, TouchPhase::End, pos, true, false, &mut events);
            let clicks = events.iter().filter_map(|event| match event {
                Event::PointerButton { button: PointerButton::Primary, pressed, .. } => Some(*pressed),
                _ => None,
            }).collect::<Vec<_>>();
            assert_eq!(clicks, [true, false]);
            assert!(state.look.is_empty());
        }
    }

    #[test]
    fn walking_keeps_camera_and_ending_pinch_resumes_look() {
        let mut state = state_with_view();
        let mut events = Vec::new();
        state.on_touch(1, TouchPhase::Start, egui::pos2(150.0, 250.0), true, false, &mut events);
        state.on_touch(2, TouchPhase::Start, egui::pos2(650.0, 150.0), true, false, &mut events);
        state.on_touch(2, TouchPhase::Move, egui::pos2(675.0, 150.0), true, false, &mut events);
        assert!(!state.play_pinch());
        assert_eq!(state.look, [TouchLookEvent::Button(true), TouchLookEvent::Motion(egui::vec2(25.0, 0.0))]);
        state.on_touch(1, TouchPhase::End, egui::pos2(150.0, 250.0), true, false, &mut events);
        state.on_touch(3, TouchPhase::Start, egui::pos2(630.0, 120.0), true, false, &mut events);
        assert!(state.play_pinch());
        state.on_touch(2, TouchPhase::Move, egui::pos2(710.0, 150.0), true, false, &mut events);
        state.on_touch(3, TouchPhase::End, egui::pos2(630.0, 120.0), true, false, &mut events);
        state.on_touch(2, TouchPhase::Move, egui::pos2(715.0, 150.0), true, false, &mut events);
        state.on_touch(2, TouchPhase::End, egui::pos2(715.0, 150.0), true, false, &mut events);
        assert_eq!(state.look, [
            TouchLookEvent::Button(true),
            TouchLookEvent::Motion(egui::vec2(25.0, 0.0)),
            TouchLookEvent::Button(false),
            TouchLookEvent::Button(true),
            TouchLookEvent::Motion(egui::vec2(5.0, 0.0)),
            TouchLookEvent::Button(false),
        ]);
    }

    #[test]
    fn touching_hud_does_not_cancel_a_camera_drag() {
        let mut state = state_with_view();
        let mut events = Vec::new();
        let hud = egui::pos2(100.0, 20.0);
        state.excl.push(Rect::from_center_size(hud, Vec2::splat(44.0)));
        for (id, phase, pos) in [
            (1, TouchPhase::Start, egui::pos2(610.0, 170.0)),
            (1, TouchPhase::Move, egui::pos2(630.0, 170.0)),
            (2, TouchPhase::Start, hud),
            (1, TouchPhase::Move, egui::pos2(639.0, 171.0)),
            (2, TouchPhase::End, hud),
        ] {
            state.on_touch(id, phase, pos, true, false, &mut events);
        }
        assert!(!state.play_pinch());
        assert_eq!(state.look, [TouchLookEvent::Button(true), TouchLookEvent::Motion(egui::vec2(29.0, 1.0))]);
        state.reset_touches(&mut events);
        assert_eq!(state.look.last(), Some(&TouchLookEvent::Button(false)));
        assert!(state.touches.is_empty());
    }

    #[test]
    fn swipe_with_only_a_start_and_end_is_not_a_click() {
        let mut state = state_with_view();
        let mut events = Vec::new();
        state.on_touch(1, TouchPhase::Start, egui::pos2(610.0, 170.0), true, false, &mut events);
        state.on_touch(1, TouchPhase::End, egui::pos2(660.0, 175.0), true, false, &mut events);
        assert_eq!(state.look, [
            TouchLookEvent::Button(true),
            TouchLookEvent::Motion(egui::vec2(50.0, 5.0)),
            TouchLookEvent::Button(false),
        ]);
        assert!(!events.iter().any(|event| matches!(event, Event::PointerButton { .. })));
    }
}
