//! Play (F5), Run (F8) and Stop (Shift+F5).
//!
//! Play runs two DataModels like a Roblox test session: the **server** (a
//! deep copy of the edit DataModel, running Scripts and physics) and one
//! **client** (a replicated copy without ServerStorage / ServerScriptService
//! contents, running LocalScripts). Server changes stream to the client
//! every frame; client changes stay local, except the character, which the
//! client owns (its movement is written to both sides). Remotes carry calls
//! across. Run (F8) is the server alone. Stop restores the edit DataModel.
//!
//! While a session runs, the editor shows one side (`showing`); the other
//! waits in `other_doc`. Each tick puts the server in front, works, and puts
//! the chosen side back.

use std::collections::{HashMap, HashSet};

use glam::{Mat3, Vec3};
use mlua::Value;
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};

use crate::game::{Game, GameHost};
use rustudio_render::camera::{Camera, CameraMode};
use rustudio_core::doc::{self, Document};
use rustudio_core::log::Level;
use rustudio_engine::gui::{GuiArg, GuiEvent};
use rustudio_engine::ltypes::LEnumItem;
use rustudio_core::math::Xform;
use rustudio_core::mech;
use rustudio_physics::physics::{CharBody, Physics, PhysicsFrame, PhysicsJob, StepResult};
use rustudio_engine::replicate::{self, RepMap};
use rustudio_core::body::State;
use rustudio_engine::rig::{self, Rig, RigType};
use rustudio_engine::runtime::signal_timing::SignalMode;
use rustudio_engine::runtime::{RemoteKind, RepOp, RunMode, Runtime, Side, World};
use rustudio_render::scene::f32_of;

#[path = "play_game.rs"]
pub mod game;
#[path = "play_drag.rs"]
mod drag_input;
#[path = "play_humanoid.rs"]
mod hum_glue;
#[path = "play_npc.rs"]
mod npc;

/// The player's character (refs are server-side).
pub struct Character {
    pub model: Ref,
    /// Accessories the rig's attachments were last worked out for (unused:
    /// the Npcs journal reader redoes them, play_npc.rs `npc_journal`).
    pub accessories_seen: usize,
    /// Humanoid.FloorMaterial as last written (Enum.Material value).
    pub floor_material: u32,
    pub humanoid: Ref,
    pub rig: Rig,
    pub rig_type: RigType,
    pub body: CharBody,
    /// HumanoidRootPart centre above the capsule centre.
    root_lift: f32,
    pub yaw: f32,
    /// Velocity last frame (studs/s): what AssemblyLinearVelocity reports.
    pub velocity: Vec3,
    pub dead: bool,
    touching: HashSet<Ref>,
    touch_pairs: rustc_hash::FxHashSet<(Ref, Ref)>,
    /// What it touched when the seats last looked (a Seat sits whoever
    /// starts touching it, not whoever stands on it).
    seat_seen: HashSet<Ref>,
    pub state: State,
    landed_for: f32,
    last_speed: f32,
    /// Where the controller last put the HumanoidRootPart (a script moving
    /// it elsewhere teleports the character).
    last_root: Option<Xform>,
    /// The Seat / VehicleSeat it sits in (server ref).
    pub seat: Option<Ref>,
    /// The client copy's Humanoid.Sit last frame (the client owns its
    /// character: a LocalScript setting Sit = false gets up).
    client_sit: bool,
    /// What Studio's Humanoid controller knows of the body (round 22).
    pub shape: rustudio_physics::physics::HumShape,
    /// The last frame was a floating mode (swim, climb, mover, seat).
    hum_mode_left: bool,
    /// A wall the body was pushed into last frame (its outward normal):
    /// AutoRotate turns the body flush against it, as Studio's does.
    wall: Option<Vec3>,
    /// The root's forward tilt (radians, negative = looking down): a
    /// swimming Humanoid lies flat (Studio's root looks down while it swims).
    pitch: f32,
    /// Seconds before it may sit again after leaving a seat.
    seat_cooldown: f32,
    /// FallingDown / GettingUp / Ragdoll / Physics asked for by ChangeState.
    forced: Option<State>,
    /// Time in the forced state.
    forced_for: f32,
    /// Climbing / Swimming speed last reported (their events fire on change).
    last_move_speed: f32,
    /// The ladder sensor's last result (hit normal) and the updates left
    /// before FindLadder runs again (see `ladder_interval`).
    ladder: Option<Vec3>,
    ladder_wait: u8,
    /// Studio's Humanoid controller state (the dynamic body, play_humanoid.rs).
    dynamic: hum_glue::HumDyn,
    /// Parts welded onto the body after it spawned (play_npc.rs `npc_journal`):
    /// taken out of the loose simulation while they hang from it.
    pub(crate) carried: HashSet<Ref>,
}

/// FindLadder's period in a state, vtable : Running (and Landed,
/// Jumping) Studio → 2, Climbing and Freefall Studio → 0.
pub fn ladder_interval(state: State) -> u8 {
    if matches!(state, State::Climbing | State::Freefall) { 0 } else { 2 }
}

/// A change of seat this frame: Humanoid.Seated(active, seat).
struct SeatChange {
    active: bool,
    seat: Option<Ref>,
}

/// Which input decided the Play character's rig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigSource {
    /// The R15 / R6 picker in the test bar (an explicit choice wins).
    Picker,
    /// StarterPlayer.GameSettingsAvatar.
    Place,
    /// The signed-in person's avatar (playerAvatarType).
    Avatar,
    /// Nothing chose: rustudio's default.
    Default,
}

impl RigSource {
    pub fn label(self) -> &'static str {
        match self {
            RigSource::Picker => "picker",
            RigSource::Place => "place setting",
            RigSource::Avatar => "avatar",
            RigSource::Default => "default",
        }
    }
}

/// One side's runtime and (while not entered) its World.
pub struct SideState {
    pub runtime: Runtime,
    world: Option<World>,
}

impl SideState {
    /// `f` on this side's World, entered or not.
    fn with_world<R>(&self, f: impl FnOnce(&World) -> R) -> Option<R> {
        match &self.world {
            Some(w) => Some(f(w)),
            None => self.runtime.world_mut().map(|w| f(&w)),
        }
    }
}

/// Physics is present during the simulation phases. While a worker owns it,
/// the editor and player viewport use the last published documents instead.
pub struct PlayPhysics(Option<Box<Physics>>);

impl std::ops::Deref for PlayPhysics {
    type Target = Physics;
    fn deref(&self) -> &Physics {
        self.0.as_deref().expect("physics is in flight")
    }
}

impl std::ops::DerefMut for PlayPhysics {
    fn deref_mut(&mut self) -> &mut Physics {
        self.0.as_deref_mut().expect("physics is in flight")
    }
}

impl PlaySession {
    /// Character/tool exclusions used by the mouse query during presentation.
    pub(super) fn physics_skip(&self) -> &rustc_hash::FxHashSet<Ref> {
        match &self.pending_frame {
            Some(pending) => &pending.skip,
            None => self.presentation.as_ref().map_or_else(|| &self.physics.skip, |view| &view.skip),
        }
    }
}

struct PlayFrameInput {
    mode: RunMode,
    dt: f32,
    realtime: bool,
    humanoid: Option<hum_glue::HumFrameOut>,
    local_input_edge: bool,
}

struct PlayKeys {
    focused: bool,
    keys: HashSet<String>,
    began: Vec<(String, bool)>,
    ended: Vec<(String, bool)>,
    perf_stats_toggle: bool,
}

pub(crate) struct PlayKeyboard {
    focused: bool,
    blocked: bool,
    textbox: bool,
    modifiers: egui::Modifiers,
    shift_tapped: bool,
    perf_stats_toggle: bool,
    no_ui: bool,
    shift: bool,
}

impl PlayKeyboard {
    pub(crate) fn read(ctx: &egui::Context, textbox: bool, no_ui: bool, shift: bool) -> Self {
        let (focused, modifiers, shift_tapped, perf_stats_toggle) = ctx.input(|i| {
            let taps = rustudio_engine::mouse_lock::shift_taps(false, &i.raw.events, i.modifiers.shift);
            let perf = i.raw.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::F7,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } if modifiers.ctrl && modifiers.shift
                )
            });
            (i.focused, i.modifiers, taps > 0, perf)
        });
        Self {
            focused,
            blocked: ctx.egui_wants_keyboard_input(),
            textbox,
            modifiers,
            shift_tapped,
            perf_stats_toggle,
            no_ui,
            shift,
        }
    }
}

struct PendingPlayFrame {
    input: PlayFrameInput,
    job: Option<PhysicsJob>,
    ready: Option<PhysicsFrame>,
    deferred_dt: f32,
    skip: rustc_hash::FxHashSet<Ref>,
}

/// What the server half of a finished Play tick hands its client half
/// (play.rs `finish_server_phase` / `finish_client_phase`).
pub(crate) struct ServerPhase {
    mode: RunMode,
    dt: f32,
    realtime: bool,
    part_touches: Vec<(Ref, Ref, bool)>,
    character_touch_start: usize,
    client_only: HashSet<Ref>,
    velocities: rustc_hash::FxHashMap<Ref, Vec3>,
    state_names: HashMap<Ref, &'static str>,
    npc_events: Vec<npc::NpcEvent>,
    state_change: Option<(State, State)>,
    seat_change: Option<SeatChange>,
    move_event: Option<(&'static str, f32)>,
    running_speed: Option<f32>,
    humanoid_server: Option<Ref>,
    seat_server: Option<Ref>,
    server_out: (Vec<Ref>, Vec<Ref>),
}

impl ServerPhase {
    fn fire_npcs(&self, rt: &Runtime, map: Option<&rustudio_core::RefMap>) {
        let m = |r: Ref| match map {
            Some(m) => m.get(&r).copied(),
            None => Some(r),
        };
        for e in &self.npc_events {
            match e {
                npc::NpcEvent::State(h, old, new) => {
                    let Some(h) = m(*h) else { continue };
                    let item = |s: State| {
                        LEnumItem::from_name("HumanoidStateType", s.enum_name())
                            .and_then(|e| rustudio_engine::ltypes::enum_value(&rt.lua, e).ok())
                            .unwrap_or(Value::Nil)
                    };
                    rt.fire(h, "StateChanged", vec![item(*old), item(*new)]);
                    match new {
                        State::Jumping => rt.fire(h, "Jumping", vec![Value::Boolean(true)]),
                        State::Freefall => rt.fire(h, "FreeFalling", vec![Value::Boolean(true)]),
                        _ => {}
                    }
                    if *old == State::Freefall {
                        rt.fire(h, "FreeFalling", vec![Value::Boolean(false)]);
                    }
                    for (s, on) in [(*old, false), (*new, true)] {
                        if matches!(s, State::FallingDown | State::GettingUp | State::Ragdoll) {
                            rt.fire(h, s.enum_name(), vec![Value::Boolean(on)]);
                        }
                    }
                }
                npc::NpcEvent::Running(h, sp) => {
                    if let Some(h) = m(*h) {
                        rt.fire(h, "Running", vec![Value::Number(*sp as f64)]);
                    }
                }
                npc::NpcEvent::Move(h, ev, sp) => {
                    if let Some(h) = m(*h) {
                        rt.fire(h, ev, vec![Value::Number(*sp as f64)]);
                    }
                }
            }
        }
    }

    fn fire_touches(&self, rt: &Runtime, map: Option<&rustudio_core::RefMap>) {
        let m = |r: Ref| match map {
            Some(m) => m.get(&r).copied().or_else(|| self.client_only.contains(&r).then_some(r)),
            None => (!self.client_only.contains(&r)).then_some(r),
        };
        for (index, (a, b, started)) in self.part_touches.iter().enumerate() {
            let ev = if *started { "Touched" } else { "TouchEnded" };
            let character_touch = index >= self.character_touch_start;
            if let (Some(a), Some(b)) = (m(*a), m(*b)) {
                rt.fire(a, ev, vec![rt.inst(b)]);
                rt.fire(b, ev, vec![rt.inst(a)]);
                if *started {
                    rustudio_engine::rig::fire_humanoid_touched(rt, a, b);
                    if character_touch == map.is_some() {
                        rt.fire(a, "LocalSimulationTouched", vec![rt.inst(b)]);
                    }
                    if map.is_none() {
                        rt.fire(b, "LocalSimulationTouched", vec![rt.inst(a)]);
                    }
                } else {
                    rt.fire(a, "StoppedTouching", vec![rt.inst(b)]);
                    rt.fire(b, "StoppedTouching", vec![rt.inst(a)]);
                }
            }
        }
    }

    fn fire_humanoid(&self, rt: &Runtime, h: Option<Ref>, seat: Option<Ref>, client: bool) {
        let Some(h) = h else { return };
        if let Some((old, new)) = self.state_change {
            let item = |s: State| {
                LEnumItem::from_name("HumanoidStateType", s.enum_name())
                    .and_then(|i| rustudio_engine::ltypes::enum_value(&rt.lua, i).ok())
                    .unwrap_or(Value::Nil)
            };
            rt.fire(h, "StateChanged", vec![item(old), item(new)]);
            match new {
                State::Jumping => rt.fire(h, "Jumping", vec![Value::Boolean(true)]),
                State::Freefall => rt.fire(h, "FreeFalling", vec![Value::Boolean(true)]),
                _ => {}
            }
            if old == State::Freefall {
                rt.fire(h, "FreeFalling", vec![Value::Boolean(false)]);
            }
            // The (active) events of held states: true on entering,
            // false on leaving (Humanoid.FallingDown / GettingUp / Ragdoll).
            for (s, on) in [(old, false), (new, true)] {
                if matches!(s, State::FallingDown | State::GettingUp | State::Ragdoll) {
                    rt.fire(h, s.enum_name(), vec![Value::Boolean(on)]);
                }
                // PlatformStanding reaches the client (its Animate) from
                // the state; the side whose script set PlatformStand got it
                // then (runtime_ext). A test place sets it on the server.
                if client && s == State::PlatformStanding {
                    rt.fire(h, "PlatformStanding", vec![Value::Boolean(on)]);
                }
            }
        }
        if let Some(c) = &self.seat_change {
            let part = if c.active {
                seat.map(|s| rt.inst(s)).unwrap_or(Value::Nil)
            } else {
                Value::Nil
            };
            rt.fire(h, "Seated", vec![Value::Boolean(c.active), part]);
        }
        if let Some((ev, sp)) = self.move_event {
            rt.fire(h, ev, vec![Value::Number(sp as f64)]);
        }
        if let Some(sp) = self.running_speed {
            rt.fire(h, "Running", vec![Value::Number(sp as f64)]);
        }
    }
}

pub struct PlaySession {
    pub mode: RunMode,
    /// Place content loads only Roblox content while this session lives
    /// (assets::place_content_allowed).
    _scripts_running: Option<rustudio_core::assets::ScriptsRunning>,
    saved_doc: Option<Document>,
    saved_cam: Camera,
    saved_sel: Vec<Ref>,
    server: Option<SideState>,
    client: Option<SideState>,
    pub(super) presentation: Option<crate::play_worker::PlayView>,
    /// The DataModel not shown in the editor right now.
    other_doc: Option<Document>,
    /// Which DataModel the editor shows (Explorer, Properties, viewport).
    pub showing: Side,
    pub map: RepMap,
    pub physics: PlayPhysics,
    pending_frame: Option<PendingPlayFrame>,
    /// A tick whose server half the gameplay worker already ran while it
    /// waited for the display boundary (`play_worker_idle`), with the
    /// elapsed time its step deferred.
    server_phase: Option<(ServerPhase, f32)>,
    /// (server, client) `World::touch_rev` the physics' touch interest was built from.
    touch_seen: Option<(u64, u64)>,
    pub character: Option<Character>,
    /// The Player instance (server ref).
    pub player: Ref,
    initial_spawn_frames: Option<u8>,
    respawn_at: Option<f64>,
    /// The dead character's parts the server took over (lane DEATH): server part → the
    /// client copy's interpolation buffer (physics::hum_body::Handover).
    handover: HashMap<Ref, rustudio_physics::physics::hum_body::Handover>,
    pub time: f64,
    keys_prev: HashSet<String>,
    /// Keys whose InputBegan was game-processed (pressed into a TextBox):
    /// their InputEnded is too.
    keys_processed: HashSet<String>,
    /// The OS window had focus last frame (UserInputService.WindowFocused).
    window_focused: bool,
    /// Joystick devices seen (sorted names; slot i = Enum.UserInputType.Gamepad(i+1))
    /// and when they were last scanned.
    gamepads: crate::gamepad::Gamepads,
    /// When the instance collector last ran (`collect_garbage`), and parts the
    /// engine itself took out of the tree (fallen) for it to free.
    gc_at: f64,
    engine_orphans: Vec<Ref>,
    /// RUSTUDIO_MEMSTATS: when the last line was printed, and frames since.
    memstats_at: f64,
    memstats_frames: u32,
    memstats_worst: f32,
    /// Places the parts of assemblies only the client has.
    client_joints: rustudio_physics::client_joints::ClientJoints,
    /// Places the parts of server-side AnimationController / anchored Humanoid rigs.
    server_joints: rustudio_physics::client_joints::ClientJoints,
    /// The client DataModel's own simulation (crates/rustudio-physics/src/physics_client.rs).
    client_sim: rustudio_physics::physics::client::ClientSim,
    /// Touch changes between the client world's parts (client refs), fired on
    /// the client in its next script phase: Studio fires Touched where the
    /// part is simulated, so a LocalScript's clones (a pot lid and
    /// ingredients) touch on their client.
    client_touches: Vec<(Ref, Ref, bool)>,
    /// Humanoid state transitions so far (shown in Output, read by the self-test).
    pub state_log: Vec<String>,
    /// Clicks, chat, tools, sounds and other in-game interaction.
    pub game: game::GameState,
    /// Mouse motion (pixels) and wheel notches over the game view since last frame.
    pub mouse_delta: glam::Vec2,
    /// Characters spawned so far (the first one keeps the joining Backpack).
    pub spawns: u32,
    pub mouse_moved: bool,
    pub mouse_move_processed: bool,
    pub wheel: f32,
    pub wheel_processed: bool,
    /// The client's UserInputService.MouseBehavior (0 Default, 1 LockCenter,
    /// 2 LockCurrentPosition) and MouseIconEnabled, read after its scripts ran.
    pub mouse_behavior: u32,
    pub mouse_icon: bool,
    /// The game's cursor image: UserInputService.MouseIcon, else the
    /// PlayerMouse's Icon (None = the system arrow).
    pub cursor_image: Option<String>,
    /// UserGameSettings.RotationType is CameraRelative (shift-lock, first person).
    pub camera_relative: bool,
    /// The other Humanoids in Workspace (NPCs), driven like the player's
    /// with the dynamic Humanoid (play_npc.rs).
    pub(crate) npcs: npc::Npcs,
    /// The client refs of the physics skip set (play_game.rs `own_parts`),
    /// with the fingerprint of the set and map sizes it was mapped from.
    pub(crate) own_client: std::cell::RefCell<Option<((u64, usize, u64), rustc_hash::FxHashSet<Ref>)>>,
}

impl PlaySession {
    pub(super) fn new(mode: RunMode, saved_doc: Document, saved_cam: Camera, saved_sel: Vec<Ref>) -> Self {
        Self {
            npcs: Default::default(),
            own_client: Default::default(),
            mode,
            _scripts_running: None,
            saved_doc: Some(saved_doc),
            saved_cam,
            saved_sel,
            server: None,
            client: None,
            presentation: None,
            other_doc: None,
            showing: Side::Server,
            map: Default::default(),
            physics: PlayPhysics(None),
            pending_frame: None,
            server_phase: None,
            touch_seen: None,
            character: None,
            player: Ref::none(),
            initial_spawn_frames: None,
            respawn_at: None,
            handover: HashMap::default(),
            time: 0.0,
            keys_prev: HashSet::new(),
            keys_processed: HashSet::new(),
            window_focused: true,
            gamepads: Default::default(),
            gc_at: 0.0,
            engine_orphans: Vec::new(),
            memstats_at: 0.0,
            memstats_frames: 0,
            memstats_worst: 0.0,
            client_joints: Default::default(),
            server_joints: Default::default(),
            client_sim: Default::default(),
            client_touches: Vec::new(),
            state_log: Vec::new(),
            game: Default::default(),
            mouse_delta: glam::Vec2::ZERO,
            spawns: 0,
            mouse_moved: false,
            mouse_move_processed: false,
            wheel: 0.0,
            wheel_processed: false,
            mouse_behavior: 0,
            mouse_icon: true,
            cursor_image: None,
            camera_relative: false,
        }
    }

    /// The server side's collector counts while its runtime isn't entered:
    /// (unparented instances watched, instances freed so far, instances with
    /// a Lua slot). The self-test's `gc` step reads these.
    pub fn gc_counts(&self) -> (usize, u64, usize) {
        let Some(server) = self.server.as_ref() else { return (0, 0, 0) };
        let (orphans, freed) = server
            .world
            .as_ref()
            .map(|w| (w.orphans.len(), w.freed))
            .unwrap_or_default();
        (orphans, freed, server.runtime.lua_slots())
    }

    /// The id of the DataModel not shown (the server's while the client is).
    pub fn other_doc_id(&self) -> Option<u64> {
        self.other_doc.as_ref().map(|d| d.id)
    }
}

/// KeyCode names for the keys the play session reports to scripts. `all`
/// (no-UI runs, where the editor reserves no key) adds the keys the editor
/// keeps for itself or never reported: F5–F15, Slash.
fn key_name(k: egui::Key, all: bool) -> Option<&'static str> {
    use egui::Key::*;
    if all {
        match k {
            F5 => return Some("F5"),
            F6 => return Some("F6"),
            F7 => return Some("F7"),
            F8 => return Some("F8"),
            F9 => return Some("F9"),
            F10 => return Some("F10"),
            F11 => return Some("F11"),
            F12 => return Some("F12"),
            F13 => return Some("F13"),
            F14 => return Some("F14"),
            F15 => return Some("F15"),
            Slash => return Some("Slash"),
            _ => {}
        }
    }
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
        Space => "Space",
        ArrowUp => "Up",
        ArrowDown => "Down",
        ArrowLeft => "Left",
        ArrowRight => "Right",
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
        Tab => "Tab",
        Escape => "Escape",
        Backspace => "Backspace",
        Delete => "Delete",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        Comma => "Comma",
        Period => "Period",
        Slash => "Slash",
        Minus => "Minus",
        Equals => "Equals",
        Semicolon => "Semicolon",
        Quote => "Quote",
        OpenBracket => "LeftBracket",
        CloseBracket => "RightBracket",
        Backslash => "BackSlash",
        Backtick => "Backquote",
        PageUp => "PageUp",
        PageDown => "PageDown",
        Home => "Home",
        End => "End",
        Insert => "Insert",
        _ => return None,
    })
}

fn new_side(mode: RunMode, side: Side, peer: bool, signal_mode: SignalMode) -> Result<SideState, String> {
    let runtime = Runtime::new()?;
    runtime
        .install_side()
        .map_err(|e| rustudio_engine::runtime::describe_error(&e))?;
    rustudio_core::load_trace::mark("runtime: side installed");
    // --fixed-dt: this side's math.random / Random.new seeds, not the
    // VM's address and the time (crates/rustudio-core/src/sim_clock.rs).
    rustudio_engine::lua_bridge::seed_vm(&runtime.lua, if side == Side::Server { 1 } else { 2 });
    let mut world = World::new(Document::empty(), mode);
    world.doc = None;
    world.side = side;
    world.has_peer = peer;
    world.signal_mode = signal_mode;
    Ok(SideState {
        runtime,
        world: Some(world),
    })
}

impl Game {
    pub fn is_playing(&self) -> bool {
        self.play.is_some()
    }

    /// The edit document Play keeps while it runs (the plugin host's).
    /// The edit document Play keeps aside while it runs.
    pub fn play_saved_doc(&self) -> Option<&Document> {
        self.play.as_ref()?.saved_doc.as_ref()
    }

    pub fn play_saved_doc_mut(&mut self) -> Option<&mut Document> {
        self.play.as_mut()?.saved_doc.as_mut()
    }

    /// True while the editor shows the client of a Play session: mouse and
    /// keys go to the game's scripts.
    pub fn play_game_input(&self) -> bool {
        self.play.as_ref().is_some_and(|p| {
            p.mode == RunMode::Play && p.showing == Side::Client
        })
    }

    /// True while a Play session drives a character.
    pub fn play_has_character(&self) -> bool {
        self.play.as_ref().is_some_and(|p| {
            p.mode == RunMode::Play && p.presentation.as_ref().map_or_else(
                || p.character.as_ref().is_some_and(|c| !c.dead), |p| p.has_character,
            )
        })
    }

    /// Puts the server DataModel in `self.doc` (the client one in `other_doc`).
    pub(super) fn server_front(&mut self) {
        if let Some(s) = self.play.as_mut() {
            if s.showing == Side::Client {
                if let Some(other) = s.other_doc.as_mut() {
                    std::mem::swap(&mut self.doc, other);
                }
            }
        }
    }

    /// Undoes `server_front`.
    pub(super) fn shown_front(&mut self) {
        self.server_front();
    }

    /// Switches the editor between the server's and the client's DataModel.
    pub fn show_play_side(&mut self, side: Side, host: &mut dyn GameHost) {
        if self.play_thread.is_some() {
            self.show_worker_side(side, host);
            return;
        }
        let Some(s) = self.play.as_mut() else { return };
        if s.showing == side || s.other_doc.is_none() {
            return;
        }
        if let Some(other) = s.other_doc.as_mut() {
            std::mem::swap(&mut self.doc, other);
        }
        s.showing = side;
        self.doc.selection.clear();
        self.scene = rustudio_render::scene::Scene::default();
        host.document_changed(&self.doc);
    }

    /// Runs `f` with one side's runtime entered (its DataModel lent to Lua).
    /// `peer` is the other side's DataModel. Must be called with the server
    /// in front. Output is tagged with the side.
    pub(crate) fn with_side<R>(
        &mut self,
        side: Side,
        f: impl FnOnce(&Runtime, Option<&Document>, &mut RepMap) -> R,
    ) -> Option<R> {
        let s = self.play.as_mut()?;
        let (result, output) = match side {
            Side::Server => {
                let server = s.server.as_mut()?;
                let mut w = server.world.take()?;
                w.doc = Some(std::mem::replace(&mut self.doc, Document::placeholder()));
                server.runtime.enter(w);
                let r = f(&server.runtime, s.other_doc.as_ref(), &mut s.map);
                let mut w = server.runtime.leave();
                self.doc = w.doc.take().expect("server document returned");
                let out = std::mem::take(&mut w.output);
                server.world = Some(w);
                (r, out)
            }
            Side::Client => {
                let c = s.client.as_mut()?;
                let mut w = c.world.take()?;
                w.doc = Some(s.other_doc.take()?);
                c.runtime.enter(w);
                let r = f(&c.runtime, Some(&self.doc), &mut s.map);
                let mut w = c.runtime.leave();
                s.other_doc = w.doc.take();
                let out = std::mem::take(&mut w.output);
                c.world = Some(w);
                (r, out)
            }
        };
        let tag = side.label();
        // Run has only a server, and Studio still marks its lines Server
        // (the green bar).
        let in_session = self.play.is_some();
        for (level, text) in output {
            if in_session {
                self.push_log(level, format!("[{tag}] {text}"));
            } else {
                self.push_log(level, text);
            }
        }
        Some(result)
    }

    fn server_world(&mut self) -> Option<&mut World> {
        self.play.as_mut()?.server.as_mut()?.world.as_mut()
    }

    fn client_world(&mut self) -> Option<&mut World> {
        self.play.as_mut()?.client.as_mut()?.world.as_mut()
    }

    fn record(&mut self, op: RepOp) {
        if let Some(w) = self.server_world() {
            w.record(op);
        }
    }

    pub fn start_play(&mut self, mode: RunMode, host: &mut dyn GameHost, clear_output: bool, live_ids: Option<(u64, u64)>) {
        if self.play.is_some() {
            return;
        }
        self.edit_audio.stop_playback();
        {
            let mut sh = rustudio_engine::gui::share();
            sh.core_menu_open = false;
            sh.selection_visible = false;
            sh.ui_nav_toggle_disabled = false;
            sh.fullscreen_req = None;
            sh.microprofiler_req = None;
            sh.saved_quality = None;
        }
        rustudio_core::load_trace::mark("play: start asked");
        // A play-testing teleport plays its destination instead of the edit
        // document, and its player arrives with the teleport's data.
        let teleport = self.teleport_to.take();
        rustudio_engine::play_teleport::clear_request();
        rustudio_engine::play_teleport::set_in_flight(false);
        rustudio_engine::play_teleport::set_arrival(teleport.as_ref().map(|t| t.arrival.clone()));
        // A teleport's destination plays with its own ids (not a Team Create
        // session's); the first Play records where the session began.
        let live_ids = match &teleport {
            Some(t) => Some((t.universe_id, t.place_id)),
            None => {
                self.play_origin = live_ids;
                live_ids
            }
        };
        // rustudio's per-session physics options (solver, spin guard, client
        // link): RUSTUDIO_* env, else the saved option, read once per Play.
        let solver = rustudio_core::rustudio_settings::latch_for_play();
        // Studio Settings > Output > Clear Output On Start, for runs a person
        // drives (harness logs and --log keep everything).
        if clear_output {
            self.log.clear();
            self.log_cleared = true;
            self.log_drained = 0;
        }
        // The local player is the signed-in account (identity and avatar),
        // as in Studio: a session check still running at Play is waited for.
        if mode == RunMode::Play {
            // Wait for the signed-in user's avatar before scripts start, even
            // when CharacterAutoLoads is false: scripts commonly call
            // Players:GetHumanoidDescriptionFromUserId in PlayerAdded for lobby
            // or class-preview rigs.
            for (level, text) in self.account.settle_for_play(std::time::Duration::from_secs(20), true) {
                self.push_log(level, text);
            }
        }
        let two = mode == RunMode::Play;
        // From before any script runs until the session ends: no local files
        // as place content (assets::place_content_allowed).
        let scripts_running = rustudio_core::assets::ScriptsRunning::new();
        // Cloud data stays in memory for any published place but the
        // scratch experience (domains::data::local_mode_for).
        let universe = live_ids
            .map(|(u, _)| u)
            .or_else(|| self.doc.cloud.as_ref().map(|o| o.universe_id))
            .unwrap_or(0);
        rustudio_engine::runtime::domains::data::PLAY_UNIVERSE
            .store(universe, std::sync::atomic::Ordering::Relaxed);
        if rustudio_engine::runtime::domains::data::local_mode_for(universe) && universe != 0 {
            self.push_log(
                rustudio_core::log::Level::Warn,
                format!(
                    "DataStores, MemoryStores and Messaging stay in memory for this Play \
                     (universe {universe} is a published game; Studio Settings > rustudio > \
                     Use live DataStores uses its real data)"
                ),
            );
        }
        if rustudio_core::rustudio_settings::live_data() {
            self.info("DataStores: live");
        }
        let play_doc = teleport.as_ref().map_or(&self.doc, |destination| &destination.doc);
        let signal_mode = SignalMode::of(play_doc);
        let server = match new_side(mode, Side::Server, two, signal_mode) {
            Ok(s) => s,
            Err(e) => {
                self.error(format!("Could not start the Luau runtime: {e}"));
                return;
            }
        };
        let client = if two {
            match new_side(mode, Side::Client, true, signal_mode) {
                Ok(c) => Some(c),
                Err(e) => {
                    self.error(format!("Could not start the client runtime: {e}"));
                    return;
                }
            }
        } else {
            None
        };
        rustudio_core::load_trace::mark("play: runtimes made");
        // (the destination stands in for the edit document until the
        // session's copy is made; the edit document is what Stop brings back)
        let mut edit_doc = teleport.map(|t| std::mem::replace(&mut self.doc, t.doc));
        let label = if two { "Play" } else { "Run" };
        let mut server_doc = self
            .doc
            .clone_for_play(format!("{} ({label}: Server)", self.doc.title));
        // Roblox's default PlayerModule, PlayerScriptsLoader and
        // RbxCharacterSounds join StarterPlayerScripts, as Studio does.
        // Every Roblox DataModel has these services, even when the file
        // doesn't list them (a place saved with only a Workspace).
        // Workspace (with Terrain and a CurrentCamera) and the other services
        // exist before any script runs, as in every Roblox DataModel.
        server_doc.ensure_services();
        // Players starts empty, as in Studio's Play: a live Team Create
        // DataModel holds the collaborators' Player objects (a live Place A session:
        // Players.testuser1), and copied along they were a phantom first
        // player that Place A's match spawned and placed instead of the real one.
        if let Some(players) = server_doc.find_child(server_doc.dom.root_ref(), "Players") {
            let stale: Vec<_> = server_doc
                .dom
                .get_by_ref(players)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| server_doc.dom.get_by_ref(*c).is_some_and(|i| i.class == "Player"))
                .collect();
            for c in stale {
                server_doc.dom.destroy(c);
            }
        }
        // A place opened from Roblox plays with its real ids, as in Studio
        // (an unpublished one keeps 0: runtime_ext's DataModel defaults).
        if let Some(o) = server_doc.cloud.clone() {
            let root = server_doc.dom.root_ref();
            if let Some(i) = server_doc.dom.get_by_ref_mut(root) {
                use rbx_dom_weak::{types::Enum, ustr};
                i.properties.insert(ustr("PlaceId"), Variant::Int64(o.place_id as i64));
                i.properties.insert(ustr("GameId"), Variant::Int64(o.universe_id as i64));
                i.properties.insert(ustr("CreatorId"), Variant::Int64(o.creator_id as i64));
                i.properties.insert(
                    ustr("CreatorType"),
                    Variant::Enum(Enum::from_u32(o.creator_type.enum_value())),
                );
                i.properties.insert(ustr("PlaceVersion"), Variant::Int32(o.version as i32));
            }
        }
        // Direct live joins have no REST CloudOrigin. Their session still
        // supplies the published identity required by game initialization.
        if let Some((universe, place)) = live_ids {
            let root = server_doc.dom.root_ref();
            if let Some(i) = server_doc.dom.get_by_ref_mut(root) {
                i.properties.insert(ustr("PlaceId"), Variant::Int64(place as i64));
                i.properties.insert(ustr("GameId"), Variant::Int64(universe as i64));
            }
        }
        rustudio_core::load_trace::mark("play: server DataModel copied");
        let defaults = if two {
            match rustudio_core::default_scripts::insert_player_scripts(&mut server_doc) {
                Ok(added) => added,
                Err(error) => {
                    self.error(format!("Could not load Studio PlayerModule for Play: {error}"));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        // Tools saved in an NPC's hand: gripped as the session's DataModel loads (Handle
        // CanCollide false, tool_grip.rs), before the client's copy is made from it.
        rustudio_engine::tool_grip::grip_at_load(&mut server_doc);
        // Ensure RobloxReplicatedStorage + version RemoteFunctions exist before
        // initial_client so SettingsHub's GetServerVersionBlocking() maps 1:1.
        if two && rustudio_core::rustudio_settings::core_scripts() {
            let root = server_doc.dom.root_ref();
            let rrs = server_doc
                .find_child(root, "RobloxReplicatedStorage")
                .unwrap_or_else(|| {
                    server_doc
                        .dom
                        .insert(root, InstanceBuilder::new("RobloxReplicatedStorage").with_name("RobloxReplicatedStorage"))
                });
            for rf_name in ["GetServerVersion", "GetServerChannel"] {
                if server_doc.find_child(rrs, rf_name).is_none() {
                    server_doc
                        .dom
                        .insert(rrs, InstanceBuilder::new("RemoteFunction").with_name(rf_name));
                }
            }
        }
        let (mut client_doc, map) = if two {
            let (c, m) = replicate::initial_client(
                &server_doc,
                format!("{} ({label}: Client)", self.doc.title),
            );
            (Some(c), m)
        } else {
            (None, RepMap::default())
        };
        rustudio_core::load_trace::mark("play: client DataModel replicated");
        // Roblox's own CoreScripts (core_scripts.rs): CorePackages and
        // CoreGui.RobloxGui from Studio's data model patch. Install into the
        // client DataModel before server scripts start so decoding/cloning
        // DataModelPatch.rbxm does not stall wall-clock time between server
        // startup and client join. CoreScripts still start in
        // `setup_client_player` once LocalPlayer exists.
        let core_scripts = if let Some(cdoc) = client_doc.as_mut() {
            if rustudio_core::rustudio_settings::core_scripts()
                && (rustudio_core::core_scripts::patch_path().is_some()
                    || rustudio_core::rustudio_settings::core_scripts_forced())
            {
                // Populate Stats.PerformanceStats with native StatsItem children
                // (not creatable via Instance.new) for DevConsole & PerformanceStats.
                let root = cdoc.dom.root_ref();
                let stats = cdoc.find_child(root, "Stats").unwrap_or_else(|| {
                    cdoc.dom
                        .insert(root, InstanceBuilder::new("Stats").with_name("Stats"))
                });
                let perf = cdoc
                    .dom
                    .get_by_ref(stats)
                    .and_then(|s| {
                        s.children()
                            .iter()
                            .copied()
                            .find(|c| cdoc.dom.get_by_ref(*c).is_some_and(|i| i.name == "PerformanceStats"))
                    })
                    .unwrap_or_else(|| {
                        cdoc.dom
                            .insert(stats, InstanceBuilder::new("Folder").with_name("PerformanceStats"))
                    });
                for n in [
                    "Memory", "CPU", "GPU", "NetworkSent", "NetworkReceived", "Ping",
                    "MaxMemory", "MaxCPU", "MaxGPU", "MaxNetworkSent", "MaxNetworkReceived", "MaxPing",
                ] {
                    let exists = cdoc.dom.get_by_ref(perf).is_some_and(|p| {
                        p.children()
                            .iter()
                            .any(|c| cdoc.dom.get_by_ref(*c).is_some_and(|i| i.name == n))
                    });
                    if !exists {
                        cdoc.dom.insert(
                            perf,
                            InstanceBuilder::new("StatsItem")
                                .with_name(n)
                                .with_property("DisplayName", rbx_dom_weak::types::Variant::String(n.into())),
                        );
                    }
                }
                Some(load_core_scripts(cdoc))
            } else {
                None
            }
        } else {
            None
        };
        let saved_sel = std::mem::take(&mut edit_doc.as_mut().unwrap_or(&mut self.doc).selection);
        let saved_doc = std::mem::replace(&mut self.doc, server_doc);
        let saved_doc = edit_doc.take().unwrap_or(saved_doc);
        let mut physics = Physics::new(&mut self.doc);
        rustudio_core::load_trace::mark("play: physics built");
        self.info(format!("Physics solver: {}", solver.name()));
        // One collision-group table for the session: the server's scripts
        // set it, the client's queries and the physics read it.
        let mut server = server;
        let mut client = client;
        let groups = rustudio_core::collision_groups::shared();
        for w in [server.world.as_mut(), client.as_mut().and_then(|c| c.world.as_mut())]
            .into_iter()
            .flatten()
        {
            w.groups = groups.clone();
            w.avatar = self.account.avatar().cloned();
        }
        physics.groups = groups;
        self.play = Some(PlaySession {
            _scripts_running: Some(scripts_running),
            server: Some(server),
            client,
            other_doc: client_doc,
            map,
            physics: PlayPhysics(Some(Box::new(physics))),
            gamepads: if two { crate::gamepad::Gamepads::new() } else { Default::default() },
            ..PlaySession::new(mode, saved_doc, self.cam.clone(), saved_sel)
        });
        self.add_chat_stub();
        // The menu's settings from earlier playtests (volume, camera
        // sensitivity, inverted): onto the mixer now, and onto the client's
        // UserGameSettings on its first tick.
        rustudio_engine::runtime::domains::services::set_keep_game_settings(self.keep_game_settings);
        if self.keep_game_settings {
            let saved = rustudio_core::game_settings::load();
            if let Some(v) = saved.volume {
                rustudio_engine::audio::set_master_global(v);
            }
            if let Some(s) = self.play.as_mut() {
                s.game.menu.lua.extend(saved.client_lua());
            }
        }
        if !defaults.is_empty() {
            self.info(format!(
                "Default player scripts from Studio: {}",
                defaults.join(", ")
            ));
        }
        self.scene = rustudio_render::scene::Scene::default();
        host.document_changed(&self.doc);
        self.info(match mode {
            RunMode::Play => "▶ Play: a server and a client are running; WASD + Space to move, Shift+F5 to stop",
            RunMode::Run => "▶ Run: server scripts and physics running (no client, no character), Shift+F5 to stop",
            RunMode::Edit => "",
        });

        let bind = |rt: &Runtime, _: Option<&Document>, _: &mut RepMap| {
            if let Err(e) = rt.bind_globals() {
                if let Some(mut w) = rt.world_mut() {
                    w.output
                        .push((Level::Error, rustudio_engine::runtime::describe_error(&e)));
                }
            }
        };
        self.with_side(Side::Server, bind);
        if two {
            self.with_side(Side::Client, bind);
        }
        rustudio_core::load_trace::mark("play: globals bound");
        // Studio loads every plugin again in the Play server and client
        // DataModels (measured: each copy runs with IsEdit false).
        self.with_side(Side::Server, |rt, _, _| host.load_plugins(rt));
        if two {
            self.with_side(Side::Client, |rt, _, _| host.load_plugins(rt));
        }
        self.with_side(Side::Server, |rt, _, _| rt.warm_queries());
        rustudio_core::load_trace::mark("play: server queries warmed");
        // Server scripts start first, as on a real server; the player joins
        // after (so Players:GetPlayers() is empty when they start).
        let server_scripts = self.server_scripts(None);
        self.with_side(Side::Server, |rt, _, _| {
            for r in server_scripts {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
        rustudio_core::load_trace::mark("play: server scripts started");
        if two {
            self.create_player();
            if self.play.as_ref().is_some_and(|s| s.map.streaming()) {
                let (position, _) = self.character_spawn_point();
                if let Some(s) = &mut self.play {
                    s.map.set_streaming_spawn(position);
                }
            }
            let player = self
                .play
                .as_ref()
                .map(|p| p.player)
                .unwrap_or_else(Ref::none);
            // Tool scripts run from the player's Backpack, and legacy
            // Scripts from its PlayerGui too (Place B Newer's StarterGui
            // "version" label: Studio shows "server version: 0").
            let backpack: Vec<Ref> = ["Backpack", "PlayerGui"]
                .iter()
                .filter_map(|c| self.doc.find_child(player, c))
                .flat_map(|f| self.server_scripts(Some(f)))
                .collect();
            // Establish the client's Player and its containers in the join
            // snapshot. Server PlayerAdded writes arrive through the next
            // replication phase, after the client starts PlayerScripts.
            self.replicate_now();
            let players = self.doc.find_child(self.doc.dom.root_ref(), "Players");
            let joined = self.doc.dom.descendants_of(player).map(|i| i.referent()).collect();
            self.with_side(Side::Server, |rt, _, _| {
                if let Some(p) = players {
                    rt.fire(p, "PlayerAdded", vec![rt.inst(player)]);
                    rt.notify_parent(player, Ref::none(), p, joined);
                }
                for r in backpack {
                    rt.start_collected(r);
                }
                rt.dispatch();
            });
            rustudio_core::load_trace::mark("play: player added");
            self.with_side(Side::Client, |rt, _, _| rt.warm_queries());
            rustudio_core::load_trace::mark("play: client queries warmed");
            self.setup_client_player(core_scripts);
            // Client CoreScripts startup ran synchronously after server scripts
            // started (still at s.time == 0, before frame 1). Re-anchor server
            // timer wall clocks so server waits do not burst-fire on frame 1.
            self.with_side(Side::Server, |rt, _, _| rt.reset_timer_walls());
            rustudio_core::load_trace::mark("play: client player set up");
            // Players.CharacterAutoLoads false: no character until a script
            // calls Player:LoadCharacter (the client's camera is the scripts').
            if self.character_auto_loads() {
                // Local Studio Play measured six to seven server Heartbeats
                // between PlayerAdded and the first CharacterAdded. Give the
                // bootstrap scripts and the join replica those frames before
                // cloning the character and firing the first PlayerGui scripts.
                if let Some(s) = self.play.as_mut() {
                    s.initial_spawn_frames = Some(6);
                }
            } else {
                self.info("No character: Players.CharacterAutoLoads is false (a script's Player:LoadCharacter spawns one)");
            }
            // Studio shows the client's view while playing.
            if self.play_shows_client {
                self.show_play_side(Side::Client, host);
            }
        }
        self.start_autoplay_sounds();
        rustudio_core::load_trace::mark("play: DataModels running");
    }

    /// Player.CharacterRemoving (on both sides) just before the old character
    /// goes, as Roblox does on respawn and LoadCharacter.
    fn fire_character_removing(&mut self, model: Ref) {
        let Some((player, cplayer, cmodel)) = self.play.as_ref().map(|s| {
            (
                s.player,
                s.map.s2c.get(&s.player).copied(),
                s.map.s2c.get(&model).copied(),
            )
        }) else {
            return;
        };
        if player.is_some() {
            self.with_side(Side::Server, |rt, _, _| {
                rt.fire(player, "CharacterRemoving", vec![rt.inst(model)]);
                rt.dispatch();
            });
        }
        if let (Some(cp), Some(cm)) = (cplayer, cmodel) {
            self.with_side(Side::Client, |rt, _, _| {
                rt.fire(cp, "CharacterRemoving", vec![rt.inst(cm)]);
                rt.dispatch();
            });
        }
    }

    /// Players.PlayerRemoving when Play stops (the player leaves the server),
    /// with one dispatch so handlers run before the session goes.
    fn fire_player_removing(&mut self) {
        let Some((players, player, cplayers, cplayer)) = self.play.as_ref().map(|s| {
            let root = self.doc.dom.root_ref();
            let players = self.doc.find_child(root, "Players");
            (
                players,
                s.player,
                players.and_then(|p| s.map.s2c.get(&p).copied()),
                s.map.s2c.get(&s.player).copied(),
            )
        }) else {
            return;
        };
        if let Some(ps) = players.filter(|_| player.is_some()) {
            self.with_side(Side::Server, |rt, _, _| {
                rt.fire(ps, "PlayerRemoving", vec![rt.inst(player)]);
                rt.dispatch();
            });
        }
        if let (Some(ps), Some(p)) = (cplayers, cplayer) {
            self.with_side(Side::Client, |rt, _, _| {
                rt.fire(ps, "PlayerRemoving", vec![rt.inst(p)]);
                rt.dispatch();
            });
        }
    }

    pub fn stop_play(&mut self, host: &mut dyn GameHost) {
        // (a teleport ends this session to start the destination's)
        let teleporting = self.teleport_to.is_some() || std::mem::take(&mut self.quiet_stop);
        if teleporting {
            self.quiet_stop = true;
        }
        self.stop_play_worker();
        self.quiet_stop = false;
        // A TextBox focused in Play (the chat bar) doesn't keep the
        // editor's keyboard after the session ends.
        self.gui.drop_focus();
        // A game's orthographic camera ends with the session.
        self.cam.ortho = None;
        self.cam.stretch = 1.0;
        if self.play.is_some() {
            self.wait_for_play_physics();
            if let Some(s) = self.play.as_mut() {
                s.pending_frame = None;
                s.server_phase = None;
            }
            self.server_front();
            self.fire_player_removing();
            // The Play DataModels' plugin copies unload with them, the
            // client's first (measured in Studio: PlayClient, then PlayServer).
            self.with_side(Side::Client, |rt, _, _| host.unload_plugins(rt));
            self.with_side(Side::Server, |rt, _, _| host.unload_plugins(rt));
        }
        rustudio_core::telemetry::flush();
        self.stop_all_sounds();
        {
            let mut sh = rustudio_engine::gui::share();
            sh.core_menu_open = false;
            sh.selection_visible = false;
            sh.ui_nav_toggle_disabled = false;
            sh.fullscreen_req = None;
            sh.microprofiler_req = None;
            sh.saved_quality = None;
        }
        self.cam.basis = None;
        let Some(mut s) = self.play.take() else {
            return;
        };
        if let Some(d) = s.saved_doc.take() {
            self.doc = d;
        }
        self.edit_audio.stop_playback();
        self.doc.discard_marks_for(doc::Reader::EditAudio);
        self.doc.selection = s.saved_sel.clone();
        self.cam = s.saved_cam.clone();
        self.scene = rustudio_render::scene::Scene::default();
        host.document_changed(&self.doc);
        if !teleporting {
            rustudio_engine::play_teleport::clear_request();
            rustudio_engine::play_teleport::set_in_flight(false);
        }
        if s.presentation.is_none() && !teleporting {
            self.info(format!(
                "■ Stopped after {:.1} s; the edit DataModel is back as it was",
                s.time
            ));
        }
    }

    /// Server Scripts to start: under Workspace and ServerScriptService
    /// (Legacy / Server RunContext), or within `under` only.
    fn server_scripts(&self, under: Option<Ref>) -> Vec<Ref> {
        let d = &self.doc;
        let root = d.dom.root_ref();
        let roots: Vec<Ref> = match under {
            Some(u) => vec![u],
            None => ["Workspace", "ServerScriptService"]
                .iter()
                .filter_map(|s| d.find_child(root, s))
                .collect(),
        };
        let mut out = Vec::new();
        for s in &roots {
            for i in d.dom.descendants_of(*s) {
                let client = rustudio_render::scene::enum_u32(d.prop(i.referent(), "RunContext")) == Some(2);
                if i.class == "Script" && !client && script_enabled(d, i.referent()) {
                    out.push(i.referent());
                }
            }
        }
        // RunContext Server Scripts run wherever they are (Studio's own
        // StarterPlayer.PlayerModule.InputSetup; a game's in ReplicatedStorage),
        // but not as templates in a container that copies them to players.
        if under.is_none() {
            for name in ["ReplicatedStorage", "ReplicatedFirst", "StarterPlayer", "Lighting", "SoundService", "Teams", "Chat", "TextChatService", "Players"] {
                let Some(svc) = d.find_child(root, name) else { continue };
                for i in d.dom.descendants_of(svc) {
                    let r = i.referent();
                    if i.class != "Script" || rustudio_render::scene::enum_u32(d.prop(r, "RunContext")) != Some(1) || !script_enabled(d, r) {
                        continue;
                    }
                    let mut p = i.parent();
                    let mut template = false;
                    while let Some(a) = d.dom.get_by_ref(p) {
                        if matches!(a.class.as_str(), "StarterPlayerScripts" | "StarterCharacterScripts") {
                            template = true;
                            break;
                        }
                        p = a.parent();
                    }
                    if !template {
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    /// The local player's name: the signed-in account's, as in Studio; offline
    /// Studio's "Player" when signed out.
    pub fn player_name(&self) -> String {
        self.account.names().map_or_else(|| mech::PLAYER_NAME.to_string(), |n| n.0)
    }

    fn player_display_name(&self) -> String {
        self.account.names().map_or_else(|| mech::PLAYER_NAME.to_string(), |n| n.1)
    }

    /// Creates Players.Player1 on the server with Backpack and PlayerGui
    /// filled from StarterPack / StarterGui (both replicate to the client).
    fn create_player(&mut self) {
        let root = self.doc.dom.root_ref();
        let players = match self.doc.find_child(root, "Players") {
            Some(p) => p,
            None => self
                .doc
                .dom
                .insert(root, InstanceBuilder::new("Players").with_name("Players")),
        };
        let player = self.doc.dom.insert(
            players,
            InstanceBuilder::new("Player")
                .with_name(self.player_name())
                .with_property("DisplayName", self.player_display_name())
                .with_property("UserId", self.account.user_id().unwrap_or(1) as i64),
        );
        // Signed in: the account's AccountAge, verified badge and membership,
        // as a signed-in Studio's Play gives its local player (players_chat).
        if let Some(uid) = self.account.user_id() {
            let id = rustudio_engine::runtime::domains::players_chat::account_identity(uid as i64, std::time::Duration::from_secs(5));
            if let (Some(id), Some(inst)) = (id, self.doc.dom.get_by_ref_mut(player)) {
                inst.properties.insert(ustr("AccountAge"), Variant::Int32(id.account_age));
                inst.properties.insert(ustr("HasVerifiedBadge"), Variant::Bool(id.verified));
                // Enum.MembershipType: None 0, Premium 4.
                inst.properties.insert(
                    ustr("MembershipType"),
                    Variant::Enum(rbx_dom_weak::types::Enum::from_u32(if id.premium { 4 } else { 0 })),
                );
                inst.properties.insert(ustr("CharacterAppearanceId"), Variant::Int64(uid as i64));
            }
        }
        // The Player takes StarterPlayer's camera and control settings (Roblox docs, StarterPlayer).
        if let Some(sp) = self.doc.find_child(root, "StarterPlayer") {
            for (from, to) in mech::STARTER_PLAYER_TO_PLAYER {
                if let Some(v) = self.doc.prop(sp, from) {
                    if let Some(inst) = self.doc.dom.get_by_ref_mut(player) {
                        inst.properties.insert(ustr(to), v);
                    }
                }
            }
        }
        let backpack = self.doc.dom.insert(
            player,
            InstanceBuilder::new("Backpack").with_name("Backpack"),
        );
        let gui = self.doc.dom.insert(
            player,
            InstanceBuilder::new("PlayerGui").with_name("PlayerGui"),
        );
        self.doc.dom.insert(player, InstanceBuilder::new("StarterGear"));
        // PlayerGui starts empty: StarterGui is copied in when a character
        // spawns (reset_player_gui), as on Roblox.
        let _ = gui;
        for (from, to) in [(self.doc.find_child(root, "StarterPack"), backpack)] {
            let Some(from) = from else { continue };
            for k in self
                .doc
                .dom
                .get_by_ref(from)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
            {
                let c = self.doc.dom.clone_within(k);
                self.doc.dom.transfer_within(c, to);
            }
        }
        // Teams: join the auto-assignable team with the fewest players.
        let team = self.doc.find_child(root, "Teams").and_then(|t| {
            let d = &self.doc;
            d.dom.get_by_ref(t).and_then(|i| {
                i.children().iter().copied().find(|c| {
                    d.class_of(*c).is_some_and(|k| k == "Team")
                        && !matches!(d.prop(*c, "AutoAssignable"), Some(Variant::Bool(false)))
                })
            })
        });
        if let Some(team) = team {
            let color = self.doc.prop(team, "TeamColor");
            if let Some(inst) = self.doc.dom.get_by_ref_mut(player) {
                inst.properties.insert(ustr("Team"), Variant::Ref(team));
                inst.properties
                    .insert(ustr("Neutral"), Variant::Bool(false));
                if let Some(c) = color {
                    inst.properties.insert(ustr("TeamColor"), c);
                }
            }
        }
        self.doc.mark_tree(player);
        if let Some(p) = self.play.as_mut() {
            p.player = player;
        }
        self.record(RepOp::Parent(player));
    }

    /// Client side of joining: LocalPlayer, PlayerScripts (client-only,
    /// filled from StarterPlayerScripts), then LocalScripts start.
    fn setup_client_player(
        &mut self,
        core: Option<Result<(rustudio_core::core_scripts::Installed, String), String>>,
    ) {
        let Some(s) = self.play.as_ref() else { return };
        let Some(cplayer) = s.map.s2c.get(&s.player).copied() else {
            return;
        };
        let mut local = Vec::new();
        if let Some(cdoc) = self.play.as_mut().and_then(|s| s.other_doc.as_mut()) {
            let root = cdoc.dom.root_ref();
            if let Some(players) = cdoc.find_child(root, "Players") {
                if let Some(i) = cdoc.dom.get_by_ref_mut(players) {
                    i.properties
                        .insert(ustr("LocalPlayer"), Variant::Ref(cplayer));
                }
                cdoc.mark_prop(players, ustr("LocalPlayer"));
            }
            // Every client has a Workspace.CurrentCamera (Roblox makes one if the place has none).
            if let Some(ws) = cdoc.workspace() {
                let has = matches!(cdoc.prop(ws, "CurrentCamera"), Some(Variant::Ref(c)) if cdoc.exists(c));
                if !has {
                    let cam = cdoc.find_child(ws, "Camera").unwrap_or_else(|| {
                        cdoc.dom.insert(
                            ws,
                            InstanceBuilder::new("Camera")
                                .with_name("Camera")
                                .with_property("FieldOfView", mech::FOV_DEG),
                        )
                    });
                    if let Some(i) = cdoc.dom.get_by_ref_mut(ws) {
                        i.properties
                            .insert(ustr("CurrentCamera"), Variant::Ref(cam));
                    }
                    cdoc.mark_tree(cam);
                }
                // A joining client's camera is Custom (the PlayerModule's
                // cameras drive it), whatever the edit camera was saved as.
                if let Some(Variant::Ref(cam)) = cdoc.prop(ws, "CurrentCamera") {
                    if let Some(i) = cdoc.dom.get_by_ref_mut(cam) {
                        i.properties.insert(
                            ustr("CameraType"),
                            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(
                                mech::CAMERA_TYPE_CUSTOM,
                            )),
                        );
                    }
                    cdoc.mark_prop(cam, ustr("CameraType"));
                }
            }
            let scripts = cdoc.dom.insert(
                cplayer,
                InstanceBuilder::new("PlayerScripts").with_name("PlayerScripts"),
            );
            if let Some(sps) = cdoc
                .find_child(root, "StarterPlayer")
                .and_then(|sp| cdoc.find_child(sp, "StarterPlayerScripts"))
            {
                for k in cdoc
                    .dom
                    .get_by_ref(sps)
                    .map(|i| i.children().to_vec())
                    .unwrap_or_default()
                {
                    let c = cdoc.dom.clone_within(k);
                    cdoc.dom.transfer_within(c, scripts);
                }
            }
            cdoc.mark_tree(scripts);
            // ReplicatedFirst's LocalScripts run before anything else on the
            // client (Roblox docs, ReplicatedFirst; Studio Play agrees), so a
            // loading cover they parent is already there when the
            // PlayerScripts / PlayerGui scripts look for it.
            local = client_scripts(
                cdoc,
                &[cdoc.find_child(root, "ReplicatedFirst"), Some(cplayer)],
            );
            // RunContext = Client Scripts run anywhere replicated (skip
            // client-only CoreGui / CorePackages installed from the patch).
            let services: Vec<Ref> = cdoc
                .dom
                .get_by_ref(root)
                .map(|r| {
                    r.children()
                        .iter()
                        .copied()
                        .filter(|&c| {
                            cdoc.dom
                                .get_by_ref(c)
                                .is_some_and(|i| i.class != "CoreGui" && i.class != "CorePackages")
                        })
                        .collect()
                })
                .unwrap_or_default();
            for svc in services {
                for i in cdoc.dom.descendants_of(svc) {
                    if i.class == "Script"
                        && rustudio_render::scene::enum_u32(cdoc.prop(i.referent(), "RunContext")) == Some(2)
                        && script_enabled(cdoc, i.referent())
                        && !local.contains(&i.referent())
                    {
                        local.push(i.referent());
                    }
                }
            }
        }
        self.sync_view_size();
        match core {
            Some(Ok((installed, note))) => {
                self.info(note);
                // Attach server handlers for RobloxReplicatedStorage version queries.
                let _ = self.run_luau_as(
                    Side::Server,
                    r#"
                    local rrs = game:GetService("RobloxReplicatedStorage")
                    local gv = rrs:FindFirstChild("GetServerVersion")
                    if gv then gv.OnServerInvoke = function() return "0.747.0.7470000" end end
                    local gc = rrs:FindFirstChild("GetServerChannel")
                    if gc then gc.OnServerInvoke = function() return "LIVE" end end
                    "#,
                    rustudio_engine::runtime::security::CORE,
                );
                let started = self.with_side(Side::Client, |rt, _, _| rt.start_core_scripts(installed));
                match started {
                    Some(Ok(())) => {
                        if let Some(s) = self.play.as_mut() {
                            s.game.core_scripts_active = true;
                        }
                    }
                    Some(Err(e)) => self.error(format!("CoreScripts: {e}")),
                    None => {}
                }
            }
            Some(Err(e)) => self.error(e),
            None => {}
        }
        self.with_side(Side::Client, |rt, _, _| {
            for r in local {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
    }

    /// Frees what each side's scripts can no longer reach (Runtime::collect),
    /// twice a second, and drops the freed instances from the replication
    /// pairs. Roblox's Lua GC does the same; without it every tracer, hole
    /// and effect part a game ever made stayed in memory for the session.
    fn collect_garbage(&mut self, dt: f32, host: &dyn GameHost) {
        let Some(s) = self.play.as_mut() else { return };
        s.memstats_frames += 1;
        s.memstats_worst = s.memstats_worst.max(dt);
        let pass = s.time - s.gc_at >= rustudio_core::mech::GC_INTERVAL;
        {
            let _sc = rustudio_core::bench::scope("p9_gc_step");
            for side in [Side::Server, Side::Client] {
                self.with_side(side, |rt, _, _| rt.gc_step(dt as f64));
            }
        }
        let Some(s) = self.play.as_mut() else { return };
        if !pass {
            return;
        }
        s.gc_at = s.time;
        let fallen = std::mem::take(&mut s.engine_orphans);
        let player = s.player;
        let model = s.character.as_ref().map(|c| c.model).unwrap_or_else(Ref::none);
        let due = rustudio_engine::runtime::memstats_on() && s.time - s.memstats_at >= 5.0;
        let _sc = rustudio_core::bench::scope("p9_collect");
        let (server_freed, srv) = self
            .with_side(Side::Server, |rt, _, map| {
                for r in &fallen {
                    rt.note_orphan(*r);
                }
                // (Plugin roots and the objects plugins made stay: src/plugin_host.)
                let plugin_pins = host.plugin_pins(&rt.lua);
                let freed = rt.collect(
                    &|r| r == player || r == model || map.s2c.contains_key(&r) || plugin_pins.contains(&r),
                    rustudio_core::mech::GC_BUDGET,
                );
                (freed.len(), if due { rt.mem_stats() } else { String::new() })
            })
            .unwrap_or_default();
        let (client_freed, cli) = self
            .with_side(Side::Client, |rt, _, map| {
                let plugin_pins = host.plugin_pins(&rt.lua);
                let freed = rt.collect(&|r| map.c2s.contains_key(&r) || plugin_pins.contains(&r), rustudio_core::mech::GC_BUDGET);
                map.forget_collected_client(&freed);
                (freed.len(), if due { rt.mem_stats() } else { String::new() })
            })
            .unwrap_or_default();
        let Some(s) = self.play.as_mut() else { return };
        if !due {
            return;
        }
        let span = (s.time - s.memstats_at).max(1e-3);
        let fps = s.memstats_frames as f64 / span;
        let worst = s.memstats_worst * 1000.0;
        s.memstats_at = s.time;
        s.memstats_frames = 0;
        s.memstats_worst = 0.0;
        let rss = rustudio_core::platform::resident_bytes().map(|b| (b / 1024).to_string()).unwrap_or_default();
        let time = s.time;
        let (rs2c, rc2s) = (s.map.s2c.len(), s.map.c2s.len());
        eprintln!(
            "MEMSTATS t={time:.0} rss_kb={rss} fps={fps:.1} worst_ms={worst:.0} gc_freed={server_freed}/{client_freed} rep={rs2c}/{rc2s} | server {srv} | client {cli}"
        );
    }

    /// Streams queued server changes to the client, within the frame's
    /// replication budget (`replicate::apply_budgeted`).
    fn replicate(&mut self) {
        self.replicate_with(true);
    }

    /// Applies every waiting server change now: engine steps that read the
    /// client copy right after (player join, character spawn).
    fn replicate_now(&mut self) {
        self.replicate_with(false);
    }

    fn replicate_with(&mut self, budgeted: bool) {
        let (mut ops, requests) = match self.server_world() {
            Some(w) => (std::mem::take(&mut w.rep), std::mem::take(&mut w.stream_requests)),
            None => return,
        };
        let Some(play) = self.play.as_ref().filter(|p| p.client.is_some()) else { return };
        let streaming = play.map.streaming();
        if !streaming && ops.is_empty() && play.map.backlog() == 0 {
            return;
        }
        let player = play.player;
        self.with_side(Side::Client, |rt, server, map| {
            if let Some(server) = server {
                if streaming {
                    replicate::update_streaming(server, rt, map, player, &mut ops, requests);
                }
                if budgeted {
                    replicate::apply_budgeted(server, rt, map, ops);
                } else {
                    replicate::apply(server, rt, map, ops);
                }
                if streaming {
                    replicate::finish_streaming(server, rt, map);
                }
            }
            let _sc = rustudio_core::bench::scope("rep_dispatch");
            rt.dispatch();
        });
    }

    /// Legacy button input can reach server Scripts in the player's
    /// replicated PlayerGui during local Play.
    fn fire_server_gui_buttons(&mut self, buttons: Vec<GuiEvent>) {
        if buttons.is_empty() {
            return;
        }
        let Some(gui) = self.play.as_ref()
            .and_then(|s| self.doc.find_child(s.player, "PlayerGui"))
        else {
            return;
        };
        self.with_side(Side::Server, |rt, _, map| {
            for event in buttons {
                let GuiEvent::Fire(client, name @ ("MouseButton1Down" | "MouseButton1Click"), args) = event else {
                    continue;
                };
                let Some(server) = map.c2s.get(&client).copied() else {
                    continue;
                };
                let owned_button = rt.world_mut().is_some_and(|w| {
                    w.d().class_of(server).is_some_and(|class| doc::is_a(&class, "GuiButton"))
                        && w.d().is_ancestor(gui, server)
                });
                if owned_button {
                    let args = args.into_iter().filter_map(|arg| match arg {
                        GuiArg::Num(n) => Some(Value::Number(n)),
                        _ => None,
                    }).collect();
                    rt.fire(server, name, args);
                }
            }
            rt.dispatch();
        });
    }

    /// Carries remote calls across: client → server first (so replies can
    /// come back in the same frame), then server → client.
    fn deliver_remotes(&mut self) {
        let Some(player) = self.play.as_ref().map(|p| p.player) else {
            return;
        };
        for _ in 0..2 {
            let to_server = self
                .client_world()
                .map(|w| std::mem::take(&mut w.outbox))
                .unwrap_or_default();
            rustudio_engine::runtime::domains::services::perf::remotes(false, &to_server);
            if std::env::var_os("RUSTUDIO_GUI_DEBUG").is_some() {
                for m in &to_server {
                    let name = self
                        .play_other_doc()
                        .and_then(|d| d.dom.get_by_ref(m.inst).map(|i| i.name.clone()))
                        .unwrap_or_default();
                    let mapped = self
                        .play
                        .as_ref()
                        .is_some_and(|s| s.map.c2s.contains_key(&m.inst));
                    self.info(format!(
                        "[remote] client → server {name} ({:?}) mapped={mapped} args={:?}",
                        m.kind, m.args
                    ));
                }
            }
            if !to_server.is_empty() {
                self.with_side(Side::Server, |rt, _, map| {
                    let m: &RepMap = map;
                    let c2s = |c: Ref| m.c2s.get(&c).copied();
                    for msg in to_server {
                        if let RemoteKind::Reply(id, ok) = msg.kind {
                            rt.finish_invoke(id, ok, &msg.args, &c2s);
                            continue;
                        }
                        let Some(remote) = m.c2s.get(&msg.inst).copied() else {
                            continue;
                        };
                        match msg.kind {
                            RemoteKind::Event if rustudio_engine::runtime::domains::web_social::is_engine_msg(&msg.args) => {
                                rustudio_engine::runtime::domains::web_social::engine_message(rt,remote,player,&msg.args,&c2s)
                            }
                            RemoteKind::Event if rustudio_engine::runtime::domains::gui_input::is_engine_msg(&msg.args) => {
                                rustudio_engine::runtime::domains::gui_input::engine_message(rt, remote, player, &msg.args, &c2s)
                            }
                            RemoteKind::Event => rt.deliver_event(
                                remote,
                                "OnServerEvent",
                                Some(player),
                                &msg.args,
                                &c2s,
                            ),
                            RemoteKind::Invoke(id) => {
                                rt.serve_invoke(remote, player, id, &msg.args, &c2s)
                            }
                            RemoteKind::Reply(..) => {}
                        }
                    }
                    rt.dispatch();
                });
            }
            self.replicate();
            let to_client = self
                .server_world()
                .map(|w| std::mem::take(&mut w.outbox))
                .unwrap_or_default();
            // Behind the replication sent before them (Roblox keeps one order).
            let to_client = match self.play.as_mut() {
                Some(s) => s.map.gate_remotes(to_client),
                None => to_client,
            };
            rustudio_engine::runtime::domains::services::perf::remotes(true, &to_client);
            if to_client.is_empty() {
                if self.client_world().is_none_or(|w| w.outbox.is_empty()) {
                    break;
                }
                continue;
            }
            self.with_side(Side::Client, |rt, _, map| {
                let m: &RepMap = map;
                let s2c = |s: Ref| m.s2c.get(&s).copied();
                for msg in to_client {
                    match msg.kind {
                        RemoteKind::Event if rustudio_engine::runtime::domains::web_social::is_engine_msg(&msg.args) => {
                            if let Some(remote)=m.s2c.get(&msg.inst).copied() {
                                rustudio_engine::runtime::domains::web_social::engine_message(rt,remote,Ref::none(),&msg.args,&s2c);
                            }
                        }
                        RemoteKind::Event => {
                            if let Some(remote) = m.s2c.get(&msg.inst).copied() {
                                rt.deliver_event(remote, "OnClientEvent", None, &msg.args, &s2c);
                            }
                        }
                        RemoteKind::Reply(id, ok) => rt.finish_invoke(id, ok, &msg.args, &s2c),
                        RemoteKind::Invoke(id) => {
                            if let Some(remote) = m.s2c.get(&msg.inst).copied() {
                                rt.serve_client_invoke(remote, id, &msg.args, &s2c);
                            }
                        }
                    }
                }
                rt.dispatch();
            });
        }
    }

    /// Where characters appear: the capsule centre above the first enabled
    /// SpawnLocation, and the yaw they face there: the spawn's own facing,
    /// as Roblox keeps the SpawnLocation's rotation (about Y) for the root.
    fn character_spawn_point(&self) -> (Vec3, f32) {
        // Test Here: where the camera was when the test started.
        if let Some(p) = self.play_here {
            return (p, 0.0);
        }
        let d = &self.doc;
        // A team player spawns on a Neutral spawn or one in the team's colour.
        let player = self.play.as_ref().map(|p| p.player).filter(|p| p.is_some());
        let team_color = player
            .filter(|p| !matches!(d.prop(*p, "Neutral"), Some(Variant::Bool(true))))
            .and_then(|p| d.prop(p, "TeamColor"));
        let fits = |s: Ref| match &team_color {
            None => true,
            Some(c) => {
                !matches!(d.prop(s, "Neutral"), Some(Variant::Bool(false)))
                    || d.prop(s, "TeamColor").as_ref() == Some(c)
            }
        };
        if let Some(ws) = d.workspace() {
            for i in d.dom.descendants_of(ws) {
                if i.class == "SpawnLocation"
                    && !matches!(d.prop(i.referent(), "Enabled"), Some(Variant::Bool(false)))
                    && fits(i.referent())
                {
                    if let Some(xf) = rustudio_render::scene::xform_of(d, i.referent()) {
                        let size = rustudio_render::scene::vec3_of(d.prop(i.referent(), "Size"))
                            .unwrap_or(Vec3::ONE);
                        let top = xf.pos + Vec3::Y * (size.y * 0.5);
                        // rot_y(yaw) * -Z = (-sin yaw, 0, -cos yaw).
                        let look = xf.rot * Vec3::NEG_Z;
                        let yaw = if look.x.abs() + look.z.abs() > 1e-4 {
                            (-look.x).atan2(-look.z)
                        } else {
                            0.0
                        };
                        return (
                            top + Vec3::Y
                                * (mech::CHARACTER_HALF_HEIGHT + mech::CHARACTER_RADIUS + 0.5),
                            yaw,
                        );
                    }
                }
            }
        }
        (Vec3::new(0.0, mech::NO_SPAWN_HEIGHT, 0.0), 0.0)
    }

    /// The Humanoid's settings come from StarterPlayer (Character* properties).
    fn humanoid_builder(&self) -> InstanceBuilder {
        let d = &self.doc;
        let sp = d.find_child(d.dom.root_ref(), "StarterPlayer");
        let f =
            |name: &str, default: f32| sp.and_then(|s| f32_of(d.prop(s, name))).unwrap_or(default);
        let use_power = sp
            .map(|s| {
                matches!(
                    d.prop(s, "CharacterUseJumpPower"),
                    Some(Variant::Bool(true))
                )
            })
            .unwrap_or(false);
        InstanceBuilder::new("Humanoid")
            .with_name("Humanoid")
            .with_property("Health", 100.0f32)
            .with_property("MaxHealth", 100.0f32)
            .with_property("WalkSpeed", f("CharacterWalkSpeed", mech::WALK_SPEED))
            .with_property("JumpHeight", f("CharacterJumpHeight", mech::JUMP_HEIGHT))
            .with_property("JumpPower", f("CharacterJumpPower", mech::JUMP_POWER))
            .with_property("UseJumpPower", use_power)
            .with_property(
                "MaxSlopeAngle",
                f("CharacterMaxSlopeAngle", mech::MAX_SLOPE_DEG),
            )
            // Spawned characters carry an Animator (Animate finds it with
            // FindFirstChildOfClass).
            .with_child(InstanceBuilder::new("Animator").with_name("Animator"))
    }

    fn spawn_character(&mut self) {
        let Some(player) = self.play.as_ref().map(|p| p.player).filter(|p| p.is_some()) else {
            self.warn("No character: the play session has no Player (Run has none; Play makes one)");
            return;
        };
        let Some(ws) = self.doc.workspace() else {
            self.warn("No character: the place has no Workspace to spawn it in");
            return;
        };
        let (mut rig_type, colors, scales) = self.character_look();
        // StarterPlayer.StarterCharacter: a Model with a Humanoid is cloned
        // as every player's character, without their avatar (as on Roblox).
        let starter = self.starter_character();
        if let Some((_, r)) = starter {
            rig_type = r;
        }
        let (_, source) = self.rig_choice();
        let rig_name = |r: RigType| if r == RigType::R15 { "R15" } else { "R6" };
        let mut why = format!(
            "Character: {} (from the {})",
            rig_name(rig_type),
            source.label()
        );
        if let (RigSource::Picker, Some(p)) = (source, self.place_rig()) {
            if p != rig_type {
                why.push_str(&format!("; the place asks for {}", rig_name(p)));
            }
        }
        if starter.is_some() {
            why = format!("Character: StarterPlayer.StarterCharacter ({})", rig_name(rig_type));
        }
        self.info(why);
        let humanoid = self.humanoid_builder();
        let custom_starter = starter.is_some();
        let name = self.player_name();
        let m = match starter {
            Some((sc, _)) => {
                let m = self.doc.dom.clone_within(sc);
                self.doc.dom.transfer_within(m, ws);
                if let Some(i) = self.doc.dom.get_by_ref_mut(m) {
                    i.name = name.clone();
                }
                // Animate finds the Animator with FindFirstChildOfClass.
                if let Some(h) = self.doc.dom.descendants_of(m).find(|i| i.class == "Humanoid").map(|i| i.referent()) {
                    let has = self.doc.dom.get_by_ref(h).is_some_and(|i| {
                        i.children().iter().any(|c| self.doc.dom.get_by_ref(*c).is_some_and(|c| c.class == "Animator"))
                    });
                    if !has {
                        self.doc.dom.insert(h, InstanceBuilder::new("Animator").with_name("Animator"));
                    }
                }
                m
            }
            None => {
                let m = rig::insert_character(
                    &mut self.doc.dom,
                    ws,
                    &name,
                    rig_type,
                    &colors,
                    &scales,
                    humanoid,
                );
                // What the body is made of, for a report of a character that
                // can't be seen (the Studio files' meshes or the stand-in blocks).
                let (parts, meshes) = self
                    .doc
                    .dom
                    .get_by_ref(m)
                    .map(|i| {
                        i.children().iter().filter_map(|c| self.doc.dom.get_by_ref(*c)).fold((0usize, 0usize), |(p, mm), ch| {
                            (p + usize::from(doc::is_a(&ch.class, "BasePart")), mm + usize::from(&*ch.class == "MeshPart"))
                        })
                    })
                    .unwrap_or((0, 0));
                self.info(format!("Character body: {parts} parts, {meshes} of them MeshParts"));
                // Everything a player expects of the character comes from
                // Studio's content: the body with its face, Animate, and the
                // PlayerModule whose CameraModule follows it and whose
                // ControlModule moves it. Without, the game looks broken in
                // three ways at once; say which one it is.
                if meshes == 0 && rustudio_core::default_scripts::studio_dir().is_none() {
                    self.warn(concat!(
                        "Studio's content isn't installed yet, so this character is rustudio's stand-in: ",
                        "no face, no animations, and no camera or movement scripts follow it. ",
                        "The download that starts with rustudio ends with a \"Studio content … installed\" line above ",
                        "(or says what failed); stop and press Play again after it.",
                    ));
                }
                self.dress_character(m);
                // The avatar's equipped emotes (Humanoid:PlayEmote, /e, the
                // emote wheel read them from here).
                if let Some(look) = self.account.avatar().filter(|l| !l.emotes.is_empty()).cloned() {
                    if let Some(h) = self.doc.dom.descendants_of(m).find(|i| i.class == "Humanoid").map(|i| i.referent()) {
                        self.doc.dom.insert(h, rustudio_engine::avatar::emote_description(&look));
                    }
                }
                m
            }
        };
        // Studio's avatar joint upgrade (StarterPlayer.AvatarJointUpgrade):
        // an R15 default body's rig Motor6Ds become AnimationConstraints + ball
        // sockets; custom StarterCharacter models keep their authored Motor6Ds.
        if !custom_starter && rig_type == RigType::R15 && rig::joint_upgrade(&self.doc) {
            rig::upgrade_rig(&mut self.doc.dom, m);
        }
        // StarterCharacterScripts go into the character.
        let root_ref = self.doc.dom.root_ref();
        if let Some(scs) = self
            .doc
            .find_child(root_ref, "StarterPlayer")
            .and_then(|sp| self.doc.find_child(sp, "StarterCharacterScripts"))
        {
            for k in self
                .doc
                .dom
                .get_by_ref(scs)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
            {
                let c = self.doc.dom.clone_within(k);
                self.doc.dom.transfer_within(c, m);
            }
        }
        // Roblox's Health and Animate, unless the place brought its own.
        rustudio_core::default_scripts::insert_character_scripts(&mut self.doc, m, rig_type);
        // Animations (R15 only, as on Roblox): the avatar's packages unless
        // the game picks Standard, then the game's per-slot overrides and
        // custom clips (game_avatar.rs).
        if rig_type == RigType::R15 && starter.is_none() {
            let rules = rustudio_engine::game_avatar::rules(&self.doc);
            let client = self.account.shared_client();
            let mut packs = if rules.player_packs {
                self.account.avatar().map(|l| l.animations.clone()).unwrap_or_default()
            } else {
                Vec::new()
            };
            for (kind, id) in &rules.overrides {
                packs.retain(|p| &p.0 != kind);
                if *id > 0 {
                    match rustudio_engine::game_avatar::package(client.as_ref(), *id) {
                        Some(b) => packs.push((kind.clone(), *id, b)),
                        None => self.warn(format!("Game animation override {kind} {id} could not be downloaded")),
                    }
                }
            }
            let slots = rustudio_engine::avatar::put_on_animation_packs(&mut self.doc, m, &packs);
            if !slots.is_empty() {
                let whose = if rules.overrides.is_empty() { "Avatar" } else { "Avatar / game" };
                self.info(format!("{whose} animations: {}", slots.join(", ")));
            }
            let clips = rustudio_engine::avatar::put_on_custom_clips(&mut self.doc, m, &rules.clips);
            if !clips.is_empty() {
                self.info(format!("Game animation clips: {}", clips.join(", ")));
            }
        }
        if let Some(inst) = self.doc.dom.get_by_ref_mut(player) {
            inst.properties.insert(ustr("Character"), Variant::Ref(m));
        }
        self.doc.mark_prop(player, ustr("Character"));
        self.doc.mark_tree(m);
        let humanoid = self
            .doc
            .dom
            .get_by_ref(m)
            .and_then(|i| {
                i.children()
                    .iter()
                    .copied()
                    .find(|c| self.doc.class_of(*c).is_some_and(|k| k == "Humanoid"))
            })
            .unwrap_or_else(Ref::none);
        // Which parts collide, as on Studio's spawned character (round 22,
        // movement_probe: R15 HumanoidRootPart, UpperTorso, LowerTorso, Head;
        // R6 Torso, Head; limbs and R6's root don't). rustudio's controller
        // moves the body itself; scripts read these.
        let body: Vec<(Ref, bool)> = self
            .doc
            .dom
            .get_by_ref(m)
            .map(|i| i.children().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let i = self.doc.dom.get_by_ref(c)?;
                doc::is_a(&i.class, "BasePart").then(|| (c, rig::studio_collides(&i.name, rig_type)))
            })
            .collect();
        for (c, on) in body {
            if let Some(i) = self.doc.dom.get_by_ref_mut(c) {
                i.properties.insert(ustr("CanCollide"), Variant::Bool(on));
            }
        }
        let rig = Rig::read(&self.doc, m);
        let (start, start_yaw) = self.character_spawn_point();
        let root_lift =
            rig::root_height(&self.doc, m)
                - (mech::CHARACTER_HALF_HEIGHT + mech::CHARACTER_RADIUS + mech::CHARACTER_HOVER);
        let ch = Character {
            model: m,
            accessories_seen: usize::MAX,
            floor_material: 0,
            humanoid,
            rig,
            rig_type,
            body: CharBody {
                pos: start,
                vel_y: 0.0,
                grounded: false,
                // Not just off a ledge: no coyote jump mid-air at spawn.
                air_time: f32::MAX,
                ..Default::default()
            },
            root_lift,
            yaw: start_yaw,
            velocity: Vec3::ZERO,
            dead: false,
            touching: HashSet::new(),
            touch_pairs: Default::default(),
            seat_seen: HashSet::new(),
            state: State::Freefall,
            landed_for: 0.0,
            last_speed: -1.0,
            last_root: None,
            seat: None,
            client_sit: false,
            shape: hum_shape(&self.doc, m, root_lift),
            hum_mode_left: false,
            pitch: 0.0,
            wall: None,
            seat_cooldown: 0.0,
            forced: None,
            forced_for: 0.0,
            last_move_speed: -1.0,
            ladder: None,
            ladder_wait: 0,
            dynamic: Default::default(),
            carried: HashSet::new(),
        };
        let mut ch = ch;
        let pose = character_pose(&ch, &HashMap::new());
        // Where it stands now: a script moving it before the first frame
        // (PivotTo at spawn) is a teleport too.
        ch.last_root = pose.iter().find(|(r, _)| *r == ch.rig.root).map(|(_, x)| *x);
        rig::write_pose(&mut self.doc, &pose, None);
        let parts: Vec<Ref> = self
            .doc
            .dom
            .descendants_of(m)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .map(|i| i.referent())
            .collect();
        if let Some(p) = self.play.as_mut() {
            p.physics.skip.extend(parts.iter().copied());
            p.physics.refresh(parts);
            p.character = Some(ch);
            p.respawn_at = None;
        }

        // Apply the player's StarterGear on each spawn. The first spawn
        // keeps the StarterPack copies that were made at player join.
        let first = self.play.as_ref().is_some_and(|s| s.spawns == 0);
        if let Some(s) = self.play.as_mut() {
            s.initial_spawn_frames = None;
            s.spawns += 1;
        }
        let tools = self.refill_backpack(player, first);

        // Publish the rig and Character ref before spawn callbacks can add children
        // to the rig. Such children follow the rig into the client replica.
        self.record(RepOp::Parent(m));
        self.record(RepOp::Prop(player, "Character".into()));
        let server_scripts = self.server_scripts(Some(m));
        let spawned = self.doc.dom.descendants_of(m).map(|i| i.referent()).collect();
        self.with_side(Side::Server, |rt, _, _| {
            rt.fire(player, "CharacterAdded", vec![rt.inst(m)]);
            rt.notify_prop(player, "Character");
            rt.notify_parent(m, Ref::none(), ws, spawned);
            rt.spawn_global("__rs_character_appearance_loaded", vec![rt.inst(player), rt.inst(m)]);
            rt.dispatch();
        });
        self.with_side(Side::Server, |rt, _, _| {
            for r in server_scripts {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
        // Replicate the new character, then the client's CharacterAdded and LocalScripts.
        // (Its LocalScripts wait for CharacterAdded and the GUI's: `held` below.)
        self.with_side(Side::Client, |rt, _, _| {
            if let Some(mut w) = rt.world_mut() {
                w.hold_character_scripts = true;
            }
        });
        self.replicate_now();
        let held = self
            .with_side(Side::Client, |rt, _, _| {
                rt.world_mut()
                    .map(|mut w| {
                        w.hold_character_scripts = false;
                        std::mem::take(&mut w.held)
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let (cplayer, cmodel) = match self.play.as_ref() {
            Some(s) => (s.map.s2c.get(&player).copied(), s.map.s2c.get(&m).copied()),
            None => (None, None),
        };
        let mut character_scripts = Vec::new();
        if let (Some(cp), Some(cm)) = (cplayer, cmodel) {
            let local = self
                .play
                .as_ref()
                .and_then(|s| {
                    let cd = s.other_doc.as_ref()?;
                    let mut under = vec![Some(cm)];
                    under.extend(tools.iter().map(|t| s.map.s2c.get(t).copied()));
                    Some(client_scripts(cd, &under))
                })
                .unwrap_or_default();
            // The client's camera follows the new character's Humanoid.
            let chum = self
                .play
                .as_ref()
                .and_then(|s| s.map.s2c.get(&humanoid).copied());
            let ccam = self
                .play
                .as_ref()
                .and_then(|s| s.other_doc.as_ref())
                .and_then(
                    |cd| match cd.workspace().and_then(|ws| cd.prop(ws, "CurrentCamera")) {
                        Some(Variant::Ref(c)) if cd.exists(c) => Some(c),
                        _ => None,
                    },
                );
            self.with_side(Side::Client, |rt, _, _| {
                if let (Some(cam), Some(h)) = (ccam, chum) {
                    if first {
                        // In Studio Play, the client's first character load (when
                        // CameraSubject was nil, e.g. CharacterAutoLoads = false)
                        // resets CurrentCamera.CameraType back to Custom so the
                        // default camera controller follows the new character even
                        // if a pre-spawn loading script set Scriptable. Subsequent
                        // respawns preserve CameraType.
                        rt.apply_prop(
                            cam,
                            "CameraType",
                            Variant::Enum(rbx_dom_weak::types::Enum::from_u32(
                                mech::CAMERA_TYPE_CUSTOM,
                            )),
                        );
                    }
                    rt.set(cam, "CameraSubject", rt.inst(h));
                }
                rt.fire(cp, "CharacterAdded", vec![rt.inst(cm)]);
                rt.dispatch();
            });
            character_scripts = held.into_iter().chain(local).collect();
        }
        // StarterGui into PlayerGui after CharacterAdded on both sides (Studio:
        // StarterGui's LocalScripts start after the client's CharacterAdded
        // and the deferred work it queued).
        let gui_copies = self.reset_player_gui(player, first);
        self.replicate_now();
        if let Some(cp) = cplayer {
            self.start_client_gui(cp, &gui_copies, first);
        }
        // Then the new character's (and its tools') LocalScripts: Studio Play's
        // client runs Added → the new GUI LocalScript → the new character
        // LocalScript (measured in Play).
        if !character_scripts.is_empty() {
            self.with_side(Side::Client, |rt, _, _| {
                for r in character_scripts {
                    rt.start_collected(r);
                }
                rt.dispatch();
            });
        }
    }

    /// A character spawn's PlayerGui reset on the server (measured in Studio
    /// Play): on a respawn everything there goes but the LayerCollectors with
    /// ResetOnSpawn false (a Folder too, made by a script or copied); the
    /// first spawn keeps what scripts made before it. Then StarterGui's
    /// children are copied in (a ResetOnSpawn false gui only the first time).
    /// Server Scripts in the copies start now; returns the copies for the
    /// client, whose LocalScripts start after its CharacterAdded.
    fn reset_player_gui(&mut self, player: Ref, first: bool) -> Vec<Ref> {
        let root = self.doc.dom.root_ref();
        let (Some(pg), Some(sg)) = (
            self.doc.find_child(player, "PlayerGui"),
            self.doc.find_child(root, "StarterGui"),
        ) else {
            return Vec::new();
        };
        let old: Vec<Ref> = self
            .doc
            .dom
            .get_by_ref(pg)
            .map(|i| i.children().to_vec())
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !first && !kept_on_spawn(&self.doc, *c))
            .collect();
        self.with_side(Side::Server, |rt, _, _| {
            for c in &old {
                rt.apply_destroy(*c);
            }
        });
        let mut copies = Vec::new();
        for k in self.doc.dom.get_by_ref(sg).map(|i| i.children().to_vec()).unwrap_or_default() {
            if kept_on_spawn(&self.doc, k) && !first {
                continue;
            }
            let c = self.doc.dom.clone_within(k);
            self.doc.dom.transfer_within(c, pg);
            self.doc.mark_tree(c);
            self.record(RepOp::Parent(c));
            copies.push(c);
        }
        let scripts: Vec<Ref> = copies.iter().flat_map(|c| self.server_scripts(Some(*c))).collect();
        self.with_side(Side::Server, |rt, _, _| {
            for r in scripts {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
        copies
    }

    /// The client's side of a PlayerGui reset, after its CharacterAdded:
    /// what it made there goes too unless kept (the server's went by replication),
    /// then the replicated StarterGui copies' LocalScripts start.
    fn start_client_gui(&mut self, cplayer: Ref, copies: &[Ref], first: bool) {
        let Some((gone, local)) = self.play.as_ref().and_then(|s| {
            let cd = s.other_doc.as_ref()?;
            let pg = cd.find_child(cplayer, "PlayerGui")?;
            let ccopies: Vec<Ref> = copies.iter().filter_map(|c| s.map.s2c.get(c).copied()).collect();
            let gone: Vec<Ref> = cd
                .dom
                .get_by_ref(pg)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| !first && !ccopies.contains(c) && !kept_on_spawn(cd, *c))
                .collect();
            let under: Vec<Option<Ref>> = ccopies.into_iter().map(Some).collect();
            Some((gone, client_scripts(cd, &under)))
        }) else {
            return;
        };
        self.with_side(Side::Client, |rt, _, _| {
            for c in gone {
                rt.apply_destroy(c);
            }
            for r in local {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
    }

    /// StarterPlayer.StarterCharacter (a Model holding a Humanoid) and its
    /// rig type (UpperTorso = R15).
    fn starter_character(&self) -> Option<(Ref, RigType)> {
        let root = self.doc.dom.root_ref();
        let sp = self.doc.find_child(root, "StarterPlayer")?;
        let sc = self.doc.dom.get_by_ref(sp)?.children().iter().copied().find(|c| {
            self.doc.dom.get_by_ref(*c).is_some_and(|i| i.name == "StarterCharacter" && i.class == "Model")
        })?;
        self.doc.dom.descendants_of(sc).find(|i| i.class == "Humanoid")?;
        let r15 = self.doc.find_named(sc, "UpperTorso").is_some();
        Some((sc, if r15 { RigType::R15 } else { RigType::R6 }))
    }

    /// The place's avatar setting (StarterPlayer.GameSettingsAvatar: R6 /
    /// R15; PlayerChoice leaves it to the avatar).
    pub fn place_rig(&self) -> Option<RigType> {
        self.doc
            .explicit_avatar_type()
            .and_then(|v| match v {
                0 => Some(RigType::R6),
                1 => Some(RigType::R15),
                _ => None,
            })
    }

    /// Which rig the next character gets and which input decided it: the
    /// picker when it names one (an explicit choice wins), else the place's
    /// avatar setting, else the signed-in avatar, else the default (R15).
    pub fn rig_choice(&self) -> (RigType, RigSource) {
        if let Some(r) = self.play_rig {
            return (r, RigSource::Picker);
        }
        if let Some(r) = self.place_rig() {
            return (r, RigSource::Place);
        }
        if let Some(a) = self.account.avatar() {
            return (
                if a.r15 { RigType::R15 } else { RigType::R6 },
                RigSource::Avatar,
            );
        }
        let def = if mech::DEFAULT_RIG_R15 {
            RigType::R15
        } else {
            RigType::R6
        };
        (def, RigSource::Default)
    }

    /// Rig type, colours and scales for the next character: the rig from
    /// [`Self::rig_choice`]; colours and (R15) scales from the signed-in
    /// avatar when it has been fetched, else the default body's.
    fn character_look(&self) -> (RigType, rig::BodyColors, rig::Scales) {
        let (rig, _) = self.rig_choice();
        match self.account.avatar() {
            Some(a) => (
                rig,
                a.colors,
                if rig == RigType::R15 {
                    a.scales
                } else {
                    rig::Scales::default()
                },
            ),
            None => (rig, rig::BodyColors::default(), rig::Scales::default()),
        }
    }

    /// A property of the player's Humanoid as its owner (the client) has it.
    fn owned_humanoid_prop(&self, h: Ref, name: &str) -> Option<Variant> {
        let client = self
            .play
            .as_ref()
            .and_then(|s| s.other_doc.as_ref().zip(s.map.s2c.get(&h).copied()));
        match client {
            Some((cd, ch)) => cd.prop(ch, name),
            None => self.doc.prop(h, name),
        }
    }

    fn owned_humanoid_f32(&self, h: Ref, name: &str, default: f32) -> f32 {
        f32_of(self.owned_humanoid_prop(h, name)).unwrap_or(default)
    }

    fn humanoid_f32(&self, h: Ref, name: &str, default: f32) -> f32 {
        f32_of(self.doc.prop(h, name)).unwrap_or(default)
    }

    /// Death: joints break (BreakJointsOnDeath), the parts fall loose, and
    /// the character respawns after Players.RespawnTime.
    fn kill_character(&mut self) {
        let auto = self.character_auto_loads();
        let (model, parts) = {
            let Some(s) = self.play.as_mut() else { return };
            let Some(ch) = s.character.as_mut() else {
                return;
            };
            if ch.dead {
                return;
            }
            ch.dead = true;
            ch.state = State::Dead;
            s.respawn_at = auto.then(|| s.time + mech::RESPAWN_TIME as f64);
            let parts: Vec<Ref> = self
                .doc
                .dom
                .descendants_of(ch.model)
                .filter(|i| doc::is_a(&i.class, "BasePart"))
                .map(|i| i.referent())
                .collect();
            (ch.model, parts)
        };
        let motors: Vec<Ref> = self
            .doc
            .dom
            .descendants_of(model)
            .filter(|i| rustudio_engine::rig::is_animated_joint(&i.class))
            .map(|i| i.referent())
            .collect();
        for m in &motors {
            self.doc.dom.transfer_within(*m, Ref::none());
            self.doc.mark_tree(*m);
        }
        // An upgraded R15 body's ball sockets are disabled with it (Studio).
        for b in rustudio_engine::rig::ball_sockets(&self.doc, model) {
            if let Some(inst) = self.doc.dom.get_by_ref_mut(b) {
                inst.properties.insert(ustr("Enabled"), Variant::Bool(false));
            }
            self.doc.mark_prop(b, ustr("Enabled"));
            self.record(RepOp::Prop(b, "Enabled".into()));
        }
        for r in &parts {
            if let Some(inst) = self.doc.dom.get_by_ref_mut(*r) {
                inst.properties
                    .insert(ustr("CanCollide"), Variant::Bool(true));
            }
            self.doc.mark_prop(*r, ustr("CanCollide"));
            // (The client reads it too: Studio's probe T14 sees every limb
            // CanCollide true in the frame the joints go.)
            self.record(RepOp::Prop(*r, "CanCollide".into()));
        }
        if let Some(s) = self.play.as_mut() {
            // The character's assembly splits (BreakJointsOnDeath): each part
            // its own body, keeping the velocity the assembly had.
            {
                let v = s.physics.humanoid_root(model).map(|(_, v, _)| v);
                s.physics.remove_humanoid(model);
                if let Some(v) = v {
                    for r in &parts {
                        s.physics.set_velocity(*r, v);
                    }
                }
            }
            s.physics.skip.retain(|x| !parts.contains(x));
            s.physics.dead_parts.extend(parts.iter().copied());
            // The server takes the loose parts over; the client shows its copy through
            // the interpolation buffer from now on (studio path).
            if s.physics.studio.is_some() && s.client.is_some() {
                for r in &parts {
                    s.handover.insert(*r, Default::default());
                    s.map.buffered.insert(*r);
                }
            }
            s.physics.refresh(parts.iter().copied());
        }
        for m in motors {
            self.record(RepOp::Parent(m));
        }
        if auto {
            self.warn(format!(
                "{} died; respawning in {:.0} s",
                self.player_name(),
                mech::RESPAWN_TIME
            ));
        } else {
            self.warn(format!(
                "{} died (CharacterAutoLoads is off: a script must call LoadCharacter)",
                self.player_name()
            ));
        }
    }

    /// The client copies of the parts the server took over (`kill_character`): once a
    /// frame, after the frame's replication, the server's state goes into each part's
    /// buffer and the client copy takes the buffer's pose (CHAR-NOTES round DEATH).
    fn handover_frame(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        if s.handover.is_empty() {
            return;
        }
        let now = s.time;
        if s.other_doc.is_none() {
            return;
        }
        let mut writes: Vec<(Ref, Xform)> = Vec::new();
        let gone: Vec<Ref> = s.handover.keys().copied().filter(|r| self.doc.dom.get_by_ref(*r).is_none()).collect();
        for r in gone {
            s.handover.remove(&r);
            s.map.buffered.remove(&r);
        }
        for (r, h) in s.handover.iter_mut() {
            let (Some(c), Some(Variant::CFrame(cf))) = (s.map.s2c.get(r).copied(), self.doc.prop(*r, "CFrame")) else {
                continue;
            };
            let owner = Xform::from_cframe(&cf);
            let (v, w) = s.physics.part_motion(*r).unwrap_or((Vec3::ZERO, Vec3::ZERO));
            let shown = s
                .other_doc
                .as_ref()
                .and_then(|d| match d.prop(c, "CFrame") {
                    Some(Variant::CFrame(x)) => Some(Xform::from_cframe(&x)),
                    _ => None,
                })
                .unwrap_or(owner);
            if let Some(x) = h.frame(now, (owner, v, w), shown) {
                writes.push((c, x));
            }
        }
        if writes.is_empty() {
            return;
        }
        self.with_side(Side::Client, |rt, _, _| {
            if let Some(mut w) = rt.world_mut() {
                let d = w.dm();
                for (c, x) in &writes {
                    if let Some(i) = d.dom.get_by_ref_mut(*c) {
                        i.properties.insert(ustr("CFrame"), Variant::CFrame(x.to_cframe()));
                    }
                    d.mark_prop(*c, ustr("CFrame"));
                }
            }
        });
    }

    fn respawn(&mut self) {
        if let Some(old) = self.play.as_ref().and_then(|s| s.character.as_ref()).map(|c| c.model) {
            self.fire_character_removing(old);
        }
        // The tool it held goes with it (it isn't put back in the Backpack).
        self.drop_equipped();
        self.unequip_client_tool_dropped();
        // The old character leaves Workspace before the new one is stored and
        // CharacterAdded fires, and is destroyed (Destroying on it and all it
        // holds; a no-op when a script destroyed it already) only after: the        // Character setter's pre-store hook emits
        // CharacterRemoving, detaches and defers the disposal
        // (Studio Play's server: Removing,
        // detach -> new CharacterAdded -> old Destroying).
        let old = self.play.as_mut().and_then(|s| s.character.take()).map(|c| c.model);
        if let Some(old) = old {
            self.with_side(Side::Server, |rt, _, _| {
                rt.apply_parent(old, Ref::none());
                rt.dispatch();
            });
        }
        self.spawn_character();
        if let Some(old) = old {
            self.with_side(Side::Server, |rt, _, _| {
                rt.apply_destroy(old);
                rt.dispatch();
            });
        }
    }

    /// Copies StarterGear on every spawn and refreshes the StarterPack
    /// tools on later spawns. Returns the copies for client LocalScripts.
    fn refill_backpack(&mut self, player: Ref, first: bool) -> Vec<Ref> {
        let Some(bp) = self.doc.find_child(player, "Backpack") else {
            return Vec::new();
        };
        if !first {
            let old = self.doc.dom.get_by_ref(bp)
                .map(|i| i.children().to_vec())
                .unwrap_or_default();
            self.with_side(Side::Server, |rt, _, _| {
                for c in &old {
                    rt.apply_destroy(*c);
                }
            });
        }
        let pack = (!first)
            .then(|| self.doc.find_child(self.doc.dom.root_ref(), "StarterPack"))
            .flatten();
        let gear = self.doc.find_child(player, "StarterGear");
        let mut copies = Vec::new();
        for source in [pack, gear].into_iter().flatten() {
            let children = self.doc.dom.get_by_ref(source)
                .map(|i| i.children().to_vec())
                .unwrap_or_default();
            for child in children {
                let copy = self.doc.dom.clone_within(child);
                self.doc.dom.transfer_within(copy, bp);
                self.doc.mark_tree(copy);
                self.record(RepOp::Parent(copy));
                copies.push(copy);
            }
        }
        if copies.is_empty() {
            return copies;
        }
        let scripts: Vec<Ref> = copies
            .iter()
            .flat_map(|c| self.server_scripts(Some(*c)))
            .collect();
        self.with_side(Side::Server, |rt, _, _| {
            for r in scripts {
                rt.start_collected(r);
            }
            rt.dispatch();
        });
        copies
    }

    /// After the client's scripts ran: UserInputService.MouseBehavior /
    /// MouseIconEnabled (the view grabs the cursor) and UserGameSettings.RotationType.
    fn read_client_input_state(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        if let Some(cd) = s.other_doc.as_ref() {
            if let Some(u) = cd.find_child(cd.dom.root_ref(), "UserInputService") {
                let inst = cd.dom.get_by_ref(u);
                s.mouse_behavior = match inst.and_then(|i| i.properties.get(&ustr("MouseBehavior")))
                {
                    Some(Variant::Enum(e)) => e.to_u32(),
                    _ => 0,
                };
                s.mouse_icon = !matches!(
                    inst.and_then(|i| i.properties.get(&ustr("MouseIconEnabled"))),
                    Some(Variant::Bool(false))
                );
            }
            // The cursor image: UserInputService.MouseIcon wins over the
            // PlayerMouse's Icon (Roblox); "" means the system arrow.
            let content = |r: Ref, name: &str| match cd.prop(r, name) {
                Some(Variant::String(t)) => Some(t),
                Some(Variant::ContentId(c)) => Some(c.as_str().to_string()),
                Some(Variant::Content(c)) => c.as_uri().map(|u| u.to_string()),
                _ => None,
            }
            .filter(|t| !t.trim().is_empty());
            let uis_icon = cd
                .find_child(cd.dom.root_ref(), "UserInputService")
                .and_then(|u| content(u, "MouseIcon"));
            let mouse = s
                .client
                .as_ref()
                .and_then(|c| c.world.as_ref())
                .and_then(|w| w.input.mouse);
            s.cursor_image = uis_icon.or_else(|| mouse.and_then(|m| content(m, "Icon")));
        }
        if let Some(c) = s.client.as_ref() {
            let rot: Option<String> = c
                .runtime
                .lua
                .globals()
                .get::<mlua::Table>("__rs_user_game_settings")
                .ok()
                .and_then(|t| t.get::<Value>("RotationType").ok())
                .and_then(|v| match v {
                    Value::UserData(ud) => ud.borrow::<LEnumItem>().ok().map(|e| e.name.clone()),
                    _ => None,
                });
            s.camera_relative = rot.as_deref() == Some("CameraRelative");
        }
    }

    /// Each joint's Motor6D.Transform as its owner (the client) has it: what
    /// the Animator's tracks, or the game's own scripts, wrote this frame.
    /// Keyed by the server's motor; unset ones are left out.
    fn joint_transforms(&self) -> HashMap<Ref, Xform> {
        let mut out = HashMap::new();
        let Some(s) = self.play.as_ref() else {
            return out;
        };
        let Some(ch) = s.character.as_ref() else {
            return out;
        };
        for j in &ch.rig.joints {
            let client = s.other_doc.as_ref().zip(s.map.s2c.get(&j.motor).copied());
            let v = match client {
                Some((cd, m)) => cd
                    .dom
                    .get_by_ref(m)
                    .and_then(|i| i.properties.get(&ustr("Transform")).cloned()),
                None => self
                    .doc
                    .dom
                    .get_by_ref(j.motor)
                    .and_then(|i| i.properties.get(&ustr("Transform")).cloned()),
            };
            if let Some(Variant::CFrame(cf)) = v {
                out.insert(j.motor, Xform::from_cframe(&cf));
            }
        }
        out
    }

    /// The client Humanoid's MoveDirection (at most length 1; it only has a
    /// vertical part while swimming) and AutoRotate, for the server-side
    /// `humanoid`.
    fn client_move_request(&self, humanoid: Ref) -> (Vec3, bool) {
        let Some(s) = self.play.as_ref() else {
            return (Vec3::ZERO, true);
        };
        let Some((cd, h)) = s.other_doc.as_ref().zip(s.map.s2c.get(&humanoid).copied()) else {
            return (Vec3::ZERO, true);
        };
        let dir = match cd.prop(h, "MoveDirection") {
            Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
            _ => Vec3::ZERO,
        };
        let dir = if dir.length() > 1.0 {
            dir.normalize()
        } else {
            dir
        };
        let auto = !matches!(cd.prop(h, "AutoRotate"), Some(Variant::Bool(false)));
        (if dir.is_finite() { dir } else { Vec3::ZERO }, auto)
    }

    /// Seats (Seat / VehicleSeat), as Roblox does them: touching an enabled,
    /// empty seat (or Seat:Sit) sits the humanoid — a SeatWeld from the seat
    /// to the HumanoidRootPart, Seat.Occupant, Humanoid.Sit and SeatPart —
    /// and jumping, Sit = false, a removed SeatWeld or a gone / disabled seat
    /// gets it up again. The welds and properties are made by the server's
    /// library (`__rs_seat` / `__rs_unseat`) so they replicate and fire
    /// Changed like a script's.
    fn update_seat(&mut self, humanoid: Ref, jump: bool, dt: f32) -> Option<SeatChange> {
        let (seat, touching, cooldown, root, dead, asked, seat_off, seen) = {
            let s = self.play.as_mut()?;
            let asked = s.game.sit_on.take();
            let off = s.game.states_off.contains("Seated");
            let ch = s.character.as_mut()?;
            ch.seat_cooldown = (ch.seat_cooldown - dt).max(0.0);
            let seen = std::mem::replace(&mut ch.seat_seen, ch.touching.clone());
            (
                ch.seat,
                ch.touching.clone(),
                ch.seat_cooldown,
                ch.rig.root,
                ch.dead,
                asked,
                off,
                seen,
            )
        };
        let usable = |doc: &Document, r: Ref| {
            doc.class_of(r)
                .is_some_and(|c| c == "Seat" || c == "VehicleSeat")
                && doc.dom.get_by_ref(r).is_some_and(|i| i.parent().is_some())
                && !matches!(doc.prop(r, "Disabled"), Some(Variant::Bool(true)))
        };
        // The client's Sit going from true to false (its LocalScript stood up).
        let client_sit = matches!(self.owned_humanoid_prop(humanoid, "Sit"), Some(Variant::Bool(true)));
        let client_stood = {
            let ch = self.play.as_mut().and_then(|s| s.character.as_mut());
            ch.map(|c| std::mem::replace(&mut c.client_sit, client_sit) && !client_sit).unwrap_or(false)
        };
        if let Some(seat) = seat {
            let gone = !usable(&self.doc, seat)
                || self.doc.find_named(seat, "SeatWeld").is_none()
                || client_stood
                || matches!(self.doc.prop(humanoid, "Sit"), Some(Variant::Bool(false)));
            if !(jump || gone || dead) {
                return None;
            }
            self.with_side(Side::Server, |rt, _, _| {
                rt.spawn_global("__rs_unseat", vec![rt.inst(humanoid), rt.inst(seat)]);
                rt.dispatch();
            });
            // Out of the seat: the capsule rises clear of the seat's top (it
            // sat sunk into it).
            let top = rustudio_render::scene::xform_of(&self.doc, seat).map(|xf| {
                let h = match self.doc.prop(seat, "Size") {
                    Some(Variant::Vector3(v)) => v.y,
                    _ => 1.0,
                };
                xf.pos.y + h / 2.0
            });
            if let Some(ch) = self.play.as_mut().and_then(|s| s.character.as_mut()) {
                ch.seat = None;
                ch.dynamic.seat_left = Some(seat);
                ch.seat_cooldown = mech::SEAT_COOLDOWN_S;
                if let Some(top) = top {
                    let clear = top
                        + mech::CHARACTER_HALF_HEIGHT
                        + mech::CHARACTER_RADIUS
                        + mech::CHARACTER_HOVER;
                    ch.body.pos.y = ch.body.pos.y.max(clear);
                }
            }
            return Some(SeatChange {
                active: false,
                seat: Some(seat),
            });
        }
        if dead || seat_off {
            return None;
        }
        let empty = |doc: &Document, r: Ref| match doc.prop(r, "Occupant") {
            Some(Variant::Ref(o)) => o.is_none() || o == humanoid,
            _ => true,
        };
        // Only a touch that began (Studio: standing on a seat after jumping
        // off it doesn't sit again; round 22 movement_probe seat).
        // The seat just left doesn't seat again for a while; another one
        // does at once (Studio 0.747: a different Seat touched 0.7 s after
        // leaving one seats the character, the same one doesn't).
        let left = self.play.as_ref().and_then(|s| s.character.as_ref()).and_then(|c| c.dynamic.seat_left);
        let touched = touching
            .iter()
            .copied()
            .filter(|r| !seen.contains(r))
            .filter(|r| cooldown <= 0.0 || Some(*r) != left)
            .find(|r| usable(&self.doc, *r) && empty(&self.doc, *r));
        // With the dynamic Humanoid a touch seats `SEAT_TOUCH_FRAMES` later
        // (the server's SeatWeld on its way back: play_humanoid.rs).
        let touched = {
            let ch = self.play.as_mut().and_then(|s| s.character.as_mut())?;
            match (ch.dynamic.seat_wait, touched) {
                (None, Some(r)) => {
                    ch.dynamic.seat_wait = Some((r, hum_glue::SEAT_TOUCH_FRAMES - 1));
                    None
                }
                (Some((r, 0)), _) => {
                    ch.dynamic.seat_wait = None;
                    Some(r).filter(|r| usable(&self.doc, *r) && empty(&self.doc, *r))
                }
                (Some((r, n)), _) => {
                    ch.dynamic.seat_wait = Some((r, n - 1));
                    None
                }
                (None, None) => None,
            }
        };
        let pick = asked.filter(|r| usable(&self.doc, *r)).or(touched)?;
        self.with_side(Side::Server, |rt, _, _| {
            rt.spawn_global(
                "__rs_seat",
                vec![rt.inst(humanoid), rt.inst(pick), rt.inst(root)],
            );
            rt.dispatch();
        });
        if let Some(ch) = self.play.as_mut().and_then(|s| s.character.as_mut()) {
            ch.seat = Some(pick);
        }
        // Mirror the character's new seated CFrames onto the client document
        // right away so that when SeatWeld replicates to the client,
        // entering_joint_moves sees the character already at the seat rather
        // than pulling an unanchored vehicle to the character's old position.
        if let Some(root_xf) = rustudio_render::scene::xform_of(&self.doc, root) {
            let pose = self
                .play
                .as_ref()
                .and_then(|s| s.character.as_ref())
                .map(|ch| ch.rig.solve(root_xf, &ch.dynamic.transforms));
            if let Some(pose) = pose {
                if let Some(s) = self.play.as_mut() {
                    if let Some(cdoc) = s.other_doc.as_mut() {
                        rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
                    }
                    if let Some(ch) = s.character.as_mut() {
                        ch.last_root = Some(root_xf);
                    }
                }
            }
        }
        Some(SeatChange {
            active: true,
            seat: Some(pick),
        })
    }

    /// Players.CharacterAutoLoads (default true).
    pub(super) fn character_auto_loads(&self) -> bool {
        let root = self.doc.dom.root_ref();
        !self.doc.find_child(root, "Players").is_some_and(|p| {
            matches!(
                self.doc.prop(p, "CharacterAutoLoads"),
                Some(Variant::Bool(false))
            )
        })
    }

    /// Join the outstanding job. Bench summaries also use this to read the
    /// completed world's counts after collecting their final display frame.
    pub(crate) fn wait_for_play_physics(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(pending) = s.pending_frame.as_mut() else { return };
        if let Some(job) = pending.job.take() {
            let (physics, ready) = job.join();
            s.physics.0 = Some(physics);
            pending.ready = Some(ready);
        }
    }

    fn capture_play_keys(&mut self, keyboard: &PlayKeyboard) -> PlayKeys {
        // Keys: character movement and UserInputService (client side).
        // A focused TextBox has the keys: the character doesn't hear them, but
        // UserInputService still does, with gameProcessedEvent true (chat typing).
        // The key positions are physical, like Roblox's KeyCodes: WASD moves
        // from the keys labelled ZQSD on AZERTY (src/keys.rs).
        let textbox = keyboard.textbox;
        // No-UI runs report every key Roblox has a KeyCode for (F5–F12, /).
        let all_keys = keyboard.no_ui;
        let blocked = keyboard.blocked
            || self.play.as_ref().is_some_and(|s| s.game.menu_open);
        let typing = blocked || textbox;
        let raw_keys: HashSet<String> = if blocked {
            HashSet::new()
        } else {
            let mut k: HashSet<String> = self
                .phys
                .iter()
                .filter_map(|k| key_name(k, all_keys))
                .map(String::from)
                .collect();
            // egui reports modifiers apart from keys (shift-lock is LeftShift).
            let mods = keyboard.modifiers;
            for (on, name) in [
                (mods.shift || keyboard.shift, "LeftShift"),
                (mods.ctrl, "LeftControl"),
                (mods.alt, "LeftAlt"),
            ] {
                if on {
                    k.insert(name.to_string());
                }
            }
            k
        };
        let keys: HashSet<String> = if typing { HashSet::new() } else { raw_keys.clone() };
        // (name, gameProcessedEvent)
        let (began, ended): (Vec<(String, bool)>, Vec<(String, bool)>) = {
            let s = self.play.as_mut().expect("playing");
            let mut b: Vec<(String, bool)> = raw_keys
                .difference(&s.keys_prev)
                .map(|k| (k.clone(), textbox))
                .collect();
            let mut e: Vec<String> = s.keys_prev.difference(&raw_keys).cloned().collect();
            // A tap that came and went between two frames: began and ended now
            // (processed when a TextBox had the keys as it was pressed, the
            // Enter that submits it included).
            let taps = std::mem::take(&mut s.game.key_taps);
            {
                // (Escape only arrives here in no-UI runs, processed, and gets
                // past the menu it just opened or closed.)
                for (k, processed) in taps {
                    if blocked && k != egui::Key::Escape {
                        continue;
                    }
                    let Some(k) = key_name(k, all_keys) else { continue };
                    let k = k.to_string();
                    if !raw_keys.contains(&k)
                        && !s.keys_prev.contains(&k)
                        && !b.iter().any(|(n, _)| *n == k)
                    {
                        b.push((k.clone(), processed));
                        e.push(k);
                    }
                }
            }
            if !blocked {
                // Shift has no key event in egui, only modifier state: a
                // tap pressed and released between two frames (a hitch, a
                // quick press) would never reach shift lock without this.
                let shift = "LeftShift".to_string();
                if !raw_keys.contains(&shift) && !s.keys_prev.contains(&shift) {
                    if keyboard.shift_tapped {
                        b.push((shift.clone(), textbox));
                        e.push(shift);
                    }
                }
            }
            for (k, p) in &b {
                if *p {
                    s.keys_processed.insert(k.clone());
                }
            }
            let e = e
                .into_iter()
                .map(|k| {
                    let p = s.keys_processed.remove(&k);
                    (k, p)
                })
                .collect();
            s.keys_prev = raw_keys;
            (b, e)
        };
        if !typing && began.iter().any(|(key, processed)| {
            !processed && matches!(key.as_str(), "W" | "A" | "S" | "D" | "Up" | "Down" | "Space")
        }) {
            self.play_input_pose_due = true;
        }
        PlayKeys {
            focused: keyboard.focused,
            keys,
            began,
            ended,
            perf_stats_toggle: !typing && keyboard.perf_stats_toggle,
        }
    }

    /// One frame of the play session.
    pub fn play_tick(&mut self, ctx: &egui::Context, dt: f32, host: &mut dyn GameHost, no_ui: bool, shift: bool) {
        rustudio_core::load_trace::play_frame();
        let keyboard = PlayKeyboard::read(ctx, self.gui.focus.is_some(), no_ui, shift);
        if self.play_thread.is_some() {
            self.threaded_step(keyboard, dt, host);
        } else {
            self.step_play(dt, host, &keyboard, false);
        }
    }

    /// One Play step while the compositor sends the window no redraws (another
    /// workspace, minimized): nothing is drawn, and the game sees no keys, as
    /// when Studio's window loses focus.
    pub fn play_tick_hidden(&mut self, dt: f32, host: &mut dyn GameHost, no_ui: bool) {
        let keyboard = PlayKeyboard {
            focused: false,
            blocked: true,
            textbox: false,
            modifiers: egui::Modifiers::default(),
            shift_tapped: false,
            perf_stats_toggle: false,
            no_ui,
            shift: false,
        };
        if self.play_thread.is_some() {
            self.threaded_step(keyboard, dt, host);
        } else {
            self.step_play(dt, host, &keyboard, false);
        }
    }

    /// Whether the gameplay worker should look in on its world's physics
    /// while it waits for the next step (`play_worker_idle`).
    pub(crate) fn play_worker_idle_due(&self) -> bool {
        self.fixed_dt.is_none()
            && self.play.as_ref().is_some_and(|s| {
                s.server_phase.is_none() && s.pending_frame.as_ref().is_some_and(|p| p.job.is_some())
            })
    }

    /// The gameplay worker between steps: once the world's batch has
    /// finished, the tick's server half runs now instead of after the next
    /// display boundary. It needs nothing from that boundary (the keys reach
    /// the client half, which still runs there), and it shortens the next
    /// step's path to its physics batch by the server's scripts and
    /// replication (a test place on a low-end laptop: ~3.3 of ~11 ms). An unfinished
    /// batch is left to the step, which can still hurry it.
    pub(crate) fn play_worker_idle(&mut self) {
        if !self.play_worker_idle_due() {
            return;
        }
        let Some(s) = self.play.as_mut() else { return };
        let Some(pending) = s.pending_frame.as_mut() else { return };
        let Some((physics, ready)) = pending.job.as_ref().and_then(|job| job.try_join()) else { return };
        pending.job = None;
        s.physics.0 = Some(physics);
        pending.ready = Some(ready);
        let Some(pending) = s.pending_frame.take() else { return };
        let _early = rustudio_core::bench::scope("gameplay_early_server");
        self.server_front();
        // (Before the batch's poses land: a script's move of the player's
        // character while it ran, `hum_script_moves`.)
        self.hum_script_moves();
        let result = self.play.as_mut().expect("playing").physics.finish_frame(
            &mut self.doc, pending.ready.expect("joined physics frame"),
        );
        let phase = self.finish_server_phase(pending.input, result);
        if let Some(s) = self.play.as_mut() {
            s.server_phase = Some((phase, pending.deferred_dt));
        }
        self.shown_front();
    }

    pub(crate) fn step_play(&mut self, dt: f32, host: &mut dyn GameHost, keyboard: &PlayKeyboard, render_in_parallel: bool) -> bool {
        let mut dt = dt;
        let mut display_keys = None;
        if let Some((phase, deferred)) = self.play.as_mut().and_then(|s| s.server_phase.take()) {
            // (Its server half ran early: play_worker_idle.)
            dt += deferred;
            self.server_front();
            let keys = display_keys.insert(self.capture_play_keys(keyboard));
            self.finish_client_phase(host, phase, keys);
        }
        if let Some(s) = self.play.as_mut() {
            if let Some(pending) = s.pending_frame.as_mut() {
                if let Some(job) = pending.job.as_ref() {
                    let mut result = job.try_join();
                    if result.is_none() && rustudio_physics::physics::hurry_enabled() && job.should_hurry() {
                        // The world's batch is still running: it yields to this
                        // frame (the Humanoids finish their ticks against the
                        // environment held still; the environment's ticks are owed
                        // and caught up between frames, physics.rs `PhysicsJob::hurry`).
                        // Studio's client steps the local character apart from the
                        // server's world, whose physics never holds up its frame.
                        // (Not when hurrying would gain little: `should_hurry`.)
                        job.hurry();
                        let _wait = rustudio_core::bench::scope("physics_pose_wait");
                        let _tail = rustudio_core::bench::scope("physics_hurry_tail");
                        result = Some(pending.job.take().expect("pending physics job").join());
                    }
                    let result = result.or_else(|| {
                        // During movement, continue as soon as physics is
                        // ready. Threaded Play keeps drawing its last snapshot
                        // while this waits, except at the first input edge.
                        let moving = pending.input.humanoid.as_ref().is_some_and(|h| h.active_input);
                        // On the gameplay worker every step completes: waiting
                        // here blocks only the worker (the viewport keeps
                        // drawing the last publication), while skipping cost a
                        // whole display boundary for a batch that was usually
                        // a few ms from done (a test place on a low-end laptop: 215 of 1,893
                        // displays carried no new state). The direct path
                        // still skips while idle so it never stalls a frame.
                        let must_publish = moving || render_in_parallel;
                        must_publish.then(|| {
                            let _wait = rustudio_core::bench::scope("physics_pose_wait");
                            pending.job.take().expect("pending physics job").join()
                        })
                    });
                    let Some((physics, ready)) = result else {
                        // An unfinished step can miss presentation without
                        // blocking it. Limit the elapsed time the next step
                        // inherits to the same simulation hitch budget.
                        let elapsed = pending.deferred_dt + dt;
                        pending.deferred_dt = elapsed.min(rustudio_core::mech::MAX_FRAME_DT);
                        if elapsed > pending.deferred_dt {
                            rustudio_core::bench::add("physics_elapsed_clipped", f64::from(elapsed - pending.deferred_dt) * 1000.0, 1);
                        }
                        rustudio_core::bench::add("physics_wait_frames", 0.0, 1);
                        return false;
                    };
                    pending.job = None;
                    s.physics.0 = Some(physics);
                    pending.ready = Some(ready);
                }
            }
        }
        if let Some(pending) = self.play.as_mut().and_then(|s| s.pending_frame.take()) {
            dt += pending.deferred_dt;
            self.server_front();
            // (Before the batch's poses land: a script's move of the player's
            // character while it ran, `hum_script_moves`.)
            self.hum_script_moves();
            let result = self.play.as_mut().unwrap().physics.finish_frame(
                &mut self.doc, pending.ready.expect("joined physics frame"),
            );
            // Keyboard edges belong to the upcoming RenderStepped, which
            // feeds the local controller for the batch we prepare below.
            let keys = display_keys.insert(self.capture_play_keys(keyboard));
            self.finish_play_tick(host, pending.input, result, keys);
        }
        let Some(mode) = self.play.as_ref().map(|p| p.mode) else {
            return true;
        };
        // An avatar that arrived after Play started (it wasn't waited for:
        // CharacterAutoLoads off) is the next LoadCharacter's.
        if let (Some(look), Some(s)) = (self.account.avatar(), self.play.as_mut()) {
            for w in [s.server.as_mut().and_then(|server| server.world.as_mut()), s.client.as_mut().and_then(|c| c.world.as_mut())].into_iter().flatten() {
                if w.avatar.is_none() {
                    w.avatar = Some(look.clone());
                }
            }
        }
        // Roblox steps the game by at most 1/15 s a frame: slower frames slow
        // time(), physics and animations, while waits keep wall time (World::realtime).
        let realtime = self.fixed_dt.is_none();
        let elapsed = dt.clamp(0.0, rustudio_core::mech::MAX_FRAME_DT);
        if realtime && dt > elapsed {
            rustudio_core::bench::add("physics_elapsed_clipped", f64::from(dt - elapsed) * 1000.0, 1);
        }
        let dt = elapsed;
        // The in-game menu's Leave stops the session; Reset Character kills
        // the character on the server (Humanoid.Health = 0, as Roblox does).
        if self.play.as_ref().is_some_and(|s| {
            s.game.leave_request
                || [s.server.as_ref().and_then(|vm| vm.world.as_ref()), s.client.as_ref().and_then(|vm| vm.world.as_ref())]
                    .into_iter().flatten().any(|w| w.shutdown_requested)
        }) {
            self.stop_play(host);
            return true;
        }
        // StudioTestService:EndTest ends the session; the Studio services
        // read the editor's settings (runtime/domains/testing*.rs).
        {
            use rustudio_engine::runtime::domains::testing as t;
            let ended = self.play.as_ref()
                .and_then(|p| p.server.as_ref())
                .and_then(|s| t::take_end_request(&s.runtime.lua));
            if let Some(value) = ended {
                self.stop_play(host);
                host.end_test(value, &self.log);
                return true;
            }
            t::set_editor_state(t::EditorState {
                snap_move: self.snap_move,
                move_inc: self.move_inc,
                snap_rotate: self.snap_rotate,
                rotate_inc: self.rotate_inc,
                local_space: self.local_space,
                ..Default::default()
            });
        }
        if rustudio_core::profiler::active() {
            use rustudio_core::profiler::{counter_unit, frame, Cat, FrameKind, Unit};
            frame(FrameKind::Heartbeat);
            if let Some(s) = self.play.as_ref() {
                let server = s.server.as_ref().map_or(0, |x| x.runtime.lua.used_memory());
                let client = s.client.as_ref().map_or(0, |x| x.runtime.lua.used_memory());
                counter_unit("Luau heap (server)", Cat::Script, Unit::Bytes, server as f64);
                counter_unit("Luau heap (client)", Cat::Script, Unit::Bytes, client as f64);
            }
        }
        self.server_front();
        if let Some(h) = self.play.as_mut().and_then(|s| {
            std::mem::take(&mut s.game.reset_request)
                .then(|| s.character.as_ref().map(|c| c.humanoid))
                .flatten()
        }) {
            self.with_side(Side::Server, |rt, _, _| {
                rt.set(h, "Health", Value::Number(0.0));
            });
        }
        // The menu's Settings: UserGameSettings changes on the client.
        let menu_lua = self
            .play
            .as_mut()
            .map(|s| std::mem::take(&mut s.game.menu.lua))
            .unwrap_or_default();
        for code in menu_lua {
            if let Some(e) = self.run_luau_as(Side::Client, &code, rustudio_engine::runtime::security::CORE) {
                self.warn(format!("Settings: {e}"));
            }
        }

        // 1. Server script changes from last frame reach physics.
        let sc = rustudio_core::bench::scope("p1_physics_sync");
        {
            let s = self.play.as_mut().expect("playing");
            let w = s.server.as_mut().and_then(|server| server.world.as_mut()).expect("server world");
            let vel = std::mem::take(&mut w.velocity_requests);
            s.physics.sync(&mut self.doc);
            s.physics.set_streaming_visibility(
                s.map.streaming_collider_revision(),
                s.map.unstreamed_parts(),
            );
            if let Some(c) = s.other_doc.as_mut() {
                s.physics.follow_client(&self.doc, c, &s.map.c2s);
            }
            for (r, v) in vel {
                s.physics.set_velocity(r, v);
            }
            // The client's own loose parts near the character, in this world as
            // stand-ins the character stands on and pushes (studio path,
            // physics_client_link.rs).
            if studio_path() && mode == RunMode::Play {
                let _l = rustudio_core::bench::scope("p1b_client_link");
                let model = s.character.as_ref().map(|c| c.model);
                s.physics.link_client_bodies(&s.client_sim, &s.map.c2s, model);
            }
            s.time += dt as f64;
        }

        drop(sc);
        // Clicks, chat, tool keys and mouse buttons from the viewport.
        self.game_inputs();
        // Studio's frame order (checks/humanoid-physics SPEC §0.1): the
        // RunService events before the world steps fire before it, the
        // Humanoid steps between PreAnimation and the Animator.
        {
            let _s = rustudio_core::bench::scope("p2a_lua_pre");
            // (CameraRelative's turn: on the studio path after the client's
            // RenderStepped, at the end of the last frame, `hum_face_camera`.)
            if mode == RunMode::Play && !studio_path() {
                self.hum_face_camera();
            }
            self.lua_pre_physics(mode, dt, 0);
        }
        // 2. Character (owned by the client: written to both DataModels).
        let sc = rustudio_core::bench::scope("p2_character");
        let mut hum_out: Option<hum_glue::HumFrameOut> = None;
        if mode == RunMode::Play && self.play_has_character()
            && !self.play.as_ref().is_some_and(|s| s.map.gameplay_paused())
        {
            let _s = rustudio_core::bench::scope("p2_player_humanoid");
            hum_out = Some(self.hum_pre(dt));
        }
        {
            let _s = rustudio_core::bench::scope("p2_npc_pre");
            self.npc_pre(dt);
        }
        {
            let _s = rustudio_core::bench::scope("p2b_lua_pre");
            self.lua_pre_physics(mode, dt, 1);
        }
        // The client's writes to its character in Stepped / PreSimulation (a mover's
        // value, AssemblyLinearVelocity) are this frame's (probe T26_presim_*).
        if hum_out.is_some() {
            let _s = rustudio_core::bench::scope("p2_client_writes");
            if let Some(model) = self.play.as_ref().and_then(|s| s.character.as_ref()).map(|c| c.model) {
                self.hum_client_writes(model);
            }
        }
        // Changes from Stepped / PreSimulation reach this frame's solve.
        {
            let _s = rustudio_core::bench::scope("p2_stepped_sync");
            if let Some(s) = self.play.as_mut() {
                let vel = s.server.as_mut().and_then(|server| server.world.as_mut()).map(|w| std::mem::take(&mut w.velocity_requests)).unwrap_or_default();
                s.physics.sync(&mut self.doc);
                if let Some(c) = s.other_doc.as_mut() {
                    s.physics.follow_client(&self.doc, c, &s.map.c2s);
                }
                for (r, v) in vel {
                    s.physics.set_velocity(r, v);
                }
            }
            // If `sync` rebuilt the vehicle assembly (e.g. on the frame SeatWeld was
            // created), re-attach the seated character's FixedJoint to the live seat body.
            if let Some((model, seat, seat_root)) = self
                .play
                .as_ref()
                .and_then(|s| s.character.as_ref())
                .and_then(|c| c.seat.map(|seat| (c.model, seat)))
                .and_then(|(m, seat)| self.seat_root_xform().map(|xf| (m, seat, xf)))
            {
                if let Some(s) = self.play.as_mut() {
                    s.physics.seat_humanoid(model, Some((seat, seat_root)));
                }
            }
        }
        if hum_out.is_some() {
            self.hum_roots_seen();
        }
        if hum_out.is_some() {
            let _pose = rustudio_core::bench::scope("p2_humanoid_pose");
            self.hum_pose();
        }
        {
            let _pose = rustudio_core::bench::scope("p2_npc_pose");
            self.npc_pose();
            self.server_pose_rigs();
        }

        drop(sc);
        // Keep the rendered document on the UI thread while the solver works
        // on its owned world. Commit and replicate at the next frame boundary.
        let sc = rustudio_core::bench::scope("p3_physics_step");
        self.sync_touch_interest();
        let input = PlayFrameInput {
            mode, dt, realtime, humanoid: hum_out, local_input_edge: self.play_input_pose_due,
        };
        let s = self.play.as_mut().expect("playing");
        let mut physics = s.physics.0.take().expect("physics is ready");
        physics.set_realtime(realtime);
        let queued = if realtime {
            let skip = physics.skip.clone();
            physics.start_frame(&mut self.doc, dt).map(|job| (job, skip))
        } else {
            Err(physics)
        };
        match queued {
            Ok((job, skip)) => {
                s.pending_frame = Some(PendingPlayFrame {
                    input, job: Some(job), ready: None, deferred_dt: 0.0, skip,
                });
                self.shown_front();
            }
            Err(mut physics) => {
                let result = physics.step(&mut self.doc, dt);
                s.physics.0 = Some(physics);
                drop(sc);
                let keys = display_keys.get_or_insert_with(|| self.capture_play_keys(keyboard));
                self.finish_play_tick(host, input, result, keys);
            }
        }
        true
    }

    fn finish_play_tick(&mut self, host: &mut dyn GameHost, input: PlayFrameInput, result: StepResult, display_keys: &mut PlayKeys) {
        let phase = self.finish_server_phase(input, result);
        self.finish_client_phase(host, phase, display_keys);
    }

    /// The server half of a finished tick (the world's step published,
    /// touches, server scripts, replication). Its client half follows at
    /// the display boundary with that boundary's keys (`finish_client_phase`);
    /// a gameplay worker whose physics finished early runs this half in
    /// the idle time before the boundary (`play_worker_idle`).
    fn finish_server_phase(&mut self, input: PlayFrameInput, result: StepResult) -> ServerPhase {
        let PlayFrameInput { mode, dt, realtime, humanoid: hum_out, .. } = input;
        let sc = rustudio_core::bench::scope("p3_physics_publish");
        let mut char_touches = None;
        let mut state_change: Option<(State, State)> = None;
        let mut seat_change: Option<SeatChange> = None;
        let mut move_event: Option<(&'static str, f32)> = None;
        let mut running_speed: Option<f32> = None;
        let mut floor_changed: Option<u32> = None;
        let mut humanoid_ref = Ref::none();
        {
            let _s = rustudio_core::bench::scope("p3a_npc_post");
            self.npc_post();
            self.server_pose_rigs();
        }
        if let Some(mut o) = hum_out {
            let _s = rustudio_core::bench::scope("p3a_humanoid_post");
            self.hum_post(&mut o);
            char_touches = std::mem::take(&mut o.touches);
            state_change = o.state_change;
            if let (Some((_, next)), Some(s)) = (state_change, self.play.as_mut()) {
                s.state_log.push(next.enum_name().to_string());
            }
            seat_change = o.seat_change.take();
            move_event = o.move_event;
            running_speed = o.running_speed;
            floor_changed = o.floor_changed;
            humanoid_ref = o.humanoid;
        }
        for (r, _) in &result.moved {
            self.record(RepOp::Moved(*r));
        }
        // Below FallenPartsDestroyHeight a part is taken out of the tree: its
        // Parent goes to nil (Studio: ChildRemoved and AncestryChanged fire, not
        // Destroying, and it can be parented back), ancestor Models lose a
        // PrimaryPart that fell, and ancestor Models with no remaining BaseParts
        // are removed as well.
        let (fallen_signals, fallen_removed) = unparent_fallen_parts(&mut self.doc, &result.fallen);
        for r in &fallen_removed {
            self.record(RepOp::Parent(*r));
        }
        for (r, old, below) in fallen_signals {
            self.with_side(Side::Server, |rt, _, _| rt.notify_parent(r, old, Ref::none(), below));
        }
        if let Some(s) = self.play.as_mut() {
            s.engine_orphans.extend(fallen_removed);
        }

        drop(sc);
        // Touch events (CanTouch parts only).
        let sc = rustudio_core::bench::scope("p3b_touch_prep");
        // (A part only the client has, met by its character (physics_client_link.rs),
        // is read from the client's document.)
        let client_doc = self.play.as_ref().and_then(|s| s.other_doc.as_ref());
        let can_touch = |doc: &Document, r: Ref| {
            let v = doc.prop(r, "CanTouch").or_else(|| {
                doc.dom.get_by_ref(r).is_none().then(|| client_doc.and_then(|cd| cd.prop(r, "CanTouch"))).flatten()
            });
            !matches!(v, Some(Variant::Bool(false)))
        };
        // The local character's solver can regenerate several manifolds for
        // the same part pair inside a frame. Its pair snapshot also includes
        // contacts with client-only stand-ins. Use it for persistent edges,
        // while the rest of the world keeps its solver events.
        let player = self.play.as_ref().and_then(|s| s.character.as_ref().map(|c| (s, c.model)));
        let is_character_part = |r| player.is_some_and(|(s, model)| s.physics.hums.of_part.get(&r) == Some(&model));
        let mut part_touches = Vec::new();
        let mut transient = rustc_hash::FxHashMap::default();
        for (a, b, began) in result.touches {
            if !can_touch(&self.doc, a) || !can_touch(&self.doc, b) {
                continue;
            }
            let pair = if char_touches.is_none() {
                None
            } else if is_character_part(a) {
                Some((a, b))
            } else if is_character_part(b) {
                Some((b, a))
            } else {
                None
            };
            if let Some(pair) = pair {
                if began {
                    transient.entry(pair).or_insert(false);
                } else if let Some(ended) = transient.get_mut(&pair) {
                    *ended = true;
                }
            } else {
                part_touches.push((a, b, began));
            }
        }
        let character_touch_start = part_touches.len();
        let mut client_only = HashSet::new();
        if let Some(mut now) = char_touches {
            now.retain(|(a, b)| can_touch(&self.doc, *a) && can_touch(&self.doc, *b));
            if let Some(ch) = self.play.as_mut().and_then(|s| s.character.as_mut()) {
                part_touches.extend(now.difference(&ch.touch_pairs).map(|&(a, b)| (a, b, true)));
                part_touches.extend(ch.touch_pairs.difference(&now).map(|&(a, b)| (a, b, false)));
                // A fast crossing can begin and end between snapshots.
                // Keep one complete solver event pair for that crossing.
                for (pair @ (a, b), ended) in transient {
                    if ended && !now.contains(&pair) && !ch.touch_pairs.contains(&pair) {
                        part_touches.extend([(a, b, true), (a, b, false)]);
                    }
                }
                client_only.extend(part_touches[character_touch_start..].iter().map(|&(_, b, _)| b)
                    .filter(|&r| self.doc.dom.get_by_ref(r).is_none()));
                ch.touching = now.iter().map(|&(_, b)| b).collect();
                ch.touching.extend(ch.dynamic.floor_part.filter(|r| {
                    !matches!(self.doc.prop(*r, "CanTouch"), Some(Variant::Bool(false)))
                }));
                ch.touch_pairs = now;
            }
        }
        let velocities = self
            .play
            .as_ref()
            .map(|s| s.physics.velocities())
            .unwrap_or_default();
        let time = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);
        let mut state_names: HashMap<Ref, &'static str> = self
            .play
            .as_ref()
            .and_then(|s| s.character.as_ref())
            .map(|c| HashMap::from([(c.humanoid, c.state.enum_name())]))
            .unwrap_or_default();
        // The other Humanoids (play_npc.rs): their states and this frame's
        // StateChanged / Running / Climbing / Swimming events.
        let npc_events: Vec<npc::NpcEvent> = match self.play.as_mut() {
            Some(s) => {
                state_names.extend(s.npcs.states());
                std::mem::take(&mut s.npcs.events)
            }
            None => Vec::new(),
        };
        let seat_server = seat_change.as_ref().and_then(|c| c.seat);
        drop(sc);
        let sc = rustudio_core::bench::scope("p4_server_lua");
        let root = self.doc.dom.root_ref();
        let run_service = self.doc.find_child(root, "RunService");
        let humanoid_server = humanoid_ref.is_some().then_some(humanoid_ref);
        let mut phase = ServerPhase {
            mode, dt, realtime, part_touches, character_touch_start, client_only, velocities,
            state_names, npc_events, state_change, seat_change, move_event, running_speed, humanoid_server,
            seat_server, server_out: Default::default(),
        };
        phase.server_out = self
            .with_side(Side::Server, |rt, _, _| {
                if let Some(mut w) = rt.world_mut() {
                    w.velocities = phase.velocities.iter().map(|(k, v)| (*k, *v)).collect();
                    w.humanoid_states.clone_from(&phase.state_names);
                }
                phase.fire_touches(rt, None);
                phase.fire_humanoid(rt, humanoid_server, seat_server, false);
                phase.fire_npcs(rt, None);
                if let Some(rs) = run_service {
                    // A server frame in Studio's order (Run mode probe):
                    // PreAnimation (then the Animator), Stepped,
                    // PreSimulation, PostSimulation, Heartbeat (then tweens).
                    // (With the dynamic Humanoid the first three fired before
                    // the world steps: lua_pre_physics.)
                    rt.fire(rs, "PostSimulation", vec![Value::Number(dt as f64)]);
                    rt.fire(rs, "Heartbeat", vec![Value::Number(dt as f64)]);
                }
                if let Some(mut w) = rt.world_mut() {
                    w.realtime = realtime;
                }
                rt.step(dt as f64);
                rt.world_mut()
                    .map(|mut w| {
                        (
                            std::mem::take(&mut w.died),
                            std::mem::take(&mut w.load_character),
                        )
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.game_requests(Side::Server);

        drop(sc);
        // 5. Replicate to the client, then client scripts.
        let sc = rustudio_core::bench::scope("p5_replicate");
        // The server's Animator track slots, then the frame's replication.
        self.with_side(Side::Server, |rt, _, _| rustudio_engine::anim_replicate::publish(&rt.lua));
        if let Some(s) = self.play.as_mut() {
            s.map.begin_frame(dt as f64);
        }
        self.replicate();
        self.handover_frame();
        drop(sc);
        phase
    }

    /// The client half of a finished tick: client scripts with the keys of
    /// this display boundary, the client's own simulation, respawns, the view.
    fn finish_client_phase(&mut self, host: &mut dyn GameHost, mut phase: ServerPhase, display_keys: &mut PlayKeys) {
        let PlayKeys { focused: focused_now, keys, began, ended, perf_stats_toggle } = display_keys;
        let focused_now = *focused_now;
        let began = std::mem::take(began);
        let ended = std::mem::take(ended);
        let (mode, dt, realtime) = (phase.mode, phase.dt, phase.realtime);
        let (humanoid_server, seat_server) = (phase.humanoid_server, phase.seat_server);
        let velocities = std::mem::take(&mut phase.velocities);
        let state_names = std::mem::take(&mut phase.state_names);
        let sc = rustudio_core::bench::scope("p6_client_lua");
        if mode == RunMode::Play {
            let prep = rustudio_core::bench::scope("p6a_prep");
            // Server → client refs for just what this phase looks up (touches,
            // the character, humanoid states, moving parts): cloning the whole
            // map (116k instances) was 0.75 ms a frame.
            let current_seat = self
                .play
                .as_ref()
                .and_then(|s| s.character.as_ref())
                .and_then(|c| c.seat);
            let map_s2c: rustudio_core::RefMap = self
                .play
                .as_ref()
                .map(|s| {
                    let full = &s.map.s2c;
                    phase.part_touches
                        .iter()
                        .flat_map(|(a, b, _)| [*a, *b])
                        .chain(humanoid_server)
                        .chain(seat_server)
                        .chain(current_seat)
                        .chain(velocities.keys().copied())
                        .chain(state_names.keys().copied())
                        .chain(s.character.as_ref().map(|c| c.rig.root))
                        .filter_map(|r| full.get(&r).map(|c| (r, *c)))
                        .collect()
                })
                .unwrap_or_default();
            let (crs, cuis) = match self.play.as_ref().and_then(|s| s.other_doc.as_ref()) {
                Some(cd) => {
                    let r = cd.dom.root_ref();
                    (
                        cd.find_child(r, "RunService"),
                        cd.find_child(r, "UserInputService"),
                    )
                }
                None => (None, None),
            };
            self.game_client_inputs();
            self.check_client_tool();
            let client_touches = self
                .play
                .as_mut()
                .map(|s| std::mem::take(&mut s.client_touches))
                .unwrap_or_default();
            let humanoid_client = humanoid_server.and_then(|h| map_s2c.get(&h).copied());
            let client_states = state_names
                .iter()
                .filter_map(|(k, v)| map_s2c.get(k).map(|c| (*c, *v)))
                .collect();
            let (gui_events, gui_layout, mouse_events, menu_events, menu_open) =
                match self.play.as_mut() {
                    Some(s) => (
                        std::mem::take(&mut s.game.gui_events),
                        std::mem::take(&mut s.game.gui_layout),
                        std::mem::take(&mut s.game.uis_mouse),
                        std::mem::take(&mut s.game.menu_events),
                        s.game.menu_open,
                    ),
                    None => Default::default(),
                };
            let server_buttons = gui_events.iter()
                .filter(|event| matches!(event, GuiEvent::Fire(_, "MouseButton1Down" | "MouseButton1Click", _)))
                .cloned()
                .collect();
            let gui_service = self
                .play
                .as_ref()
                .and_then(|s| s.other_doc.as_ref())
                .and_then(|cd| cd.find_child(cd.dom.root_ref(), "GuiService"));
            // Mouse over the game view: position (GUI coordinates, under the
            // top bar), motion and wheel since last frame.
            let (mouse_xy, mouse_delta, mouse_moved, mouse_move_processed, wheel, wheel_processed) =
                match self.play.as_mut() {
                    Some(s) => {
                        let xy = s.game.last_pointer.map(|p| p.xy).unwrap_or([0.0, 0.0]);
                        (
                            xy,
                            std::mem::take(&mut s.mouse_delta),
                            std::mem::take(&mut s.mouse_moved),
                            std::mem::take(&mut s.mouse_move_processed),
                            std::mem::take(&mut s.wheel),
                            std::mem::take(&mut s.wheel_processed),
                        )
                    }
                    None => ([0.0, 0.0], glam::Vec2::ZERO, false, false, 0.0, false),
                };
            let (mx, my) = (
                mouse_xy[0] as f64,
                (mouse_xy[1] - mech::GUI_TOP_INSET) as f64,
            );
            // The OS window gained or lost focus since last frame.

            let window_focus_change = self.play.as_mut().and_then(|s| {
                (s.window_focused != focused_now).then(|| {
                    s.window_focused = focused_now;
                    focused_now
                })
            });
            let accepts_gamepad = focused_now && self.play_game_input() && !menu_open;
            let gamepads = self
                .play
                .as_mut()
                .map(|s| s.gamepads.poll(accepts_gamepad))
                .unwrap_or_default();
            // Moving parts' velocities on the client, its own character's too.
            let client_velocities: rustc_hash::FxHashMap<Ref, Vec3> = {
                let mut v: rustc_hash::FxHashMap<Ref, Vec3> = velocities
                    .iter()
                    .filter_map(|(k, v)| map_s2c.get(k).map(|c| (*c, *v)))
                    .collect();
                if let Some((root, vel)) = self
                    .play
                    .as_ref()
                    .and_then(|s| s.character.as_ref())
                    .map(|c| (c.rig.root, c.velocity))
                {
                    if let Some(cr) = map_s2c.get(&root) {
                        v.insert(*cr, vel);
                    }
                }
                v
            };
            // What the client simulates itself (parts its scripts made or
            // cloned) reads the client world's motion, as in Studio.
            let client_own = self
                .play
                .as_ref()
                .map(|s| s.client_sim.own_motion(&s.map.c2s))
                .unwrap_or_default();
            let mut client_velocities = client_velocities;
            client_velocities.extend(client_own.iter().map(|(r, m)| (*r, m.0)));
            drop(prep);
            let vehicle_input: Option<(Ref, Ref, f32, f32)> = current_seat
                .filter(|&seat| self.doc.class_of(seat).as_deref() == Some("VehicleSeat"))
                .and_then(|seat| map_s2c.get(&seat).copied().map(|cseat| (seat, cseat)))
                .map(|(seat, cseat)| {
                    let fwd = keys.contains("W") || keys.contains("Up");
                    let back = keys.contains("S") || keys.contains("Down");
                    let right = keys.contains("D") || keys.contains("Right");
                    let left = keys.contains("A") || keys.contains("Left");
                    let mut tf = (i32::from(fwd) - i32::from(back)) as f32;
                    let mut sf = (i32::from(right) - i32::from(left)) as f32;
                    if tf == 0.0 && sf == 0.0 {
                        for input in &gamepads.inputs {
                            if input.key == "Thumbstick1" {
                                if input.position.y.abs() > 0.14 {
                                    tf = input.position.y.clamp(-1.0, 1.0);
                                }
                                if input.position.x.abs() > 0.14 {
                                    sf = input.position.x.clamp(-1.0, 1.0);
                                }
                            }
                        }
                    }
                    (seat, cseat, tf, sf)
                });
            let mut vehicle_changed: Option<(Ref, i32, f32, i32, f32)> = None;
            // The client's own joints follow this frame's replicated parts
            // before its scripts run: parts only the client has (a place's
            // LocalScript-built limbs on a server Torso) were still where the
            // last frame's solve (p6d, after RenderStepped) left them, so
            // Heartbeat and RenderStepped read limbs a frame behind a moving
            // torso (0.27 studs at a walk, 60 fps). In Studio the owner's
            // step poses its whole assembly before PostSimulation.
            {
                let _cj = rustudio_core::bench::scope("p6a_client_joints");
                if let Some(s) = self.play.as_mut() {
                    if let Some(cd) = s.other_doc.as_mut() {
                        s.client_joints.solve(cd, &s.map.c2s);
                    }
                }
            }
            let lua_sc = rustudio_core::bench::scope("p6b_lua");
            let server_doc_id = self.doc.id;
            self.with_side(Side::Client, |rt, _, _| {
                if let Some((seat, cseat, tf, sf)) = vehicle_input {
                    let ti = if tf > 0.0 { 1 } else if tf < 0.0 { -1 } else { 0 };
                    let si = if sf > 0.0 { 1 } else if sf < 0.0 { -1 } else { 0 };
                    let (cur_t, cur_tf, cur_s, cur_sf) = rt
                        .world_mut()
                        .map(|w| {
                            let d = w.d();
                            let i32p = |k: &str| match d.prop(cseat, k) {
                                Some(Variant::Int32(n)) => n,
                                Some(Variant::Float32(f)) => f as i32,
                                _ => 0,
                            };
                            let f32p = |k: &str| match d.prop(cseat, k) {
                                Some(Variant::Float32(f)) => f,
                                Some(Variant::Int32(n)) => n as f32,
                                _ => 0.0,
                            };
                            (i32p("Throttle"), f32p("ThrottleFloat"), i32p("Steer"), f32p("SteerFloat"))
                        })
                        .unwrap_or((0, 0.0, 0, 0.0));
                    if cur_t != ti || (cur_tf - tf).abs() > 1e-4 || cur_s != si || (cur_sf - sf).abs() > 1e-4 {
                        if cur_t != ti {
                            rt.apply_prop(cseat, "Throttle", Variant::Int32(ti));
                        }
                        if (cur_tf - tf).abs() > 1e-4 {
                            rt.apply_prop(cseat, "ThrottleFloat", Variant::Float32(tf));
                        }
                        if cur_s != si {
                            rt.apply_prop(cseat, "Steer", Variant::Int32(si));
                        }
                        if (cur_sf - sf).abs() > 1e-4 {
                            rt.apply_prop(cseat, "SteerFloat", Variant::Float32(sf));
                        }
                        vehicle_changed = Some((seat, ti, tf, si, sf));
                    }
                }
                if let Some(mut w) = rt.world_mut() {
                    w.velocities = client_velocities;
                    // Angular velocities / awake counts for the client's
                    // scripts (domains/world_part.rs reads them by document).
                    let _s = rustudio_core::bench::scope("p6b_mirror");
                    rustudio_physics::physics::live::mirror(server_doc_id, w.d().id, &map_s2c);
                    rustudio_physics::physics::live::overlay_spins(
                        w.d().id,
                        client_own.iter().map(|(r, m)| (*r, (m.1, m.2))),
                    );
                    drop(_s);
                    w.keys_down = keys.clone();
                    w.humanoid_states = client_states;
                    if !gui_layout.is_empty() {
                        // (the client's plugin widgets keep their boxes:
                        // plugin_host/widgets_ui.rs lays them out)
                        let old = std::mem::replace(&mut w.input.gui_layout, gui_layout);
                        host.keep_play_layout(&old, &mut w.input.gui_layout);
                    }
                    w.input.menu_open = menu_open;
                }
                // AbsoluteSize / AbsolutePosition / ViewportSize changes from
                // the new layout (a resized window refits screen-sized GUIs).
                {
                    let _s = rustudio_core::bench::scope("p6b_layout_changes");
                    let _ = rustudio_engine::runtime::ext::fire_layout_changes(&rt.lua);
                }
                // GuiService.MenuOpened / MenuClosed (the Esc menu).
                if let Some(gs) = gui_service {
                    for open in &menu_events {
                        rt.fire(gs, if *open { "MenuOpened" } else { "MenuClosed" }, vec![]);
                    }
                }
                // Input reaches ContextActionService's bound actions, then
                // UserInputService (InputBegan / InputChanged / InputEnded,
                // PointerAction for the wheel), one InputObject per device.
                let ev = |state: &str,
                          uit: &str,
                          key: &str,
                          pos: (f64, f64, f64),
                          delta: (f64, f64),
                          gpe: bool| {
                    let s = |x: &str| Value::String(rt.lua.create_string(x).expect("lua string"));
                    rt.spawn_global(
                        "__rs_input_event",
                        vec![
                            s(state),
                            s(uit),
                            s(key),
                            Value::Number(pos.0),
                            Value::Number(pos.1),
                            Value::Number(pos.2),
                            Value::Number(delta.0),
                            Value::Number(delta.1),
                            Value::Boolean(gpe),
                        ],
                    );
                };
                rt.spawn_global("__rs_input_frame_end", vec![Value::Number(dt as f64)]);
                // A press reaches UserInputService.InputBegan before the GUI's
                // MouseButton1Down / Up / Click / Activated, even when press and
                // release land in the same frame (the harness's --click).
                let mouse_buttons = |begin: bool| {
                    for (button, down, processed) in &mouse_events {
                        if *down != begin {
                            continue;
                        }
                        let uit = if *button == 1 { "MouseButton1" } else { "MouseButton2" };
                        ev(
                            if *down { "Begin" } else { "End" },
                            uit,
                            "Unknown",
                            (mx, my, 0.0),
                            (0.0, 0.0),
                            *processed,
                        );
                    }
                };
                mouse_buttons(true);
                game::fire_gui_events(rt, gui_events, gui_service);
                game::step_smooth_scrolls(rt, dt);
                if let Some(on) = window_focus_change {
                    rt.spawn_global("__rs_window_focus", vec![Value::Boolean(on)]);
                }
                if mouse_moved {
                    ev(
                        "Change",
                        "MouseMovement",
                        "Unknown",
                        (mx, my, 0.0),
                        (mouse_delta.x as f64, mouse_delta.y as f64),
                        mouse_move_processed,
                    );
                }
                if wheel != 0.0 {
                    ev(
                        "Change",
                        "MouseWheel",
                        "Unknown",
                        (mx, my, wheel as f64),
                        (0.0, 0.0),
                        wheel_processed,
                    );
                }
                // Button releases after the GUI's events (Up / Click / Activated).
                mouse_buttons(false);
                phase.fire_touches(rt, Some(&map_s2c));
                for (a, b, started) in &client_touches {
                    let (ev, twin) = if *started { ("Touched", "LocalSimulationTouched") } else { ("TouchEnded", "StoppedTouching") };
                    rt.fire(*a, ev, vec![rt.inst(*b)]);
                    rt.fire(*b, ev, vec![rt.inst(*a)]);
                    if *started {
                        rustudio_engine::rig::fire_humanoid_touched(rt, *a, *b);
                    }
                    rt.fire(*a, twin, vec![rt.inst(*b)]);
                    rt.fire(*b, twin, vec![rt.inst(*a)]);
                }
                let cseat = seat_server.and_then(|s| map_s2c.get(&s).copied());
                // Roblox switches CurrentCamera.CameraSubject to a VehicleSeat
                // on sit (activating PlayerModule's VehicleCamera) and restores
                // it to the Humanoid on stand.
                if let (Some(sc), Some(chum)) = (&phase.seat_change, humanoid_client) {
                    let cam_update = rt.world_mut().and_then(|w| {
                        let d = w.d();
                        let ws = d.workspace()?;
                        let cam = match d.prop(ws, "CurrentCamera") {
                            Some(Variant::Ref(c)) if d.exists(c) => c,
                            _ => return None,
                        };
                        let cur = match d.prop(cam, "CameraSubject") {
                            Some(Variant::Ref(r)) if r.is_some() => Some(r),
                            _ => None,
                        };
                        if sc.active {
                            let s = cseat?;
                            if d.class_of(s).as_deref() == Some("VehicleSeat") && cur == Some(chum) {
                                Some((cam, s))
                            } else {
                                None
                            }
                        } else if cur.is_some()
                            && (cur == cseat
                                || cur.is_some_and(|r| {
                                    !d.exists(r) || d.class_of(r).as_deref() == Some("VehicleSeat")
                                }))
                        {
                            Some((cam, chum))
                        } else {
                            None
                        }
                    });
                    if let Some((cam, subj)) = cam_update {
                        rt.set(cam, "CameraSubject", rt.inst(subj));
                    }
                }
                phase.fire_humanoid(rt, humanoid_client, cseat, true);
                phase.fire_npcs(rt, Some(&map_s2c));
                if let Some(mut w) = rt.world_mut() {
                    w.input.gamepads = gamepads.connected;
                }
                if let Some(uis) = cuis {
                    let pad = |i: usize| {
                        LEnumItem::from_name("UserInputType", &format!("Gamepad{}", i + 1))
                            .and_then(|e| rustudio_engine::ltypes::enum_value(&rt.lua, e).ok())
                            .unwrap_or(Value::Nil)
                    };
                    for i in 0..8 {
                        if gamepads.removed & (1 << i) != 0 {
                            rt.fire(uis, "GamepadDisconnected", vec![pad(i)]);
                        }
                        if gamepads.added & (1 << i) != 0 {
                            rt.fire(uis, "GamepadConnected", vec![pad(i)]);
                        }
                    }
                }
                if !gamepads.inputs.is_empty() {
                    rustudio_engine::gui::share().selection_visible = true;
                }
                for input in &gamepads.inputs {
                    ev(
                        input.state,
                        &format!("Gamepad{}", input.slot + 1),
                        input.key,
                        (f64::from(input.position.x), f64::from(input.position.y), 0.0),
                        (f64::from(input.delta.x), f64::from(input.delta.y)),
                        false,
                    );
                }
                // Ctrl+Shift+F7 toggles PerformanceStats overlay (like Studio/Client).
                let perf_shortcut = *perf_stats_toggle
                    || (began.iter().any(|(k, processed)| !processed && k == "F7")
                        && keys.contains("LeftControl")
                        && keys.contains("LeftShift"));
                if perf_shortcut {
                    let _ = rustudio_engine::runtime::security::with_main_identity(
                        &rt.lua,
                        rustudio_engine::runtime::security::CORE,
                        || {
                            rt.lua
                                .load(r#"local g = UserSettings():GetService("UserGameSettings"); g.PerformanceStatsVisible = not g.PerformanceStatsVisible"#)
                                .set_name("=perf_stats_shortcut")
                                .exec()
                        },
                    );
                }
                for (list, state) in [(&began, "Begin"), (&ended, "End")] {
                    for (k, processed) in list {
                        ev(state, "Keyboard", k, (0.0, 0.0, 0.0), (0.0, 0.0), *processed);
                    }
                }
                if let Some(rs) = crs {
                    // A client frame in Roblox's order: PreRender (RenderStepped),
                    // PreAnimation, PreSimulation (Stepped), PostSimulation (Heartbeat).
                    // With the split frame order (the dynamic Humanoid) the
                    // events before the world steps fired there, and
                    // RenderStepped is the last thing before the frame is
                    // drawn: Studio renders the state RenderStepped leaves,
                    // so the camera a script placed there and the character
                    // it followed are drawn together (fired before the world
                    // steps, the drawn character was a frame of motion ahead
                    // of its camera: checks/humanoid-physics/feel).
                    let d = || Value::Number(dt as f64);
                    {
                        // PostSimulation and Heartbeat with everything they
                        // resume (waits end at Heartbeat) run before
                        // RenderStepped fires: what a thread connects or
                        // sets when Heartbeat:Wait() or task.wait() returns
                        // is there for the render step that follows, as in
                        // Studio, where that render step opens the next
                        // frame. (Fired together, the render step missed a
                        // connection made at Heartbeat and a script's first
                        // Move came a frame late: tools/fidelity sprint.)
                        rt.fire(rs, "PostSimulation", vec![d()]);
                        rt.fire(rs, "Heartbeat", vec![d()]);
                        if let Some(mut w) = rt.world_mut() {
                            w.realtime = realtime;
                        }
                        rt.step(dt as f64);
                        if studio_path() {
                            if let Some(mut w) = rt.world_mut() {
                                hum_glue::scriptable_camera_focus(w.dm());
                            }
                        }
                        rt.fire(rs, "RenderStepped", vec![d()]);
                        rt.fire(rs, "PreRender", vec![d()]);
                        rt.dispatch();
                    }
                }
                if let Some(mut w) = rt.world_mut() {
                    w.realtime = realtime;
                }
                let _s = rustudio_core::bench::scope("p6b_rt_step");
                if crs.is_none() {
                    rt.step(dt as f64);
                }
            });
            drop(lua_sc);
            if let Some((seat, ti, tf, si, sf)) = vehicle_changed {
                self.with_side(Side::Server, |rt, _, _| {
                    rt.apply_prop(seat, "Throttle", Variant::Int32(ti));
                    rt.apply_prop(seat, "ThrottleFloat", Variant::Float32(tf));
                    rt.apply_prop(seat, "Steer", Variant::Int32(si));
                    rt.apply_prop(seat, "SteerFloat", Variant::Float32(sf));
                    rt.dispatch();
                });
            }
            self.fire_server_gui_buttons(server_buttons);
            let req = rustudio_core::bench::scope("p6c_requests");
            self.game_requests(Side::Client);
            self.read_client_input_state();
            // Shift lock / first person: the root turns to the camera RenderStepped
            // left, before the frame is drawn (play_humanoid.rs `hum_face_camera`).
            if mode == RunMode::Play && studio_path() {
                self.hum_face_camera();
            }
            self.deliver_remotes();
            // Client -> Server Animator replication for the local player's character
            // (in Studio, the owning client drives its character's Animator and
            // replicates those tracks back to the server Animator).
            if let Some((s_anim, c_anim)) = self.play.as_ref().and_then(|s| {
                let hum = s.character.as_ref()?.humanoid;
                let sa = self
                    .doc
                    .dom
                    .get_by_ref(hum)?
                    .children()
                    .iter()
                    .copied()
                    .find(|r| self.doc.class_of(*r).is_some_and(|c| c == "Animator"))?;
                let ca = s.map.s2c.get(&sa).copied()?;
                Some((sa, ca))
            }) {
                let snaps = self
                    .with_side(Side::Client, |rt, _, _| {
                        rustudio_engine::anim_replicate::collect_client_tracks(&rt.lua, c_anim)
                    })
                    .unwrap_or_default();
                self.with_side(Side::Server, |rt, _, _| {
                    rustudio_engine::anim_replicate::sync_from_client(&rt.lua, s_anim, &snaps);
                });
            }
            drop(req);
            // The client's own assemblies (its simulation step in Studio).
            let _cj = rustudio_core::bench::scope("p6d_client_joints");
            if let Some(s) = self.play.as_mut() {
                if let Some(cd) = s.other_doc.as_mut() {
                    s.client_joints.solve(cd, &s.map.c2s);
                }
            }
            drop(_cj);
            let _cp = rustudio_core::bench::scope("p6e_client_physics");
            let server_doc = self.doc.id;
            let mut client_fallen_signals: Vec<(Ref, Ref, Vec<Ref>)> = Vec::new();
            if let Some(s) = self.play.as_mut() {
                // Velocities client scripts gave parts (the character's own
                // stay for client_root_movers).
                let own: HashSet<Ref> =
                    s.physics.skip.iter().filter_map(|r| s.map.s2c.get(r).copied()).collect();
                let vel: Vec<(Ref, glam::Vec3)> = match s.client.as_mut().and_then(|c| c.world.as_mut()) {
                    Some(mut w) => {
                        let (keep, take) = std::mem::take(&mut w.velocity_requests)
                            .into_iter()
                            .partition(|(r, _)| own.contains(r));
                        w.velocity_requests = keep;
                        take
                    }
                    None => Vec::new(),
                };
                // … and the assemblies this player owns besides its character
                // (SetNetworkOwner(player)): in Studio the owning client simulates
                // them, so its scripts' AssemblyLinearVelocity / AssemblyAngularVelocity
                // writes and impulses move them for everyone (probe T23_kickvel /
                // T23_kickimp: a client-owned swing set going by a LocalScript swings
                // and carries the character standing on it). Here the server's world
                // is the one simulation: they go there. (Replicated parts the client
                // does not own stay as they were: Studio ignores those writes.)
                // (RUSTUDIO_SOLVER=studio only: the rapier path keeps its behaviour.)
                let player = s.player;
                let roots: Vec<Ref> = if studio_path() {
                    rustudio_engine::runtime::domains::world::part::player_owned_roots_of(server_doc, player)
                } else {
                    Vec::new()
                };
                let owned = s.physics.owned_bodies(&roots);
                s.physics.client_owned = owned.clone();
                let vel: Vec<(Ref, glam::Vec3)> = if owned.is_empty() {
                    vel
                } else {
                    let (mine, rest): (Vec<_>, Vec<_>) =
                        vel.into_iter().partition(|(r, _)| s.map.c2s.get(r).is_some_and(|sr| s.physics.in_bodies(*sr, &owned)));
                    for (r, v) in mine {
                        if let Some(sr) = s.map.c2s.get(&r).copied() {
                            s.physics.set_velocity(sr, v);
                        }
                    }
                    if let Some(cid) = s.other_doc.as_ref().map(|d| d.id) {
                        let pick = |r: Ref| s.map.c2s.get(&r).is_some_and(|sr| s.physics.in_bodies(*sr, &owned));
                        let sets = rustudio_physics::physics::live::take_spin_sets_of(cid, pick);
                        let adds = rustudio_physics::physics::live::take_spins_of(cid, pick);
                        for (r, w, add) in sets.into_iter().map(|(r, w)| (r, w, false)).chain(adds.into_iter().map(|(r, w)| (r, w, true))) {
                            if let Some(sr) = s.map.c2s.get(&r).copied() {
                                s.physics.set_spin(sr, w, add);
                            }
                        }
                    }
                    rest
                };
                // What the character's step did to those stand-ins goes to the bodies.
                let linked = studio_path() && s.physics.link_active();
                if linked {
                    let kicks = s.physics.link_kicks();
                    let bodies = s.physics.link_bodies();
                    s.client_sim.kick(&kicks, &bodies);
                }
                if let Some(cd) = s.other_doc.as_mut() {
                    let out = s.client_sim.step(&s.physics, cd, &s.map.s2c, &s.map.c2s, &vel, dt);
                    if linked {
                        let motion = s.client_sim.link_motion();
                        s.physics.link_note_motion(&motion, dt);
                    }
                    let can_touch = |r: Ref| !matches!(cd.prop(r, "CanTouch"), Some(Variant::Bool(false)));
                    s.client_touches
                        .extend(out.touches.into_iter().filter(|(a, b, _)| can_touch(*a) && can_touch(*b)));
                    let (c_sigs, _) = unparent_fallen_parts(cd, &out.fallen);
                    client_fallen_signals.extend(c_sigs);
                    // Its step published the client world's stats under the
                    // client's document: the server's mirror goes back under
                    // them for scripts that run before the next frame's.
                    if s.client_sim.has_bodies() {
                        rustudio_physics::physics::live::mirror(server_doc_id, cd.id, &s.map.s2c);
                        let own = s.client_sim.own_motion(&s.map.c2s);
                        rustudio_physics::physics::live::overlay_spins(cd.id, own.into_iter().map(|(r, m)| (r, (m.1, m.2))));
                    }
                }
            }
            for (r, old, below) in client_fallen_signals {
                self.with_side(Side::Client, |rt, _, _| rt.notify_parent(r, old, Ref::none(), below));
            }
        }
        drop(sc);

        self.collect_garbage(dt, host);

        // 6. Deaths and respawns.
        let (died, reload) = std::mem::take(&mut phase.server_out);
        let my_humanoid = self
            .play
            .as_ref()
            .and_then(|s| s.character.as_ref())
            .map(|c| c.humanoid);
        if my_humanoid.is_some_and(|h| died.contains(&h)) {
            if let Some(s) = self.play.as_mut() {
                s.state_log.push("Dead".into());
            }
            self.kill_character();
        }
        let due = self
            .play
            .as_ref()
            .is_some_and(|s| s.respawn_at.is_some_and(|t| s.time >= t));
        if due || !reload.is_empty() {
            self.respawn();
        }
        let initial_due = self.play.as_mut().is_some_and(|s| {
            let Some(frames) = s.initial_spawn_frames.as_mut() else { return false };
            *frames -= 1;
            if *frames != 0 {
                return false;
            }
            s.initial_spawn_frames = None;
            true
        });
        if initial_due && self.character_auto_loads() {
            self.spawn_character();
        }
        self.replicate();

        self.game_tick_end();

        // 7. The view is the client's Workspace.CurrentCamera, whatever its
        // CameraType: Roblox's CameraModule (or the place's own camera
        // script) moves it. Looking at the server side frees the editor camera.
        let client_cam = (mode == RunMode::Play
            && self
                .play
                .as_ref()
                .is_some_and(|s| s.showing == Side::Client))
        .then(|| self.play.as_ref().and_then(|s| s.other_doc.as_ref()))
        .flatten()
        .and_then(|cd| {
            let ws = cd.workspace()?;
            let cam = match cd.prop(ws, "CurrentCamera") {
                Some(Variant::Ref(r)) if cd.exists(r) => r,
                _ => cd.find_child(ws, "Camera")?,
            };
            let xf = rustudio_render::scene::xform_of(cd, cam)?;
            let fov = f32_of(cd.prop(cam, "FieldOfView")).unwrap_or(mech::FOV_DEG);
            // ProjectionType Orthographic (domains/world_camera.rs).
            let ortho = (rustudio_render::scene::enum_u32(cd.prop(cam, "ProjectionType")) == Some(1)).then(|| {
                (
                    f32_of(cd.prop(cam, "OrthographicSize")).unwrap_or(35.0),
                    rustudio_render::scene::enum_u32(cd.prop(cam, "OrthographicMode")).unwrap_or(0),
                )
            });
            let stretch = f32_of(cd.prop(cam, "ViewStretch")).unwrap_or(1.0);
            Some((xf, fov, ortho, stretch))
        });
        if let Some((xf, fov, ortho, stretch)) = client_cam {
            self.cam.ortho = ortho;
            self.cam.stretch = stretch;
            let look = -xf.rot.z_axis;
            self.cam.pos = xf.pos;
            self.cam.set_rotation(xf.rot);
            self.cam.focus = xf.pos + look * 10.0;
            self.cam.fov_deg = fov;
            self.cam.mode = CameraMode::Fly;
        } else {
            self.cam.ortho = None;
            self.cam.stretch = 1.0;
        }
        // RUSTUDIO_FEEL_TRACE: what this frame draws (the view's camera and
        // the character's root as they are when the tick ends), for
        // checks/humanoid-physics/feel.
        if feel_trace() {
            if let Some(s) = self.play.as_ref() {
                let root = s.character.as_ref().and_then(|c| {
                    let cr = s.map.s2c.get(&c.rig.root).copied()?;
                    rustudio_render::scene::xform_of(s.other_doc.as_ref()?, cr)
                });
                if let Some(r) = root {
                    let yaw = |look: Vec3| (-look.x).atan2(-look.z);
                    rustudio_core::log::tee_log(&format!(
                        "FEELR|{:.4}|{:.4}|{:.4},{:.4},{:.4}|{:.5}|{:.4},{:.4},{:.4}|{:.5}",
                        s.time,
                        dt,
                        r.pos.x,
                        r.pos.y,
                        r.pos.z,
                        yaw(-r.rot.z_axis),
                        self.cam.pos.x,
                        self.cam.pos.y,
                        self.cam.pos.z,
                        yaw(self.cam.forward()),
                    ));
                }
            }
        }
        self.shown_front();
    }


    /// Runs a Luau chunk on one side of the Play session (test harness);
    /// its prints go to Output like a script's. Returns an error message.
    pub fn run_luau(&mut self, side: Side, code: &str) -> Option<String> {
        self.run_luau_as(side, code, rustudio_engine::runtime::security::COMMAND)
    }

    /// `run_luau` at identity `id` (the in-game menu runs as a CoreScript).
    pub fn run_luau_as(&mut self, side: Side, code: &str, id: u8) -> Option<String> {
        if self.play_thread.is_some() {
            return self.threaded_luau(side, code, id);
        }
        self.server_front();
        let r = self.with_side(side, |rt, _, _| {
            // The given identity, for this chunk and what it spawns.
            let out = rustudio_engine::runtime::security::with_main_identity(&rt.lua, id, || {
                // Scripts' _G and shared, with every host global.
                let env = rustudio_engine::globals_parity::harness_env(&rt.lua)?;
                // As a thread, like the Studio command bar: the chunk may
                // yield (task.wait), and its errors still come back here.
                let f = rt.lua.load(code).set_name("=harness").set_environment(env).into_function()?;
                let th = rustudio_engine::runtime::domains::assets_cloud::scene::create_thread(&rt.lua, f)?;
                th.resume::<mlua::MultiValue>(()).map(|_| ())
            })
            .err()
            .map(|e| rustudio_engine::runtime::describe_error(&e));
            rt.dispatch();
            out
        });
        self.shown_front();
        r.flatten()
    }

    /// The chat button state and free rectangle of the client's top bar.
    pub(crate) fn play_topbar(&self) -> (bool, Option<rustudio_engine::ltypes::LRect>) {
        let Some(s) = self.play.as_ref() else {
            return (true, None);
        };
        if let Some(view) = s.presentation.as_ref() {
            return (view.chat_on, view.topbar_inset);
        }
        s.client
            .as_ref()
            .and_then(|c| c.world.as_ref())
            .map_or((true, None), |w| (!w.core_gui_off.contains("Chat"), w.topbar_inset))
    }

    /// `with_side` from outside the Play tick (the editor's UI, the plugin
    /// host): the server is put in front first and the shown side back
    /// after, so each side's runtime gets its own DataModel whichever side
    /// the editor shows.
    pub fn with_play_side<R>(
        &mut self,
        side: Side,
        f: impl FnOnce(&Runtime, Option<&Document>, &mut RepMap) -> R,
    ) -> Option<R> {
        self.server_front();
        let r = self.with_side(side, f);
        self.shown_front();
        r
    }

    /// The Play session's sides (server, and the client in Play).
    pub fn play_sides(&self) -> Vec<Side> {
        match self.play.as_ref() {
            Some(s) if s.mode == RunMode::Play => vec![Side::Server, Side::Client],
            Some(_) => vec![Side::Server],
            None => Vec::new(),
        }
    }

    /// A side's Lua state between frames (read its app data; don't call
    /// into the runtime with it).
    pub fn play_side_lua(&self, side: Side) -> Option<&mlua::Lua> {
        let s = self.play.as_ref()?;
        match side {
            Side::Server => s.server.as_ref().map(|server| &server.runtime.lua),
            Side::Client => s.client.as_ref().map(|c| &c.runtime.lua),
        }
    }

    /// A side's DataModel between frames (the shown one is `self.doc`).
    pub fn play_side_doc_mut(&mut self, side: Side) -> Option<&mut Document> {
        let s = self.play.as_mut()?;
        if s.showing == side {
            Some(&mut self.doc)
        } else {
            s.other_doc.as_mut()
        }
    }

    /// The other side's DataModel while playing (the one not shown).
    pub fn play_other_doc(&self) -> Option<&Document> {
        self.play.as_ref()?.other_doc.as_ref()
    }

    /// Status text for the toolbar / status bar.
    pub fn play_status(&self) -> Option<String> {
        if self.play_thread.is_some() {
            return self.threaded_status();
        }
        let s = self.play.as_ref()?;
        let what = if s.mode == RunMode::Play {
            "Playing"
        } else {
            "Running"
        };
        let ch = match &s.character {
            Some(c) if c.dead => " · respawning".to_string(),
            Some(c) => {
                let h = if s.showing == Side::Server {
                    self.humanoid_f32(c.humanoid, "Health", 100.0)
                } else {
                    100.0
                };
                format!(" · {} · health {h:.0}", c.state.enum_name())
            }
            None => String::new(),
        };
        let side = if s.client.is_some() {
            format!(" · viewing {}", s.showing.label())
        } else {
            String::new()
        };
        Some(format!("{what} {:.1} s{ch}{side}", s.time))
    }
}

/// `RUSTUDIO_FEEL_TRACE` is set (the per-frame FEELR lines, with RUSTUDIO_LOG_TEE).
fn feel_trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_FEEL_TRACE").is_some())
}

fn script_enabled(d: &Document, r: Ref) -> bool {
    !matches!(d.prop(r, "Disabled"), Some(Variant::Bool(true)))
        && !matches!(d.prop(r, "Enabled"), Some(Variant::Bool(false)))
}

/// LocalScripts (and RunContext=Client Scripts) under the given client instances.
/// What a character spawn leaves in PlayerGui: a LayerCollector (ScreenGui,
/// BillboardGui, SurfaceGui) with ResetOnSpawn false. Everything else there
/// goes (measured in Studio Play, a script's Folder too).
fn kept_on_spawn(d: &Document, r: Ref) -> bool {
    d.class_of(r).is_some_and(|c| doc::is_a(&c, "LayerCollector"))
        && matches!(d.prop(r, "ResetOnSpawn"), Some(Variant::Bool(false)))
}

/// Puts Studio's data model patch in the client's DataModel
/// (core_scripts::install). Returns what to start and a line for Output.
fn load_core_scripts(cdoc: &mut Document) -> Result<(rustudio_core::core_scripts::Installed, String), String> {
    let path = rustudio_core::core_scripts::patch_path().ok_or_else(|| {
        format!("CoreScripts: no {} in the Studio install", rustudio_core::core_scripts::PATCH)
    })?;
    let t0 = std::time::Instant::now();
    let (installed, cached) =
        rustudio_core::core_scripts::install_cached(cdoc, &path).map_err(|e| format!("CoreScripts: {e}"))?;
    let note = format!(
        "CoreScripts: {} modules and {} CoreScripts from {} ({} ms, {})",
        installed.modules.len(),
        installed.scripts.len(),
        path.display(),
        t0.elapsed().as_millis(),
        if cached { "cached" } else { "decoded" }
    );
    rustudio_core::load_trace::mark("play: core scripts installed");
    Ok((installed, note))
}

fn client_scripts(cd: &Document, under: &[Option<Ref>]) -> Vec<Ref> {
    let mut out = Vec::new();
    for u in under.iter().flatten() {
        for i in cd.dom.descendants_of(*u) {
            let client_script = i.class == "Script"
                && rustudio_render::scene::enum_u32(cd.prop(i.referent(), "RunContext")) == Some(2);
            if (i.class == "LocalScript" || client_script) && script_enabled(cd, i.referent()) {
                out.push(i.referent());
            }
        }
    }
    out
}

/// The character's pose: capsule → HumanoidRootPart → Motor6D chain, each
/// joint turned by its Motor6D.Transform.
fn character_pose(ch: &Character, transforms: &HashMap<Ref, Xform>) -> Vec<(Ref, Xform)> {
    let rot = Mat3::from_rotation_y(ch.yaw) * Mat3::from_rotation_x(ch.pitch);
    let root = Xform {
        pos: ch.body.pos + Vec3::Y * ch.root_lift,
        rot,
    };
    ch.rig.solve(root, transforms)
}

/// The Humanoid controller's view of a character (round 22): its hip
/// (R6: the 2-stud legs), root size and the box its torso, head and
/// root fill in the root's frame, from the root's bottom up (Studio's limbs
/// don't collide; anything under the root's bottom is the floor rays' job).
fn hum_shape(doc: &Document, model: Ref, root_lift: f32) -> rustudio_physics::physics::HumShape {
    let root = doc.find_named(model, "HumanoidRootPart");
    let root_xf = root.and_then(|r| rustudio_render::scene::xform_of(doc, r));
    let size = root.and_then(|r| rustudio_render::scene::vec3_of(doc.prop(r, "Size"))).unwrap_or(Vec3::new(2.0, 2.0, 1.0));
    let hip = rig::root_height(doc, model) - size.y * 0.5;
    let mut lo = -size * 0.5;
    let mut hi = size * 0.5;
    for inst in doc.dom.descendants_of(model) {
        if !doc::is_a(&inst.class, "BasePart") {
            continue;
        }
        let r = inst.referent();
        let s = rustudio_render::scene::vec3_of(doc.prop(r, "Size")).unwrap_or(Vec3::ONE);
        if !matches!(inst.name.as_str(), "UpperTorso" | "LowerTorso" | "Torso" | "Head") {
            continue;
        }
        let (Some(rx), Some(px)) = (root_xf, rustudio_render::scene::xform_of(doc, r)) else { continue };
        let inv = rx.rot.transpose();
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { -s.x } else { s.x },
                if i & 2 == 0 { -s.y } else { s.y },
                if i & 4 == 0 { -s.z } else { s.z },
            ) * 0.5;
            let w = px.pos + px.rot * c;
            let l = inv * (w - rx.pos);
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    // From the root's bottom up, whatever hangs lower.
    lo.y = -size.y * 0.5;
    let mut mass = 0.0f32;
    let mut parts = 0u32;
    for inst in doc.dom.descendants_of(model) {
        if doc::is_a(&inst.class, "BasePart") {
            parts += 1;
            if let Some(s) = rustudio_render::scene::vec3_of(doc.prop(inst.referent(), "Size")) {
                mass += s.x * s.y * s.z * mech::PART_DENSITY;
            }
        }
    }
    let r6 = doc.find_named(model, "UpperTorso").is_none() && doc.find_named(model, "Torso").is_some();
    rustudio_physics::physics::HumShape { hip, root_size: size, box_lo: lo, box_hi: hi, root_lift, mass, parts, r6 }
}

/// RUSTUDIO_SOLVER=studio (or studio-nextgen): the Play-side changes that came with the
/// Studio solver port (client-owned assemblies' script velocities, shift lock's turn
/// before the frame is drawn) apply; the default rapier path keeps its old behaviour.
fn studio_path() -> bool {
    rustudio_physics::solver::kind() != rustudio_physics::solver::SolverKind::Rapier
}

impl Game {
    /// The parts whose touches a script hears, on either side (`World::touch_refs`):
    /// the part itself, or every part of a character whose Humanoid.Touched is heard
    /// (rig.rs `fire_humanoid_touched`: the model's direct BasePart children). Given to
    /// the physics when a side's set grew.
    fn sync_touch_interest(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(server) = s.server.as_ref() else { return };
        let srev = server.with_world(|w| w.touch_rev).unwrap_or(0);
        let crev = s.client.as_ref().and_then(|c| c.with_world(|w| w.touch_rev)).unwrap_or(0);
        if s.touch_seen == Some((srev, crev)) {
            return;
        }
        s.touch_seen = Some((srev, crev));
        let mut refs: Vec<Ref> = server.with_world(|w| w.touch_refs.iter().copied().collect()).unwrap_or_default();
        let client: Vec<Ref> = s.client.as_ref().and_then(|c| c.with_world(|w| w.touch_refs.iter().copied().collect())).unwrap_or_default();
        for r in client {
            refs.push(r);
            if let Some(sr) = s.map.c2s.get(&r) {
                refs.push(*sr);
            }
        }
        let mut parts: rustc_hash::FxHashSet<Ref> = Default::default();
        for r in refs {
            parts.insert(r);
            for d in [Some(&self.doc), s.other_doc.as_ref()].into_iter().flatten() {
                let Some(i) = d.dom.get_by_ref(r) else { continue };
                if i.class != "Humanoid" {
                    continue;
                }
                let model = i.parent();
                for c in d.dom.get_by_ref(model).map(|m| m.children().to_vec()).unwrap_or_default() {
                    if d.dom.get_by_ref(c).is_some_and(|ci| doc::is_a(&ci.class, "BasePart")) {
                        parts.insert(c);
                        if let Some(sr) = s.map.c2s.get(&c) {
                            parts.insert(*sr);
                        }
                    }
                }
            }
        }
        s.physics.set_touch_interest(parts);
    }
}

#[cfg(test)]
mod client_start_order_tests {
    use crate::game::Game;
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_engine::runtime::{RunMode, Side};

    #[test]
    fn play_uses_workspace_signal_behavior_on_both_sides() {
        use super::SignalMode;
        use rbx_dom_weak::{types::Enum, ustr};

        let mut game = Game::new(None);
        let workspace = game.doc.workspace().unwrap();
        game.doc.dom.get_by_ref_mut(workspace).unwrap().properties.insert(
            ustr("SignalBehavior2"),
            Enum::from_u32(2).into(),
        );
        game.start_play(RunMode::Play, &mut (), false, None);
        let play = game.play.as_ref().unwrap();
        for side in [&play.server, &play.client] {
            let world = side.as_ref().unwrap().world.as_ref().unwrap();
            assert_eq!(world.signal_mode, SignalMode::Deferred);
        }
    }

    /// Studio Play: a ReplicatedFirst LocalScript runs before the
    /// StarterPlayerScripts and StarterGui ones. A Future test place's
    /// boot cover (made in ReplicatedFirst, found and removed by a
    /// StarterPlayerScripts script) stayed black because rustudio ran the
    /// PlayerScripts first, which made a second cover nobody removed.
    #[test]
    fn replicated_first_local_scripts_run_first() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let ls = |src: &str| InstanceBuilder::new("LocalScript").with_property("Source", src.to_string());
        let rf = game.doc.find_child(root, "ReplicatedFirst")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("ReplicatedFirst").with_name("ReplicatedFirst")));
        let sp = game.doc.find_child(root, "StarterPlayer")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("StarterPlayer").with_name("StarterPlayer")));
        let sps = game.doc.find_child(sp, "StarterPlayerScripts")
            .unwrap_or_else(|| game.doc.dom.insert(sp, InstanceBuilder::new("StarterPlayerScripts").with_name("StarterPlayerScripts")));
        let sg = game.doc.find_child(root, "StarterGui")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("StarterGui").with_name("StarterGui")));
        let gui = game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_name("G"));
        game.doc.dom.insert(sps, ls(r#"
            _G.order = (_G.order or "") .. "sps,"
            local pg = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
            _G.found = pg:FindFirstChild("Cover") ~= nil
        "#));
        game.doc.dom.insert(gui, ls(r#"_G.order = (_G.order or "") .. "gui,""#));
        game.doc.dom.insert(rf, ls(r#"
            _G.order = (_G.order or "") .. "rf,"
            local c = Instance.new("ScreenGui")
            c.Name = "Cover"
            c.Parent = game:GetService("Players").LocalPlayer:WaitForChild("PlayerGui")
        "#));
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        for _ in 0..30 {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
        assert_eq!(
            game.run_luau(Side::Client, r#"
                assert(string.sub(_G.order or "", 1, 3) == "rf,", "order " .. tostring(_G.order))
                assert(_G.found == true, "PlayerScripts script did not see ReplicatedFirst's cover")
            "#),
            None
        );
    }

    fn tick(game: &mut Game, n: usize) {
        let ctx = egui::Context::default();
        for _ in 0..n {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
    }

    #[test]
    fn engine_join_and_spawn_notify_server_scripts_before_client_gui() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let sss = game.doc.find_child(root, "ServerScriptService").unwrap();
        let sg = game.doc.find_child(root, "StarterGui").unwrap();
        game.doc.dom.insert(sss, InstanceBuilder::new("Script").with_property("Source", r#"
            local players = game:GetService("Players")
            players.ChildAdded:Connect(function(player)
                assert(player:IsA("Player") and player:FindFirstChildOfClass("Backpack"))
                Instance.new("Folder", player).Name = "Joined"
            end)
            players.PlayerAdded:Connect(function(player)
                player.Changed:Connect(function(property)
                    if property == "Character" then
                        Instance.new("Folder", player.Character).Name = "Assigned"
                        local tool = Instance.new("Tool")
                        tool.TextureId = "rbxassetid://123"
                        tool.Parent = player.Backpack
                    end
                end)
            end)
            workspace.ChildAdded:Connect(function(model)
                if players:GetPlayerFromCharacter(model) then
                    Instance.new("Folder", model).Name = "Parented"
                end
            end)
        "#.to_string()));
        let gui = game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui"));
        game.doc.dom.insert(gui, InstanceBuilder::new("LocalScript").with_property("Source", r#"
            local player = game:GetService("Players").LocalPlayer
            assert(player:FindFirstChild("Joined"), "join data missing when GUI started")
            assert(player.Character:FindFirstChild("Assigned"), "Character Changed data missing when GUI started")
            assert(player.Character:FindFirstChild("Parented"), "Workspace ChildAdded data missing when GUI started")
            local tool = player.Backpack:FindFirstChildOfClass("Tool")
            assert(tool and tool.TextureId == "rbxassetid://123", "spawn loadout missing when GUI started")
            _G.spawnReady = true
        "#.to_string()));
        game.start_play(RunMode::Play, &mut (), false, None);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local player = game:GetService("Players").LocalPlayer
                assert(player.Character == nil and _G.spawnReady == nil, "character or GUI ran at join")
                assert(not player:FindFirstChild("Joined"), "join data arrived in the bootstrap snapshot")
            "#),
            None
        );
        tick(&mut game, 5);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local player = game:GetService("Players").LocalPlayer
                assert(player:FindFirstChild("Joined"), "join data did not replicate")
                assert(player.Character == nil and _G.spawnReady == nil, "character or GUI ran before the sixth heartbeat")
            "#),
            None
        );
        tick(&mut game, 1);
        assert_eq!(
            game.run_luau(Side::Client, r#"assert(_G.spawnReady == true, "spawn GUI did not see initialized data")"#),
            None
        );
    }

    #[test]
    fn legacy_player_gui_clicks_reach_server_scripts() {
        use rustudio_engine::gui::{GuiArg, GuiEvent};

        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let sg = game.doc.find_child(root, "StarterGui").unwrap();
        game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_child(
            InstanceBuilder::new("TextButton").with_child(
                InstanceBuilder::new("Script").with_property("Source", r#"
                    _G.events = {}
                    script.Parent.MouseButton1Down:Connect(function(x, y)
                        table.insert(_G.events, x .. "," .. y)
                    end)
                    script.Parent.MouseButton1Click:Connect(function()
                        table.insert(_G.events, "clicked")
                    end)
                    script.Parent.Activated:Connect(function()
                        table.insert(_G.events, "activated")
                    end)
                "#.to_string()),
            ),
        ));
        game.play_shows_client = false;
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 6);
        let s = game.play.as_ref().unwrap();
        let gui = game.doc.find_child(s.player, "PlayerGui").unwrap();
        let button = game.doc.dom.descendants_of(gui).find(|i| i.class == "TextButton").unwrap().referent();
        let client = s.map.s2c[&button];
        game.fire_server_gui_buttons(vec![
            GuiEvent::Fire(client, "MouseButton1Down", vec![GuiArg::Num(12.0), GuiArg::Num(34.0)]),
            GuiEvent::Fire(client, "MouseButton1Click", vec![]),
            GuiEvent::Fire(client, "Activated", vec![]),
        ]);
        assert_eq!(
            game.run_luau(Side::Server, r#"assert(table.concat(_G.events, ";") == "12,34;clicked")"#),
            None,
        );
    }

    /// Studio Play (measured): ReplicatedFirst, then StarterPlayerScripts
    /// (no character yet), then CharacterAdded, and only then StarterGui's
    /// LocalScripts, copied into PlayerGui when the character spawns. A
    /// CharacterAdded handler's deferred work runs before they start.
    #[test]
    fn starter_gui_copies_in_after_character_added() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let ls = |src: &str| InstanceBuilder::new("LocalScript").with_property("Source", src.to_string());
        let sp = game.doc.find_child(root, "StarterPlayer")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("StarterPlayer").with_name("StarterPlayer")));
        let sps = game.doc.find_child(sp, "StarterPlayerScripts")
            .unwrap_or_else(|| game.doc.dom.insert(sp, InstanceBuilder::new("StarterPlayerScripts").with_name("StarterPlayerScripts")));
        let sg = game.doc.find_child(root, "StarterGui")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("StarterGui").with_name("StarterGui")));
        let gui = game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_name("G"));
        let sss = game.doc.find_child(root, "ServerScriptService")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("ServerScriptService")));
        game.doc.dom.insert(sss, InstanceBuilder::new("Script").with_property("Source", r#"
            local heartbeat = game:GetService("RunService").Heartbeat
            game:GetService("Players").PlayerAdded:Connect(function(player)
                local early = Instance.new("Folder")
                early.Name = "Early"
                early.Parent = player
                for _ = 1, 5 do heartbeat:Wait() end
                local afterBeat = Instance.new("Folder")
                afterBeat.Name = "AfterBeat"
                afterBeat.Parent = player
            end)
        "#.to_string()));
        game.doc.dom.insert(sps, ls(r#"
            local p = game:GetService("Players").LocalPlayer
            _G.log = { "sps char=" .. tostring(p.Character ~= nil) .. " g=" .. tostring(p.PlayerGui:FindFirstChild("G") ~= nil) }
            assert(not p:FindFirstChild("Early"), "PlayerAdded changes arrived in the bootstrap packet")
            p.ChildAdded:Connect(function(child)
                if child.Name == "Early" or child.Name == "AfterBeat" then
                    table.insert(_G.log, child.Name)
                end
            end)
            -- made before the first spawn: Studio keeps both through it
            Instance.new("ScreenGui", p.PlayerGui).Name = "PreGui"
            Instance.new("Folder", p.PlayerGui).Name = "PreFolder"
            p.CharacterAdded:Connect(function()
                table.insert(_G.log, "added")
                task.defer(function() table.insert(_G.log, "deferred") end)
            end)
        "#));
        game.doc.dom.insert(gui, ls(r#"
            local p = game:GetService("Players").LocalPlayer
            assert(p:FindFirstChild("AfterBeat"), "the GUI started before the player's replicated data")
            table.insert(_G.log, "gui char=" .. tostring(p.Character ~= nil))
        "#));
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local got = table.concat(_G.log, ",")
                assert(got == "sps char=false g=false,Early,AfterBeat,added,deferred,gui char=true", got)
                local pg = game:GetService("Players").LocalPlayer.PlayerGui
                assert(pg:FindFirstChild("PreGui") and pg:FindFirstChild("PreFolder"), "first spawn reset PlayerGui")
            "#),
            None
        );
    }

    /// Studio Play's respawn (measured): a ResetOnSpawn ScreenGui in PlayerGui
    /// is replaced by a fresh StarterGui copy, one a LocalScript made goes, a
    /// Folder too; a ResetOnSpawn false one (copied or made) stays, never
    /// copied twice.
    #[test]
    fn respawn_resets_player_gui() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let sg = game.doc.find_child(root, "StarterGui")
            .unwrap_or_else(|| game.doc.dom.insert(root, InstanceBuilder::new("StarterGui").with_name("StarterGui")));
        game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_name("Resets"));
        game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_name("Keeps").with_property("ResetOnSpawn", false));
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local pg = game:GetService("Players").LocalPlayer.PlayerGui
                _G.r, _G.k = pg:WaitForChild("Resets"), pg:WaitForChild("Keeps")
                local a = Instance.new("ScreenGui"); a.Name = "MadeResets"; a.Parent = pg
                local b = Instance.new("ScreenGui"); b.Name = "MadeKeeps"; b.ResetOnSpawn = false; b.Parent = pg
                local f = Instance.new("Folder"); f.Name = "MadeFolder"; f.Parent = pg
            "#),
            None
        );
        assert_eq!(game.run_luau(Side::Server, "game:GetService('Players'):GetPlayers()[1]:LoadCharacter()"), None);
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local pg = game:GetService("Players").LocalPlayer.PlayerGui
                local function n(name) local k = 0 for _, c in pg:GetChildren() do if c.Name == name then k += 1 end end return k end
                assert(n("Resets") == 1 and pg.Resets ~= _G.r, "Resets not replaced")
                assert(n("Keeps") == 1 and pg.Keeps == _G.k, "Keeps copied again or replaced")
                assert(n("MadeResets") == 0, "MadeResets survived")
                assert(n("MadeKeeps") == 1, "MadeKeeps went")
                assert(n("MadeFolder") == 0, "MadeFolder survived")
            "#),
            None
        );
    }

    #[test]
    fn chat_modules_folder_added_when_load_default_chat_enabled() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        if game.doc.find_child(root, "Chat").is_none() {
            game.doc.dom.insert(root, InstanceBuilder::new("Chat").with_name("Chat"));
        }
        game.start_play(RunMode::Play, &mut (), false, None);
        assert_eq!(
            game.run_luau(Side::Server, r#"assert(game:GetService("Chat"):FindFirstChild("ChatModules") ~= nil, "missing ChatModules")"#),
            None
        );

        let mut game_off = Game::new(None);
        let root_off = game_off.doc.dom.root_ref();
        let chat_off = game_off
            .doc
            .find_child(root_off, "Chat")
            .unwrap_or_else(|| game_off.doc.dom.insert(root_off, InstanceBuilder::new("Chat").with_name("Chat")));
        game_off
            .doc
            .dom
            .get_by_ref_mut(chat_off)
            .unwrap()
            .properties
            .insert(rbx_dom_weak::ustr("LoadDefaultChat"), rbx_dom_weak::types::Variant::Bool(false));
        game_off.start_play(RunMode::Play, &mut (), false, None);
        assert_eq!(
            game_off.run_luau(Side::Server, r#"assert(game:GetService("Chat"):FindFirstChild("ChatModules") == nil, "unexpected ChatModules")"#),
            None
        );
    }
}

#[cfg(test)]
mod character_weld_and_fall_tests {
    use crate::game::Game;
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_engine::runtime::{RunMode, Side};

    fn tick(game: &mut Game, n: usize) {
        let ctx = egui::Context::default();
        for _ in 0..n {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
    }

    fn with_floor() -> Game {
        let mut game = Game::new(None);
        let ws = game.doc.workspace().expect("workspace");
        game.doc.dom.insert(ws, InstanceBuilder::new("Part").with_name("Floor")
            .with_property("Anchored", true)
            .with_property("Size", rbx_dom_weak::types::Vector3::new(200.0, 2.0, 200.0))
            .with_property("CFrame", rbx_dom_weak::types::CFrame::new(rbx_dom_weak::types::Vector3::new(0.0, -1.0, 0.0), rbx_dom_weak::types::Matrix3::identity())));
        game
    }

    /// A part a server script welds onto the character after it spawned
    /// (the weld made while the part is still unparented, then the part put
    /// in the character, as a game's accessory copy does) moves with the
    /// character as one assembly, as in Studio. It used to be simulated loose
    /// as well, fell away and was culled at FallenPartsDestroyHeight.
    #[test]
    fn part_welded_onto_character_after_spawn_rides_with_it() {
        let mut game = with_floor();
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 60);
        assert_eq!(game.run_luau(Side::Server, r#"
            local ch = game:GetService("Players"):GetPlayers()[1].Character
            assert(ch, "no character")
            local head = ch:FindFirstChild("Head")
            local p = Instance.new("Part")
            p.Name = "Carried"; p.Size = Vector3.new(1, 1, 1); p.CanCollide = false; p.Massless = true
            p.CFrame = head.CFrame
            local w = Instance.new("Weld"); w.Part0 = head; w.Part1 = p; w.Parent = p
            p.Parent = ch
        "#), None);
        tick(&mut game, 180);
        assert_eq!(game.run_luau(Side::Server, r#"
            local ch = game:GetService("Players"):GetPlayers()[1].Character
            local p = ch:FindFirstChild("Carried")
            assert(p, "the welded part left the character")
            local d = (p.Position - ch.Head.Position).Magnitude
            assert(d < 0.5, "welded part " .. d .. " studs from the head")
        "#), None);
    }

    #[test]
    fn player_uses_workspace_fallen_parts_height() {
        let mut game = with_floor();
        let workspace = game.doc.workspace().unwrap();
        game.doc.set_prop_live(workspace, "FallenPartsDestroyHeight".into(), (-1000.0f32).into());
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 10);
        assert_eq!(game.run_luau(Side::Server, r#"
            local character = game:GetService("Players"):GetPlayers()[1].Character
            character:PivotTo(CFrame.new(0, -600, 0))
        "#), None);
        tick(&mut game, 10);
        assert_eq!(game.run_luau(Side::Server, r#"
            local character = game:GetService("Players"):GetPlayers()[1].Character
            assert(character.HumanoidRootPart.Position.Y < -500)
            assert(character.Humanoid.Health > 0)
            character:PivotTo(CFrame.new(0, -1100, 0))
        "#), None);
        tick(&mut game, 10);
        assert!(game.play.as_ref().unwrap().character.as_ref().unwrap().dead);
    }

    /// Studio (measured in Play): a part below FallenPartsDestroyHeight goes
    /// to Parent nil: ChildRemoved and AncestryChanged fire, Destroying does
    /// not, and it can be parented back.
    #[test]
    fn fallen_part_is_unparented_with_signals() {
        let mut game = with_floor();
        game.start_play(RunMode::Play, &mut (), false, None);
        tick(&mut game, 10);
        assert_eq!(game.run_luau(Side::Server, r#"
            _G.fall = {}
            local m = Instance.new("Model"); m.Name = "FallTest"; m.Parent = workspace
            local p = Instance.new("Part"); p.Size = Vector3.new(1, 1, 1)
            p.Position = Vector3.new(50, -490, 50); p.Parent = m
            p.AncestryChanged:Connect(function(_, par) table.insert(_G.fall, "ancestry " .. tostring(par)) end)
            p.Destroying:Connect(function() table.insert(_G.fall, "destroying") end)
            m.ChildRemoved:Connect(function(c) table.insert(_G.fall, "removed " .. c.Name) end)
            _G.fallPart = p
        "#), None);
        tick(&mut game, 180);
        assert_eq!(game.run_luau(Side::Server, r#"
            local got = table.concat(_G.fall, ",")
            assert(_G.fallPart.Parent == nil, "still parented: " .. got)
            assert(string.find(got, "removed Part", 1, true), got)
            assert(string.find(got, "ancestry nil", 1, true), got)
            assert(not string.find(got, "destroying", 1, true), got)
            _G.fallPart.Parent = workspace
            assert(_G.fallPart.Parent == workspace)
        "#), None);
    }

    /// When a Model's PrimaryPart falls below FallenPartsDestroyHeight,
    /// PrimaryPart clears to nil and an ancestor Model left without BaseParts
    /// is removed from the tree (matching Studio).
    #[test]
    fn fallen_primary_part_clears_primary_part_and_empty_model() {
        let mut game = Game::new(None);
        game.start_play(RunMode::Play, &mut (), false, None);
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 10);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local m = Instance.new("Model"); m.Name = "FallModel"
                local p = Instance.new("Part"); p.Name = "Handle"; p.Size = Vector3.new(1, 1, 1)
                p.Position = Vector3.new(80, -490, 80); p.Parent = m
                m.PrimaryPart = p
                m.Parent = workspace
                _G.fm = m
                _G.fp = p
            "#),
            None
        );
        tick(&mut game, 180);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                assert(_G.fp.Parent == nil, "Handle should unparent")
                assert(_G.fm.PrimaryPart == nil, "PrimaryPart should clear to nil")
                assert(_G.fm.Parent == nil, "empty Model should unparent after last BasePart fell")
            "#),
            None
        );
    }
}

#[cfg(test)]
mod spawn_script_order_tests {
    use crate::game::Game;
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_engine::runtime::{RunMode, Side};

    /// Studio Play: on each spawn the client
    /// runs CharacterAdded, then the new ResetOnSpawn GUI's LocalScript, then the
    /// new character's LocalScript (StarterCharacterScripts). rustudio started the
    /// character's scripts with CharacterAdded, before the GUI's.
    #[test]
    fn gui_local_scripts_start_before_the_characters() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let ls = |src: &str| InstanceBuilder::new("LocalScript").with_property("Source", src.to_string());
        let child = |game: &mut Game, parent, class: &str| {
            game.doc.find_child(parent, class)
                .unwrap_or_else(|| game.doc.dom.insert(parent, InstanceBuilder::new(class).with_name(class)))
        };
        let sp = child(&mut game, root, "StarterPlayer");
        let scs = child(&mut game, sp, "StarterCharacterScripts");
        let sps = child(&mut game, sp, "StarterPlayerScripts");
        let sg = child(&mut game, root, "StarterGui");
        let gui = game.doc.dom.insert(sg, InstanceBuilder::new("ScreenGui").with_name("G"));
        game.doc.dom.insert(sps, ls(r#"
            _G.log = {}
            game:GetService("Players").LocalPlayer.CharacterAdded:Connect(function() table.insert(_G.log, "added") end)
        "#));
        game.doc.dom.insert(gui, ls(r#"table.insert(_G.log, "gui")"#));
        game.doc.dom.insert(scs, ls(r#"table.insert(_G.log, "char")"#).with_name("Probe"));
        game.start_play(RunMode::Play, &mut (), false, None);
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local got = table.concat(_G.log, ",")
                assert(got == "added,gui,char", got)
            "#),
            None
        );
        assert_eq!(game.run_luau(Side::Server, "game:GetService('Players'):GetPlayers()[1]:LoadCharacter()"), None);
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local got = table.concat(_G.log, ",")
                assert(got == "added,gui,char,added,gui,char", got)
            "#),
            None
        );
    }
}

/// Unparent parts that fell below `Workspace.FallenPartsDestroyHeight`.
/// Matches Studio: clears `PrimaryPart` (preserving `WorldPivotData`) on any
/// ancestor `Model` whose `PrimaryPart` fell, unparents the fallen part, and
/// also unparents ancestor `Model`s (other than Workspace/characters) left
/// with no remaining `BasePart` descendants. Returns `(signals, removed)`.
fn unparent_fallen_parts(doc: &mut Document, fallen: &[Ref]) -> (Vec<(Ref, Ref, Vec<Ref>)>, Vec<Ref>) {
    let mut signals: Vec<(Ref, Ref, Vec<Ref>)> = Vec::new();
    let mut removed: Vec<Ref> = Vec::new();
    for &r in fallen {
        let Some(old) = doc.dom.get_by_ref(r).map(|i| i.parent()) else { continue };
        if old.is_none() {
            continue;
        }
        // Clear PrimaryPart on any ancestor Model pointing to `r` or its subtree.
        let mut clear_models: Vec<(Ref, Option<rbx_dom_weak::types::CFrame>)> = Vec::new();
        let mut cur = old;
        while cur.is_some() {
            let Some(inst) = doc.dom.get_by_ref(cur) else { break };
            if doc::is_a(&inst.class, "Model") {
                if let Some(Variant::Ref(pp)) = doc.prop(cur, "PrimaryPart") {
                    if pp.is_some() && (pp == r || doc.is_ancestor(r, pp)) {
                        let cf = match doc.prop(pp, "CFrame") {
                            Some(Variant::CFrame(c)) => Some(c),
                            _ => None,
                        };
                        clear_models.push((cur, cf));
                    }
                }
            }
            cur = inst.parent();
        }
        for (model, cf) in clear_models {
            doc.set_prop_live(model, "PrimaryPart".into(), Variant::Ref(Ref::none()));
            if let Some(c) = cf {
                doc.set_prop_live(model, "WorldPivotData".into(), Variant::OptionalCFrame(Some(c)));
            }
        }
        let below: Vec<Ref> = std::iter::once(r)
            .chain(doc.dom.descendants_of(r).map(|i| i.referent()).filter(|d| *d != r))
            .collect();
        doc.dom.transfer_within(r, Ref::none());
        doc.mark_tree(r);
        signals.push((r, old, below));
        removed.push(r);

        // Studio removes ancestor Models left without any BasePart descendants
        // after a part falls below FallenPartsDestroyHeight (excluding Workspace
        // and character models carrying a Humanoid).
        let mut anc = old;
        while anc.is_some() {
            let Some(inst) = doc.dom.get_by_ref(anc) else { break };
            if inst.class != "Model" {
                break;
            }
            let has_part_or_hum = doc
                .dom
                .descendants_of(anc)
                .any(|d| d.referent() != anc && (doc::is_a(&d.class, "BasePart") || d.class == "Humanoid"));
            if has_part_or_hum {
                break;
            }
            let m_old = inst.parent();
            let m_below: Vec<Ref> = std::iter::once(anc)
                .chain(doc.dom.descendants_of(anc).map(|i| i.referent()).filter(|d| *d != anc))
                .collect();
            doc.dom.transfer_within(anc, Ref::none());
            doc.mark_tree(anc);
            if m_old.is_some() {
                signals.push((anc, m_old, m_below));
            }
            removed.push(anc);
            anc = m_old;
        }
    }
    (signals, removed)
}

#[cfg(test)]
mod respawn_gui_tests {
    use crate::game::Game;
    use rbx_dom_weak::types::{self as rbx_types, Variant};
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_engine::runtime::{RunMode, Side};

    /// Studio 0.748 (a test place's HP bar): on each
    /// spawn after the first, PlayerGui's ResetOnSpawn children are destroyed and
    /// StarterGui's copied in again, with Player.Character already the new model, so
    /// the copy's LocalScript reads the new Humanoid (100 / 100). rustudio kept the
    /// first copy, still bound to the dead Humanoid (HP 0 / 100).
    #[test]
    fn respawn_recopies_reset_on_spawn_guis_bound_to_the_new_humanoid() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let sg = match game.doc.find_child(root, "StarterGui") {
            Some(s) => s,
            None => game.doc.dom.insert(root, InstanceBuilder::new("StarterGui").with_name("StarterGui")),
        };
        let script = |counter: &str| {
            format!(
                r#"local p = game:GetService("Players").LocalPlayer
                local c = p.Character or p.CharacterAdded:Wait()
                local h = c:WaitForChild("Humanoid")
                script.Parent:SetAttribute("hp", h.Health)
                _G.{counter} = (_G.{counter} or 0) + 1"#
            )
        };
        for (name, reset) in [("Reset", true), ("Keep", false)] {
            let g = game.doc.dom.insert(
                sg,
                InstanceBuilder::new("ScreenGui").with_name(name).with_property("ResetOnSpawn", reset),
            );
            game.doc.dom.insert(
                g,
                InstanceBuilder::new("LocalScript").with_name("Bind").with_property("Source", script(name)),
            );
        }
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                if not plr.Character then plr:LoadCharacter() end
            "#),
            None
        );
        tick(&mut game, 60);
        assert_eq!(
            game.run_luau(Side::Client, "local pg = game:GetService('Players').LocalPlayer.PlayerGui
                assert(pg.Reset:GetAttribute('hp') == 100, 'first hp ' .. tostring(pg.Reset:GetAttribute('hp')))
                _G.keepGui = pg.Keep"),
            None
        );
        // Dies (Health 0), then respawns.
        assert_eq!(
            game.run_luau(Side::Server, "game:GetService('Players'):GetPlayers()[1].Character.Humanoid.Health = 0"),
            None
        );
        tick(&mut game, 10);
        assert_eq!(game.run_luau(Side::Server, "game:GetService('Players'):GetPlayers()[1]:LoadCharacter()"), None);
        tick(&mut game, 60);
        assert_eq!(
            game.run_luau(Side::Client, "local pg = game:GetService('Players').LocalPlayer.PlayerGui
                local n = 0
                for _, g in pg:GetChildren() do if g.Name == 'Reset' then n += 1 end end
                assert(n == 1, n .. ' Reset guis')
                assert(_G.Reset == 2, 'Reset ran ' .. tostring(_G.Reset))
                assert(pg.Reset:GetAttribute('hp') == 100, 'hp after respawn ' .. tostring(pg.Reset:GetAttribute('hp')))
                assert(_G.Keep == 1, 'Keep ran ' .. tostring(_G.Keep))
                assert(pg.Keep == _G.keepGui, 'Keep was replaced')"),
            None
        );
    }

    /// Sitting in an unanchored VehicleSeat moves the character onto the seat
    /// without pulling the vehicle toward the character, switches
    /// CurrentCamera.CameraSubject to the VehicleSeat on the client, and
    /// unseating zeroes Throttle/Steer and restores CameraSubject to Humanoid.
    #[test]
    fn vehicle_seat_seating_camera_subject_and_unseat() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let ws = game.doc.workspace().unwrap();
        let car = game.doc.dom.insert(ws, InstanceBuilder::new("Model").with_name("Car"));
        let chassis = game.doc.dom.insert(
            car,
            InstanceBuilder::new("Part")
                .with_name("Chassis")
                .with_property("Size", Variant::Vector3(rbx_types::Vector3::new(6.0, 1.0, 10.0)))
                .with_property("CFrame", Variant::CFrame(rbx_types::CFrame::new(
                    rbx_types::Vector3::new(40.0, 3.0, 0.0),
                    rbx_types::Matrix3::identity(),
                )))
                .with_property("Anchored", Variant::Bool(false)),
        );
        let seat = game.doc.dom.insert(
            car,
            InstanceBuilder::new("VehicleSeat")
                .with_name("DriveSeat")
                .with_property("Size", Variant::Vector3(rbx_types::Vector3::new(2.0, 1.0, 2.0)))
                .with_property("CFrame", Variant::CFrame(rbx_types::CFrame::new(
                    rbx_types::Vector3::new(40.0, 4.0, 0.0),
                    rbx_types::Matrix3::identity(),
                )))
                .with_property("Anchored", Variant::Bool(false)),
        );
        game.doc.dom.insert(
            seat,
            InstanceBuilder::new("WeldConstraint")
                .with_name("SeatToChassis")
                .with_property("Part0", Variant::Ref(chassis))
                .with_property("Part1", Variant::Ref(seat)),
        );
        // Floor under spawn and car.
        game.doc.dom.insert(
            ws,
            InstanceBuilder::new("Part")
                .with_name("Floor")
                .with_property("Size", Variant::Vector3(rbx_types::Vector3::new(200.0, 2.0, 200.0)))
                .with_property("CFrame", Variant::CFrame(rbx_types::CFrame::new(
                    rbx_types::Vector3::new(20.0, 0.0, 0.0),
                    rbx_types::Matrix3::identity(),
                )))
                .with_property("Anchored", Variant::Bool(true)),
        );
        let _ = root;
        game.start_play(RunMode::Play, &mut (), false, None);
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                local hum = plr.Character:WaitForChild("Humanoid")
                workspace.Car.DriveSeat:Sit(hum)
            "#),
            None
        );
        tick(&mut game, 15);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local seat = workspace.Car.DriveSeat
                local ch = workspace.Car.Chassis
                assert(seat.Occupant ~= nil, "Occupant should be set")
                assert(ch.Position.X > 35, "unanchored car must not teleport to spawn: " .. tostring(ch.Position))
                local cam = workspace.CurrentCamera
                assert(cam.CameraSubject == seat, "CameraSubject should switch to VehicleSeat, got " .. tostring(cam.CameraSubject))
            "#),
            None
        );
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                plr.Character.Humanoid.Sit = false
            "#),
            None
        );
        tick(&mut game, 15);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local plr = game:GetService("Players").LocalPlayer
                local hum = plr.Character.Humanoid
                local seat = workspace.Car.DriveSeat
                assert(seat.Occupant == nil, "Occupant should clear on unseat")
                assert(seat.Throttle == 0 and seat.Steer == 0, "Throttle/Steer should zero on unseat")
                local cam = workspace.CurrentCamera
                assert(cam.CameraSubject == hum, "CameraSubject should restore to Humanoid, got " .. tostring(cam.CameraSubject))
            "#),
            None
        );
    }

    /// Custom StarterPlayer.StarterCharacter models keep their authored Motor6D
    /// joints rather than being upgraded to read-only-C0 AnimationConstraints.
    #[test]
    fn custom_starter_character_keeps_motor6ds() {
        let mut game = Game::new(None);
        let sp = game.doc.find_child(game.doc.dom.root_ref(), "StarterPlayer").unwrap();
        let sc = rustudio_engine::rig::insert_character(
            &mut game.doc.dom,
            sp,
            "StarterCharacter",
            rustudio_engine::rig::RigType::R15,
            &rustudio_engine::rig::BodyColors::default(),
            &rustudio_engine::rig::Scales::default(),
            rbx_dom_weak::InstanceBuilder::new("Humanoid"),
        );
        assert!(game.doc.dom.descendants_of(sc).any(|i| i.class == "Motor6D"));
        game.start_play(RunMode::Play, &mut (), false, None);
        let ctx = egui::Context::default();
        for _ in 0..30 {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local ch = game:GetService("Players").LocalPlayer.Character
                assert(ch, "character missing")
                local waist = ch.UpperTorso:FindFirstChild("Waist")
                assert(waist and waist:IsA("Motor6D"), "expected Motor6D Waist, got " .. tostring(waist and waist.ClassName))
                local c0 = waist.C0
                waist.C0 = c0 * CFrame.Angles(0.1, 0, 0)
            "#),
            None
        );
    }

    #[test]
    fn first_character_load_resets_camera_type_to_custom_but_respawn_preserves_scriptable() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let players = match game.doc.find_child(root, "Players") {
            Some(p) => p,
            None => game
                .doc
                .dom
                .insert(root, InstanceBuilder::new("Players").with_name("Players")),
        };
        if let Some(i) = game.doc.dom.get_by_ref_mut(players) {
            i.properties.insert(
                rbx_dom_weak::ustr("CharacterAutoLoads"),
                rbx_dom_weak::types::Variant::Bool(false),
            );
        }
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 5);
        assert_eq!(
            game.run_luau(
                Side::Client,
                r#"local cam = workspace.CurrentCamera
                cam.CameraType = Enum.CameraType.Scriptable
                assert(cam.CameraType == Enum.CameraType.Scriptable)"#,
            ),
            None
        );
        assert_eq!(
            game.run_luau(
                Side::Server,
                "game:GetService('Players'):GetPlayers()[1]:LoadCharacter()",
            ),
            None
        );
        tick(&mut game, 10);
        assert_eq!(
            game.run_luau(
                Side::Client,
                r#"local cam = workspace.CurrentCamera
                assert(cam.CameraType == Enum.CameraType.Custom, 'first load should reset to Custom, got ' .. tostring(cam.CameraType))
                assert(cam.CameraSubject ~= nil and cam.CameraSubject:IsA('Humanoid'), 'subject should be Humanoid')
                cam.CameraType = Enum.CameraType.Scriptable"#,
            ),
            None
        );
        assert_eq!(
            game.run_luau(
                Side::Server,
                "game:GetService('Players'):GetPlayers()[1]:LoadCharacter()",
            ),
            None
        );
        tick(&mut game, 10);
        assert_eq!(
            game.run_luau(
                Side::Client,
                r#"local cam = workspace.CurrentCamera
                assert(cam.CameraType == Enum.CameraType.Scriptable, 'respawn should preserve Scriptable, got ' .. tostring(cam.CameraType))"#,
            ),
            None
        );
    }

    #[test]
    fn remote_event_fire_before_client_connect_is_delivered_on_connect() {
        let mut game = Game::new(None);
        let root = game.doc.dom.root_ref();
        let rs = game
            .doc
            .find_child(root, "ReplicatedStorage")
            .unwrap_or_else(|| {
                game.doc
                    .dom
                    .insert(root, InstanceBuilder::new("ReplicatedStorage").with_name("ReplicatedStorage"))
            });
        game.doc
            .dom
            .insert(rs, InstanceBuilder::new("RemoteEvent").with_name("WaveEvent"));
        let sss = game
            .doc
            .find_child(root, "ServerScriptService")
            .unwrap_or_else(|| {
                game.doc.dom.insert(
                    root,
                    InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"),
                )
            });
        game.doc.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("WaveSender")
                .with_property(
                    "Source",
                    r#"
                    local Players = game:GetService("Players")
                    local re = game:GetService("ReplicatedStorage"):WaitForChild("WaveEvent")
                    -- Fired before any player has joined: must reach zero clients.
                    re:FireAllClients("pre-join-ignored")
                    Players.PlayerAdded:Connect(function(p)
                        re:FireClient(p, "early-1")
                        re:FireClient(p, "early-2")
                    end)
                    "#,
                ),
        );
        let sp = game
            .doc
            .find_child(root, "StarterPlayer")
            .unwrap_or_else(|| {
                game.doc
                    .dom
                    .insert(root, InstanceBuilder::new("StarterPlayer").with_name("StarterPlayer"))
            });
        let sps = game
            .doc
            .find_child(sp, "StarterPlayerScripts")
            .unwrap_or_else(|| {
                game.doc.dom.insert(
                    sp,
                    InstanceBuilder::new("StarterPlayerScripts")
                        .with_name("StarterPlayerScripts"),
                )
            });
        game.doc.dom.insert(
            sps,
            InstanceBuilder::new("LocalScript")
                .with_name("LateListener")
                .with_property(
                    "Source",
                    r#"
                    local re = game:GetService("ReplicatedStorage"):WaitForChild("WaveEvent")
                    -- Delay connecting past the first frames, like Knit bootstrapping controllers.
                    task.wait(0.1)
                    _G.received = {}
                    re.OnClientEvent:Connect(function(tag)
                        table.insert(_G.received, tag)
                    end)
                    "#,
                ),
        );

        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        for _ in 0..20 {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
        assert_eq!(
            game.run_luau(
                Side::Client,
                r#"local got = table.concat(_G.received or {}, ",")
                assert(got == "early-1,early-2", "expected queued remotes, got: " .. got)"#,
            ),
            None
        );
    }
}

#[cfg(test)]
mod autoplay_sound_tests {
    use crate::game::Game;
    use rbx_dom_weak::types::{ContentId, Variant};
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_engine::runtime::{RunMode, Side};

    /// A Sound saved with Playing = true starts when Play begins, on both
    /// sides. Start_autoplay_sounds used to run with the client's DataModel
    /// in front (start_play switches to it first), treat its client refs as
    /// the server's, and set neither side's IsPlaying.
    #[test]
    fn saved_playing_sound_autoplays_on_both_sides() {
        let mut game = Game::new(None);
        let ws = game.doc.workspace().unwrap();
        let speaker = game
            .doc
            .dom
            .insert(ws, InstanceBuilder::new("Part").with_name("Speaker"));
        game.doc.dom.insert(
            speaker,
            InstanceBuilder::new("Sound")
                .with_name("Theme")
                .with_property("Playing", Variant::Bool(true))
                .with_property("SoundId", ContentId::from("rbxasset://sounds/autoplay-probe.wav")),
        );
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        for _ in 0..10 {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
        for side in [Side::Server, Side::Client] {
            assert_eq!(
                game.run_luau(
                    side,
                    r#"local s = workspace.Speaker.Theme
                    assert(s.Playing == true, "Sound.Playing is not true")
                    assert(s.IsPlaying == true, "Sound.IsPlaying is not true")"#,
                ),
                None,
                "{side:?}"
            );
        }
    }
}
