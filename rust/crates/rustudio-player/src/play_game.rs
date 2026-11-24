//! In-game interaction during Play, following the public Roblox docs:
//! ClickDetectors (MouseClick, RightMouseClick, hover, MaxActivationDistance),
//! the player's Mouse, chat (Player.Chatted), Tools and the hotbar,
//! Explosions, Sounds (real audio), Humanoid:MoveTo for NPCs, and a stand-in
//! for the chat system's ChatServiceRunner.

use std::sync::Arc;

use egui::{Color32, Rect, Stroke};

use super::*;
use rustudio_engine::audio::Audio;
use rustudio_engine::runtime::ext::SoundCmd;
use rustudio_render::scene::vec3_of;

#[path = "game_menu.rs"]
pub mod game_menu;

/// What's under the cursor (refs belong to the DataModel shown when picked).
#[derive(Debug, Clone, Copy)]
pub struct Pointer {
    pub origin: Vec3,
    pub dir: Vec3,
    pub hit: Option<(Ref, Vec3)>,
    /// What a ClickDetector click meets along the same ray: Studio's click
    /// ray passes through parts with Transparency above 0.95 that have no
    /// ClickDetector (Mouse.Target still reports them).
    pub click: Option<(Ref, Vec3)>,
    pub xy: [f32; 2],
    pub view: [f32; 2],
    pub side: Side,
}

/// Input the viewport collected for the next play tick.
#[derive(Default)]
pub struct PlayInput {
    pub pointer: Option<Pointer>,
    /// (part clicked, where the click ray met it, mouse button 1 or 2).
    pub clicks: Vec<(Ref, Vec3, u8)>,
    /// (mouse button, pressed).
    pub buttons: Vec<(u8, bool)>,
    pub chat: Vec<String>,
    /// Hotbar slots toggled (0-based).
    pub slots: Vec<usize>,
    /// Clicks whose world hit will be resolved by the gameplay worker.
    pub(crate) ray_clicks: Vec<(Pointer, u8)>,
}

struct Equipped {
    tool: Ref,
    /// Each tool part relative to the handle.
    offsets: Vec<(Ref, Xform)>,
    client_told: bool,
}

/// A Tool's handle: its child BasePart named "Handle" (Roblox's RequiresHandle part).
/// (`Document::find_child` matches a class, and there is no class "Handle": every
/// equipped tool had no handle here and was never put in the hand.)
fn tool_handle(d: &Document, tool: Ref) -> Option<Ref> {
    d.dom.get_by_ref(tool)?.children().iter().copied().find(|c| {
        d.dom.get_by_ref(*c).is_some_and(|i| i.name == "Handle" && doc::is_a(&i.class, "BasePart"))
    })
}

struct Walk {
    target: Vec3,
    since: f64,
}

struct Track {
    server: Option<Ref>,
    client: Option<Ref>,
    /// Sound id still downloading.
    pending: Option<String>,
    /// The SoundId it last started with.
    id: Option<String>,
    /// SoundService:PlayLocalSound's copy: plays, but the Sound's own state
    /// (Playing, TimePosition, Ended) is not touched.
    detached: bool,
}

#[derive(Clone)]
pub struct Flash {
    pub pos: Vec3,
    pub radius: f32,
    pub born: f64,
}

#[derive(Default)]
pub struct GameState {
    pub input: PlayInput,
    pub(crate) last_pointer: Option<Pointer>,
    hover_detector: Option<(Ref, bool)>,
    /// Whether the cursor is over something clickable (hand cursor).
    pub clickable: bool,
    equipped: Option<Equipped>,
    /// A Tool only the client has, equipped by a client script (client refs): Studio's
    /// client welds it to its own character's hand and the server never hears of it
    /// (a game's script hands out its tools that way).
    client_tool: Option<Equipped>,
    /// Tools on the hotbar (server refs), in slot order.
    pub slots: Vec<Ref>,
    walks: HashMap<Ref, Walk>,
    /// Humanoid:MoveTo target for the player's own character.
    pub move_target: Option<Vec3>,
    pub jump: bool,
    /// Humanoid:ChangeState for the player's own character (state name).
    pub forced_state: Option<String>,
    /// Seat:Sit asked for the player's own character (server seat).
    pub sit_on: Option<Ref>,
    /// States its Humanoid:SetStateEnabled turned off.
    pub states_off: HashSet<String>,
    audio: Option<Audio>,
    tracks: HashMap<u64, Track>,
    removed_sounds: Vec<rustudio_engine::audio_scene::RemovedSound>,
    /// SoundService:PlayLocalSound calls so far (keys their copies).
    local_plays: u64,
    pub flashes: Vec<Flash>,
    pub chat_open: bool,
    /// True once Roblox's real CoreScripts started in this Play session
    /// (suppresses rustudio's native fallback HUD / Escape menu / hotbar).
    pub core_scripts_active: bool,
    /// Frames the playtest view had chat available but closed: where the
    /// old "Press / to chat" hint was drawn (the self-test checks none is).
    pub chat_ready_frames: u64,
    pub chat_text: String,
    /// (line, time) shown over the viewport.
    pub chat_log: Vec<(String, f64)>,
    /// Player:Kick's message: the player was disconnected (Studio's
    /// "Disconnected" dialog shows over the game).
    pub kicked: Option<String>,
    /// Play time of the player's last input, and of the last Player.Idled.
    pub last_activity: f64,
    idled_at: f64,
    warned: HashSet<String>,
    /// Audio::play starts per sound key (RUSTUDIO_AUDIO_LOG measurement).
    pub audio_starts: HashMap<u64, u32>,
    /// The same per SoundId, and how often each id was fetched (self-test).
    pub audio_starts_by_id: HashMap<String, u32>,
    pub sound_fetches: HashMap<String, u32>,
    /// Sound ids that came back unavailable: not fetched again this session.
    sound_unavailable: HashSet<String>,
    /// GUI events from the viewport, fired on the client next tick.
    pub gui_events: Vec<rustudio_engine::gui::GuiEvent>,
    /// Where the client's GUI objects were drawn (AbsolutePosition / AbsoluteSize).
    pub gui_layout: HashMap<Ref, rustudio_engine::gui::GuiBox>,
    pub(crate) gui_layout_changed: bool,
    pub(crate) input_active: bool,
    /// Mouse buttons for UserInputService: (button, down, gameProcessedEvent).
    pub uis_mouse: Vec<(u8, bool, bool)>,
    pub touch_look: Vec<crate::game_view::TouchLookEvent>,
    /// The in-game menu (Esc) is open: the game gets no keys.
    pub menu_open: bool,
    /// Menu opened (true) / closed (false) since the last client tick
    /// (GuiService.MenuOpened / MenuClosed).
    pub menu_events: Vec<bool>,
    /// The menu's pages, tween and settings (game_menu.rs).
    pub menu: game_menu::MenuState,
    /// The menu's Reset Character / Leave were chosen.
    pub reset_request: bool,
    pub leave_request: bool,
    /// Keys pressed since the last tick (a same-frame tap included), and
    /// whether a focused TextBox had the keys then (gameProcessedEvent).
    pub key_taps: Vec<(egui::Key, bool)>,
    /// The same presses for the viewport's hotbar and chat keys: app.rs
    /// strips unmodified key events from egui's input while playing, so
    /// `key_pressed` never sees them there (only presses no TextBox took).
    pub hotbar_taps: Vec<egui::Key>,
}

impl GameState {
    /// Sounds playing now (the profiler's audio voices counter).
    pub(crate) fn audio_voices(&self) -> usize {
        self.audio.as_ref().map_or(0, |a| a.playing_count())
    }

    /// View > Mute: silences the sounds playing now (later ones read the flag).
    pub fn set_muted(&mut self, on: bool) {
        if let Some(a) = self.audio.as_mut() {
            a.set_muted(on);
        }
    }

    /// Whether a Sound with this SoundId is still being played (self-test).
    pub fn is_sound_live(&self, id: &str) -> bool {
        self.tracks.iter().any(|(k, t)| {
            t.id.as_deref() == Some(id) && self.audio.as_ref().is_some_and(|a| a.is_playing(*k))
        })
    }
}

/// An InputObject for a mouse button (1, 2; 0 = movement) at a GUI position.
pub fn mouse_input_object(
    lua: &mlua::Lua,
    button: u8,
    began: bool,
    at: Option<(f32, f32)>,
) -> mlua::Result<Value> {
    mouse_input_object_for_event(lua, None, button, began, at)
}

fn mouse_input_object_for_event(
    lua: &mlua::Lua,
    event_name: Option<&str>,
    button: u8,
    began: bool,
    at: Option<(f32, f32)>,
) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    let kind = match button {
        1 => "MouseButton1",
        2 => "MouseButton2",
        // The wheel, forward / backward (Position.Z +1 / -1).
        3 | 4 => "MouseWheel",
        _ => "MouseMovement",
    };
    let state = match event_name {
        Some("InputBegan") => "Begin",
        Some("InputEnded") => "End",
        Some("InputChanged") => "Change",
        _ if matches!(button, 0 | 3 | 4) => "Change",
        _ if began => "Begin",
        _ => "End",
    };
    for (prop, en, item) in [
        ("UserInputType", "UserInputType", kind),
        ("UserInputState", "UserInputState", state),
        ("KeyCode", "KeyCode", "Unknown"),
    ] {
        if let Some(k) = LEnumItem::from_name(en, item) {
            t.set(prop, rustudio_engine::ltypes::enum_value(lua, k)?)?;
        }
    }
    let (x, y) = at.unwrap_or((0.0, 0.0));
    let z = match button {
        3 => 1.0,
        4 => -1.0,
        _ => 0.0,
    };
    t.set(
        "Position",
        rustudio_engine::ltypes::LVector3(Vec3::new(x, y, z)).into_value(),
    )?;
    t.set(
        "Delta",
        rustudio_engine::ltypes::LVector3(Vec3::ZERO).into_value(),
    )?;
    // As a real InputObject (a userdata), like UserInputService's.
    match lua.globals().get::<mlua::Function>("__rs_new_input_object") {
        Ok(f) => f.call(t),
        Err(_) => Ok(Value::Table(t)),
    }
}

/// Fires the viewport's GUI events on the client: clicks, hover, text,
/// selection and scrolling.
pub fn fire_gui_events(rt: &Runtime, events: Vec<rustudio_engine::gui::GuiEvent>, gui_service: Option<Ref>) {
    use rustudio_engine::gui::{GuiArg, GuiEvent};
    let lua = &rt.lua;
    let debug = std::env::var_os("RUSTUDIO_GUI_DEBUG").is_some();
    rustudio_engine::runtime::domains::gui_input::focus_now(lua);
    for ev in events {
        if debug
            && !matches!(
                &ev,
                GuiEvent::Fire(_, "MouseMoved" | "MouseEnter" | "MouseLeave", _)
            )
        {
            if let Some(mut w) = rt.world_mut() {
                let line = match &ev {
                    GuiEvent::Fire(r, name, _) => format!("[gui] {name} on {}", w.full_name(*r)),
                    other => format!("[gui] {other:?}"),
                };
                w.output.push((Level::Info, line));
            }
        }
        match ev {
            GuiEvent::Fire(r, name, args) => {
                let vals: Vec<Value> = args
                    .iter()
                    .filter_map(|a| match a {
                        GuiArg::Bool(b) => Some(Value::Boolean(*b)),
                        GuiArg::Num(n) => Some(Value::Number(*n)),
                        GuiArg::Input {
                            button,
                            began,
                            x,
                            y,
                        } => mouse_input_object_for_event(lua, Some(name), *button, *began, Some((*x, *y))).ok(),
                    })
                    .collect();
                rt.fire(r, name, vals);
                // UserInputService.TextBoxFocused / TextBoxFocusReleased(textBox)
                // go with a TextBox's Focused / FocusLost.
                let uis_ev = match name {
                    "Focused" => Some("TextBoxFocused"),
                    "FocusLost" => Some("TextBoxFocusReleased"),
                    _ => None,
                };
                if let Some(ev) = uis_ev {
                    let uis = rt.world_mut().and_then(|w| {
                        let d = w.d();
                        d.find_child(d.dom.root_ref(), "UserInputService")
                    });
                    if let Some(u) = uis {
                        rt.fire(u, ev, vec![rt.inst(r)]);
                    }
                }
            }
            GuiEvent::SetText(r, text) => {
                if let Ok(s) = lua.create_string(&text) {
                    rt.set(r, "Text", Value::String(s));
                }
            }
            GuiEvent::TextBoxUpdate {
                r,
                text,
                cursor,
                selection,
            } => {
                rustudio_engine::runtime::domains::gui_input::apply_textbox_edit(
                    lua, r, text, cursor, selection,
                );
            }
            GuiEvent::Select(sel) => {
                if let Some(gs) = gui_service {
                    rt.set(
                        gs,
                        "SelectedObject",
                        sel.map(|s| rt.inst(s)).unwrap_or(Value::Nil),
                    );
                }
            }
            GuiEvent::SelectCore(sel) => {
                if let Some(gs) = gui_service {
                    let _ = rustudio_engine::runtime::security::with_main_identity(
                        lua,
                        rustudio_engine::runtime::security::CORE,
                        || {
                            rt.set(
                                gs,
                                "SelectedCoreObject",
                                sel.map(|s| rt.inst(s)).unwrap_or(Value::Nil),
                            );
                            Ok::<(), mlua::Error>(())
                        },
                    );
                }
            }
            GuiEvent::GuiStateChanged(r) => {
                rustudio_engine::runtime::domains::gui_input::check_gui_state(lua, r);
            }
            GuiEvent::Scroll(r, d) => scroll_by(rt, r, d),
        }
    }
    rt.dispatch();
}

/// A ScrollingFrame on its way to where the wheel sent it (Studio eases
/// each notch in over a few frames: captures/studio-widgets.txt in
/// checks/plugins/widgets, two notches 0 → 8, 24, 42 … 92, 103, 120 … 200).
struct SmoothScroll {
    pos: glam::Vec2,
    vel: glam::Vec2,
    target: glam::Vec2,
    /// The CanvasPosition last written: anything else there means a script
    /// moved the canvas, which ends the glide.
    written: glam::Vec2,
    acc: f32,
}

/// The glides of one VM (the game's, or the plugins' edit VM), kept in that
/// Lua state's app data: they go when the VM goes, and a new VM (a reload,
/// a new Play session) starts with none.
#[derive(Default)]
struct SmoothScrolls(HashMap<Ref, SmoothScroll>);

fn scrolls_mut(lua: &mlua::Lua) -> mlua::AppDataRefMut<'_, SmoothScrolls> {
    if lua.app_data_ref::<SmoothScrolls>().is_none() {
        lua.set_app_data(SmoothScrolls::default());
    }
    lua.app_data_mut::<SmoothScrolls>().expect("smooth scroll state")
}

/// A ScrollingFrame's CanvasPosition and how far it can scroll now (canvas
/// less the frame), from its last layout and its CanvasSize as it is now
/// (so a canvas a script resized is seen before the next layout). None
/// when the frame is gone.
fn scroll_range(rt: &rustudio_engine::runtime::Runtime, r: Ref) -> Option<(glam::Vec2, glam::Vec2)> {
    let w = rt.world_mut()?;
    let doc = w.d();
    if !doc.exists(r) {
        return None;
    }
    let cp = match doc.prop(r, "CanvasPosition") {
        Some(Variant::Vector2(v)) => glam::Vec2::new(v.x, v.y),
        _ => glam::Vec2::ZERO,
    };
    let laid = w.input.gui_layout.get(&r);
    let size = laid.map(|b| b.size).unwrap_or([0.0, 0.0]);
    let win = laid.and_then(|b| b.window).unwrap_or(size);
    let cs_prop = match doc.prop(r, "CanvasSize") {
        Some(Variant::UDim2(u)) => Some([u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32]),
        _ => None,
    };
    let sd = rustudio_render::scene::enum_u32(doc.prop(r, "ScrollingDirection")).unwrap_or(4);
    // The drawn canvas (AutomaticCanvasSize included), redone with the
    // current CanvasSize when the layout recorded how.
    let cs = match (laid.and_then(|b| cs_prop.and_then(|c| b.canvas_for(c))), laid.and_then(|b| b.canvas), cs_prop) {
        (Some(c), _, _) => c,
        (None, Some(c), _) => c,
        (None, None, Some(u)) => [u[0] * size[0] + u[1], u[2] * size[1] + u[3]],
        _ => size,
    };
    let max_x = if sd == 2 { 0.0 } else { (cs[0] - win[0]).max(0.0) };
    let max_y = if sd == 1 { 0.0 } else { (cs[1] - win[1]).max(0.0) };
    Some((cp, glam::Vec2::new(max_x, max_y)))
}

/// The wheel over a ScrollingFrame: its target moves by the notches
/// (ScrollWheelDeltaAmount pixels each), within the canvas; the canvas
/// glides there in `step_smooth_scrolls`. Another notch during a glide
/// adds to its target (Studio's two-notch capture); after a script wrote
/// CanvasPosition the next notch starts from the script's value.
fn scroll_by(rt: &rustudio_engine::runtime::Runtime, r: Ref, d: [f32; 2]) {
    let Some((cur, max)) = scroll_range(rt, r) else {
        return;
    };
    let delta = glam::Vec2::new(d[0], d[1]) * mech::GUI_SCROLL_PX_PER_POINT;
    let mut m = scrolls_mut(&rt.lua);
    let fresh = SmoothScroll { pos: cur, vel: glam::Vec2::ZERO, target: cur, written: cur, acc: 0.0 };
    let s = m.0.entry(r).or_insert(fresh);
    if s.written != cur {
        *s = SmoothScroll { pos: cur, vel: glam::Vec2::ZERO, target: cur, written: cur, acc: 0.0 };
    }
    s.target = (s.target + delta).clamp(glam::Vec2::ZERO, max);
}

/// One frame of the gliding ScrollingFrames (`scroll_by`), in steps of
/// Studio's 60 Hz frame (assumed: the capture has no timestamps): the speed
/// eases toward a share of the distance left (mech GUI_SCROLL_EASE_*),
/// CanvasPosition takes whole pixels as Studio reports them, and lands
/// exactly on the target. The scroll range is read again each frame: a
/// canvas that shrank mid-glide pulls the target in, and the canvas never
/// goes past either end (rustudio's rule; no Studio capture of a resize
/// mid-glide).
pub fn step_smooth_scrolls(rt: &rustudio_engine::runtime::Runtime, dt: f32) {
    let lua = &rt.lua;
    let todo: Vec<Ref> = match lua.app_data_ref::<SmoothScrolls>() {
        Some(m) if !m.0.is_empty() => m.0.keys().copied().collect(),
        _ => return,
    };
    for r in todo {
        let range = scroll_range(rt, r);
        let next = {
            let mut m = scrolls_mut(lua);
            let Some(s) = m.0.get_mut(&r) else { continue };
            let Some((_, max)) = range.filter(|(now, _)| *now == s.written) else {
                // Gone, or a script moved the canvas.
                m.0.remove(&r);
                continue;
            };
            s.target = s.target.clamp(glam::Vec2::ZERO, max);
            s.acc += dt.max(0.0);
            let frame = 1.0 / 60.0;
            while s.acc >= frame * 0.999 {
                s.acc -= frame;
                let want = (s.target - s.pos) * mech::GUI_SCROLL_EASE_PULL;
                s.vel += (want - s.vel) * mech::GUI_SCROLL_EASE_BLEND;
                s.pos += s.vel;
                // Past an end (the canvas shrank): stop there on that axis.
                let held = s.pos.clamp(glam::Vec2::ZERO, max);
                s.vel = glam::Vec2::select(held.cmpne(s.pos), glam::Vec2::ZERO, s.vel);
                s.pos = held;
            }
            let done = (s.target - s.pos).abs().max_element() < 0.5 && s.vel.abs().max_element() < 0.5;
            let out = if done { s.target } else { s.pos.round() };
            let changed = out != s.written;
            s.written = out;
            if done {
                m.0.remove(&r);
            }
            changed.then_some(out)
        };
        if let Some(v) = next {
            if let Ok(u) = lua.create_userdata(rustudio_engine::ltypes::LVector2(v)) {
                rt.set(r, "CanvasPosition", Value::UserData(u));
            }
        }
    }
    rt.dispatch();
}

fn sound_key(r: Ref) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.hash(&mut h);
    h.finish()
}

fn f32_prop(d: &Document, r: Ref, name: &str, default: f32) -> f32 {
    f32_of(d.prop(r, name)).unwrap_or(default)
}

/// Source of the stand-in ChatService module (rustudio's own code).
const CHAT_SERVICE_STUB: &str = r#"-- rustudio stand-in for the chat system's ChatService module.
-- The calls exist so scripts that wait for it keep running; messages go to Output.
local Players = game:GetService("Players")
local ChatService = {}
local speakers, channels = {}, {}
local added, removed = Instance.new("BindableEvent"), Instance.new("BindableEvent")
ChatService.SpeakerAdded = added.Event
ChatService.SpeakerRemoved = removed.Event
local function channel(name)
    local c = channels[name]
    if c then return c end
    c = { Name = name }
    function c:SendSystemMessage(msg) print("[" .. name .. "] " .. tostring(msg)) end
    function c:KickSpeaker() end
    function c:MuteSpeaker() end
    function c:RegisterProcessCommandsFunction() end
    function c:RegisterFilterMessageFunction() end
    channels[name] = c
    return c
end
function ChatService:GetChannel(name) return channel(name) end
function ChatService:AddChannel(name) return channel(name) end
function ChatService:RemoveChannel(name) channels[name] = nil end
function ChatService:GetSpeaker(name) return speakers[name] end
function ChatService:AddSpeaker(name)
    local s = { Name = name }
    function s:SayMessage(msg) print("[Chat] " .. name .. ": " .. tostring(msg)) end
    function s:JoinChannel(c) return channel(c) end
    function s:LeaveChannel() end
    function s:SetExtraData() end
    function s:GetExtraData() return nil end
    function s:SendSystemMessage(msg) print("[System] " .. tostring(msg)) end
    function s:GetPlayer() return Players:FindFirstChild(name) end
    speakers[name] = s
    added:Fire(name)
    return s
end
function ChatService:RemoveSpeaker(name) speakers[name] = nil; removed:Fire(name) end
function ChatService:GetSpeakerList()
    local t = {}
    for n in pairs(speakers) do table.insert(t, n) end
    return t
end
function ChatService:RegisterProcessCommandsFunction() end
function ChatService:UnregisterProcessCommandsFunction() end
function ChatService:RegisterFilterMessageFunction() end
function ChatService:UnregisterFilterMessageFunction() end
function ChatService:InternalApplyRobloxFilter(_, msg) return msg end
return ChatService
"#;

impl Game {
    /// Adds ServerScriptService.ChatServiceRunner (with a ChatService module)
    /// and Chat.ChatModules when the place has none, so WaitForChild calls on
    /// legacy Chat children return instead of infinitely yielding.
    pub(super) fn add_chat_stub(&mut self) {
        let root = self.doc.dom.root_ref();
        if let Some(chat) = self.doc.find_child(root, "Chat") {
            let load_default = !matches!(self.doc.prop(chat, "LoadDefaultChat"), Some(Variant::Bool(false)));
            if load_default && self.doc.find_named(chat, "ChatModules").is_none() {
                let m = self
                    .doc
                    .dom
                    .insert(chat, InstanceBuilder::new("Folder").with_name("ChatModules"));
                self.doc.mark_tree(m);
            }
        }
        let Some(sss) = self.doc.find_child(root, "ServerScriptService") else {
            return;
        };
        if self.doc.find_child(sss, "ChatServiceRunner").is_some()
            || self.doc.find_named(sss, "ChatServiceRunner").is_some()
        {
            return;
        }
        let runner = InstanceBuilder::new("Script")
            .with_name("ChatServiceRunner")
            .with_property("Disabled", true)
            .with_property("Enabled", false)
            .with_child(
                InstanceBuilder::new("ModuleScript")
                    .with_name("ChatService")
                    .with_property("Source", CHAT_SERVICE_STUB.to_string()),
            );
        let r = self.doc.dom.insert(sss, runner);
        self.doc.mark_tree(r);
    }

    /// TextChannel:SendAsync messages: from the client runtime to the
    /// server's (Chatted, ShouldDeliverCallback) and back to the client
    /// (MessageReceived, the window and the bubble). Player:Kick too.
    fn pump_text_chat(&mut self) {
        use rustudio_engine::runtime::domains::players_chat as pc;
        let Some(player) = self.play.as_ref().map(|p| p.player) else {
            return;
        };
        let out = self
            .with_side(Side::Client, |rt, _, _| pc::take_outgoing(&rt.lua))
            .unwrap_or_default();
        let uid = match self.doc.prop(player, "UserId") {
            Some(Variant::Int64(u)) => u,
            _ => 0,
        };
        for m in out {
            let server_ch = match m.channel {
                Some(c) => match self.server_of(c) {
                    Some(sc) => Some(sc),
                    None => continue,
                },
                None => None,
            };
            let to = self
                .with_side(Side::Server, |rt, _, _| {
                    let ids = pc::server_receive(&rt.lua, player, server_ch, &m);
                    rt.dispatch();
                    ids
                })
                .unwrap_or_default();
            if to.contains(&uid) {
                let client_ch = server_ch.and_then(|sc| self.client_of(sc));
                if m.kind == "legacy" || client_ch.is_some() {
                    self.with_side(Side::Client, |rt, _, _| {
                        pc::client_receive(&rt.lua, client_ch, uid, &m);
                        rt.dispatch();
                    });
                }
            }
        }
        if let Some(msg) = pc::take_kick() {
            if let Some(s) = self.play.as_mut() {
                if s.game.kicked.is_none() {
                    s.game.kicked = Some(msg.clone());
                    // The client left: the server sees the player go, as in
                    // Studio: the kick logged, CharacterRemoving, then
                    // PlayerRemoving(player, Enum.PlayerExitReason.CreatorKick).
                    self.error(format!("Server Kick Message: {msg}; Metadata:"));
                    let model = self.play.as_ref().and_then(|s| s.character.as_ref()).map(|c| c.model);
                    if let Some(m) = model {
                        self.fire_character_removing(m);
                    }
                    let players = self.doc.find_child(self.doc.dom.root_ref(), "Players");
                    if let Some(ps) = players {
                        self.with_side(Side::Server, |rt, _, _| {
                            let reason = rt
                                .lua
                                .load("return Enum.PlayerExitReason.CreatorKick")
                                .eval::<Value>()
                                .unwrap_or(Value::Nil);
                            rt.fire(ps, "PlayerRemoving", vec![rt.inst(player), reason]);
                            rt.dispatch();
                        });
                    }
                }
            }
        }
    }

    /// Player.Idled(seconds idle): two minutes after the player's last
    /// input, then every 30 seconds while still idle (both sides).
    fn fire_idled(&mut self, now: f64) {
        let Some((player, last, at)) = self.play.as_ref().map(|s| (s.player, s.game.last_activity, s.game.idled_at)) else {
            return;
        };
        let idle = now - last;
        let due = if at <= last { idle >= 120.0 } else { now - at >= 30.0 };
        if !due {
            return;
        }
        if let Some(s) = self.play.as_mut() {
            s.game.idled_at = now;
        }
        self.with_side(Side::Server, |rt, _, _| rt.fire(player, "Idled", vec![Value::Number(idle)]));
        if let Some(cp) = self.client_of(player) {
            self.with_side(Side::Client, |rt, _, _| rt.fire(cp, "Idled", vec![Value::Number(idle)]));
        }
    }

    fn client_of(&self, server: Ref) -> Option<Ref> {
        self.play.as_ref()?.map.s2c.get(&server).copied()
    }

    fn server_of(&self, client: Ref) -> Option<Ref> {
        self.play.as_ref()?.map.c2s.get(&client).copied()
    }

    fn to_server(&self, side: Side, r: Ref) -> Option<Ref> {
        match side {
            Side::Server => Some(r),
            Side::Client => self.server_of(r),
        }
    }

    /// Fires `event(player)` on the server instance and its client copy.
    fn fire_with_player(&mut self, server_target: Ref, event: &str) {
        let Some(player) = self.play.as_ref().map(|p| p.player) else {
            return;
        };
        let ev = event.to_string();
        self.with_side(Side::Server, |rt, _, _| {
            rt.fire(server_target, &ev, vec![rt.inst(player)])
        });
        if let (Some(c), Some(cp)) = (self.client_of(server_target), self.client_of(player)) {
            self.with_side(Side::Client, |rt, _, _| rt.fire(c, &ev, vec![rt.inst(cp)]));
        }
    }

    /// Fires `event()` on the server instance and its client copy.
    fn fire_both(&mut self, server_target: Ref, event: &str) {
        let ev = event.to_string();
        self.with_side(Side::Server, |rt, _, _| rt.fire(server_target, &ev, vec![]));
        if let Some(c) = self.client_of(server_target) {
            self.with_side(Side::Client, |rt, _, _| rt.fire(c, &ev, vec![]));
        }
    }

    /// The ClickDetector a click on `part` reaches: one in the part or in a
    /// Model / folder above it (below Workspace).
    fn click_detector_for(&self, part: Ref) -> Option<Ref> {
        click_detector_in(&self.doc, part)
    }

    /// A ClickDetector on a part only the client has (a client-cloned
    /// island): the click and hover fire on the client alone, as in Roblox.
    fn client_click_detector_for(&self, part: Ref, at: Vec3) -> Option<Ref> {
        let s = self.play.as_ref()?;
        let cd = s.other_doc.as_ref()?;
        let det = click_detector_in(cd, part)?;
        reach_in(cd, det, at, self.character_pos()).then_some(det)
    }

    /// The ClickDetector in reach that the mouse on `part` (a `side` ref, met
    /// at `at`) reaches, with whether it lives on the client alone.
    fn detector_under(&self, side: Side, part: Ref, at: Vec3) -> Option<(Ref, bool)> {
        // The client's DataModel decides what a click on the client's view
        // reaches, as in Roblox: a ClickDetector a LocalScript put on a
        // replicated part (a game's lobby tab, its egg targets) exists only
        // there. A replicated detector fires on both sides.
        if side == Side::Client {
            if let Some(cd) = self.play.as_ref().and_then(|s| s.other_doc.as_ref()) {
                let det = click_detector_in(cd, part)?;
                if !reach_in(cd, det, at, self.character_pos()) {
                    return None;
                }
                return Some(match self.server_of(det) {
                    Some(sdet) if self.doc.exists(sdet) => (sdet, false),
                    _ => (det, true),
                });
            }
        }
        match self.to_server(side, part) {
            Some(sp) => self.click_detector_for(sp).filter(|d| self.in_reach(*d, at)).map(|d| (d, false)),
            None if side == Side::Client => self.client_click_detector_for(part, at).map(|d| (d, true)),
            None => None,
        }
    }

    fn fire_client_with_player(&mut self, det: Ref, event: &str) {
        let Some(cp) = self.play.as_ref().and_then(|p| self.client_of(p.player)) else { return };
        let ev = event.to_string();
        self.with_side(Side::Client, |rt, _, _| rt.fire(det, &ev, vec![rt.inst(cp)]));
    }
}

/// Whether the character's root at `root` is within `detector`'s
/// MaxActivationDistance of `hit`, where the click ray met the part (no
/// character: always). Studio measures from the HumanoidRootPart to the
/// mouse's hit point: neither the camera nor the part's centre counts (a
/// 40-stud part clicked at its near end is in reach of a max of 12).
fn reach_in(d: &Document, detector: Ref, hit: Vec3, root: Option<Vec3>) -> bool {
    let max = f32_prop(d, detector, "MaxActivationDistance", mech::CLICK_DISTANCE);
    root.is_none_or(|root| (hit - root).length() <= max)
}

/// Whether a ClickDetector click passes through `part`: Transparency above
/// 0.95 and no ClickDetector of its own (in it or a Model above it).
fn click_see_through(d: &Document, part: Ref) -> bool {
    f32_prop(d, part, "Transparency", 0.0) > mech::CLICK_SEE_THROUGH && click_detector_in(d, part).is_none()
}

fn click_detector_in(d: &Document, part: Ref) -> Option<Ref> {
    {
        let ws = d.workspace()?;
        let mut cur = part;
        while cur.is_some() && cur != ws {
            let inst = d.dom.get_by_ref(cur)?;
            if let Some(c) = inst
                .children()
                .iter()
                .copied()
                .find(|c| d.class_of(*c).is_some_and(|k| k == "ClickDetector"))
            {
                return Some(c);
            }
            cur = inst.parent();
        }
        None
    }
}

impl Game {
    /// The live character's HumanoidRootPart (server DataModel), else its body.
    fn character_pos(&self) -> Option<Vec3> {
        let c = self.play.as_ref()?.character.as_ref().filter(|c| !c.dead)?;
        let root = self
            .doc
            .find_named(c.model, "HumanoidRootPart")
            .and_then(|r| rustudio_render::scene::xform_of(&self.doc, r));
        Some(root.map_or(c.body.pos, |xf| xf.pos))
    }

    fn in_reach(&self, detector: Ref, hit: Vec3) -> bool {
        reach_in(&self.doc, detector, hit, self.character_pos())
    }

    /// The input the viewport queued, before the server's scripts run.
    pub(super) fn game_inputs(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let input = std::mem::take(&mut s.game.input);
        if input.pointer.is_some() {
            s.game.last_pointer = input.pointer;
        }
        let player = s.player;
        let now = s.time;
        let me = self.player_name();

        for msg in input.chat {
            // TextChatService chat: the line goes out through the target
            // channel's SendAsync (players_chat.luau), which fires Chatted.
            let tcs = self
                .with_side(Side::Client, |rt, _, _| rustudio_engine::runtime::domains::players_chat::chat_input(&rt.lua, &msg))
                .unwrap_or(false);
            if tcs {
                continue;
            }
            let line = format!("{me}: {msg}");
            self.info(format!("[Chat] {line}"));
            if let Some(s) = self.play.as_mut() {
                s.game.chat_log.push((line, now));
            }
            let m = msg.clone();
            self.with_side(Side::Server, |rt, _, _| {
                if let Ok(text) = rt.lua.create_string(&m) {
                    rt.fire(player, "Chatted", vec![Value::String(text), Value::Nil]);
                }
            });
            if let Some(cp) = self.client_of(player) {
                self.with_side(Side::Client, |rt, _, _| {
                    if let Ok(text) = rt.lua.create_string(&msg) {
                        rt.fire(cp, "Chatted", vec![Value::String(text), Value::Nil]);
                    }
                });
            }
        }

        self.pump_text_chat();
        self.fire_idled(now);

        // Hovering a ClickDetector.
        let pointer = self.play.as_ref().and_then(|s| s.game.last_pointer);
        let hovered = pointer.and_then(|p| p.click.and_then(|(r, at)| self.detector_under(p.side, r, at)));
        let before = self.play.as_ref().and_then(|s| s.game.hover_detector);
        if hovered != before {
            match before {
                Some((old, false)) if self.doc.exists(old) => self.fire_with_player(old, "MouseHoverLeave"),
                Some((old, true)) => self.fire_client_with_player(old, "MouseHoverLeave"),
                _ => {}
            }
            match hovered {
                Some((new, false)) => self.fire_with_player(new, "MouseHoverEnter"),
                Some((new, true)) => self.fire_client_with_player(new, "MouseHoverEnter"),
                None => {}
            }
        }
        if let Some(s) = self.play.as_mut() {
            s.game.hover_detector = hovered;
            s.game.clickable = hovered.is_some();
        }

        // Clicks on ClickDetectors (fired on both sides with the player).
        let side = pointer.map(|p| p.side).unwrap_or(Side::Server);
        for (r, at, button) in input.clicks {
            let ev = if button == 1 { "MouseClick" } else { "RightMouseClick" };
            match self.detector_under(side, r, at) {
                Some((det, false)) => self.fire_with_player(det, ev),
                Some((det, true)) => self.fire_client_with_player(det, ev),
                None => {}
            }
        }

        // DragDetector drags (play_drag.rs).
        self.drag_detectors(&input.buttons);

        for slot in input.slots {
            let tool = self
                .play
                .as_ref()
                .and_then(|s| s.game.slots.get(slot).copied());
            let equipped = self
                .play
                .as_ref()
                .and_then(|s| s.game.equipped.as_ref().map(|e| e.tool));
            match tool {
                Some(t) if Some(t) == equipped => self.unequip_tool(),
                Some(t) => self.equip_tool(t),
                None => {}
            }
        }

        // Mouse buttons: the equipped tool activates; the Mouse object fires.
        let equipped = self
            .play
            .as_ref()
            .and_then(|s| s.game.equipped.as_ref().map(|e| e.tool));
        let mouse = self
            .play
            .as_ref()
            .and_then(|s| s.client.as_ref())
            .and_then(|c| c.world.as_ref())
            .and_then(|w| w.input.mouse);
        let client_tool = self.play.as_ref().and_then(|s| s.game.client_tool.as_ref().map(|e| e.tool));
        for (button, down) in input.buttons {
            if button == 1 {
                if let Some(t) = equipped {
                    self.fire_both(t, if down { "Activated" } else { "Deactivated" });
                }
                if let Some(t) = client_tool {
                    self.with_side(Side::Client, |rt, _, _| rt.fire(t, if down { "Activated" } else { "Deactivated" }, vec![]));
                }
            }
            if let Some(m) = mouse {
                let ev = format!("Button{button}{}", if down { "Down" } else { "Up" });
                self.with_side(Side::Client, |rt, _, _| rt.fire(m, &ev, vec![]));
            }
        }
    }

    /// Mouse state for client scripts, before they run.
    /// The client's Camera.ViewportSize: the 3D view's size, known before the
    /// mouse ever moves over it.
    pub(super) fn sync_view_size(&mut self) {
        let r = self.viewport_rect;
        if r.is_positive() {
            if let Some(w) = self
                .play
                .as_mut()
                .and_then(|s| s.client.as_mut())
                .and_then(|c| c.world.as_mut())
            {
                w.input.view_size = [r.width(), r.height()];
            }
        }
    }

    pub(super) fn game_client_inputs(&mut self) {
        self.sync_view_size();
        self.tell_client_equipped();
        let Some(p) = self.play.as_ref().and_then(|s| s.game.last_pointer) else {
            return;
        };
        let hit = p.hit.and_then(|(r, pos)| {
            let client = match p.side {
                Side::Client => Some(r),
                Side::Server => self.client_of(r),
            };
            Some((client?, pos))
        });
        let Some(s) = self.play.as_mut() else { return };
        let Some(w) = s.client.as_mut().and_then(|c| c.world.as_mut()) else {
            return;
        };
        w.input.mouse_ray = (p.origin, p.dir);
        w.input.mouse_xy = p.xy;
        w.input.view_size = p.view;
        w.input.mouse_target = hit.map(|h| h.0);
        w.input.mouse_hit = Some(Xform {
            pos: hit.map(|h| h.1).unwrap_or(p.origin + p.dir * 1000.0),
            rot: rustudio_engine::ltypes::look_at(p.origin, p.origin + p.dir, Vec3::Y).rot,
        });
    }

    /// Tells the client tool it was equipped once it has replicated: with
    /// or without a mouse over the view (a hotbar key with the pointer
    /// elsewhere, or a LockCenter grab not yet begun, still equips).
    fn tell_client_equipped(&mut self) {
        let pending = self.play.as_ref().and_then(|s| {
            s.game
                .equipped
                .as_ref()
                .filter(|e| !e.client_told)
                .map(|e| e.tool)
        });
        let Some(ct) = pending.and_then(|t| self.client_of(t)) else {
            return;
        };
        let Some(s) = self.play.as_mut() else { return };
        let mouse = s
            .client
            .as_ref()
            .and_then(|c| c.world.as_ref())
            .and_then(|w| w.input.mouse);
        if let Some(e) = s.game.equipped.as_mut() {
            e.client_told = true;
        }
        self.with_side(Side::Client, |rt, _, _| {
            let m = mouse.map(|m| rt.inst(m)).unwrap_or(Value::Nil);
            rt.fire(ct, "Equipped", vec![m]);
        });
    }

    /// Acts on what scripts on one side asked for this frame.
    pub(super) fn game_requests(&mut self, side: Side) {
        let reqs = {
            let Some(s) = self.play.as_mut() else { return };
            let w = match side {
                Side::Server => s.server.as_mut().and_then(|server| server.world.as_mut()),
                Side::Client => s.client.as_mut().and_then(|c| c.world.as_mut()),
            };
            match w {
                Some(w) => std::mem::take(&mut w.requests),
                None => return,
            }
        };
        let mine_server = self
            .play
            .as_ref()
            .and_then(|s| s.character.as_ref().map(|c| c.humanoid));
        let mine = |h: Ref, ed: &Game| -> bool {
            match side {
                Side::Server => Some(h) == mine_server,
                Side::Client => mine_server.and_then(|m| ed.client_of(m)) == Some(h),
            }
        };
        for (r, cmd) in reqs.sounds {
            let (server, client) = match side {
                Side::Server => (Some(r), self.client_of(r)),
                Side::Client => (self.server_of(r), Some(r)),
            };
            if cmd == SoundCmd::PlayLocal {
                // The client's own instance only (its copy of a server Sound
                // plays the same asset there).
                self.sound_command(None, Some(r), cmd);
                continue;
            }
            self.sound_command(server, client, cmd);
        }
        if let Some(s) = self.play.as_mut() {
            s.game.removed_sounds.extend(reqs.removed_sounds);
        }
        for (r, t) in reqs.sound_seeks {
            let (server, client) = match side {
                Side::Server => (Some(r), self.client_of(r)),
                Side::Client => (self.server_of(r), Some(r)),
            };
            self.seek_sound(server, client, t);
        }
        for (h, target) in reqs.move_to {
            if mine(h, self) {
                if let Some(s) = self.play.as_mut() {
                    s.game.move_target = Some(target);
                }
            } else if side == Side::Server {
                let now = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);
                if let Some(s) = self.play.as_mut() {
                    s.game.walks.insert(h, Walk { target, since: now });
                }
            }
        }
        for h in reqs.jumps {
            if mine(h, self) {
                if let Some(s) = self.play.as_mut() {
                    s.game.jump = true;
                }
            } else if side == Side::Server && self.play.as_ref().is_some_and(|s| s.npcs.has_humanoid(h)) {
                // A Humanoid the controller drives (play_npc.rs) jumps by its
                // state machine.
                if let Some(s) = self.play.as_mut() {
                    s.npcs.jump(h);
                }
            } else if side == Side::Server {
                if let Some(root) = self.npc_root(h) {
                    let power = f32_prop(&self.doc, h, "JumpPower", mech::JUMP_POWER);
                    if let Some(s) = self.play.as_mut() {
                        let v = s
                            .physics
                            .velocities()
                            .get(&root)
                            .copied()
                            .unwrap_or(Vec3::ZERO);
                        s.physics.set_velocity(root, Vec3::new(v.x, power, v.z));
                    }
                }
            }
        }
        for (h, tool) in reqs.equip {
            if !mine(h, self) {
                continue;
            }
            if side == Side::Client {
                match tool {
                    Some(t) if self.server_of(t).is_none() => {
                        self.equip_client_tool(t);
                        continue;
                    }
                    None => self.unequip_client_tool(true),
                    _ => self.unequip_client_tool(true),
                }
            }
            let tool = match (side, tool) {
                (Side::Client, Some(t)) => self.server_of(t),
                (_, t) => t,
            };
            match tool {
                Some(t) => {
                    self.equip_tool(t);
                    // A client script's EquipTool: its tool hears Equipped
                    // now (Studio fires it right after the call), pointer or
                    // not.
                    if side == Side::Client {
                        let ct = self.client_of(t);
                        let told = self.play.as_mut().and_then(|s| s.game.equipped.as_mut()).filter(|e| e.tool == t);
                        if let (Some(ct), Some(e)) = (ct, told) {
                            if !e.client_told {
                                e.client_told = true;
                                self.with_side(Side::Client, |rt, _, _| rt.fire(ct, "Equipped", vec![Value::Nil]));
                            }
                        }
                    }
                }
                None => self.unequip_tool(),
            }
        }
        for e in reqs.explosions {
            if side == Side::Server {
                self.explode(e);
            }
        }
        for (h, state) in reqs.change_state {
            if mine(h, self) {
                if let Some(s) = self.play.as_mut() {
                    s.game.forced_state = Some(state);
                }
            }
        }
        for (seat, h) in reqs.sits {
            if mine(h, self) {
                let seat = match side {
                    Side::Client => self.server_of(seat),
                    Side::Server => Some(seat),
                };
                if let Some(s) = self.play.as_mut() {
                    s.game.sit_on = seat;
                }
            }
        }
        for (h, state, on) in reqs.state_enabled {
            if mine(h, self) {
                if let Some(s) = self.play.as_mut() {
                    if on {
                        s.game.states_off.remove(&state);
                    } else {
                        s.game.states_off.insert(state);
                    }
                }
            }
        }
    }

    /// The part an NPC Humanoid moves with.
    fn npc_root(&self, humanoid: Ref) -> Option<Ref> {
        let d = &self.doc;
        let model = d.dom.get_by_ref(humanoid)?.parent();
        if let Some(Variant::Ref(r)) = d
            .prop(humanoid, "RootPart")
            .or_else(|| d.prop(model, "PrimaryPart"))
        {
            if r.is_some() && d.exists(r) {
                return Some(r);
            }
        }
        d.find_named(model, "HumanoidRootPart")
            .or_else(|| d.find_named(model, "Torso"))
    }

    // ------------------------------------------------------------- tools

    fn backpack(&self) -> Option<Ref> {
        let p = self.play.as_ref()?.player;
        self.doc.find_child(p, "Backpack")
    }

    fn character_model(&self) -> Option<Ref> {
        self.play
            .as_ref()?
            .character
            .as_ref()
            .filter(|c| !c.dead)
            .map(|c| c.model)
    }

    fn equip_tool(&mut self, tool: Ref) {
        let Some(model) = self.character_model() else {
            return;
        };
        if !self
            .doc
            .class_of(tool)
            .is_some_and(|c| doc::is_a(&c, "BackpackItem"))
        {
            return;
        }
        self.unequip_tool();
        self.with_side(Side::Server, |rt, _, _| rt.apply_parent(tool, model));
        let handle = tool_handle(&self.doc, tool);
        let parts: Vec<Ref> = self
            .doc
            .dom
            .descendants_of(tool)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .map(|i| i.referent())
            .collect();
        let base = handle.and_then(|h| rustudio_render::scene::xform_of(&self.doc, h));
        let offsets: Vec<(Ref, Xform)> = match base {
            Some(b) => parts
                .iter()
                .filter_map(|p| {
                    rustudio_render::scene::xform_of(&self.doc, *p)
                        .map(|x| (*p, rustudio_engine::ltypes::cf_mul(&rustudio_engine::ltypes::cf_inv(&b), &x)))
                })
                .collect(),
            None => Vec::new(),
        };
        if let Some(s) = self.play.as_mut() {
            s.physics.skip.extend(parts.iter().copied());
            s.physics.refresh(parts);
            s.game.equipped = Some(Equipped {
                tool,
                offsets,
                client_told: false,
            });
        }
        self.with_side(Side::Server, |rt, _, _| {
            rt.fire(tool, "Equipped", vec![Value::Nil])
        });
    }

    /// Equips a Tool only the client has (client ref): into the client's character, its
    /// parts out of the client's world while they follow the hand (`client_tool_pose`).
    fn equip_client_tool(&mut self, tool: Ref) {
        let Some(model) = self.character_model().and_then(|m| self.client_of(m)) else { return };
        self.unequip_tool();
        self.unequip_client_tool(true);
        let Some(s) = self.play.as_mut() else { return };
        let Some(cd) = s.other_doc.as_ref() else { return };
        if !cd.class_of(tool).is_some_and(|c| doc::is_a(&c, "BackpackItem")) {
            return;
        }
        let parts: Vec<Ref> = cd.dom.descendants_of(tool).filter(|i| doc::is_a(&i.class, "BasePart")).map(|i| i.referent()).collect();
        let handle = tool_handle(cd, tool);
        let base = handle.and_then(|h| rustudio_render::scene::xform_of(cd, h));
        let offsets: Vec<(Ref, Xform)> = match base {
            Some(b) => parts
                .iter()
                .filter_map(|p| rustudio_render::scene::xform_of(cd, *p).map(|x| (*p, rustudio_engine::ltypes::cf_mul(&rustudio_engine::ltypes::cf_inv(&b), &x))))
                .collect(),
            None => Vec::new(),
        };
        let in_hand = cd.dom.get_by_ref(tool).map(|i| i.parent()) == Some(model);
        s.client_sim.hold(&parts, true);
        s.game.client_tool = Some(Equipped { tool, offsets, client_told: true });
        let mouse = s.client.as_ref().and_then(|c| c.world.as_ref()).and_then(|w| w.input.mouse);
        // In the hand before Equipped fires: Roblox has the grip made by then, so a script
        // that reads Handle.CFrame there (a hanging lantern's, building on it) gets the hand.
        self.place_client_tool_now();
        self.with_side(Side::Client, |rt, _, _| {
            if !in_hand {
                rt.apply_parent(tool, model);
            }
            let m = mouse.map(|m| rt.inst(m)).unwrap_or(Value::Nil);
            rt.fire(tool, "Equipped", vec![m]);
        });
    }

    /// Lets go of the client-only Tool: back into the client's Backpack (`to_backpack`,
    /// an UnequipTools) or left where a script put it.
    fn unequip_client_tool(&mut self, to_backpack: bool) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(e) = s.game.client_tool.take() else { return };
        let parts: Vec<Ref> = e.offsets.iter().map(|(r, _)| *r).collect();
        s.client_sim.hold(&parts, false);
        s.client_sim.set_keep_awake(Vec::new());
        let player = s.player;
        let bp = self.client_of(player).and_then(|cp| self.play.as_ref()?.other_doc.as_ref()?.find_child(cp, "Backpack"));
        let tool = e.tool;
        self.with_side(Side::Client, |rt, _, _| {
            if let (true, Some(bp)) = (to_backpack, bp) {
                rt.apply_parent(tool, bp);
            }
            rt.fire(tool, "Unequipped", vec![]);
        });
    }

    /// Writes the client-only Tool's parts at the hand now (from the server's pose of
    /// the hand), between frames' body poses.
    fn place_client_tool_now(&mut self) {
        let Some(ch) = self.play.as_ref().and_then(|s| s.character.as_ref()) else { return };
        let model = ch.model;
        let d = &self.doc;
        let Some(hand) = d.find_named(model, "RightHand").or_else(|| d.find_named(model, "Right Arm")) else { return };
        let Some(hx) = rustudio_render::scene::xform_of(d, hand) else { return };
        let posed = self.client_tool_pose(&[(hand, hx)]);
        if let Some(cd) = self.play.as_mut().and_then(|s| s.other_doc.as_mut()) {
            rustudio_engine::rig::write_pose(cd, &posed, None);
        }
    }

    /// Parts welded to the held client-only Tool's handle since it was equipped follow
    /// the hand with it (one assembly with the character in Roblox): a hanging
    /// lantern welds its Top to the Handle on Equipped and hangs the rest from Top on
    /// ropes. Parts no longer in the tool are let go.
    fn follow_client_tool_welds(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let (Some(e), Some(cd)) = (s.game.client_tool.as_mut(), s.other_doc.as_ref()) else { return };
        let Some(handle) = tool_handle(cd, e.tool) else { return };
        let Some(hx) = rustudio_render::scene::xform_of(cd, handle) else { return };
        let mut edges: Vec<(Ref, Ref)> = Vec::new();
        let mut in_tool: HashSet<Ref> = HashSet::new();
        for i in cd.dom.descendants_of(e.tool) {
            let r = i.referent();
            if doc::is_a(&i.class, "BasePart") {
                in_tool.insert(r);
            } else if rustudio_core::body::is_rigid_joint(&i.class) && rustudio_core::body::joint_enabled(cd, r) {
                if let Some(ends) = rustudio_core::body::joint_parts(cd, r) {
                    edges.push(ends);
                }
            }
        }
        let mut reach: HashSet<Ref> = [handle].into_iter().collect();
        let mut grew = true;
        while grew {
            grew = false;
            for (a, b) in &edges {
                for (from, to) in [(a, b), (b, a)] {
                    if reach.contains(from) && in_tool.contains(to) && reach.insert(*to) {
                        grew = true;
                    }
                }
            }
        }
        let known: HashSet<Ref> = e.offsets.iter().map(|(r, _)| *r).collect();
        let inv = rustudio_engine::ltypes::cf_inv(&hx);
        let mut added = Vec::new();
        for r in reach.iter().filter(|r| !known.contains(r)) {
            if let Some(x) = rustudio_render::scene::xform_of(cd, *r) {
                e.offsets.push((*r, rustudio_engine::ltypes::cf_mul(&inv, &x)));
                added.push(*r);
            }
        }
        let gone: Vec<Ref> = e.offsets.iter().map(|(r, _)| *r).filter(|r| !in_tool.contains(r)).collect();
        e.offsets.retain(|(r, _)| in_tool.contains(r));
        let loose: Vec<Ref> = in_tool.iter().copied().filter(|r| !reach.contains(r)).collect();
        s.client_sim.set_keep_awake(loose);
        if !added.is_empty() {
            s.client_sim.hold(&added, true);
        }
        if !gone.is_empty() {
            s.client_sim.hold(&gone, false);
        }
    }

    /// The character died or respawned: the client-only Tool it held is let go of.
    pub(super) fn unequip_client_tool_dropped(&mut self) {
        self.unequip_client_tool(false);
    }

    /// Once a frame: a client-only Tool a script moved out of the character (or
    /// destroyed) isn't held any more.
    pub(super) fn check_client_tool(&mut self) {
        let Some(s) = self.play.as_ref() else { return };
        let Some(e) = s.game.client_tool.as_ref() else { return };
        let model = s.character.as_ref().filter(|c| !c.dead).and_then(|c| s.map.s2c.get(&c.model).copied());
        let held = s.other_doc.as_ref().is_some_and(|cd| model.is_some_and(|m| cd.dom.get_by_ref(e.tool).map(|i| i.parent()) == Some(m)));
        if !held {
            self.unequip_client_tool(false);
        } else {
            self.follow_client_tool_welds();
        }
    }

    /// The client-only Tool's parts (client refs) for this frame's body pose.
    pub(super) fn client_tool_pose(&self, pose: &[(Ref, Xform)]) -> Vec<(Ref, Xform)> {
        let Some(s) = self.play.as_ref() else { return Vec::new() };
        let (Some(e), Some(cd)) = (s.game.client_tool.as_ref(), s.other_doc.as_ref()) else { return Vec::new() };
        let grip = match cd.prop(e.tool, "Grip") {
            Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
            _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
        };
        let Some(hand) = self.grip_frame(pose) else { return Vec::new() };
        use rustudio_engine::ltypes::{cf_inv, cf_mul};
        let handle_xf = cf_mul(&hand, &cf_inv(&grip));
        e.offsets.iter().map(|(r, off)| (*r, cf_mul(&handle_xf, off))).collect()
    }

    /// The right hand's grip frame this frame (hand · RightGripAttachment, turned to point
    /// forward when it is the identity), from the body pose.
    fn grip_frame(&self, pose: &[(Ref, Xform)]) -> Option<Xform> {
        let s = self.play.as_ref()?;
        let ch = s.character.as_ref()?;
        let d = &self.doc;
        let hand = d.find_named(ch.model, "RightHand").or_else(|| d.find_named(ch.model, "Right Arm"))?;
        let hand_xf = pose.iter().find(|(r, _)| *r == hand).map(|(_, x)| *x)?;
        let att = d.find_named(hand, "RightGripAttachment").and_then(|a| match d.prop(a, "CFrame") {
            Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(&cf)),
            _ => None,
        });
        let mut c0 = att.unwrap_or(Xform { pos: Vec3::new(0.0, -1.0, 0.0), rot: Mat3::IDENTITY });
        if c0.rot.abs_diff_eq(Mat3::IDENTITY, 1e-4) {
            c0.rot = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        }
        Some(rustudio_engine::ltypes::cf_mul(&hand_xf, &c0))
    }

    /// Forgets the equipped tool without putting it back in the Backpack
    /// (its character is going, and the tool with it).
    pub(super) fn drop_equipped(&mut self) {
        let Some(e) = self.play.as_mut().and_then(|s| s.game.equipped.take()) else {
            return;
        };
        let parts: Vec<Ref> = e.offsets.iter().map(|(r, _)| *r).collect();
        if let Some(s) = self.play.as_mut() {
            s.physics.skip.retain(|r| !parts.contains(r));
        }
    }

    fn unequip_tool(&mut self) {
        let Some(e) = self.play.as_mut().and_then(|s| s.game.equipped.take()) else {
            return;
        };
        let parts: Vec<Ref> = e.offsets.iter().map(|(r, _)| *r).collect();
        if let Some(s) = self.play.as_mut() {
            s.physics.skip.retain(|r| !parts.contains(r));
            s.physics.refresh(parts);
        }
        if let Some(bp) = self.backpack() {
            if self.doc.exists(e.tool) {
                self.with_side(Side::Server, |rt, _, _| rt.apply_parent(e.tool, bp));
            }
        }
        self.fire_both(e.tool, "Unequipped");
    }

    /// Where the equipped tool's parts go, given this frame's body pose.
    pub(super) fn tool_pose(&self, pose: &[(Ref, Xform)]) -> Vec<(Ref, Xform)> {
        let Some(s) = self.play.as_ref() else {
            return Vec::new();
        };
        let (Some(e), Some(ch)) = (s.game.equipped.as_ref(), s.character.as_ref()) else {
            return Vec::new();
        };
        let d = &self.doc;
        let hand = d
            .find_named(ch.model, "RightHand")
            .or_else(|| d.find_named(ch.model, "Right Arm"));
        let Some(hand) = hand else { return Vec::new() };
        let Some(hand_xf) = pose.iter().find(|(r, _)| *r == hand).map(|(_, x)| *x) else {
            return Vec::new();
        };
        // RightGrip: hand * grip attachment (turned to point forward) * Tool.Grip⁻¹.
        let att =
            d.find_named(hand, "RightGripAttachment")
                .and_then(|a| match d.prop(a, "CFrame") {
                    Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(&cf)),
                    _ => None,
                });
        let mut c0 = att.unwrap_or(Xform {
            pos: Vec3::new(0.0, -1.0, 0.0),
            rot: Mat3::IDENTITY,
        });
        if c0.rot.abs_diff_eq(Mat3::IDENTITY, 1e-4) {
            c0.rot = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        }
        let grip = match d.prop(e.tool, "Grip") {
            Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
            _ => Xform {
                pos: Vec3::ZERO,
                rot: Mat3::IDENTITY,
            },
        };
        use rustudio_engine::ltypes::{cf_inv, cf_mul};
        let handle_xf = cf_mul(&cf_mul(&hand_xf, &c0), &cf_inv(&grip));
        e.offsets
            .iter()
            .map(|(r, off)| (*r, cf_mul(&handle_xf, off)))
            .collect()
    }

    /// Keeps the hotbar list in step with the Backpack and the equipped tool.
    fn update_slots(&mut self) {
        let mut present: Vec<Ref> = Vec::new();
        if let Some(bp) = self.backpack() {
            if let Some(i) = self.doc.dom.get_by_ref(bp) {
                present.extend(i.children().iter().copied().filter(|c| {
                    self.doc
                        .class_of(*c)
                        .is_some_and(|k| doc::is_a(&k, "BackpackItem"))
                }));
            }
        }
        if let Some(e) = self.play.as_ref().and_then(|s| s.game.equipped.as_ref()) {
            if self.doc.exists(e.tool) {
                present.push(e.tool);
            }
        }
        let Some(s) = self.play.as_mut() else { return };
        s.game.slots.retain(|t| present.contains(t));
        for t in present {
            if !s.game.slots.contains(&t) {
                s.game.slots.push(t);
            }
        }
    }

    // --------------------------------------------------------- explosions

    fn explode(&mut self, e: Ref) {
        let d = &self.doc;
        let pos = vec3_of(d.prop(e, "Position")).unwrap_or(Vec3::ZERO);
        let radius = f32_prop(d, e, "BlastRadius", 4.0).max(0.0);
        let pressure = f32_prop(d, e, "BlastPressure", 500_000.0).max(0.0);
        let joint_radius =
            radius * f32_prop(d, e, "DestroyJointRadiusPercent", 1.0).clamp(0.0, 1.0);
        let Some(ws) = d.workspace() else { return };
        let mut hits: Vec<(Ref, f32, Vec3)> = Vec::new();
        for i in d.dom.descendants_of(ws) {
            if i.class == "Terrain" || !doc::is_a(&i.class, "BasePart") {
                continue;
            }
            let r = i.referent();
            let Some(xf) = rustudio_render::scene::xform_of(d, r) else {
                continue;
            };
            let size = vec3_of(d.prop(r, "Size")).unwrap_or(Vec3::ONE);
            let dist = ((xf.pos - pos).length() - size.length() * 0.5).max(0.0);
            if dist <= radius {
                hits.push((r, dist, xf.pos));
            }
        }
        let now = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);
        if let Some(s) = self.play.as_mut() {
            s.game.flashes.push(Flash {
                pos,
                radius,
                born: now,
            });
        }
        // Hit events, a push outward, broken joints and dead humanoids.
        let mut humanoids: HashSet<Ref> = HashSet::new();
        let mut break_parts: HashSet<Ref> = HashSet::new();
        for (r, dist, at) in &hits {
            let (r, dist, at) = (*r, *dist, *at);
            self.with_side(Side::Server, |rt, _, _| {
                rt.fire(e, "Hit", vec![rt.inst(r), Value::Number(dist as f64)])
            });
            let anchored = matches!(self.doc.prop(r, "Anchored"), Some(Variant::Bool(true)));
            if !anchored {
                let size = vec3_of(self.doc.prop(r, "Size")).unwrap_or(Vec3::ONE);
                let mass = (size.x * size.y * size.z * mech::PART_DENSITY).max(0.1);
                let dir = (at - pos).normalize_or(Vec3::Y);
                let dv = (pressure * mech::EXPLOSION_PUSH / mass).min(mech::EXPLOSION_MAX_SPEED);
                if let Some(s) = self.play.as_mut() {
                    let v = s
                        .physics
                        .velocities()
                        .get(&r)
                        .copied()
                        .unwrap_or(Vec3::ZERO);
                    s.physics.set_velocity(r, v + dir * dv);
                }
            }
            if dist <= joint_radius {
                break_parts.insert(r);
                if let Some(model) = self.doc.dom.get_by_ref(r).map(|i| i.parent()) {
                    if let Some(h) = self.doc.find_child(model, "Humanoid") {
                        humanoids.insert(h);
                    }
                }
            }
        }
        let joints: Vec<Ref> = self
            .doc
            .dom
            .descendants_of(ws)
            .filter(|i| matches!(i.class.as_str(), "Weld" | "ManualWeld" | "Snap" | "Glue" | "WeldConstraint" | "Motor6D" | "Motor"))
            .filter(|i| ["Part0", "Part1"].iter().any(|p| matches!(i.properties.get(&ustr(p)), Some(Variant::Ref(x)) if break_parts.contains(x))))
            .map(|i| i.referent())
            .collect();
        let mine = self
            .play
            .as_ref()
            .and_then(|s| s.character.as_ref().map(|c| c.humanoid));
        self.with_side(Side::Server, |rt, _, _| {
            for j in &joints {
                rt.apply_parent(*j, Ref::none());
            }
            for h in &humanoids {
                rt.set(*h, "Health", Value::Number(0.0));
            }
        });
        if mine.is_some_and(|m| humanoids.contains(&m)) {
            self.kill_character();
        }
    }

    // ------------------------------------------------------------- sounds

    fn sound_bytes(&mut self, id: &str) -> rustudio_core::assets::Fetch {
        use rustudio_core::assets::Fetch;
        if !rustudio_core::assets::place_content_ok(id) {
            return Fetch::Unavailable(format!("{id}: local files aren't place content while scripts run"));
        }
        let dir = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        if let Some(path) = rustudio_core::assets::resolve(id, dir.as_deref()) {
            return match std::fs::read(&path) {
                Ok(b) => Fetch::Ready(Arc::new(b)),
                Err(e) => Fetch::Unavailable(format!("{}: {e}", path.display())),
            };
        }
        match rustudio_core::assets::asset_ref(id) {
            Some(a) => self.assets.fetch(a),
            None => Fetch::Unavailable("no SoundId".into()),
        }
    }

    /// The DataModel that has `r` (server in front during play_tick).
    fn sound_doc(&self, server: Option<Ref>, client: Option<Ref>) -> Option<(&Document, Ref)> {
        if let (Some(c), Some(cd)) = (
            client,
            self.play.as_ref().and_then(|s| s.other_doc.as_ref()),
        ) {
            if cd.exists(c) {
                return Some((cd, c));
            }
        }
        server
            .filter(|r| self.doc.exists(*r))
            .map(|r| (&self.doc, r))
    }

    fn sound_command(&mut self, server: Option<Ref>, client: Option<Ref>, cmd: SoundCmd) {
        // Keyed by the server Sound when there is one: a Sound made and
        // played on the server in one frame has no client copy yet, and a
        // later Stop (after replication) must reach the same track.
        let Some(key_ref) = server.or(client) else {
            return;
        };
        let mut key = sound_key(key_ref);
        if cmd == SoundCmd::PlayLocal {
            // Each PlayLocalSound is its own detached copy.
            let Some(s) = self.play.as_mut() else { return };
            s.game.local_plays += 1;
            key ^= s.game.local_plays.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            s.game.tracks.insert(
                key,
                Track { server, client, pending: None, id: None, detached: true },
            );
            self.start_sound(key);
            return;
        }
        if let Some(s) = self.play.as_mut() {
            let t = s.game.tracks.entry(key).or_insert(Track {
                server,
                client,
                pending: None,
                id: None,
                detached: false,
            });
            t.server = t.server.or(server);
            t.client = t.client.or(client);
        }
        // The other side's view of the same Sound (the side that asked has
        // set its own in the runtime).
        let stopped = cmd == SoundCmd::Stop;
        self.set_sound_info(server, client, |inp, r| match cmd {
            SoundCmd::Play | SoundCmd::Resume => {
                inp.sound_playing.insert(r);
                inp.sound_paused.remove(&r);
            }
            _ => {
                inp.sound_playing.remove(&r);
                if stopped {
                    inp.sound_paused.remove(&r);
                    inp.sound_pos.insert(r, 0.0);
                } else {
                    inp.sound_paused.insert(r);
                }
            }
        });
        match cmd {
            SoundCmd::PlayLocal => {}
            SoundCmd::Play => self.start_sound(key),
            SoundCmd::Stop | SoundCmd::Pause | SoundCmd::Resume => {
                if let Some(a) = self.play.as_mut().and_then(|s| s.game.audio.as_mut()) {
                    match cmd {
                        SoundCmd::Stop => a.stop(key),
                        SoundCmd::Pause => a.pause(key),
                        _ => a.resume(key),
                    }
                }
                if cmd == SoundCmd::Stop {
                    if let Some(t) = self.play.as_mut().and_then(|s| s.game.tracks.get_mut(&key)) {
                        t.pending = None;
                    }
                }
            }
        }
    }

    fn audio_listener(&self) -> Option<(u32, Option<rustudio_core::math::Xform>, Option<Ref>)> {
        let s = self.play.as_ref()?;
        s.client
            .as_ref()
            .and_then(|c| c.world.as_ref())
            .or(s.server.as_ref().and_then(|server| server.world.as_ref()))
            .and_then(|w| w.input.sound_listener)
    }
    fn start_sound(&mut self, key: u64) {
        let Some((server, client)) = self
            .play
            .as_ref()
            .and_then(|s| s.game.tracks.get(&key))
            .map(|t| (t.server, t.client))
        else {
            return;
        };
        let Some((id, volume, speed, looped, start, region)) =
            self.sound_doc(server, client).map(|(d, r)| {
                let range = |k: &str| match d.prop(r, k) {
                    Some(Variant::NumberRange(n)) => Some((n.min as f64, n.max as f64)),
                    _ => None,
                };
                // Sound.PlaybackRegionsEnabled: play PlaybackRegion, loop LoopRegion.
                let region = if matches!(
                    d.prop(r, "PlaybackRegionsEnabled"),
                    Some(Variant::Bool(true))
                ) {
                    rustudio_engine::audio::Region {
                        play: range("PlaybackRegion"),
                        looped: range("LoopRegion"),
                    }
                } else {
                    rustudio_engine::audio::Region::default()
                };
                let id = match d.prop(r, "SoundId") {
                    Some(Variant::ContentId(c)) => c.as_str().to_string(),
                    Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
                    Some(Variant::String(s)) => s,
                    _ => String::new(),
                };
                (
                    id,
                    f32_prop(d, r, "Volume", 0.5),
                    f32_prop(d, r, "PlaybackSpeed", 1.0),
                    matches!(d.prop(r, "Looped"), Some(Variant::Bool(true))),
                    0.0f64,
                    region,
                )
            })
        else {
            return;
        };
        // Where Play() starts: the TimePosition a script set on either side
        // (both are put back to 0 once used).
        let doc_time = |d: &Document, r: Option<Ref>| {
            r.filter(|r| d.exists(*r))
                .map_or(0.0, |r| (f32_prop(d, r, "TimePosition", 0.0) as f64).max(0.0))
        };
        let start = start
            .max(doc_time(&self.doc, server))
            .max(self.play.as_ref().and_then(|s| s.other_doc.as_ref()).map_or(0.0, |d| doc_time(d, client)));
        if id.trim().is_empty() {
            return;
        }
        if self
            .play
            .as_ref()
            .is_some_and(|s| s.game.sound_unavailable.contains(&id))
        {
            return;
        }
        if let Some(s) = self.play.as_mut() {
            *s.game.sound_fetches.entry(id.clone()).or_insert(0) += 1;
        }
        match self.sound_bytes(&id) {
            rustudio_core::assets::Fetch::Ready(bytes) => {
                let graph = self.sound_doc(server, client).map(|(d, r)| {
                    rustudio_engine::audio_scene::settings(
                        d,
                        r,
                        self.cam.pos,
                        self.cam.right(),
                        self.audio_listener(),
                    )
                });
                let len = Audio::length(&bytes);
                let Some(s) = self.play.as_mut() else { return };
                let audio = s.game.audio.get_or_insert_with(Audio::new);
                if let Some((groups, settings)) = graph {
                    audio.configure(key, groups, settings);
                }
                if let Err(e) = audio.play(key, bytes, volume, speed, looped, start, region) {
                    if s.game.warned.insert(id.clone()) {
                        // Studio's wording for a sound that fails to load.
                        self.warn(format!("Failed to load sound {id}: {e}"));
                    }
                    return;
                }
                let detached = match self.play.as_mut().and_then(|s| s.game.tracks.get_mut(&key)) {
                    Some(t) => {
                        t.pending = None;
                        t.id = Some(id.clone());
                        t.detached
                    }
                    None => false,
                };
                self.log_audio_start(key, &id);
                if let Some(len) = len {
                    self.set_sound_info(server, client, |inp, r| {
                        inp.sound_len.insert(r, len);
                    });
                }
                if !detached {
                    // The TimePosition a script set is used up: the next
                    // Play() starts at 0 unless one is set again (Studio).
                    self.set_sound_info(server, client, |inp, r| {
                        inp.sound_pos.insert(r, start);
                    });
                    self.reset_sound_doc_time(server, client);
                }
            }
            rustudio_core::assets::Fetch::Pending => {
                if let Some(t) = self.play.as_mut().and_then(|s| s.game.tracks.get_mut(&key)) {
                    t.pending = Some(id);
                }
            }
            rustudio_core::assets::Fetch::Unavailable(why) => {
                // Given up on: no per-frame retry, one warning, the game goes on.
                if let Some(s) = self.play.as_mut() {
                    if let Some(t) = s.game.tracks.get_mut(&key) {
                        t.pending = None;
                    }
                    s.game.sound_unavailable.insert(id.clone());
                }
                let first = self
                    .play
                    .as_mut()
                    .is_some_and(|s| s.game.warned.insert(id.clone()));
                if first {
                    self.warn(format!("Sound {id} isn't available: {why}"));
                }
            }
        }
    }

    fn play_removed_sounds(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let removed = std::mem::take(&mut s.game.removed_sounds);
        for sound in removed {
            use rustudio_core::assets::Fetch;
            match self.sound_bytes(&sound.id) {
                Fetch::Ready(bytes) => {
                    let Some(s) = self.play.as_mut() else { return };
                    s.game.local_plays += 1;
                    let key = sound_key(sound.source)
                        ^ (s.game.local_plays.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
                    let audio = s.game.audio.get_or_insert_with(Audio::new);
                    audio.configure(key, sound.groups, sound.settings);
                    if audio.play(key, bytes, sound.volume, sound.speed, false, sound.position, sound.region).is_ok() {
                        s.game.tracks.insert(key, Track {
                            server: None, client: None, pending: None, id: None, detached: true,
                        });
                        self.log_audio_start(key, &sound.id);
                    } else {
                        audio.stop(key);
                    }
                }
                Fetch::Pending => {
                    if let Some(s) = self.play.as_mut() {
                        s.game.removed_sounds.push(sound);
                    }
                }
                Fetch::Unavailable(_) => {}
            }
        }
    }

    /// RUSTUDIO_AUDIO_LOG=FILE: one line per Audio::play start (key, sound id,
    /// frame, how many times this key has started), to tell a caller that
    /// re-triggers from a mixer that repeats.
    fn log_audio_start(&mut self, key: u64, id: &str) {
        let frame = self.frame_no;
        let Some(s) = self.play.as_mut() else { return };
        *s.game.audio_starts_by_id.entry(id.to_string()).or_insert(0) += 1;
        let n = s.game.audio_starts.entry(key).or_insert(0);
        *n += 1;
        let n = *n;
        let live = s.game.audio.as_ref().map_or(0, |a| a.playing_count());
        if let Some(path) = std::env::var_os("RUSTUDIO_AUDIO_LOG") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "start key={key:016x} id={id} frame={frame} t={:.3} count={n} live={live}", s.time);
            }
        }
    }

    /// Updates the scripts' view of a sound on both sides.
    fn set_sound_info(
        &mut self,
        server: Option<Ref>,
        client: Option<Ref>,
        f: impl Fn(&mut rustudio_engine::runtime::ext::Inputs, Ref),
    ) {
        let Some(s) = self.play.as_mut() else { return };
        if let (Some(r), Some(w)) = (server, s.server.as_mut().and_then(|server| server.world.as_mut())) {
            f(&mut w.input, r);
        }
        if let (Some(r), Some(w)) = (client, s.client.as_mut().and_then(|c| c.world.as_mut())) {
            f(&mut w.input, r);
        }
    }

    /// Changes a Sound's instance on both sides (no Changed signal).
    fn set_sound_doc(
        &mut self,
        server: Option<Ref>,
        client: Option<Ref>,
        f: impl Fn(&mut rbx_dom_weak::Instance),
    ) {
        if let Some(i) = server.and_then(|r| self.doc.dom.get_by_ref_mut(r)) {
            f(i);
        }
        if let (Some(c), Some(d)) = (client, self.play.as_mut().and_then(|s| s.other_doc.as_mut())) {
            if let Some(i) = d.dom.get_by_ref_mut(c) {
                f(i);
            }
        }
    }

    /// The stored TimePosition (where the next Play() starts) back to 0.
    fn reset_sound_doc_time(&mut self, server: Option<Ref>, client: Option<Ref>) {
        self.set_sound_doc(server, client, |i| {
            i.properties.insert(ustr("TimePosition"), Variant::Float64(0.0));
        });
    }

    /// A script set TimePosition while its Sound plays (or is paused): the
    /// audio goes on from there.
    fn seek_sound(&mut self, server: Option<Ref>, client: Option<Ref>, t: f64) {
        let Some(key_ref) = server.or(client) else { return };
        let key = sound_key(key_ref);
        let sought = self
            .play
            .as_mut()
            .and_then(|s| s.game.audio.as_mut())
            .is_some_and(|a| a.seek(key, t));
        self.set_sound_info(server, client, |inp, r| {
            inp.sound_pos.insert(r, t);
        });
        // (still downloading: the stored TimePosition is where it starts)
        if sought {
            self.reset_sound_doc_time(server, client);
        }
    }

    fn fire_sound_event(
        &mut self,
        server: Option<Ref>,
        client: Option<Ref>,
        event: &str,
        id: &str,
        count: Option<u64>,
    ) {
        for (side, sound) in [(Side::Server, server), (Side::Client, client)] {
            if let Some(sound) = sound {
                self.with_side(side, |rt, _, _| {
                    if rt.world_mut().is_some_and(|w| w.d().exists(sound)) {
                        if let Ok(id) = rt.lua.create_string(id) {
                            let mut args = vec![Value::String(id)];
                            if let Some(count) = count {
                                args.push(Value::Number(count as f64));
                            }
                            rt.fire(sound, event, args);
                        }
                    }
                });
            }
        }
    }

    /// Per frame: NPC walking, sounds (positions, 3D volume, Ended), the hotbar.
    pub(super) fn game_tick_end(&mut self) {
        let now = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);

        // NPC Humanoid:MoveTo.
        let walks: Vec<(Ref, Vec3, f64)> = self
            .play
            .as_ref()
            .map(|s| {
                s.game
                    .walks
                    .iter()
                    .map(|(h, w)| (*h, w.target, w.since))
                    .collect()
            })
            .unwrap_or_default();
        let mut finished: Vec<(Ref, bool)> = Vec::new();
        for (h, target, since) in walks {
            let Some(root) = self.npc_root(h) else {
                finished.push((h, false));
                continue;
            };
            let Some(pos) = rustudio_render::scene::xform_of(&self.doc, root).map(|x| x.pos) else {
                continue;
            };
            let mut to = target - pos;
            to.y = 0.0;
            let speed = f32_prop(&self.doc, h, "WalkSpeed", mech::WALK_SPEED);
            let reached = to.length() < mech::MOVE_TO_REACHED;
            let timed_out = now - since > mech::MOVE_TO_TIMEOUT_S as f64;
            let Some(s) = self.play.as_mut() else { break };
            // A Humanoid the controller drives walks there itself
            // (play_npc.rs reads the target); only the end is told here.
            if s.npcs.has_humanoid(h) {
                if reached || timed_out {
                    finished.push((h, reached));
                }
                continue;
            }
            let v = s
                .physics
                .velocities()
                .get(&root)
                .copied()
                .unwrap_or(Vec3::ZERO);
            if reached || timed_out {
                s.physics.set_velocity(root, Vec3::new(0.0, v.y, 0.0));
                finished.push((h, reached));
            } else {
                let dir = to.normalize();
                s.physics
                    .set_velocity(root, Vec3::new(dir.x * speed, v.y, dir.z * speed));
            }
        }
        for (h, reached) in finished {
            if let Some(s) = self.play.as_mut() {
                s.game.walks.remove(&h);
            }
            self.with_side(Side::Server, |rt, _, _| {
                rt.fire(h, "MoveToFinished", vec![Value::Boolean(reached)])
            });
        }

        // A Sound taken out of the game (Destroy, Debris, Parent = nil)
        // stops, as in Roblox: Place A removes each SoundBank clip with Debris.
        let gone: Vec<u64> = self
            .play
            .as_ref()
            .map(|s| {
                let alive = |d: Option<&Document>, r: Option<Ref>| {
                    d.zip(r)
                        .is_some_and(|(d, r)| d.exists(r) && d.is_ancestor(d.dom.root_ref(), r))
                };
                s.game
                    .tracks
                    .iter()
                    .filter(|(_, t)| {
                        (t.server.is_some() || t.client.is_some())
                            && !alive(Some(&self.doc), t.server)
                            && !alive(s.other_doc.as_ref(), t.client)
                    })
                    .map(|(k, _)| *k)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(s) = self.play.as_mut() {
            for k in gone {
                s.game.tracks.remove(&k);
                if let Some(a) = s.game.audio.as_mut() {
                    a.stop(k);
                }
            }
        }

        self.play_removed_sounds();

        // Sounds still downloading.
        let pending: Vec<u64> = self
            .play
            .as_ref()
            .map(|s| {
                s.game
                    .tracks
                    .iter()
                    .filter(|(_, t)| t.pending.is_some())
                    .map(|(k, _)| *k)
                    .collect()
            })
            .unwrap_or_default();
        for k in pending {
            self.start_sound(k);
        }

        // Ended / DidLoop, playback position and 3D volume.
        let cam = self.cam.pos;
        let (done, looped) = match self.play.as_mut().and_then(|s| s.game.audio.as_mut()) {
            Some(a) => (a.poll_finished(), a.looped_since_poll()),
            None => (Vec::new(), Vec::new()),
        };
        for k in done {
            let Some((server, client, detached, id)) = self
                .play
                .as_ref()
                .and_then(|s| s.game.tracks.get(&k))
                .map(|t| (t.server, t.client, t.detached, t.id.clone().unwrap_or_default()))
            else {
                continue;
            };
            if detached {
                if let Some(s) = self.play.as_mut() {
                    s.game.tracks.remove(&k);
                }
                continue;
            }
            // Studio, at the end of a sound that isn't looped: Playing and
            // IsPlaying false, IsPaused true, TimePosition 0, all already so
            // when Ended runs (once, on each side); no Changed signal.
            self.set_sound_info(server, client, |inp, r| {
                inp.sound_playing.remove(&r);
                inp.sound_paused.remove(&r);
                inp.sound_pos.insert(r, 0.0);
            });
            self.set_sound_doc(server, client, |i| {
                i.properties.insert(ustr("Playing"), Variant::Bool(false));
                i.properties.insert(ustr("TimePosition"), Variant::Float64(0.0));
            });
            self.fire_sound_event(server, client, "Ended", &id, None);
        }
        for (k, count) in looped {
            let Some((server, client, id)) = self
                .play
                .as_ref()
                .and_then(|s| s.game.tracks.get(&k))
                .filter(|t| !t.detached)
                .map(|t| (t.server, t.client, t.id.clone().unwrap_or_default()))
            else {
                continue;
            };
            self.fire_sound_event(server, client, "DidLoop", &id, Some(count));
        }
        let tracks: Vec<(u64, Option<Ref>, Option<Ref>, bool)> = self
            .play
            .as_ref()
            .map(|s| {
                s.game
                    .tracks
                    .iter()
                    .map(|(k, t)| (*k, t.server, t.client, t.detached))
                    .collect()
            })
            .unwrap_or_default();
        for (k, server, client, detached) in tracks {
            let playing = self
                .play
                .as_ref()
                .and_then(|s| s.game.audio.as_ref())
                .is_some_and(|a| a.is_playing(k));
            if !playing {
                continue;
            }
            if !detached {
                let pos = self
                    .play
                    .as_ref()
                    .and_then(|s| s.game.audio.as_ref())
                    .map(|a| a.position(k))
                    .unwrap_or(0.0);
                self.set_sound_info(server, client, |inp, r| {
                    inp.sound_pos.insert(r, pos);
                });
            }
            let settings = self.sound_doc(server, client).map(|(d, r)| {
                (
                    f32_prop(d, r, "Volume", 0.5).clamp(0., 10.),
                    f32_prop(d, r, "PlaybackSpeed", 1.),
                    matches!(d.prop(r, "Looped"), Some(Variant::Bool(true))),
                    rustudio_engine::audio_scene::settings(d, r, cam, self.cam.right(), self.audio_listener()),
                )
            });
            if let (Some((vol, speed, looped, (groups, settings))), Some(a)) = (
                settings,
                self.play.as_mut().and_then(|s| s.game.audio.as_mut()),
            ) {
                a.configure(k, groups, settings);
                a.set_volume(k, vol);
                a.set_speed(k, speed);
                a.set_looped(k, looped);
            }
        }

        if let Some(s) = self.play.as_mut() {
            s.game
                .flashes
                .retain(|f| now - f.born < mech::EXPLOSION_FLASH_S as f64);
            s.game
                .chat_log
                .retain(|(_, t)| now - t < mech::CHAT_SHOW_S as f64);
        }
        self.update_slots();
    }

    /// Sounds saved with Playing = true start when the game does. Not the ones
    /// a script already started during its first run (their Play() sets
    /// Playing = true too and is queued): that started every such sound twice.
    pub(super) fn start_autoplay_sounds(&mut self) {
        let queued: HashSet<Ref> = self
            .play
            .as_ref()
            .map(|s| {
                let server = s.server.as_ref().and_then(|server| server.world.as_ref()).map(|w| &w.requests.sounds);
                let client = s
                    .client
                    .as_ref()
                    .and_then(|c| c.world.as_ref())
                    .map(|w| &w.requests.sounds);
                server
                    .into_iter()
                    .chain(client)
                    .flatten()
                    .map(|(r, _)| *r)
                    .collect()
            })
            .unwrap_or_default();
        let sounds: Vec<Ref> = self
            .doc
            .dom
            .descendants()
            .filter(|i| {
                i.class == "Sound"
                    && matches!(
                        i.properties.get(&ustr("Playing")),
                        Some(Variant::Bool(true))
                    )
            })
            .map(|i| i.referent())
            .filter(|r| !queued.contains(r))
            .collect();
        for r in sounds {
            if self
                .doc
                .workspace()
                .is_some_and(|ws| self.doc.is_ancestor(ws, r))
                || self
                    .doc
                    .find_child(self.doc.dom.root_ref(), "SoundService")
                    .is_some_and(|s| self.doc.is_ancestor(s, r))
            {
                let client = self.client_of(r);
                self.sound_command(Some(r), client, SoundCmd::Play);
                self.set_sound_info(Some(r), client, |inp, x| {
                    inp.sound_playing.insert(x);
                });
            }
        }
    }

    pub(super) fn stop_all_sounds(&mut self) {
        if let Some(a) = self.play.as_mut().and_then(|s| s.game.audio.as_mut()) {
            a.stop_all();
        }
    }

    // ------------------------------------------------------- viewport side

    pub fn play_chat_available(&self) -> bool {
        self.play.as_ref().is_some_and(|s| {
            s.presentation.as_ref().map_or_else(
                || s.client.as_ref().and_then(|c| c.world.as_ref()).is_none_or(|w| !w.core_gui_off.contains("Chat")),
                |view| view.chat_on,
            )
        })
    }

    /// During Play with a character: collects mouse / key input for the game
    /// and draws the hotbar, chat and explosion flashes. Returns true when
    /// clicks belong to the game (no selecting).
    pub fn game_viewport(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        resp: &egui::Response,
        gui_took: bool,
    ) -> bool {
        let Some(side) = self
            .play
            .as_ref()
            .filter(|p| p.mode == RunMode::Play)
            .map(|p| p.showing)
        else {
            return false;
        };
        let ctx = ui.ctx().clone();
        // (LockCenter: the game's mouse is the middle of the view, wherever
        // the held system cursor is.)
        let threaded = self.play_thread.is_some();
        let pointer = rustudio_engine::mouse_lock::game_pointer(self.lock_center_held(), rect, resp.hover_pos()).map(|p| {
            let ray = self.cam.ray(rect, p);
            Pointer {
                origin: ray.origin,
                dir: ray.dir,
                hit: None,
                click: None,
                xy: [p.x - rect.min.x, p.y - rect.min.y],
                view: [rect.width(), rect.height()],
                side,
            }
        });
        let core_menu = rustudio_engine::gui::share().core_menu_open;
        let over_gui = gui_took || self.gui.pointer_over_sink() || core_menu;
        let pointer = if threaded {
            pointer
        } else {
            pointer.map(|p| if over_gui { p } else { self.resolve_play_pointer(p) })
        };
        let mut clicks = Vec::new();
        let mut ray_clicks = Vec::new();
        if let Some(p) = pointer.filter(|_| !over_gui) {
            for (btn, id) in [(egui::PointerButton::Primary, 1), (egui::PointerButton::Secondary, 2)] {
                if resp.clicked_by(btn) {
                    if threaded {
                        ray_clicks.push((p, id));
                    } else if let Some((r, at)) = p.click {
                        clicks.push((r, at, id));
                    }
                }
            }
        }
        // Any key, click or mouse move is activity (Player.Idled).
        if ctx.input(|i| !i.events.is_empty() || i.pointer.delta() != egui::Vec2::ZERO) {
            if let Some(s) = self.play.as_mut() {
                s.game.last_activity = s.time;
                s.game.input_active = true;
            }
        }
        let mut buttons = Vec::new();
        let mut uis = Vec::new();
        if resp.hovered() {
            ctx.input(|i| {
                for e in &i.events {
                    if let egui::Event::PointerButton {
                        button, pressed, ..
                    } = e
                    {
                        let b = match button {
                            egui::PointerButton::Primary => 1,
                            egui::PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        // UserInputService hears every click (gameProcessedEvent when GUI or menu took it).
                        uis.push((b, *pressed, over_gui));
                        if !over_gui {
                            buttons.push((b, *pressed));
                        }
                    }
                }
            });
        }
        // Number keys pick hotbar slots; "." toggles the emote wheel.
        // (Chat focus hotkey is handled natively on the CoreGui TextBox path
        // by players_chat_ui.luau via UserInputService + KeyboardKeyCode.)
        let mut slots = Vec::new();
        let taps = self
            .play
            .as_mut()
            .map(|s| std::mem::take(&mut s.game.hotbar_taps))
            .unwrap_or_default();
        if !ctx.egui_wants_keyboard_input() && self.gui.focus.is_none() {
            use egui::Key::*;
            for (n, k) in [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9]
                .iter()
                .enumerate()
            {
                if taps.contains(k) {
                    slots.push(n);
                }
            }
            // "." opens the emote wheel (Roblox's Emotes menu key).
            if taps.contains(&Period) {
                crate::emote_wheel::toggle();
            }
        }
        // StarterGui:SetCoreGuiEnabled(Chat, false) hides Roblox's chat.
        let core_gui_on = |name: &str| self.play.as_ref()
            .and_then(|s| s.client.as_ref())
            .and_then(|c| c.world.as_ref())
            .is_none_or(|w| !w.core_gui_off.contains(name));
        let presentation = self.play.as_ref().and_then(|s| s.presentation.as_ref());
        let chat_on = self.play_chat_available();
        let backpack_on = presentation.map_or_else(|| core_gui_on("Backpack"), |view| view.backpack_on);
        if !backpack_on {
            slots.clear();
        }
        // The open menu has the mouse and keys: none of it reaches the game
        // (SettingsHub's Modal shield and RbxSettingsHubStopCharacter).
        let menu = self.play.as_ref().is_some_and(|s| s.game.menu_open);
        if core_menu {
            slots.clear();
        }
        if menu {
            clicks.clear();
            ray_clicks.clear();
            buttons.clear();
            slots.clear();
            // A button let go while it's open still ends what its press began
            // (UserInputService.InputEnded, gameProcessedEvent true): a camera
            // pan or a held drag can't stay stuck holding the mouse after Resume.
            uis = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: false, .. } => Some((1, false, true)),
                        egui::Event::PointerButton { button: egui::PointerButton::Secondary, pressed: false, .. } => Some((2, false, true)),
                        _ => None,
                    })
                    .collect()
            });
        }
        let painter = ui.painter_at(rect);
        let now = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);

        // Explosion flashes.
        let flashes: Vec<(Vec3, f32, f64)> = self
            .play
            .as_ref()
            .map(|s| {
                s.game
                    .flashes
                    .iter()
                    .map(|f| (f.pos, f.radius, f.born))
                    .collect()
            })
            .unwrap_or_default();
        for (pos, radius, born) in flashes {
            let age = ((now - born) as f32 / mech::EXPLOSION_FLASH_S).clamp(0.0, 1.0);
            if let (Some(c), Some(edge)) = (
                self.cam.project(rect, pos),
                self.cam
                    .project(rect, pos + self.cam.right() * radius.max(1.0) * (0.6 + age)),
            ) {
                let a = ((1.0 - age) * 200.0) as u8;
                painter.circle_filled(
                    c,
                    (edge - c).length(),
                    Color32::from_rgba_unmultiplied(255, 150, 40, a / 2),
                );
                painter.circle_stroke(
                    c,
                    (edge - c).length(),
                    Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 220, 120, a)),
                );
            }
        }

        let core_scripts = self.play.as_ref().is_some_and(|s| s.game.core_scripts_active);

        // Hotbar (native fallback only when CoreScripts aren't running).
        let info = self.play_hotbar();
        if !core_scripts && backpack_on && !info.is_empty() {
            let doc_dir = self.doc.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf());
            let rects = self.gui.draw_hotbar(&ctx, &painter, &mut self.assets, doc_dir.as_deref(), rect, &info);
            for (n, r) in rects.into_iter().enumerate() {
                let click = ui.interact(r, ui.id().with(("slot", n)), egui::Sense::click());
                if click.clicked() {
                    slots.push(n);
                }
            }
        }

        // Chat UI (ExperienceChat, ChannelTabs, BubbleChat, TopBar toggle, and
        // LegacyChat) is drawn on the engine GUI path via CoreGui (either real
        // Roblox CoreScripts or the native players_chat_ui.luau fallback).
        if let Some(s) = self.play.as_ref().and_then(|s| s.game.kicked.clone()) {
            draw_kicked(&painter, rect, &s);
        }
        if !core_scripts {
            self.game_menu(ui, rect);
            if !menu {
                self.emote_wheel(ui, rect, &mut slots);
            }
        }
        if let Some(s) = self.play.as_mut() {
            let unfocused = self.gui.focus.is_none();
            s.game.chat_open = !unfocused;
            if chat_on && unfocused {
                s.game.chat_ready_frames += 1;
            }
            if !menu {
                s.game.input.pointer = if over_gui {
                    pointer.map(|mut p| {
                        p.click = None;
                        p
                    })
                } else {
                    pointer
                };
            }
            s.game.input.clicks.extend(clicks);
            s.game.input.ray_clicks.extend(ray_clicks);
            s.game.input.buttons.extend(buttons);
            s.game.uis_mouse.extend(uis);
            s.game.input.slots.extend(slots);
            if s.game.clickable && !menu && !over_gui {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        true
    }

    pub(crate) fn play_hotbar(&self) -> Vec<(String, String, bool)> {
        let Some(s) = self.play.as_ref() else { return Vec::new() };
        if let Some(view) = s.presentation.as_ref() {
            return view.hotbar.clone();
        }
        let equipped = s.game.equipped.as_ref().map(|e| e.tool);
        s.game.slots.iter().map(|&tool| {
            // Slots hold server refs even when the client is shown.
            let d = if self.doc.exists(tool) { Some(&self.doc) } else { self.play_other_doc() };
            let name = d.and_then(|d| d.dom.get_by_ref(tool)).map(|i| i.name.clone()).unwrap_or_default();
            let texture = rustudio_render::scene::content_string(d.and_then(|d| d.prop(tool, "TextureId")))
                .unwrap_or_default();
            (name, texture, Some(tool) == equipped)
        }).collect()
    }

    pub(crate) fn resolve_play_pointer(&mut self, mut pointer: Pointer) -> Pointer {
        let ray = rustudio_core::math::Ray { origin: pointer.origin, dir: pointer.dir };
        pointer.hit = self.mouse_pick(pointer.side, &ray).map(|(r, t)| (r, ray.at(t)));
        let terrain = self.with_play_side(pointer.side, |rt, _, _| {
            rustudio_engine::runtime::ext::mouse_terrain_hit(
                &rt.lua, ray.origin, ray.dir, rustudio_core::mech::MOUSE_RAY_LENGTH,
            )
        }).flatten();
        let near = |hit: Option<(Ref, Vec3)>| match terrain {
            Some((r, t)) if hit.is_none_or(|(_, at)| (at - ray.origin).length() > t) => Some((r, ray.at(t))),
            _ => hit,
        };
        pointer.click = near(self.click_pick(pointer.side, &ray, pointer.hit));
        pointer.hit = near(pointer.hit);
        pointer
    }

    /// The part a ClickDetector click meets: the mouse's hit, past parts with
    /// Transparency above 0.95 and no ClickDetector (Studio: a 0.95 part in
    /// front blocks the click, a 0.96 one doesn't; a Transparency 1 part
    /// with its own ClickDetector is still clicked).
    fn click_pick(&mut self, side: Side, ray: &rustudio_core::math::Ray, first: Option<(Ref, Vec3)>) -> Option<(Ref, Vec3)> {
        let mut hit = first;
        let mut skip = self.own_parts();
        for _ in 0..32 {
            let (r, _) = hit?;
            if !self.click_passes(side, r) {
                return hit;
            }
            skip.insert(r);
            let (o, d) = (ray.origin, ray.dir);
            self.server_front();
            let next = self.with_side(side, |rt, _, _| {
                rustudio_engine::runtime::ext::mouse_part_hit(&rt.lua, o, d, rustudio_core::mech::MOUSE_RAY_LENGTH, skip.clone())
            });
            self.shown_front();
            hit = next.flatten().map(|(r, t)| (r, ray.at(t)));
        }
        hit
    }

    /// Whether the click ray goes through `part` (a `side` ref).
    fn click_passes(&self, side: Side, part: Ref) -> bool {
        // (the shown side's DataModel is in front here)
        let shown = self.play.as_ref().map(|s| s.showing);
        let d = match (shown, self.play.as_ref().and_then(|s| s.other_doc.as_ref())) {
            (Some(sh), Some(other)) if sh != side => other,
            _ => &self.doc,
        };
        click_see_through(d, part)
    }

    /// The part under the game's mouse: the shown side's world query, as
    /// workspace:Raycast meets it; the drawn boxes only when that side has no
    /// runtime.
    fn mouse_pick(&mut self, side: Side, ray: &rustudio_core::math::Ray) -> Option<(Ref, f32)> {
        let skip = self.own_parts();
        let (o, d) = (ray.origin, ray.dir);
        // (with_side wants the server in front; the viewport has the shown side there)
        self.server_front();
        let hit = self.with_side(side, |rt, _, _| {
            rustudio_engine::runtime::ext::mouse_part_hit(&rt.lua, o, d, rustudio_core::mech::MOUSE_RAY_LENGTH, skip)
        });
        self.shown_front();
        match hit {
            Some(hit) => hit,
            None => self.pick_any(ray),
        }
    }

    /// The player's own character and tool on the shown side (they don't
    /// block the mouse). (Fx-hashed and copied whole on the server side:
    /// the set holds every simulated character part, and rebuilding it as a
    /// SipHash set twice per frame was ~2% of the gameplay worker.)
    fn own_parts(&self) -> rustc_hash::FxHashSet<Ref> {
        self
            .play
            .as_ref()
            .map(|s| match s.showing {
                Side::Server => s.physics_skip().clone(),
                Side::Client => {
                    // (Mapped again only when the set or the map changed:
                    // ~1,600 SipHash map lookups a frame on a test place.)
                    use std::hash::{BuildHasher, Hash, Hasher};
                    let skip = s.physics_skip();
                    let build = rustc_hash::FxBuildHasher;
                    let print = skip.iter().fold(0u64, |acc, r| {
                        let mut h = build.build_hasher();
                        r.hash(&mut h);
                        acc.wrapping_add(h.finish())
                    });
                    let key = (print, skip.len(), s.map.revision());
                    let mut cache = s.own_client.borrow_mut();
                    match cache.as_ref() {
                        Some((k, set)) if *k == key => set.clone(),
                        _ => {
                            let set: rustc_hash::FxHashSet<Ref> = skip.iter().filter_map(|r| s.map.s2c.get(r).copied()).collect();
                            *cache = Some((key, set.clone()));
                            set
                        }
                    }
                }
            })
            .unwrap_or_default()
    }

    /// The nearest drawn part box along a ray (any part, locked or not).
    fn pick_any(&self, ray: &rustudio_core::math::Ray) -> Option<(Ref, f32)> {
        let own = self.own_parts();
        let mut best: Option<(f32, Ref)> = None;
        for (sp, b) in self.scene.parts.iter().zip(&self.scene.bounds) {
            let oc = b.truncate() - ray.origin;
            let along = oc.dot(ray.dir);
            if oc.length_squared() - along * along > b.w * b.w || along < -b.w {
                continue;
            }
            if own.contains(&sp.r) {
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

/// Studio's "Disconnected" dialog after Player:Kick.
fn draw_kicked(painter: &egui::Painter, rect: Rect, msg: &str) {
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(110));
    let body = if msg.is_empty() {
        "You have been kicked by this experience or its moderators.".to_string()
    } else {
        format!("You have been kicked by this experience or its moderators. Moderation message:\n\n{msg}")
    };
    let g = painter.layout(body, egui::FontId::proportional(15.0), Color32::from_gray(220), 310.0);
    let size = egui::vec2(350.0, g.size().y + 100.0);
    let r = Rect::from_center_size(rect.center(), size);
    painter.rect_filled(r, 8.0, Color32::from_rgb(35, 37, 39));
    painter.text(r.min + egui::vec2(20.0, 18.0), egui::Align2::LEFT_TOP, "Disconnected", egui::FontId::proportional(18.0), Color32::WHITE);
    painter.galley(r.min + egui::vec2(20.0, 50.0), g, Color32::WHITE);
}

impl GameState {
    /// Where Humanoid:MoveTo is walking this (server) Humanoid, if it is
    /// (play_npc.rs walks it there).
    pub(crate) fn walk_target(&self, humanoid: Ref) -> Option<Vec3> {
        self.walks.get(&humanoid).map(|w| w.target)
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    #[test]
    fn tool_handle_accepts_named_base_part_children() {
        for class in ["Part", "MeshPart", "UnionOperation"] {
            let mut d = doc::Document::new_place();
            let tool = d.dom.insert(d.dom.root_ref(), InstanceBuilder::new("Tool"));
            let handle = d.dom.insert(tool, InstanceBuilder::new(class).with_name("Handle"));
            assert_eq!(tool_handle(&d, tool), Some(handle), "{class}");
        }
    }

    #[test]
    fn tool_handle_ignores_wrong_names_classes_and_nested_parts() {
        let mut d = doc::Document::new_place();
        let tool = d.dom.insert(d.dom.root_ref(), InstanceBuilder::new("Tool"));
        d.dom.insert(tool, InstanceBuilder::new("Part").with_name("handle"));
        let folder = d.dom.insert(tool, InstanceBuilder::new("Folder").with_name("Handle"));
        d.dom.insert(folder, InstanceBuilder::new("Part").with_name("Handle"));
        assert_eq!(tool_handle(&d, tool), None);

        let handle = d.dom.insert(tool, InstanceBuilder::new("Part").with_name("Handle"));
        assert_eq!(tool_handle(&d, tool), Some(handle));
        assert_eq!(tool_handle(&d, Ref::new()), None);
    }
}

#[cfg(test)]
mod click_tests {
    use super::*;

    fn part(d: &mut Document, parent: Ref, transparency: f32) -> Ref {
        d.dom.insert(parent, InstanceBuilder::new("Part").with_property("Transparency", transparency))
    }

    fn detector(d: &mut Document, parent: Ref, max: f32) -> Ref {
        d.dom.insert(parent, InstanceBuilder::new("ClickDetector").with_property("MaxActivationDistance", max))
    }

    #[test]
    fn reach_is_root_to_hit_point() {
        // Studio: a hit 10.105 from the HumanoidRootPart clicks with a max of
        // 10.15, not 10; the part's centre and the camera don't count.
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let p = part(&mut d, ws, 0.0);
        let near = detector(&mut d, p, 10.15);
        let root = Some(Vec3::new(0.0, 2.97, 0.0));
        let hit = Vec3::new(0.0, 2.97 + 1.45, -10.0);
        assert!(reach_in(&d, near, hit, root));
        let mut d2 = Document::new_place();
        let ws2 = d2.workspace().unwrap();
        let p2 = part(&mut d2, ws2, 0.0);
        let short = detector(&mut d2, p2, 10.0);
        assert!(!reach_in(&d2, short, hit, root));
        // No character: always in reach.
        assert!(reach_in(&d2, short, Vec3::splat(1000.0), None));
    }

    #[test]
    fn see_through_needs_transparency_above_0_95_and_no_detector() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let solid = part(&mut d, ws, 0.95);
        let clear = part(&mut d, ws, 0.96);
        let invisible = part(&mut d, ws, 1.0);
        let clickable = part(&mut d, ws, 1.0);
        detector(&mut d, clickable, 32.0);
        assert!(!click_see_through(&d, solid));
        assert!(click_see_through(&d, clear));
        assert!(click_see_through(&d, invisible));
        assert!(!click_see_through(&d, clickable), "a Transparency 1 part with a ClickDetector is clicked");
    }

    #[test]
    fn detector_in_a_model_above_the_part() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model"));
        let p = part(&mut d, model, 0.0);
        let det = detector(&mut d, model, 32.0);
        assert_eq!(click_detector_in(&d, p), Some(det));
        let loose = part(&mut d, ws, 0.0);
        assert_eq!(click_detector_in(&d, loose), None);
    }
}
