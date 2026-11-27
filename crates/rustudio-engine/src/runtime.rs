//! The Luau runtime: runs Script / LocalScript / ModuleScript sources against
//! the play DataModel. Behaviour follows Roblox's public engine reference
//! (create.roblox.com/docs/reference/engine); estimates are listed in
//! docs/MECHANICS.md.
//!
//! The play `Document` lives inside [`World`] (Lua app data) while Lua runs
//! and is handed back to the editor between frames. Scripts run as Luau
//! coroutines; `task.wait`, `Signal:Wait` and `WaitForChild` yield the running
//! coroutine and the scheduler here resumes it later.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use glam::Vec3;
use mlua::thread::ThreadStatus;
use mlua::{
    AnyUserData, FromLua, Function, Lua, MetaMethod, MultiValue, Table, Thread, UserData,
    UserDataFields, UserDataMethods, Value, Variadic,
};
use rbx_dom_weak::types::{
    Attributes, Color3, Color3uint8, Content, ContentId, Enum, Ref, UDim, UDim2, Variant,
    VariantType, Vector2, Vector3,
};
use rbx_dom_weak::{ustr, InstanceBuilder};
use rbx_reflection::{DataType, PropertyKind, PropertyTag, Scriptability};

use rustudio_core::log::Level;
use rustudio_core::doc::{self, Document};
use crate::ltypes::*;
use rustudio_core::math::Xform;
use rustudio_core::mech;

#[path = "runtime_signals.rs"]
pub mod signal_timing;

#[path = "runtime_ext.rs"]
pub mod ext;
#[path = "stubs.rs"]
pub mod stubs;
#[path = "editable.rs"]
pub mod editable;
#[path = "domains/mod.rs"]
pub mod domains;
#[path = "security.rs"]
pub mod security;
#[path = "runtime_routes.rs"]
mod routes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// F5: player, character, LocalScripts.
    Play,
    /// F8: server scripts and physics only.
    Run,
    /// The edit DataModel: only plugins run (src/plugin_host); RunService
    /// answers Studio's edit values (IsEdit, IsClient and IsServer true).
    Edit,
}

/// Which DataModel a runtime drives in a Play session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Server,
    Client,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Server => "Server",
            Side::Client => "Client",
        }
    }
}

/// A server-side change the client copy must receive.
#[derive(Debug, Clone, PartialEq)]
pub enum RepOp {
    Prop(Ref, String),
    /// One attribute changed or cleared on the server: replicated by name so
    /// client-local attributes survive server attribute changes.
    Attr(Ref, String),
    /// One CollectionService tag added or removed on the server: replicated by
    /// tag so client-local tags survive server tag changes.
    Tag(Ref, String),
    Name(Ref),
    Parent(Ref),
    /// Physics moved a part: its CFrame is copied without Changed events
    /// (Roblox doesn't fire Changed for physics movement).
    Moved(Ref),
    /// Instance:Destroy() of the instance and its subtree, sent once (Studio's
    /// DestroyInstance item); the client destroys its copy.
    Destroy(Ref),
    Persistence(Ref, Ref, bool),
}

/// A Lua value that can cross between the server and client VMs (functions
/// and coroutines can't, as in Roblox).
#[derive(Debug, Clone)]
pub enum Xfer {
    Nil,
    Bool(bool),
    Num(f64),
    Str(Vec<u8>),
    Table(Vec<(Xfer, Xfer)>),
    Inst(Ref),
    V3(Vec3),
    V2(glam::Vec2),
    CF(Xform),
    C3([f32; 3]),
    Brick(rbx_dom_weak::types::BrickColor),
    UDim(LUDim),
    UDim2(LUDim2),
    EnumItem(LEnumItem),
    /// A Luau buffer (copied, as Roblox copies buffers across remotes).
    Buffer(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum RemoteKind {
    /// FireServer (→ OnServerEvent) or FireClient/FireAllClients (→ OnClientEvent).
    Event,
    /// InvokeServer with a reply id.
    Invoke(u64),
    /// The server's answer to an invoke: (id, ok).
    Reply(u64, bool),
}

/// A remote call waiting to be delivered to the other side.
#[derive(Debug, Clone)]
pub struct RemoteMsg {
    /// The remote instance, as a ref in the sender's DataModel.
    pub inst: Ref,
    pub kind: RemoteKind,
    pub args: Vec<Xfer>,
}

/// Everything Lua callbacks touch, stored as Lua app data.
pub struct World {
    /// A privileged DataModel:Shutdown call asks the host to end Play.
    pub shutdown_requested: bool,
    pub doc: Option<Document>,
    pub mode: RunMode,
    pub side: Side,
    /// True when a separate client exists (Play); remotes are delivered.
    pub has_peer: bool,
    /// Server changes waiting to replicate to the client.
    pub rep: Vec<RepOp>,
    pub stream_requests: Vec<crate::replicate::StreamRequest>,
    pub persistent_players: HashMap<Ref, HashSet<Ref>>,
    /// Remote calls waiting to cross to the other side.
    pub outbox: Vec<RemoteMsg>,
    pub time: f64,
    /// Waits and delays also come due by wall time, as Roblox's scheduler
    /// counts them: Roblox caps the game's frame step at 1/15 s (time(),
    /// animations) but not waits (Studio 0.746 at 4 fps: delay(1) fired at
    /// time() +0.27). Play sets it; off for fixed-step (deterministic) runs.
    pub realtime: bool,
    /// Signal fires waiting to be dispatched: (instance, event, args, last
    /// connection id at fire time, deferred re-entrancy depth). Handlers
    /// connected later don't get it.
    events: Vec<(Ref, String, Vec<Value>, u64, u32)>,
    /// Fires whose handlers run before the triggering engine call returns
    /// (Workspace.SignalBehavior Immediate): see `signal_timing`.
    now: Vec<(Ref, String, Vec<Value>, u64)>,
    /// The place's Workspace.SignalBehavior, read when the world is made.
    pub signal_mode: signal_timing::SignalMode,
    /// Re-entrancy depth fires made now get (set while a deferred event's
    /// handlers run).
    defer_depth: u32,
    /// Id of the newest connection / waiter.
    conn_seq: u64,
    /// Instances were reparented, created or destroyed.
    pub structure_changed: bool,
    pub destroyed: HashSet<Ref>,
    pub output: Vec<(Level, String)>,
    /// Velocities read from physics (AssemblyLinearVelocity).
    pub velocities: rustc_hash::FxHashMap<Ref, Vec3>,
    /// Velocities scripts asked for.
    pub velocity_requests: Vec<(Ref, Vec3)>,
    /// Humanoids that died this frame.
    pub died: Vec<Ref>,
    /// Players that asked for a new character (Player:LoadCharacter).
    pub load_character: Vec<Ref>,
    /// KeyCode names held down (UserInputService:IsKeyDown).
    pub keys_down: HashSet<String>,
    /// Enum.CoreGuiType names turned off by StarterGui:SetCoreGuiEnabled.
    pub core_gui_off: HashSet<String>,
    /// StarterGui:SetCore("TopbarEnabled", false) hid Roblox's top bar
    /// (the menu button and the chat button over the game view).
    pub topbar_off: bool,
    /// The free area published by the active top-bar CoreScript.
    pub topbar_inset: Option<LRect>,
    /// Current Enum.HumanoidStateType name per Humanoid (Humanoid:GetState).
    pub humanoid_states: HashMap<Ref, &'static str>,
    /// Humanoid:SetStateEnabled(state, false) per Humanoid (state names).
    pub states_disabled: HashMap<Ref, HashSet<String>>,
    /// PhysicsService collision groups: one table per Play session, shared
    /// by the server and client runtimes and the physics (replicated in Roblox).
    pub groups: rustudio_core::collision_groups::Shared,
    /// The signed-in person's avatar (Players:GetHumanoidDescriptionFromUserId
    /// for their user id, as a signed-in Studio answers), and the
    /// HumanoidDescriptions made from it (CreateHumanoidModelFromDescription
    /// dresses those models with its clothing, face and accessories).
    pub avatar: Option<crate::avatar::AvatarLook>,
    pub avatar_descs: HashSet<Ref>,
    /// Instances with at least one connection or waiter (fires on others are dropped early).
    pub listening: rustc_hash::FxHashSet<Ref>,
    /// Instances something ever connected to (or waited on) for a touch event
    /// (`TOUCH_EVENTS`): Studio gives such a part a TouchTransmitter and only then
    /// tests its overlaps; a Humanoid here wants its limbs' touches (Humanoid.Touched).
    /// Kept after a disconnect (testing a pair nobody hears changes nothing).
    pub touch_refs: rustc_hash::FxHashSet<Ref>,
    /// Bumped when `touch_refs` grows.
    pub touch_rev: u64,
    /// Something connected to (or waits on) game.ItemChanged: every
    /// property change then becomes a Changed fire and an ItemChanged
    /// (services' `on_fire`). Set on connect, recomputed once a frame
    /// (services `step`), so the DataModel merely being listened to (for
    /// DescendantAdded, say) costs property writes nothing.
    pub item_changed: bool,
    /// Native class filters on game.DescendantAdded / DescendantRemoving
    /// for engine domains (`class_signals`): (classes, class -> matches).
    pub class_watches: Vec<(Vec<String>, HashMap<rbx_dom_weak::Ustr, bool>)>,
    /// Engine requests scripts made this frame, handled by the play session.
    pub requests: ext::Requests,
    /// What the play session tells scripts about the mouse, sounds, etc.
    pub input: ext::Inputs,
    /// Scripts that have run on this side (each starts once).
    pub started: HashSet<Ref>,
    /// Unparented instances the collector (`Runtime::collect`) looks at:
    /// freed with their subtree once no Lua value of any of them is left.
    pub orphans: HashSet<Ref>,
    /// Instances freed so far (`collect`), for RUSTUDIO_MEMSTATS.
    pub freed: u64,
    /// Scripts parented (or enabled) where Roblox runs them, started at the next dispatch.
    pub to_start: Vec<Ref>,
    /// While Play replicates a new character (client): its LocalScripts go to
    /// `held` instead of `to_start`, for Play to start after CharacterAdded and
    /// the new GUI's scripts, in Studio's order.
    pub hold_character_scripts: bool,
    pub held: Vec<Ref>,
    /// Terrain edited by scripts this frame (Terrain:Fill…): its voxels,
    /// written back to SmoothGrid once per step (runtime_ext::flush_terrain).
    pub terrain_edit: Option<(Ref, rustudio_core::terrain::Voxels)>,
    /// GuiBase2d AbsolutePosition / AbsoluteSize / AbsoluteRotation as
    /// scripts last saw them (read, or told by a change), and the Camera's
    /// ViewportSize: what `ext::fire_layout_changes` compares the new layout
    /// with.
    pub abs_seen: HashMap<Ref, ext::AbsBox>,
    pub scroll_seen: HashMap<Ref, ([f32; 2], [f32; 2])>,
    pub viewport_seen: Option<[f32; 2]>,
    /// GUI instances with live connections or waiters.
    pub gui_listening: rustc_hash::FxHashSet<Ref>,
    pub layout_listening: rustc_hash::FxHashSet<Ref>,
    /// Newly listened GUI objects and the listened objects drawn last pass.
    pub listen_new: rustc_hash::FxHashSet<Ref>,
    pub abs_laid: rustc_hash::FxHashSet<Ref>,
    /// Inputs from the last content-size pass for each listened layout.
    pub(crate) layout_content: rustc_hash::FxHashMap<Ref, ext::ContentDependencies>,
    /// What scripts changed since the drawn layout was handed over (Studio
    /// lays a GUI object out when it is read, so a box read right after a
    /// script moved or resized it is the new one): the journal position read
    /// up to, and the objects whose box (or text) may differ from the drawn
    /// one (`ext::abs_now`). `abs_all`: the journal lost track, all may.
    pub abs_scan: u64,
    pub abs_dirty: rustc_hash::FxHashSet<Ref>,
    pub abs_all: bool,
    pub(crate) layout_dirty: rustc_hash::FxHashSet<Ref>,
    pub(crate) read_layout: rustc_hash::FxHashMap<Ref, ext::ReadLayout>,
    /// The collector (`Runtime::collect`): how many unparented instances
    /// were left watched after its last pass, and whether that pass saw new
    /// ones or freed any (`Runtime::gc_step` works faster then).
    pub orphans_seen: usize,
    pub gc_busy: bool,
    /// Unparented targets kept alive by Instance `Variant::Ref` properties
    /// (e.g. `VRInventorySelector.SelectionImageObject = selectorImage`).
    // Indexed by owner so collecting an unrelated instance takes one lookup.
    prop_ref_edges: HashMap<Ref, Vec<(rbx_dom_weak::Ustr, Ref)>>,
    pub prop_ref_counts: HashMap<Ref, usize>,
    /// Roblox's CoreScripts and CorePackages put in this DataModel
    /// (`Runtime::start_core_scripts`): the modules' bytecode by instance
    /// and the CoreScripts by name.
    pub core_scripts: rustudio_core::core_scripts::Installed,
}

impl World {
    /// Whether a script at its current place runs on this side, as Roblox
    /// decides: Scripts (Legacy / Server) under Workspace or
    /// ServerScriptService on the server; LocalScripts under the player's
    /// PlayerGui, Backpack, PlayerScripts, Character or ReplicatedFirst, and
    /// RunContext Client Scripts anywhere, on the client.
    pub fn runs_here(&self, r: Ref) -> bool {
        let d = self.d();
        let Some(i) = d.dom.get_by_ref(r) else {
            return false;
        };
        let enabled = !matches!(d.prop(r, "Disabled"), Some(Variant::Bool(true)))
            && !matches!(d.prop(r, "Enabled"), Some(Variant::Bool(false)));
        if !enabled || self.mode != RunMode::Play && self.side == Side::Client {
            return false;
        }
        // In edit mode no place script runs; the plugin host starts plugin
        // scripts itself (src/plugin_host).
        if self.mode == RunMode::Edit {
            return false;
        }
        let ctx = rustudio_render::scene::enum_u32(d.prop(r, "RunContext")).unwrap_or(0);
        let root = d.dom.root_ref();
        let under = |class: &str| {
            d.find_child(root, class)
                .is_some_and(|s| d.is_ancestor(s, r))
        };
        match (self.side, i.class.as_str()) {
            // Legacy Scripts also run in a player's Backpack and PlayerGui
            // (tools; StarterGui's server Scripts, as in Studio).
            (Side::Server, "Script") => {
                // RunContext Server: wherever it is (Studio's own
                // StarterPlayer.PlayerModule.InputSetup runs in StarterPlayer),
                // but not in ServerStorage or as a template the engine
                // copies to players.
                let server_anywhere = ctx == 1 && !under("ServerStorage") && {
                    let mut p = i.parent();
                    let mut template = false;
                    while let Some(a) = d.dom.get_by_ref(p) {
                        if matches!(a.class.as_str(), "StarterPlayerScripts" | "StarterCharacterScripts" | "StarterGui" | "StarterPack") {
                            template = true;
                            break;
                        }
                        p = a.parent();
                    }
                    !template
                };
                server_anywhere
                    || ctx != 2
                    && (under("Workspace")
                        || under("ServerScriptService")
                        || d.find_child(root, "Players").is_some_and(|ps| {
                            d.dom.get_by_ref(ps).is_some_and(|pi| {
                                pi.children().iter().any(|p| {
                                    ["Backpack", "PlayerGui"].iter().any(|c| {
                                        d.find_child(*p, c).is_some_and(|f| d.is_ancestor(f, r))
                                    })
                                })
                            })
                        }))
            }
            (Side::Client, "Script") => {
                ctx == 2 && !under("ServerScriptService") && !under("ServerStorage")
            }
            (Side::Client, "LocalScript") => {
                let lp =
                    d.find_child(root, "Players")
                        .and_then(|p| match d.prop(p, "LocalPlayer") {
                            Some(Variant::Ref(lp)) if d.exists(lp) => Some(lp),
                            _ => None,
                        });
                let in_player = lp.is_some_and(|lp| {
                    ["PlayerGui", "Backpack", "PlayerScripts"]
                        .iter()
                        .any(|c| d.find_child(lp, c).is_some_and(|f| d.is_ancestor(f, r)))
                });
                let in_character = lp
                    .and_then(|lp| match d.prop(lp, "Character") {
                        Some(Variant::Ref(c)) if d.exists(c) => Some(c),
                        _ => None,
                    })
                    .is_some_and(|c| d.is_ancestor(c, r));
                in_player || in_character || under("ReplicatedFirst")
            }
            _ => false,
        }
    }

    /// Whether `r` is in the LocalPlayer's Character (client).
    fn in_local_character(&self, r: Ref) -> bool {
        let d = self.d();
        let root = d.dom.root_ref();
        d.find_child(root, "Players")
            .and_then(|p| match d.prop(p, "LocalPlayer") {
                Some(Variant::Ref(lp)) if d.exists(lp) => Some(lp),
                _ => None,
            })
            .and_then(|lp| match d.prop(lp, "Character") {
                Some(Variant::Ref(c)) if d.exists(c) => Some(c),
                _ => None,
            })
            .is_some_and(|c| d.is_ancestor(c, r))
    }

    /// Queues scripts at and under `r` that now run here.
    fn queue_scripts(&mut self, r: Ref) {
        let refs: Vec<Ref> = self.d().dom.descendants_of(r).map(|i| i.referent()).collect();
        self.queue_subtree_scripts(&refs);
    }

    fn queue_subtree_scripts(&mut self, refs: &[Ref]) {
        for &s in refs {
            if !self.started.contains(&s)
                && self
                    .d()
                    .class_of(s)
                    .is_some_and(|c| c == "Script" || c == "LocalScript")
                && self.runs_here(s)
            {
                if self.hold_character_scripts && self.in_local_character(s) {
                    self.held.push(s);
                } else {
                    self.to_start.push(s);
                }
            }
        }
    }
}

struct Timer {
    at: f64,
    started: f64,
    /// Wall-clock start: a wait returns the larger of the simulated and the
    /// real elapsed time, so it agrees with os.clock across frame hitches
    /// (Play caps dt at mech::MAX_FRAME_DT, so time() lags the wall clock then).
    wall: Instant,
    thread: Thread,
    args: Option<Vec<Value>>,
    legacy: bool,
    scheduled_step: u64,
}

#[derive(Default)]
struct Sched {
    timers: Vec<Timer>,
    ready: VecDeque<Timer>,
    deferred: VecDeque<(Thread, Vec<Value>)>,
    step: u64,
}

// Public Windows Studio uses 0.1 / 60 seconds by default.
// Studio turns that interval into a deadline for the legacy queue.
const LEGACY_TIMER_BUDGET: Duration = Duration::from_nanos(1_666_667);

impl Sched {
    fn schedule(&mut self, now: f64, delay: f64, thread: Thread, args: Option<Vec<Value>>, legacy: bool) {
        let minimum = if legacy { mech::WAIT_MIN_S } else { 0.0 };
        self.timers.push(Timer {
            at: now + delay.max(minimum),
            started: now,
            wall: rustudio_core::sim_clock::now(),
            thread,
            args,
            legacy,
            scheduled_step: self.step,
        });
    }
}

struct Conn {
    id: u64,
    func: Function,
    owner: Option<Ref>,
    once: bool,
    /// The connecting thread's identity: handlers run at it (security.rs).
    ident: u8,
}

/// Maximum pre-connect invocations buffered per `(RemoteEvent, event)` before
/// dropping and warning once (matches Roblox's RemoteEvent invocation queue).
const REMOTE_QUEUE_LIMIT: usize = 256;

fn is_remote_event_signal(name: &str) -> bool {
    matches!(name, "OnClientEvent" | "OnServerEvent")
}

#[derive(Default)]
struct Signals {
    // (Fx hashing: every fire on a listened instance looks these up.)
    conns: rustc_hash::FxHashMap<(Ref, String), Vec<Conn>>,
    waiters: rustc_hash::FxHashMap<(Ref, String), Vec<(u64, Thread)>>,
    next_id: u64,
    /// Destroy disconnects these handlers from future fires. A queued
    /// AncestryChanged can still deliver through the last pre-Destroy sequence.
    destroyed_ancestry: rustc_hash::FxHashMap<Ref, (u64, Vec<Conn>)>,
    /// Signal names indexed by instance so Destroy and GUI listener cleanup
    /// can look up handlers without walking the other instances' signals.
    conn_names: rustc_hash::FxHashMap<Ref, Vec<String>>,
    /// Active and queued AncestryChanged handlers by their callback's script.
    /// Destroy can stop a script without inspecting other scripts' callbacks.
    script_connections: rustc_hash::FxHashMap<Ref, rustc_hash::FxHashMap<(Ref, String), usize>>,
    /// Pre-connect invocations queued for reliable `RemoteEvent`s while no
    /// listener or waiter is attached to `OnClientEvent` / `OnServerEvent`.
    remote_queue: rustc_hash::FxHashMap<(Ref, String), VecDeque<Vec<Value>>>,
    remote_queue_warned: rustc_hash::FxHashSet<(Ref, String)>,
}

impl Signals {
    fn note_name(&mut self, r: Ref, name: &str) {
        let names = self.conn_names.entry(r).or_default();
        if !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }

    fn note_connection(&mut self, key: &(Ref, String), owner: Option<Ref>) {
        if let Some(owner) = owner {
            let keys = self.script_connections.entry(owner).or_default();
            *keys.entry(key.clone()).or_default() += 1;
        }
    }

    fn forget_connection(&mut self, key: &(Ref, String), owner: Option<Ref>) {
        let Some(owner) = owner else { return };
        if let Some(keys) = self.script_connections.get_mut(&owner) {
            if let Some(count) = keys.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    keys.remove(key);
                }
            }
            if keys.is_empty() {
                self.script_connections.remove(&owner);
            }
        }
    }

    fn remove_connections(&mut self, key: &(Ref, String), retired: bool, mut remove: impl FnMut(&Conn) -> bool) -> bool {
        let list = if retired {
            self.destroyed_ancestry.get_mut(&key.0).map(|(_, list)| list)
        } else {
            self.conns.get_mut(key)
        };
        let Some(list) = list else { return false };
        let before = list.len();
        let mut owners = Vec::new();
        list.retain(|conn| {
            if !remove(conn) {
                return true;
            }
            if let Some(owner) = conn.owner {
                owners.push(owner);
            }
            false
        });
        let became_empty = before != 0 && list.is_empty();
        for owner in owners {
            self.forget_connection(key, Some(owner));
        }
        became_empty
    }

    fn has_listener(&self, r: Ref) -> bool {
        self.conn_names.get(&r).is_some_and(|names| {
            names.iter().any(|name| {
                let key = (r, name.clone());
                self.conns.get(&key).is_some_and(|v| !v.is_empty())
                    || self.waiters.get(&key).is_some_and(|v| !v.is_empty())
            })
        })
    }
}

fn prune_layout_listener(lua: &Lua, r: Ref) {
    if let Ok(mut w) = world(lua) {
        if (w.gui_listening.contains(&r) || w.layout_listening.contains(&r))
            && signals(lua).is_ok_and(|s| !s.has_listener(r))
        {
            w.forget_layout_listener(r);
        }
    }
}


#[derive(Default)]
pub struct Cache {
    /// Instance → its slot in `weak`. The userdata lives in `weak`, a Lua
    /// table with weak values, so an instance's Lua value is collected once
    /// no script can reach it (a strong map here kept every instance ever
    /// touched alive: the power-up weapon's tracers grew RSS without bound).
    slots: rustc_hash::FxHashMap<Ref, i64>,
    next_slot: i64,
    weak: Option<Table>,
    /// Instances given a new Lua value since the last `collect`.
    fresh: Vec<Ref>,
    /// "slot:event" → the event's state table, weak-valued (see `signal`).
    sigs: Option<Table>,
    /// The prelude's Signal methods (every event object's).
    sig_methods: Option<Table>,
    /// Method functions by descriptor (base class, name), as Studio's
    /// bridge caches them: `Part.IsA == game.IsA`.
    methods: HashMap<(&'static str, &'static str), Function>,
    /// Rust methods that run as Lua instead (media's *Async ones yield a
    /// frame after answering): name → the Lua function, called with self.
    pub overrides: HashMap<&'static str, Function>,
    /// Instance callback writes (OnInvoke, OnServerInvoke, OnClientInvoke),
    /// paired with the thread identity that assigned them (so a CoreScript
    /// callback like TextChannel.OnIncomingMessage runs with Core capabilities
    /// even when triggered by a game script's SendAsync).
    callbacks: HashMap<(Ref, String), (Function, u8)>,
}

struct Budget {
    start: Option<Instant>,
}

/// Runs blocking work (a download a Roblox call would yield for, e.g.
/// InsertService:LoadAsset) outside the running script's timeout budget.
pub(crate) fn outside_budget<T>(lua: &Lua, f: impl FnOnce() -> T) -> T {
    let t0 = Instant::now();
    let r = f();
    if let Some(mut b) = lua.app_data_mut::<Budget>() {
        if let Some(start) = b.start.as_mut() {
            *start += t0.elapsed();
        }
    }
    r
}

pub struct Runtime {
    pub lua: Lua,
    shared: Table,
    /// This VM's lua_Callbacks (the watchdog arms its interrupt).
    callbacks: usize,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        interrupt_watchdog::unregister(self.callbacks);
        crate::script_profiler::detach(self.callbacks);
    }
}

/// Luau calls the interrupt callback at every call and loop edge while one is
/// set. The timeout check needs it only now and then, so the callback stays
/// unset and a watchdog thread sets it every WATCHDOG_MS; the callback unsets
/// itself when it runs. (Before, mlua's interrupt wrapper ran on every edge:
/// a large share of tight Luau loops.) Luau supports setting it from another
/// thread; Roblox arms its script timeout the same way.
mod interrupt_watchdog {
    use std::sync::Mutex;
    use mlua::ffi;
    type Proc = unsafe extern "C-unwind" fn(*mut ffi::lua_State, std::ffi::c_int);
    const WATCHDOG_MS: u64 = 100;
    static VMS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
    static STARTED: std::sync::Once = std::sync::Once::new();

    /// Takes the installed interrupt off `cb` and arms it periodically.
    pub fn register(cb: *mut ffi::lua_Callbacks) {
        let proc = unsafe { (*cb).interrupt.take() };
        let Some(proc) = proc else { return };
        VMS.lock().unwrap().push((cb as usize, proc as usize));
        STARTED.call_once(|| {
            std::thread::Builder::new()
                .name("lua-watchdog".into())
                .spawn(|| loop {
                    std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_MS));
                    for &(cb, proc) in VMS.lock().unwrap().iter() {
                        let cb = cb as *mut ffi::lua_Callbacks;
                        // SAFETY: registered VMs are alive until unregister,
                        // which takes this lock first.
                        unsafe {
                            std::ptr::write_volatile(
                                &mut (*cb).interrupt,
                                Some(std::mem::transmute::<usize, Proc>(proc)),
                            )
                        };
                    }
                })
                .ok();
        });
    }

    pub fn disarm(cb: usize) {
        if cb != 0 {
            let cb = cb as *mut ffi::lua_Callbacks;
            unsafe { std::ptr::write_volatile(&mut (*cb).interrupt, None) };
        }
    }

    pub fn unregister(cb: usize) {
        VMS.lock().unwrap().retain(|e| e.0 != cb);
    }
}

// ------------------------------------------------------------- utilities

pub(crate) fn flush_immediate(lua: &Lua) {
    signal_timing::flush_immediate(lua);
}

pub(crate) fn fire_filtered(lua: &Lua, r: Ref, name: &str, args: &[Value], core_only: bool) {
    signal_timing::deliver_filtered(lua, r, name, args, core_only);
}

/// Fixed-step runs (a World that is not `realtime`, i.e. RUSTUDIO_FIXED_DT Play):
/// a wall-clock epoch time (tick(), GetServerTimeNow) as a fixed epoch plus game
/// time, so scripts that read it behave the same on every run. None otherwise.
pub(crate) fn fixed_epoch_time(lua: &Lua) -> Option<f64> {
    const EPOCH: f64 = 1_767_225_600.0; // 2026-01-01 00:00 UTC
    let w = lua.app_data_ref::<World>()?;
    (!w.realtime && fixed_dt_env()).then(|| EPOCH + w.time)
}

/// RUSTUDIO_FIXED_DT is set (test runs).
fn fixed_dt_env() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_FIXED_DT").is_some_and(|v| !v.is_empty()))
}

pub fn world(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, World>> {
    lua.app_data_mut::<World>()
        .ok_or_else(|| rt_err("the play session has ended"))
}

fn cache(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, Cache>> {
    lua.app_data_mut::<Cache>()
        .ok_or_else(|| rt_err("runtime cache missing"))
}

fn sched(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, Sched>> {
    lua.app_data_mut::<Sched>()
        .ok_or_else(|| rt_err("scheduler missing"))
}

fn signals(lua: &Lua) -> mlua::Result<mlua::AppDataRefMut<'_, Signals>> {
    lua.app_data_mut::<Signals>()
        .ok_or_else(|| rt_err("signals missing"))
}

impl World {
    pub fn new(doc: Document, mode: RunMode) -> Self {
        World {
            signal_mode: signal_timing::SignalMode::of(&doc),
            doc: Some(doc),
            shutdown_requested: false,
            mode,
            side: Side::Server,
            has_peer: false,
            rep: Vec::new(),
            stream_requests: Vec::new(),
            persistent_players: HashMap::new(),
            outbox: Vec::new(),
            time: 0.0,
            realtime: false,
            events: Vec::new(),
            now: Vec::new(),
            defer_depth: 0,
            conn_seq: 0,
            structure_changed: false,
            destroyed: HashSet::new(),
            output: Vec::new(),
            velocities: Default::default(),
            velocity_requests: Vec::new(),
            died: Vec::new(),
            load_character: Vec::new(),
            keys_down: HashSet::new(),
            core_gui_off: HashSet::new(),
            topbar_off: false,
            topbar_inset: None,
            humanoid_states: HashMap::new(),
            states_disabled: HashMap::new(),
            groups: rustudio_core::collision_groups::shared(),
            avatar: None,
            avatar_descs: HashSet::new(),
            listening: Default::default(),
            touch_refs: Default::default(),
            touch_rev: 0,
            item_changed: false,
            class_watches: Vec::new(),
            abs_seen: HashMap::new(),
            scroll_seen: HashMap::new(),
            viewport_seen: None,
            gui_listening: Default::default(),
            layout_listening: Default::default(),
            listen_new: Default::default(),
            abs_laid: Default::default(),
            layout_content: Default::default(),
            abs_scan: 0,
            abs_dirty: Default::default(),
            abs_all: true,
            layout_dirty: Default::default(),
            read_layout: Default::default(),
            orphans_seen: 0,
            gc_busy: true,
            prop_ref_edges: HashMap::new(),
            prop_ref_counts: HashMap::new(),
            core_scripts: Default::default(),
            requests: Default::default(),
            input: Default::default(),
            started: HashSet::new(),
            orphans: HashSet::new(),
            freed: 0,
            to_start: Vec::new(),
            hold_character_scripts: false,
            held: Vec::new(),
            terrain_edit: None,
        }
    }

    pub fn d(&self) -> &Document {
        self.doc
            .as_ref()
            .expect("play document present while Lua runs")
    }

    pub fn dm(&mut self) -> &mut Document {
        self.doc
            .as_mut()
            .expect("play document present while Lua runs")
    }

    pub fn topbar_inset(&self, width: f32) -> LRect {
        if self.mode == RunMode::Edit {
            return LRect { min: glam::Vec2::ZERO, max: glam::Vec2::ZERO };
        }
        self.topbar_inset.unwrap_or_else(|| {
            let chat = self.side != Side::Client || !self.core_gui_off.contains("Chat");
            crate::gui::fallback_topbar_inset(width, chat)
        })
    }

    /// Notes a server change for replication (clients don't replicate back).
    pub fn record(&mut self, op: RepOp) {
        if self.side == Side::Server && self.has_peer {
            self.rep.push(op);
        }
    }

    /// A connection or waiter on `r`'s event `name`: a touch event marks `r` as
    /// wanting its overlaps tested (`touch_refs`).
    pub fn note_touch_interest(&mut self, r: Ref, name: &str) {
        if TOUCH_EVENTS.contains(&name) && self.touch_refs.insert(r) {
            self.touch_rev += 1;
        }
    }

    fn note_listener(&mut self, r: Ref, seed_box: bool) {
        let first = self.listening.insert(r);
        if self.destroyed.contains(&r) {
            return;
        }
        if self.gui_listening.contains(&r) {
            if seed_box {
                ext::seed_unlaid(self, r);
            }
            return;
        }
        if self.layout_listening.contains(&r) {
            return;
        }
        let Some(class) = self.d().class_of(r) else { return };
        if doc::is_a(&class, "GuiBase2d") {
            self.gui_listening.insert(r);
            if first && seed_box {
                ext::seed_unlaid(self, r);
            }
            if self.side == Side::Client || self.mode == RunMode::Edit {
                self.listen_new.insert(r);
            }
        } else if doc::is_a(&class, "UIGridStyleLayout") {
            self.layout_listening.insert(r);
        }
    }

    fn forget_layout_listener(&mut self, r: Ref) {
        self.gui_listening.remove(&r);
        self.layout_listening.remove(&r);
        self.layout_content.remove(&r);
        self.listen_new.remove(&r);
        self.abs_laid.remove(&r);
    }

    pub fn fire(&mut self, r: Ref, name: &str, args: Vec<Value>) {
        rustudio_core::telemetry::fired(name);
        if name == "Changed" || name.starts_with("Tag") {
            domains::services::on_fire(self, r, name, &args);
        }
        // Nobody connected to anything on this instance: nothing to queue.
        if !self.listening.contains(&r) {
            return;
        }
        if self.signal_mode.defers(name, &args) {
            self.events.push((r, name.to_string(), args, self.conn_seq, self.defer_depth));
        } else {
            self.now.push((r, name.to_string(), args, self.conn_seq));
        }
    }

    /// Queues a native frame hook right behind the frame event `after`, in
    /// the same queue (immediate or deferred) whether or not anything
    /// listens; `signal_timing` runs it at that point.
    pub fn queue_hook(&mut self, r: Ref, after: &str, hook: &str, dt: Value) {
        if self.signal_mode.defers(after, &[]) {
            self.events.push((r, hook.to_string(), vec![dt], self.conn_seq, self.defer_depth));
        } else {
            self.now.push((r, hook.to_string(), vec![dt], self.conn_seq));
        }
    }

    pub fn full_name(&self, r: Ref) -> String {
        let d = self.d();
        let root = d.dom.root_ref();
        let mut parts = Vec::new();
        let mut cur = r;
        while let Some(i) = d.dom.get_by_ref(cur) {
            if cur == root {
                break;
            }
            parts.push(i.name.clone());
            cur = i.parent();
            if cur.is_none() {
                break;
            }
        }
        parts.reverse();
        parts.join(".")
    }
}

/// RUSTUDIO_MEMSTATS=1: a line of memory / instance / frame counters every
/// 5 s of Play on stderr (src/play.rs collect_garbage).
pub fn memstats_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RUSTUDIO_MEMSTATS").is_some())
}

/// The Lua value for an instance (one userdata per instance, so `==` and
/// table keys work as in Roblox).
pub fn inst_value(lua: &Lua, r: Ref) -> mlua::Result<Value> {
    if r.is_none() {
        return Ok(Value::Nil);
    }
    if domains::assets_cloud::scene::access_blocked(lua,r){
        return Err(rt_err("The current thread cannot access 'RealtimeMedia' (lacking capability InternalTest)"));
    }
    let mut c = cache(lua)?;
    let slot = c.instance_slot(r);
    let Some(weak) = c.weak.as_ref() else {
        return new_inst_ud(lua, r).map(Value::UserData);
    };
    if let Value::UserData(u) = weak.raw_get::<Value>(slot)? {
        return Ok(Value::UserData(u));
    }
    let u = new_inst_ud(lua, r)?;
    weak.raw_set(slot, u.clone())?;
    // A new Lua value (Instance.new, Clone, or one Lua let go of before): the
    // collector looks at it if it's unparented then.
    c.fresh.push(r);
    Ok(Value::UserData(u))
}

/// An Instance's userdata, or an `LObj` for engine objects that aren't
/// Instances (Logger, EditableImage, EditableMesh…: Studio's typeof says
/// "Object" for them).
fn new_inst_ud(lua: &Lua, r: Ref) -> mlua::Result<AnyUserData> {
    if instance_is_object(lua, r) {
        lua.create_userdata(LObj(r))
    } else {
        lua.create_userdata(LInst(r))
    }
}

fn instance_is_object(lua: &Lua, r: Ref) -> bool {
    lua
        .app_data_ref::<World>()
        .and_then(|w| w.doc.as_ref().and_then(|d| d.class_of(r)))
        .is_some_and(|class| !is_instance_class(class))
}

impl Cache {
    fn instance_slot(&mut self, r: Ref) -> i64 {
        *self.slots.entry(r).or_insert_with(|| {
            self.next_slot += 1;
            self.next_slot
        })
    }
}

/// Whether Studio's API has `class` under Instance (unknown classes are).
fn is_instance_class(class: rbx_dom_weak::Ustr) -> bool {
    thread_local! {
        static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<rbx_dom_weak::Ustr, bool>> = Default::default();
    }
    CACHE.with(|c| {
        *c.borrow_mut().entry(class).or_insert_with(|| {
            let name = class.as_str();
            match rustudio_core::api::get() {
                Some(api) if api.classes.contains_key(name) => api.is_a(name, "Instance"),
                _ => true,
            }
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LInst(pub Ref);

/// A non-Instance engine object: an `LInst` in all but its typeof.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LObj(pub Ref);

impl FromLua for LInst {
    fn from_lua(v: Value, _: &Lua) -> mlua::Result<Self> {
        opt_inst(&v)
            .map(LInst)
            .ok_or_else(|| rt_err(format!("Instance expected, got {}", v.type_name())))
    }
}

pub fn opt_inst(v: &Value) -> Option<Ref> {
    match v {
        Value::UserData(u) => match u.borrow::<LInst>() {
            Ok(r) => Some(r.0),
            Err(_) => u.borrow::<LObj>().ok().map(|r| r.0),
        },
        _ => None,
    }
}

pub fn opt_ud<T: Clone + 'static>(v: &Value) -> Option<T> {
    if let Some(x) = crate::ltypes::vec_any::<T>(v) {
        return Some(x);
    }
    match v {
        Value::UserData(u) => u.borrow::<T>().ok().map(|r| (*r).clone()),
        _ => None,
    }
}

macro_rules! inst_userdata {
    ($ty:ty, $type_name:literal) => {
impl UserData for $ty {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field("__type", $type_name);
        // The bridge's C functions (crates/rustudio-engine/src/cmeta.rs, RE-BRIDGE.md): unnamed,
        // shared by every Instance; the metatable is locked (lock_bridge).
        f.add_meta_field_with("__namecall", |lua| {
            lua.named_registry_value::<Function>("rs_inst_namecall")
        });
        f.add_meta_field_with("__index", |lua| {
            lua.named_registry_value::<Function>("rs_inst_index")
        });
        f.add_meta_field_with("__newindex", |lua| {
            lua.named_registry_value::<Function>("rs_inst_newindex")
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            Ok(world(lua)?
                .d()
                .dom
                .get_by_ref(this.0)
                .map(|i| i.name.clone())
                .unwrap_or_default())
        });
        m.add_meta_method(MetaMethod::Eq, |_, this, o: Value| {
            Ok(opt_inst(&o) == Some(this.0))
        });
    }
}
    };
}
inst_userdata!(LInst, "Instance");
inst_userdata!(LObj, "Object");

/// An `RBXScriptConnection`.
#[derive(Debug, Clone)]
struct LConn {
    key: (Ref, String),
    id: u64,
}

impl UserData for LConn {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_meta_field("__type", "RBXScriptConnection");
        // C functions, as Studio's: self is checked first
        // on reads, the key first on writes.
        f.add_meta_field_with("__index", |lua| {
            crate::cmeta::cached(lua, "rs_conn_index", |lua| {
                let inner = lua.create_function(|lua, (this, key): (AnyUserData, String)| {
                    let this = this.borrow::<LConn>()?.clone();
                    match key.as_str() {
                        "Connected" => Ok(Value::Boolean(
                            signals(lua)?
                                .conns
                                .get(&this.key)
                                .is_some_and(|v| v.iter().any(|c| c.id == this.id)),
                        )),
                        // (`disconnect` is the deprecated spelling Animate
                        // still uses.) A new function each read, as Studio's.
                        "Disconnect" | "disconnect" => {
                            let check = lua.create_function(|lua, this: AnyUserData| {
                                let this = this.borrow::<LConn>()?.clone();
                                {
                                    let mut s = signals(lua)?;
                                    s.remove_connections(&this.key, false, |c| c.id == this.id);
                                    if this.key.1 == "AncestryChanged" {
                                        s.remove_connections(&this.key, true, |c| c.id == this.id);
                                    }
                                }
                                prune_layout_listener(lua, this.key.0);
                                Ok(true)
                            })?;
                            Ok(Value::Function(crate::cmeta::method(
                                lua,
                                &key,
                                Some(check),
                                None,
                                Some("RBXScriptConnection"),
                            )?))
                        }
                        _ => Err(rt_err(format!("{key} is not a valid member of RBXScriptConnection"))),
                    }
                })?;
                crate::cmeta::wrap_meta(lua, inner, false, &CONN_INDEX)
            })
        });
        f.add_meta_field_with("__newindex", |lua| {
            crate::cmeta::cached(lua, "rs_conn_newindex", |lua| {
                let inner = lua.create_function(|_, (_, key): (Value, String)| {
                    Err::<(), _>(rt_err(format!("{key} is not a valid member of RBXScriptConnection")))
                })?;
                crate::cmeta::wrap_meta(lua, inner, false, &CONN_NEWINDEX)
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Connection"));
    }
}

const CONN_INDEX: crate::cmeta::Spec =
    crate::cmeta::Spec { self_types: &[c"RBXScriptConnection"], key_first: false };
const CONN_NEWINDEX: crate::cmeta::Spec =
    crate::cmeta::Spec { self_types: &[c"RBXScriptConnection"], key_first: true };

// ------------------------------------------------------- class knowledge

/// Events the runtime knows (the reflection database lists only properties).
fn event_exists(class: &str, name: &str) -> bool {
    let is = |b: &str| doc::is_a(class, b);
    match name {
        "Changed" | "ChildAdded" | "ChildRemoved" | "DescendantAdded" | "DescendantRemoving"
        | "AncestryChanged" | "Destroying" | "AttributeChanged" => true,
        "Touched" | "TouchEnded" => is("BasePart"),
        "Heartbeat" | "Stepped" | "RenderStepped" | "PreSimulation" | "PostSimulation"
        | "PreRender" | "PreAnimation" => class == "RunService",
        "PlayerAdded" | "PlayerRemoving" => class == "Players",
        "CharacterAdded" | "CharacterRemoving" => class == "Player",
        "Died" | "HealthChanged" | "Jumping" | "StateChanged" | "Running" | "FreeFalling" => {
            class == "Humanoid"
        }
        "Event" => class == "BindableEvent",
        "OnServerEvent" | "OnClientEvent" => is("BaseRemoteEvent"),
        "InputBegan" | "InputEnded" => class == "UserInputService",
        _ => false,
    }
}

/// (method, class that has it), implemented in Rust.
const METHODS: &[(&str, &str)] = &[
    ("FindFirstChild", "Instance"),
    ("FindFirstChildOfClass", "Instance"),
    ("FindFirstChildWhichIsA", "Instance"),
    ("FindFirstAncestor", "Instance"),
    ("FindFirstAncestorOfClass", "Instance"),
    ("FindFirstAncestorWhichIsA", "Instance"),
    ("FindFirstDescendant", "Instance"),
    ("GetChildren", "Instance"),
    ("GetDescendants", "Instance"),
    ("IsA", "Instance"),
    ("IsDescendantOf", "Instance"),
    ("IsAncestorOf", "Instance"),
    ("Clone", "Instance"),
    ("Destroy", "Instance"),
    ("Remove", "Instance"),
    ("ClearAllChildren", "Instance"),
    ("GetFullName", "Instance"),
    ("GetAxis", "Attachment"),
    ("GetSecondaryAxis", "Attachment"),
    ("GetAttribute", "Instance"),
    ("SetAttribute", "Instance"),
    ("GetAttributes", "Instance"),
    ("GetPropertyChangedSignal", "Instance"),
    ("GetAttributeChangedSignal", "Instance"),
    ("GetService", "DataModel"),
    ("FindService", "DataModel"),
    ("GetPivot", "PVInstance"),
    ("PivotTo", "PVInstance"),
    ("SetPrimaryPartCFrame", "Model"),
    ("GetPrimaryPartCFrame", "Model"),
    ("MoveTo", "Model"),
    ("GetMass", "BasePart"),
    ("ApplyImpulse", "BasePart"),
    ("TakeDamage", "Humanoid"),
    ("GetState", "Humanoid"),
    ("Fire", "BindableEvent"),
    ("FireServer", "BaseRemoteEvent"),
    ("FireClient", "BaseRemoteEvent"),
    ("FireAllClients", "BaseRemoteEvent"),
    ("GetPlayers", "Players"),
    ("GetPlayerFromCharacter", "Players"),
    ("LoadCharacter", "Player"),
    ("IsClient", "RunService"),
    ("IsServer", "RunService"),
    ("IsStudio", "RunService"),
    ("IsRunning", "RunService"),
    ("IsRunMode", "RunService"),
    ("IsEdit", "RunService"),
    ("IsKeyDown", "UserInputService"),
];

/// Methods written in Lua because they yield (see PRELUDE).
const LUA_METHODS: &[(&str, &str)] = &[
    ("WaitForChild", "Instance"),
    ("Invoke", "BindableFunction"),
    ("InvokeServer", "RemoteFunction"),
    ("InvokeClient", "RemoteFunction"),
];

/// Property lookup through the class chain; aliases resolve to their
/// canonical property. Returns (canonical name, type, read-only).
/// The reflected property `name` of `class` is no member for scripts
/// (Scriptability::None, or tagged NotScriptable: Instance.Attributes and
/// Tags are Custom + NotScriptable): Studio's `inst.name` finds a child.
pub(super) fn prop_hidden_from_scripts(class: &str, name: &str) -> bool {
    let db = doc::db();
    let Some(c) = db.classes.get(class) else { return false };
    db.superclasses_iter(c)
        .find_map(|cls| cls.properties.get(name))
        .is_some_and(|p| {
            !matches!(p.kind, PropertyKind::Alias { .. })
                && (matches!(p.scriptability, Scriptability::None) || p.tags.contains(&PropertyTag::NotScriptable))
        })
}

fn prop_desc(class: &str, name: &str) -> Option<(&'static str, DataType<'static>, bool)> {
    // Sound.Pitch is a deprecated alias of PlaybackSpeed in Roblox, but the
    // reflection database lists it as its own property (RbxCharacterSounds sets it).
    if name == "Pitch" && class == "Sound" {
        return prop_desc(class, "PlaybackSpeed");
    }
    // Camera.CoordinateFrame is a legacy alias of Camera.CFrame (used by
    // classic CameraScript, RbxStamper, etc.), absent from rbx_reflection_database.
    if name == "CoordinateFrame" && class == "Camera" {
        return prop_desc(class, "CFrame");
    }
    let db = doc::db();
    let c = db.classes.get(class)?;
    for cls in db.superclasses_iter(c) {
        if let Some(p) = cls.properties.get(name) {
            if let PropertyKind::Alias { alias_for } = &p.kind {
                return prop_desc(class, alias_for);
            }
            let read_only = p.tags.contains(&PropertyTag::ReadOnly)
                || matches!(p.scriptability, Scriptability::Read | Scriptability::None);
            return Some((p.name, p.data_type.clone(), read_only));
        }
    }
    None
}

// ------------------------------------------------------- value conversion

pub fn variant_to_lua(lua: &Lua, v: &Variant, enum_name: Option<&str>) -> mlua::Result<Value> {
    let u = |x| -> mlua::Result<Value> { Ok(Value::UserData(x)) };
    Ok(match v {
        Variant::Bool(b) => Value::Boolean(*b),
        Variant::Int32(i) => Value::Number(*i as f64),
        Variant::Int64(i) => Value::Number(*i as f64),
        Variant::Float32(f) => Value::Number(*f as f64),
        Variant::Float64(f) => Value::Number(*f),
        Variant::String(s) => Value::String(lua.create_string(s)?),
        // rbx-dom reads string attributes (and some string properties) from
        // binary places as BinaryString: to scripts they are strings.
        Variant::BinaryString(b) => Value::String(lua.create_string(AsRef::<[u8]>::as_ref(b))?),
        Variant::ContentId(c) => Value::String(lua.create_string(c.as_str())?),
        Variant::Content(c) => editable::content_value(lua, c)?,
        Variant::Vector3(v) => LVector3(Vec3::new(v.x, v.y, v.z)).into_value(),
        Variant::Vector2(v) => u(lua.create_userdata(LVector2(glam::Vec2::new(v.x, v.y)))?)?,
        Variant::CFrame(cf) => LCFrame(Xform::from_cframe(cf)).into_lua(lua)?,
        Variant::Color3(c) => u(lua.create_userdata(LColor3([c.r, c.g, c.b]))?)?,
        Variant::Color3uint8(c) => u(lua.create_userdata(LColor3([
            c.r as f32 / 255.0,
            c.g as f32 / 255.0,
            c.b as f32 / 255.0,
        ]))?)?,
        Variant::BrickColor(b) => u(lua.create_userdata(LBrickColor(*b))?)?,
        Variant::UDim(x) => u(lua.create_userdata(LUDim {
            scale: x.scale,
            offset: x.offset,
        })?)?,
        Variant::UDim2(x) => u(lua.create_userdata(LUDim2 {
            x: LUDim {
                scale: x.x.scale,
                offset: x.x.offset,
            },
            y: LUDim {
                scale: x.y.scale,
                offset: x.y.offset,
            },
        })?)?,
        Variant::Enum(e) => match enum_name.and_then(|n| LEnumItem::from_value(n, e.to_u32())) {
            Some(item) => crate::ltypes::enum_value(lua, item)?,
            None => Value::Number(e.to_u32() as f64),
        },
        Variant::EnumItem(e) => match LEnumItem::from_value(&e.ty, e.value) {
            Some(item) => crate::ltypes::enum_value(lua, item)?,
            None => Value::Number(e.value as f64),
        },
        Variant::Ref(r) => inst_value(lua, *r)?,
        Variant::Font(f) => Value::UserData(lua.create_userdata(LFont(f.clone()))?),
        other => crate::ltypes::variant_to_datatype(lua, other)?.unwrap_or(Value::Nil),
    })
}

fn bad_assign(prop: &str, expected: &str, got: &Value) -> mlua::Error {
    rt_err(format!(
        "Unable to assign property {prop}. {expected} expected, got {}",
        crate::cmeta::type_name(got)
    ))
}

/// Studio packs Color3uint8 using a truncating signed conversion, then keeps
/// the low byte. Values outside the signed conversion range produce zero.
fn pack_color3uint8(c: Color3) -> Color3uint8 {
    let channel = |value: f32| {
        let scaled = value * 255.0;
        if (-2_147_483_648.0..2_147_483_648.0).contains(&scaled) {
            scaled as i32 as u8
        } else {
            0
        }
    };
    Color3uint8::new(channel(c.r), channel(c.g), channel(c.b))
}

/// Converts a Lua value to a property's type. `existing` keeps the stored
/// representation (Color3uint8 vs Color3).
fn lua_to_variant(
    prop: &str,
    v: &Value,
    ty: VariantType,
    enum_name: Option<&str>,
    existing: Option<&Variant>,
) -> mlua::Result<Variant> {
    // A numeric string converts, as Luau's tonumber (Place B's
    // `Code.Value = "042917"` on an IntValue; Studio takes it).
    let number = || {
        num(v)
            .or_else(|| match v {
                Value::String(s) => s.to_str().ok().and_then(|s| s.trim().parse::<f64>().ok()),
                _ => None,
            })
            .ok_or_else(|| bad_assign(prop, "number", v))
    };
    Ok(match ty {
        // Any value converts by Luau truthiness, as in Roblox (`Visible = 0`
        // shows the frame): Place D's second admin script binds a number state to
        // Visible, and Studio takes it (round 26).
        VariantType::Bool => Variant::Bool(!matches!(v, Value::Nil | Value::Boolean(false))),
        VariantType::Int32 => Variant::Int32(number()? as i32),
        VariantType::Int64 => Variant::Int64(number()? as i64),
        VariantType::Float32 => Variant::Float32(number()? as f32),
        VariantType::Float64 => Variant::Float64(number()?),
        VariantType::String => match v {
            Value::String(s) => Variant::String(s.to_str()?.to_string()),
            Value::Number(n) => Variant::String(n.to_string()),
            Value::Integer(n) => Variant::String(n.to_string()),
            _ => return Err(bad_assign(prop, "string", v)),
        },
        VariantType::ContentId => match v {
            Value::String(s) => Variant::ContentId(ContentId::from(s.to_str()?.to_string())),
            Value::UserData(_) if opt_ud::<editable::LContent>(v).is_some() => Variant::ContentId(ContentId::from(
                opt_ud::<editable::LContent>(v).and_then(|c| c.0.as_uri().map(str::to_string)).unwrap_or_default(),
            )),
            _ => return Err(bad_assign(prop, "string", v)),
        },
        VariantType::Content => match v {
            Value::String(s) => Variant::Content(Content::from(s.to_str()?.to_string())),
            Value::Nil => Variant::Content(Content::none()),
            Value::UserData(_) if opt_ud::<editable::LContent>(v).is_some() => {
                opt_ud::<editable::LContent>(v).map(|c| c.variant()).unwrap_or(Variant::Content(Content::none()))
            }
            _ => return Err(bad_assign(prop, "string", v)),
        },
        VariantType::Vector3 => {
            let p = opt_ud::<LVector3>(v).ok_or_else(|| bad_assign(prop, "Vector3", v))?;
            Variant::Vector3(Vector3::new(p.0.x, p.0.y, p.0.z))
        }
        VariantType::Vector2 => {
            let p = opt_ud::<LVector2>(v).ok_or_else(|| bad_assign(prop, "Vector2", v))?;
            Variant::Vector2(Vector2::new(p.0.x, p.0.y))
        }
        VariantType::CFrame => {
            let c = opt_ud::<LCFrame>(v).ok_or_else(|| bad_assign(prop, "CFrame", v))?;
            Variant::CFrame(c.0.orthonormalized().to_cframe())
        }
        VariantType::Color3 | VariantType::Color3uint8 => {
            let c = opt_ud::<LColor3>(v).ok_or_else(|| bad_assign(prop, "Color3", v))?;
            let c3 = Color3::new(c.0[0], c.0[1], c.0[2]);
            if ty == VariantType::Color3uint8 || matches!(existing, Some(Variant::Color3uint8(_))) {
                Variant::Color3uint8(pack_color3uint8(c3))
            } else {
                Variant::Color3(c3)
            }
        }
        VariantType::BrickColor => Variant::BrickColor(
            opt_ud::<LBrickColor>(v)
                .ok_or_else(|| bad_assign(prop, "BrickColor", v))?
                .0,
        ),
        VariantType::UDim => {
            let x = opt_ud::<LUDim>(v).ok_or_else(|| bad_assign(prop, "UDim", v))?;
            Variant::UDim(UDim::new(x.scale, x.offset))
        }
        VariantType::UDim2 => {
            let x = opt_ud::<LUDim2>(v).ok_or_else(|| bad_assign(prop, "UDim2", v))?;
            Variant::UDim2(UDim2::new(
                UDim::new(x.x.scale, x.x.offset),
                UDim::new(x.y.scale, x.y.offset),
            ))
        }
        VariantType::Enum => {
            let ename = enum_name.unwrap_or("");
            let value = match v {
                Value::UserData(_) => {
                    let item =
                        opt_ud::<LEnumItem>(v).ok_or_else(|| bad_assign(prop, "EnumItem", v))?;
                    if !ename.is_empty() && item.enum_type != ename {
                        return Err(rt_err(format!(
                            "Unable to assign property {prop}. Enum.{ename} expected, got Enum.{}",
                            item.enum_type
                        )));
                    }
                    item.value
                }
                Value::Number(n) => *n as u32,
                // A whole number reaches here as an integer (`TopSurface = 0`).
                Value::Integer(n) => *n as u32,
                Value::String(s) => LEnumItem::from_name(ename, &s.to_str()?)
                    .map(|i| i.value)
                    .ok_or_else(|| bad_assign(prop, "EnumItem", v))?,
                _ => return Err(bad_assign(prop, "EnumItem", v)),
            };
            Variant::Enum(Enum::from_u32(value))
        }
        VariantType::Font => Variant::Font(
            opt_ud::<LFont>(v)
                .ok_or_else(|| bad_assign(prop, "Font", v))?
                .0,
        ),
        VariantType::Ref => match v {
            Value::Nil => Variant::Ref(Ref::none()),
            // Luau's own argument error, at the assigning line (`p.Parent
            // = 3`, Studio probes 4): the value is the metamethod's #3.
            _ => Variant::Ref(opt_inst(v).ok_or_else(|| {
                crate::cmeta::located(format!(
                    "invalid argument #3 (Instance expected, got {})",
                    crate::cmeta::type_name(v)
                ))
            })?),
        },
        // `part.CustomPhysicalProperties = nil` is how a script goes back to
        // the material's physics in Roblox (the property reads nil then);
        // the stored value is PhysicalProperties::Default.
        VariantType::PhysicalProperties if matches!(v, Value::Nil) => {
            Variant::PhysicalProperties(rbx_types::PhysicalProperties::Default)
        }
        other => match crate::ltypes::datatype_to_variant(v) {
            Some(var) => var,
            None if matches!(v, Value::Nil) => {
                return Err(bad_assign(prop, &format!("{other:?}"), v))
            }
            None => {
                return Err(rt_err(format!(
                "Unable to assign property {prop}: {other:?} values can't be set from scripts yet"
            )))
            }
        },
    })
}

// ------------------------------------------------------------ index / set

fn inst_meta(lua: &Lua, r: Ref) -> mlua::Result<(String, String, Ref)> {
    let w = world(lua)?;
    let i = w
        .d()
        .dom
        .get_by_ref(r)
        .ok_or_else(|| rt_err("attempt to use an Instance that no longer exists"))?;
    Ok((i.class.to_string(), i.name.clone(), i.parent()))
}

/// An instance's class and parent, without copying its name.
pub fn inst_class(lua: &Lua, r: Ref) -> mlua::Result<(rbx_dom_weak::Ustr, Ref)> {
    let w = world(lua)?;
    let i = w
        .d()
        .dom
        .get_by_ref(r)
        .ok_or_else(|| rt_err("attempt to use an Instance that no longer exists"))?;
    Ok((i.class, i.parent()))
}

/// What `inst.key` is for a class, as far as it depends only on the class:
/// worked out once per (class, key) instead of scanning the method tables
/// (with an IsA per entry) on every property read.
#[derive(Clone, Copy)]
enum Disp {
    Callback,
    /// (name, base class): a method written in Lua.
    LuaMethod(&'static str, &'static str),
    /// (name, base class): a method implemented in Rust.
    Method(&'static str, &'static str),
    Event,
    Other,
}

fn disp(class: rbx_dom_weak::Ustr, key: &str) -> Disp {
    thread_local! {
        static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<rbx_dom_weak::Ustr, rustc_hash::FxHashMap<Box<str>, Disp>>> =
            Default::default();
    }
    if let Some(d) = CACHE.with(|c| c.borrow().get(&class).and_then(|m| m.get(key).copied())) {
        return d;
    }
    let d = (|| {
        if rustudio_core::api::get()
            .and_then(|a| a.lookup(&class, key))
            .is_some_and(|(_, m)| m.kind == rustudio_core::api::Kind::Callback)
        {
            return Disp::Callback;
        }
        // Old scripts use deprecated spellings (findFirstChild, clone, remove…).
        let method = ext::canonical(key).unwrap_or(key);
        let domain_lua_methods = domains::lua_methods();
        for (m, base) in LUA_METHODS.iter().chain(ext::LUA_METHODS).chain(domain_lua_methods) {
            if *m == method && doc::is_a(&class, base) {
                return Disp::LuaMethod(m, base);
            }
        }
        let domain_methods = domains::methods();
        for (m, base) in METHODS.iter().chain(ext::METHODS).chain(editable::METHODS).chain(domain_methods) {
            if *m == method && doc::is_a(&class, base) {
                return Disp::Method(m, base);
            }
        }
        if event_exists(&class, key) || ext::event_exists(&class, key) || domains::event_exists(&class, key) {
            return Disp::Event;
        }
        Disp::Other
    })();
    CACHE.with(|c| c.borrow_mut().entry(class).or_default().insert(key.into(), d));
    d
}

/// Whether `name` is a core and/or an extension method of `class` (cached).
fn method_owner(class: rbx_dom_weak::Ustr, name: &str) -> (bool, bool) {
    thread_local! {
        static CACHE: std::cell::RefCell<rustc_hash::FxHashMap<rbx_dom_weak::Ustr, rustc_hash::FxHashMap<Box<str>, (bool, bool)>>> =
            Default::default();
    }
    if let Some(v) = CACHE.with(|c| c.borrow().get(&class).and_then(|m| m.get(name).copied())) {
        return v;
    }
    let core = METHODS.iter().any(|(m, b)| *m == name && doc::is_a(&class, b));
    let domain_methods = domains::methods();
    let extended = ext::METHODS.iter().chain(editable::METHODS).chain(domain_methods).any(|(m, b)| *m == name && doc::is_a(&class, b));
    CACHE.with(|c| c.borrow_mut().entry(class).or_default().insert(name.into(), (core, extended)));
    (core, extended)
}

/// The self of a member function call: an Instance that has the method
/// `name` of `base`, else Studio's `Expected ':' not '.' calling member
/// function X` (no position), checked before any argument.
fn member_self(lua: &Lua, args: &MultiValue, base: &str, name: &str) -> mlua::Result<Ref> {
    let expected = || rt_err(format!("Expected ':' not '.' calling member function {name}"));
    let this = opt_inst(&arg(args, 0)).ok_or_else(expected)?;
    let class = inst_class(lua, this)?.0;
    if !doc::is_a(&class, base) {
        return Err(expected());
    }
    Ok(this)
}

/// Argument checks a Lua-written method makes before it runs, as Studio's
/// reflected arguments (WaitForChild(nil): `Argument 1 missing or nil`).
fn lua_method_args(name: &str, args: &MultiValue) -> mlua::Result<()> {
    if name == "WaitForChild" && arg(args, 1).is_nil() {
        return Err(rt_err("Argument 1 missing or nil"));
    }
    Ok(())
}

/// A method's function, one per descriptor (base, name) per VM as in
/// Studio: a C function named after the method (crates/rustudio-engine/src/cmeta.rs).
fn method_fn(lua: &Lua, name: &'static str, base: &'static str) -> mlua::Result<Function> {
    if let Some(f) = cache(lua)?.methods.get(&(base, name)) {
        return Ok(f.clone());
    }
    let over = cache(lua)?.overrides.get(name).cloned();
    if let Some(imp) = over {
        let check = lua.create_function(move |lua, args: MultiValue| {
            checked(lua, member_self(lua, &args, base, name).map(|_| MultiValue::new()))
        })?;
        let f = crate::cmeta::method(lua, name, Some(check), Some(imp), None)?;
        cache(lua)?.methods.insert((base, name), f.clone());
        return Ok(f);
    }
    let raw = lua.create_function(move |lua, args: MultiValue| {
        crate::lua_bridge::site(lua, "call", name);
        let r = rustudio_core::bench::api_timed("call", name, || {
            checked(lua, member_self(lua, &args, base, name).and_then(|_| call_method(lua, name, args)))
        });
        signal_timing::flush_immediate(lua);
        r
    })?;
    let f = crate::cmeta::method(lua, name, Some(raw), None, None)?;
    cache(lua)?.methods.insert((base, name), f.clone());
    Ok(f)
}

/// A Lua-written method's function: a named C function that checks self
/// and the arguments, then calls the Lua implementation (which may yield)
/// through a continuation.
fn lua_method_fn(lua: &Lua, name: &'static str, base: &'static str) -> mlua::Result<Function> {
    if let Some(f) = cache(lua)?.methods.get(&(base, name)) {
        return Ok(f.clone());
    }
    let check = lua.create_function(move |lua, args: MultiValue| {
        checked(
            lua,
            member_self(lua, &args, base, name)
                .and_then(|_| lua_method_args(name, &args))
                .map(|_| MultiValue::new()),
        )
    })?;
    let imp: Option<Function> = lua_methods(lua)?.get(name)?;
    let f = crate::cmeta::method(lua, name, Some(check), imp, None)?;
    cache(lua)?.methods.insert((base, name), f.clone());
    Ok(f)
}

/// The prelude's `__rs_lua_methods` (kept in the registry: inside a
/// callback, `lua.globals()` can be a script's environment).
fn lua_methods(lua: &Lua) -> mlua::Result<Table> {
    lua.named_registry_value("rs_lua_methods")
}

/// `X is not a valid member of Class "Full.Name"` (the DataModel by its
/// name: `… of DataModel "Game"`).
fn not_member(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Error {
    let full = match world(lua) {
        Ok(w) => {
            let full = w.full_name(r);
            if full.is_empty() {
                w.d().dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default()
            } else {
                full
            }
        }
        Err(_) => String::new(),
    };
    rt_err(format!("{key} is not a valid member of {class} \"{full}\""))
}

/// `inst:name(...)` (the Instance `__namecall`, crates/rustudio-engine/src/cmeta.rs): the
/// method's results, or the Lua function to call for a Lua-written one.
/// Only methods are callable: properties, events, children and unknown
/// names are `not a valid member`.
fn namecall(lua: &Lua, name: &str, atom: i32, args: MultiValue) -> mlua::Result<MultiValue> {
    let this = opt_inst(&arg(&args, 0)).ok_or_else(|| rt_err("Instance expected"))?;
    let class_u = inst_class(lua, this)?.0;
    rustudio_core::telemetry::touch(class_u.as_str(), name);
    let rt = routes::route(class_u, name, atom);
    // Capability gates (security.rs) answer for restricted members first,
    // as they do for reads.
    if rt.restricted {
        match security::index_hook(lua, this, name) {
            Ok(None) => {}
            Ok(Some(Value::Function(f))) => return Ok(MultiValue::from_iter([Value::Function(f)])),
            Ok(Some(_)) => return checked(lua, Err(not_member(lua, this, class_u.as_str(), name))),
            Err(e) => return checked(lua, Err(e)),
        }
    }
    match rt.disp {
        Disp::Method(m, _) => {
            if rt.overridable {
                if let Some(f) = cache(lua)?.overrides.get(m) {
                    return Ok(MultiValue::from_iter([Value::Function(f.clone())]));
                }
            }
            crate::lua_bridge::site(lua, "call", m);
            let r = rustudio_core::bench::api_timed("call", m, || {
                checked(lua, call_method_routed(lua, m, args, this, class_u, &rt.method))
            });
            signal_timing::flush_immediate(lua);
            r
        }
        Disp::LuaMethod(m, _) => {
            if let Err(e) = lua_method_args(m, &args) {
                return checked(lua, Err(e));
            }
            let f: Option<Function> = lua_methods(lua)?.get(m)?;
            match f {
                Some(f) => Ok(MultiValue::from_iter([Value::Function(f)])),
                None => checked(lua, Ok(MultiValue::new())),
            }
        }
        Disp::Callback | Disp::Event => checked(lua, Err(not_member(lua, this, class_u.as_str(), name))),
        Disp::Other => {
            if !matches!(name, "Name" | "name" | "ClassName" | "className" | "Parent" | "parent") {
                // A member only index knows (stubs, gated members): its
                // function, or its failure (a capability refusal).
                match index(lua, this, name) {
                    Ok(Value::Function(f)) => return Ok(MultiValue::from_iter([Value::Function(f)])),
                    Err(e) => return checked(lua, Err(e)),
                    Ok(_) => {}
                }
            }
            checked(lua, Err(not_member(lua, this, class_u.as_str(), name)))
        }
    }
}

/// Marks a stack-trace line (Studio's blue "Stack Begin" … "Stack End"
/// after an error); not shown.
pub const STACK_MARK: char = '\u{2063}';

/// "Full.Name" and line of an error message's "Full.Name:line: ..." prefix.
pub fn error_origin(msg: &str) -> Option<(&str, u32)> {
    let mut it = msg.splitn(3, ':');
    let path = it.next()?;
    let line = it.next()?.trim().parse().ok()?;
    it.next()?;
    (!path.is_empty() && !path.contains(' ')).then_some((path, line))
}

/// The cause of an error, without location or traceback.
pub fn error_message(e: &mlua::Error) -> String {
    let m = raw_error_message(e);
    match m.strip_prefix(crate::cmeta::LOCATED) {
        Some(rest) => rest.to_string(),
        None => m,
    }
}

/// `error_message`, keeping cmeta's LOCATED mark for the C bridge.
fn raw_error_message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(m) => m.lines().next().unwrap_or("").to_string(),
        mlua::Error::SyntaxError { message, .. } => {
            message.lines().next().unwrap_or("").to_string()
        }
        mlua::Error::CallbackError { cause, .. } => raw_error_message(cause),
        mlua::Error::WithContext { cause, .. } => raw_error_message(cause),
        other => other.to_string().lines().next().unwrap_or("").to_string(),
    }
}

/// `(true, results...)` or `(false, message)` for the Lua shims to rethrow.
/// `checked` for one result, without building a MultiValue (the bridge's
/// hot paths: Instance __index / __newindex).
fn checked1(lua: &Lua, r: mlua::Result<Value>) -> mlua::Result<(bool, Value)> {
    Ok(match r {
        Ok(v) => (true, v),
        Err(e) => (false, Value::String(lua.create_string(raw_error_message(&e))?)),
    })
}

fn checked(lua: &Lua, r: mlua::Result<MultiValue>) -> mlua::Result<MultiValue> {
    let mut out = MultiValue::new();
    match r {
        Ok(vals) => {
            out.push_back(Value::Boolean(true));
            out.extend(vals);
        }
        Err(e) => {
            out.push_back(Value::Boolean(false));
            out.push_back(Value::String(lua.create_string(raw_error_message(&e))?));
        }
    }
    Ok(out)
}

/// For engine domains that keep up with a few classes in the DataModel:
/// signals on `game` that fire like DescendantAdded / DescendantRemoving,
/// but only for instances that are one of `classes` (IsA), decided here
/// instead of in a Lua handler per instance (a 20k-part model arriving ran
/// four such handlers 20k times each: most of a client bulk reparent).
pub fn class_signals(lua: &Lua, classes: Vec<String>) -> mlua::Result<(Value, Value)> {
    let (root, id) = {
        let mut w = world(lua)?;
        w.class_watches.push((classes, HashMap::new()));
        (w.d().dom.root_ref(), w.class_watches.len() - 1)
    };
    Ok((
        signal(lua, root, &format!("ClassAdded:{id}"))?,
        signal(lua, root, &format!("ClassRemoving:{id}"))?,
    ))
}

/// Whether `r` has a connection or a waiter on `event` (true when the
/// signal tables are busy: then the fire is made and sorted out later).
/// The events a part's touches fire (play.rs `fire_touches`, rig.rs
/// `fire_humanoid_touched`): connecting to one is what makes Studio test the part's
/// overlaps (its TouchTransmitter).
pub const TOUCH_EVENTS: [&str; 4] = ["Touched", "TouchEnded", "LocalSimulationTouched", "StoppedTouching"];

pub(crate) fn hears(lua: &Lua, r: Ref, event: &str) -> bool {
    let Ok(Some(s)) = lua.try_app_data_ref::<Signals>() else {
        return true;
    };
    let key = (r, event.to_string());
    s.conns.get(&key).is_some_and(|v| !v.is_empty()) || s.waiters.get(&key).is_some_and(|v| !v.is_empty())
}

/// The class watches (`class_signals`) that instance `d` is one of.
fn class_watch_hits(w: &mut World, d: Ref) -> Vec<usize> {
    let Some(class) = w.d().class_of(d) else { return Vec::new() };
    let mut out = Vec::new();
    for (i, (classes, cache)) in w.class_watches.iter_mut().enumerate() {
        let hit = *cache
            .entry(class)
            .or_insert_with(|| classes.iter().any(|c| doc::is_a(&class, c)));
        if hit {
            out.push(i);
        }
    }
    out
}

fn signal(lua: &Lua, r: Ref, name: &str) -> mlua::Result<Value> {
    // A new signal object on every read, as Studio's (Studio probes
    // RBXScriptSignal.values: two reads are `==` but not rawequal). They
    // share one state table per (instance, event) while Lua holds any of
    // them, kept in a weak-valued table: the state holds its instance, so a
    // strong cache kept every instance whose events were ever read alive
    // for good. Connections live in `Signals`.
    let (key, sigs, methods) = {
        let c = cache(lua)?;
        let slot = c.slots.get(&r).copied().unwrap_or_default();
        (format!("{slot}:{name}"), c.sigs.clone(), c.sig_methods.clone())
    };
    let Some(sigs) = sigs else {
        let make: Function = lua.globals().get("__rs_new_signal")?;
        return make.call((inst_value(lua, r)?, name));
    };
    use crate::ltypes::LSignal;
    if let (Value::Table(data), Some(methods)) = (sigs.raw_get::<Value>(key.as_str())?, methods) {
        return Ok(Value::UserData(lua.create_userdata(LSignal::new(data, methods))?));
    }
    let make: Function = lua.globals().get("__rs_new_signal")?;
    let t: AnyUserData = make.call((inst_value(lua, r)?, name))?;
    {
        let s = t.borrow::<LSignal>()?;
        sigs.raw_set(key, s.data().clone())?;
        let methods = s.methods().clone();
        cache(lua)?.sig_methods = Some(methods);
    }
    Ok(Value::UserData(t))
}

/// `inst.key` from a script (the Instance `__index`): the capability
/// gates first (security.rs), for members a rule names, then `index`.
fn index_entry(lua: &Lua, r: Ref, key: &str, atom: i32) -> mlua::Result<Value> {
    let (class_u, parent) = inst_class(lua, r)?;
    let rt = routes::route(class_u, key, atom);
    if rt.restricted {
        if let Some(v) = security::index_hook(lua, r, key)? {
            return Ok(v);
        }
    }
    index_routed(lua, r, key, class_u, parent, rt)
}

/// `index_entry` for the C `__index` (runtime_routes.rs `fast_index`):
/// a plain member (name, class name, parent, a stored reflected property
/// no handler computes) is read in one borrow. Also says whether the read
/// was quiet (no handler ran: no signal flush needed).
/// A fast read's answer: a value, or an instance (pushed from the
/// instance cache without an mlua value when it has one).
pub(crate) enum Fv {
    V(Value),
    Inst(Ref),
    /// A new array of these instances (GetChildren).
    Insts(Vec<Ref>),
    OneChild(Ref),
    NoChildren,
    /// A string, pushed as is.
    Str(String),
    Static(&'static str),
    /// A new CFrame, pushed without a registry reference.
    CFrame(Xform),
}

fn index_entry_fast(lua: &Lua, r: Ref, key: &str, atom: i32) -> (mlua::Result<Fv>, bool) {
    enum Got {
        Name(String),
        Class,
        Parent(Ref),
        Position(Vec3),
        Stored(Variant, Option<&'static str>),
        Default(Variant, Option<&'static str>, rbx_dom_weak::Ustr),
        Full(Ref),
    }
    let got = (|| -> mlua::Result<(rbx_dom_weak::Ustr, routes::Route, Got)> {
        let w = world(lua)?;
        let i = w
            .d()
            .dom
            .get_by_ref(r)
            .ok_or_else(|| rt_err("attempt to use an Instance that no longer exists"))?;
        let class_u = i.class;
        let rt = routes::route(class_u, key, atom);
        let got = if rt.child_only {
            // `inst.Child` (Studio: FindFirstChild after the
            // member lookup misses): a stored non-reflected property of that
            // name still wins, as in prop_get_plain; big child lists use
            // the child index (find_named_child).
            let stored = rbx_dom_weak::Ustr::from_existing(key).is_some_and(|k| i.properties.contains_key(&k));
            if stored || i.children().len() >= CHILD_INDEX_MIN {
                Got::Full(i.parent())
            } else {
                let d = w.d();
                let hide_core = d.dom.root_ref() == r && security::hides_core_services(lua);
                match i.children().iter().copied().find(|c| {
                    d.dom.get_by_ref(*c).is_some_and(|c| {
                        c.name == key && !(hide_core && security::is_hidden_service_class(&c.class))
                    })
                }) {
                    Some(c) => Got::Parent(c),
                    None => Got::Full(i.parent()),
                }
            }
        } else if key == "Position" && rt.core_special && rt.domains == 0 && !rt.ext
            && !rt.restricted && doc::is_a(class_u.as_str(), "BasePart")
        {
            thread_local!(static CFRAME_KEY: rbx_dom_weak::Ustr = ustr("CFrame"));
            let cf_key = CFRAME_KEY.with(|k| *k);
            let pos = match i.properties.get(&cf_key) {
                Some(Variant::CFrame(cf)) => Vec3::new(cf.position.x, cf.position.y, cf.position.z),
                _ => match w.d().prop_key(r, cf_key) {
                    Some(Variant::CFrame(cf)) => Vec3::new(cf.position.x, cf.position.y, cf.position.z),
                    _ => Vec3::ZERO,
                },
            };
            Got::Position(pos)
        } else if !rt.plain {
            Got::Full(i.parent())
        } else {
            match rt.special {
                routes::Special::Name => Got::Name(i.name.clone()),
                routes::Special::ClassName => Got::Class,
                routes::Special::Parent => Got::Parent(i.parent()),
                _ => match rt.prop {
                    Some(p) => match i.properties.get(&p.canon) {
                        // An unset object property yields to a same-named
                        // child (prop_get_plain): the full path.
                        Some(Variant::Ref(x)) if x.is_none() => Got::Full(i.parent()),
                        Some(Variant::Ref(x)) => Got::Parent(*x),
                        Some(v) => Got::Stored(v.clone(), p.enum_name),
                        // The class default (prop_get_plain's answer when it
                        // isn't an object property, which may yield to a
                        // child of the same name).
                        None => match w.d().prop_key(r, p.canon) {
                            Some(v) if !matches!(v, Variant::Ref(_)) => Got::Default(v, p.enum_name, p.canon),
                            _ => Got::Full(i.parent()),
                        },
                    },
                    None => Got::Full(i.parent()),
                },
            }
        };
        Ok((class_u, rt, got))
    })();
    let (class_u, rt, got) = match got {
        Ok(x) => x,
        Err(e) => return (Err(e), false),
    };
    let quiet = rt.quiet;
    let v = match got {
        Got::Full(parent) => {
            if rt.restricted {
                match security::index_hook(lua, r, key) {
                    Ok(Some(v)) => return (Ok(Fv::V(v)), false),
                    Ok(None) => {}
                    Err(e) => return (Err(e), false),
                }
            }
            return (index_routed(lua, r, key, class_u, parent, rt).map(Fv::V), quiet);
        }
        Got::Name(name) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::Str(name)), quiet);
        }
        Got::Class => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::Static(class_u.as_str())), quiet);
        }
        Got::Position(pos) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::V(LVector3(pos).into_value())), true);
        }
        Got::Stored(Variant::String(s), _) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::Str(s)), quiet);
        }
        Got::Parent(p) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::Inst(p)), quiet);
        }
        Got::Stored(Variant::CFrame(cf), _) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            return (Ok(Fv::CFrame(Xform::from_cframe(&cf))), quiet);
        }
        Got::Stored(v, en) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            variant_to_lua(lua, &v, en)
        }
        Got::Default(v, en, canon) => {
            rustudio_core::telemetry::touch(class_u.as_str(), key);
            crate::lua_bridge::default_read(lua, class_u.as_str(), canon.as_str());
            variant_to_lua(lua, &v, en)
        }
    };
    (v.map(Fv::V), quiet)
}

/// The cached userdata of instance `r` (the instance cache's weak table
/// slot), if it has one and access isn't blocked (else None: `inst_value`).
pub(crate) fn cached_inst_slot(lua: &Lua, r: Ref) -> Option<i64> {
    if domains::assets_cloud::scene::access_blocked(lua, r) {
        return None;
    }
    lua.app_data_ref::<Cache>()?.slots.get(&r).copied()
}

/// An argument of a native method, read off the Lua stack.
pub(crate) enum NArg<'a> {
    Nil,
    Bool(bool),
    Str(&'a str, i32),
    Inst(Ref),
    /// Anything else (the general dispatcher converts it).
    Other,
}

fn child_result(children: &[Ref]) -> Fv {
    match children {
        [] => Fv::NoChildren,
        [child] => Fv::OneChild(*child),
        children => Fv::Insts(children.to_vec()),
    }
}

/// Studio-style typed thunks for the hottest core methods, called from the
/// C `__namecall` with their arguments straight off the stack
/// (runtime_routes.rs `fast_namecall`): only when the method's route shows
/// the core implementation answers (no domain, no override, no gate) and
/// the arguments are the plain types it takes; else None, and the general
/// dispatcher runs. Answers as `call_method` would, and whether it was
/// quiet (nothing fired).
pub(crate) fn native_call(lua: &Lua, r: Ref, name: &str, atom: i32, args: &[NArg]) -> Option<mlua::Result<(Fv, bool)>> {
    let w = world(lua).ok()?;
    let instance = w.d().dom.get_by_ref(r)?;
    let class_u = instance.class;
    let rt = routes::route(class_u, name, atom);
    let Disp::Method(m, _) = rt.disp else { return None };
    if rt.restricted || rt.overridable || rt.method.domains != 0 {
        return None;
    }
    let class = class_u.as_str();
    let str1 = || match args.first() {
        Some(NArg::Str(s, _)) => Some(*s),
        _ => None,
    };
    if m == "GetChildren" && rt.method.core && (args.is_empty() || matches!(args, [NArg::Nil])) {
        let hide_core = w.d().dom.root_ref() == r && security::hides_core_services(lua);
        let value = if hide_core {
            let d = w.d();
            let kids: Vec<Ref> = instance
                .children()
                .iter()
                .copied()
                .filter(|c| d.dom.get_by_ref(*c).is_none_or(|i| !security::is_hidden_service_class(&i.class)))
                .collect();
            child_result(&kids)
        } else {
            child_result(instance.children())
        };
        rustudio_core::telemetry::touch(class, name);
        return Some(Ok((value, true)));
    }
    // The same list as the general GetDescendants (call_method), pushed
    // as an Instance array through the weak cache instead of an mlua table
    // built one inst_value at a time.
    if m == "GetDescendants" && rt.method.core && (args.is_empty() || matches!(args, [NArg::Nil])) {
        let d = w.d();
        let refs: Vec<Ref> = if d.dom.root_ref() == r && security::hides_core_services(lua) {
            descendants_skip_hidden(d, r)
        } else {
            d.dom.descendants_of(r).map(|i| i.referent()).filter(|x| *x != r).collect()
        };
        rustudio_core::telemetry::touch(class, name);
        return Some(Ok((child_result(&refs), true)));
    }
    if rt.method.core && class_u == "Players" {
        let d = w.d();
        match (m, args) {
            ("GetPlayers", []) => {
                let mut players = instance.children().iter().copied()
                    .filter(|r| d.class_of(*r).is_some_and(|class| class == "Player"));
                let value = match (players.next(), players.next()) {
                    (Some(first), Some(second)) => Fv::Insts([first, second].into_iter().chain(players).collect()),
                    (Some(player), None) => Fv::OneChild(player),
                    _ => Fv::NoChildren,
                };
                rustudio_core::telemetry::touch(class, name);
                return Some(Ok((value, true)));
            }
            ("GetPlayerFromCharacter", [NArg::Inst(character)]) => {
                let player = instance.children().iter().copied().find(|r| {
                    matches!(d.prop(*r, "Character"), Some(Variant::Ref(model)) if model == *character)
                });
                rustudio_core::telemetry::touch(class, name);
                return Some(Ok((Fv::Inst(player.unwrap_or_else(Ref::none)), true)));
            }
            _ => {}
        }
    }
    if m == "FindFirstChild" && rt.method.core && instance.children().len() < CHILD_INDEX_MIN {
        let child_name = str1()?;
        if matches!(args.get(1), None | Some(NArg::Nil | NArg::Bool(false))) {
            let is_root = w.d().dom.root_ref() == r;
            let hide_core = is_root && security::hides_core_services(lua);
            let child = instance.children().iter().copied().find(|c| {
                w.d().dom.get_by_ref(*c).is_some_and(|i| {
                    i.name == child_name && !(hide_core && security::is_hidden_service_class(&i.class))
                })
            });
            // (`game:FindFirstChild("Run Service")`, a service by its
            // Roblox name: find_named_child's alias, the slow way)
            let aliased = child.is_none()
                && mech::service_class_for_name(child_name).is_some()
                && is_root;
            if !aliased {
                rustudio_core::telemetry::touch(class, name);
                return Some(Ok((Fv::Inst(child.unwrap_or_else(Ref::none)), true)));
            }
        }
    }
    if m == "GetAttribute" && rt.method.core {
        let name = str1()?;
        static ATTRS: std::sync::LazyLock<rbx_dom_weak::Ustr> = std::sync::LazyLock::new(|| ustr("Attributes"));
        let value = match instance.properties.get(&*ATTRS) {
            Some(Variant::Attributes(attributes)) => attributes.get(name),
            _ => None,
        };
        rustudio_core::telemetry::touch(class, m);
        return Some(match value {
            None => Ok((Fv::V(Value::Nil), true)),
            Some(Variant::Float64(value)) => Ok((Fv::V(Value::Number(*value)), true)),
            Some(Variant::Bool(value)) => Ok((Fv::V(Value::Boolean(*value)), true)),
            Some(value) => {
                let value = value.clone();
                drop(w);
                variant_to_lua(lua, &value, None).map(|value| (Fv::V(value), true))
            }
        });
    }
    drop(w);
    if !rt.method.core {
        if m == "GetTagged" && rt.method.extended {
            let tag = str1()?;
            rustudio_core::telemetry::touch(class, name);
            return Some(ext::tagged(lua, tag).map(|refs| (Fv::Insts(refs), true)));
        }
        return None;
    }
    let out = match m {
        "IsA" => {
            let Some(NArg::Str(base, atom)) = args.first() else { return None };
            Ok((Fv::V(Value::Boolean(routes::is_a(class_u, base, *atom))), true))
        }
        "FindFirstChild" => {
            let n = str1()?;
            let rec = match args.get(1) {
                None | Some(NArg::Nil) => false,
                Some(NArg::Bool(b)) => *b,
                _ => return None,
            };
            let found = if rec {
                find_child(lua, r, |i| i.name == n, true)
            } else {
                find_named_child(lua, r, n)
            };
            found.map(|f| (Fv::Inst(f.unwrap_or_else(Ref::none)), true))
        }
        "FindFirstChildOfClass" => {
            let c = str1()?;
            find_child(lua, r, |i| i.class == c, false).map(|f| (Fv::Inst(f.unwrap_or_else(Ref::none)), true))
        }
        "FindFirstAncestorOfClass" => {
            let c = str1()?;
            find_ancestor(lua, r, |i| i.class == c).map(|f| (Fv::Inst(f.unwrap_or_else(Ref::none)), true))
        }
        "IsDescendantOf" => {
            let other = match args.first() {
                None | Some(NArg::Nil) => None,
                Some(NArg::Inst(o)) => Some(*o),
                _ => return None,
            };
            let w = world(lua).ok()?;
            Ok((Fv::V(Value::Boolean(other.is_some_and(|o| w.d().is_ancestor(o, r)))), true))
        }
        "GetService" | "FindService" => {
            let c = str1()?;
            if security::service_hidden(lua, c) {
                Ok((Fv::V(Value::Nil), true))
            } else {
                // (A service that exists is found without side effects:
                // nothing to flush after it.)
                let existing = world(lua).ok().and_then(|w| {
                    let d = w.d();
                    d.find_child(d.dom.root_ref(), c)
                });
                match existing {
                    Some(f) if !service_studio_only(lua, c) => Ok((Fv::Inst(f), true)),
                    _ => get_service(lua, c, m == "GetService").map(|f| (Fv::Inst(f.unwrap_or_else(Ref::none)), false)),
                }
            }
        }
        _ => return None,
    };
    rustudio_core::telemetry::touch(class, name);
    Some(out)
}

/// `inst.Parent = p` from the C `__newindex` when no domain or
/// runtime_ext takes Parent for the class (else None: the full path):
/// assigning the parent it has is a no-op, answered in one borrow.
/// Returns whether the write was quiet (nothing changed or fired).
fn parent_write(lua: &Lua, r: Ref, key: &str, atom: i32, p: Ref) -> Option<mlua::Result<bool>> {
    let (class_u, same) = {
        let w = world(lua).ok()?;
        let i = w.d().dom.get_by_ref(r)?;
        (i.class, i.parent() == p)
    };
    let rt = routes::route(class_u, key, atom);
    if !rt.plain_parent_write {
        return None;
    }
    rustudio_core::telemetry::touch(class_u.as_str(), key);
    // Domains whose newindex names Parent (testing's debugger objects) are
    // asked first, as in `newindex_routed`.
    if rt.ni_domains != 0 {
        let class = class_u.as_str();
        let taken = inst_value(lua, p)
            .and_then(|v| domains::newindex_masked(lua, r, class, key, &v, rt.ni_domains));
        match taken {
            Ok(false) => {}
            Ok(true) => return Some(Ok(false)),
            Err(e) => return Some(Err(e)),
        }
    }
    if same {
        return Some(Ok(true));
    }
    Some(script_set_parent(lua, r, p).map(|_| false))
}

/// A script's `inst.Parent = p`. A client script moving a Tool into or out
/// of a character equips or unequips it (ext::script_tool_move).
fn script_set_parent(lua: &Lua, r: Ref, p: Ref) -> mlua::Result<()> {
    let equip = ext::script_tool_move(lua, r, p)?;
    set_parent(lua, r, p)?;
    if let Some(request) = equip {
        world(lua)?.requests.equip.push(request);
    }
    Ok(())
}

/// Numeric Light.Brightness assignments use the usual property store and
/// change delivery, with no Lua value conversion or second World borrow.
fn brightness_write(lua: &Lua, r: Ref, atom: i32, value: f32) -> Option<mlua::Result<bool>> {
    let w = world(lua).ok()?;
    let instance = w.d().dom.get_by_ref(r)?;
    let class = instance.class.as_str();
    if !matches!(class, "PointLight" | "SpotLight" | "SurfaceLight") {
        return None;
    }
    let route = routes::route(instance.class, "Brightness", atom);
    let info = route.set?;
    if route.restricted
        || route.ni_domains != 0
        || route.ni_ext
        || info.read_only
        || info.acts
        || !matches!(info.ty, Some((VariantType::Float32, _)))
    {
        return None;
    }
    rustudio_core::telemetry::touch(class, "Brightness");
    let key = info.canon.as_str();
    crate::lua_bridge::plain_write(lua, class, key);
    let value = Variant::Float32(mech::setter_clamp(class, key, value).unwrap_or(value));
    let no_dispatch = w.now.is_empty();
    if instance.properties.get(&info.canon) == Some(&value) {
        return Some(Ok(no_dispatch));
    }
    let quiet = no_dispatch && !w.listening.contains(&r) && !w.item_changed && !rustudio_core::telemetry::on();
    Some(store_prop_in_world(lua, w, r, info.canon, value).map(|_| quiet))
}

/// A written value read straight off the Lua stack (`typed_write`).
#[derive(Clone, Copy)]
pub(crate) enum FastVal {
    Num(f64),
    Bool(bool),
    Vec3(Vec3),
    CFrame(Xform),
    Vec2(glam::Vec2),
    Color3([f32; 3]),
    UDim(LUDim),
    UDim2(LUDim2),
}

fn joint_pose_property(class: &str, name: &str) -> bool {
    match name {
        "Anchored" => doc::is_a(class, "BasePart"),
        "C0" | "C1" | "Enabled" | "Part0" | "Part1" => domains::world::part::poses_immediately(class),
        _ => false,
    }
}

fn camera_zoom_opposite(class: &str, property: &str) -> Option<&'static str> {
    match (class, property) {
        ("Player" | "StarterPlayer", "CameraMinZoomDistance") => Some("CameraMaxZoomDistance"),
        ("Player" | "StarterPlayer", "CameraMaxZoomDistance") => Some("CameraMinZoomDistance"),
        _ => None,
    }
}

/// `inst.key = value` for a reflected property stored as given: no domain,
/// runtime_ext or capability rule takes the member, it isn't one of
/// newindex_routed's own (Name, Parent, Attachment / BasePart frames, a
/// script's Enabled / Disabled, Humanoid.Health, FontSize), and `value`
/// is already the property's type. The value is converted, clamped and
/// stored as `newindex_routed` does, without making an mlua `Value` or
/// looking the World up again; an unchanged value is not stored again
/// (it fires nothing either way). Some(quiet): done (quiet: nothing can
/// have been fired); None: take the full path.
pub(crate) fn typed_write(lua: &Lua, r: Ref, key: &str, atom: i32, val: FastVal) -> Option<mlua::Result<bool>> {
    if domains::route_check_on() {
        return None;
    }
    let w = world(lua).ok()?;
    let instance = w.d().dom.get_by_ref(r)?;
    let class_u = instance.class;
    let route = routes::route(class_u, key, atom);
    let info = route.set?;
    if route.restricted
        || route.ni_domains != 0
        || route.ni_ext
        || route.special != routes::Special::None
        || route.set_attachment
        || info.read_only
    {
        return None;
    }
    let class = class_u.as_str();
    if route.set_part {
        // BasePart.Position (newindex_routed's set_part): the part moves,
        // keeping its turn; stored as its CFrame. Only when nothing hears
        // Changed (that path fires Position after CFrame).
        let FastVal::Vec3(v) = val else { return None };
        if key != "Position" || w.listening.contains(&r) || w.item_changed || rustudio_core::telemetry::on() {
            return None;
        }
        static CFRAME: std::sync::LazyLock<rbx_dom_weak::Ustr> = std::sync::LazyLock::new(|| ustr("CFrame"));
        let rot = match w.d().prop_key(r, *CFRAME) {
            Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf).rot,
            _ => glam::Mat3::IDENTITY,
        };
        let value = Variant::CFrame(Xform { pos: v, rot }.to_cframe());
        rustudio_core::telemetry::touch(class, key);
        let no_dispatch = w.now.is_empty();
        if instance.properties.get(&*CFRAME) == Some(&value) {
            return Some(Ok(no_dispatch));
        }
        return Some(store_prop_in_world(lua, w, r, *CFRAME, value).map(|_| no_dispatch));
    }
    let canon = info.canon.as_str();
    if key == "FontSize" || canon == "Disabled" || (canon == "Health" && class == "Humanoid")
        || camera_zoom_opposite(class, canon).is_some()
    {
        return None;
    }
    let (ty, _) = info.ty?;
    let value = match (ty, val) {
        (VariantType::Float32, FastVal::Num(n)) => {
            let f = n as f32;
            Variant::Float32(mech::setter_clamp(class, canon, f).unwrap_or(f))
        }
        (VariantType::Float64, FastVal::Num(n)) => Variant::Float64(n),
        (VariantType::Int32, FastVal::Num(n)) => Variant::Int32(n as i32),
        (VariantType::Int64, FastVal::Num(n)) => Variant::Int64(n as i64),
        (VariantType::Bool, FastVal::Bool(b)) => Variant::Bool(b),
        (VariantType::Vector3, FastVal::Vec3(v)) => Variant::Vector3(Vector3::new(v.x, v.y, v.z)),
        (VariantType::CFrame, FastVal::CFrame(x)) => Variant::CFrame(x.orthonormalized().to_cframe()),
        (VariantType::Vector2, FastVal::Vec2(v)) => Variant::Vector2(Vector2::new(v.x, v.y)),
        (VariantType::Color3 | VariantType::Color3uint8, FastVal::Color3(c)) => {
            // (As lua_to_variant: a property stored as Color3uint8 stays
            // one.)
            let c3 = Color3::new(c[0], c[1], c[2]);
            let uint8 = ty == VariantType::Color3uint8
                || match instance.properties.get(&info.canon) {
                    Some(v) => matches!(v, Variant::Color3uint8(_)),
                    None => matches!(w.d().prop_key(r, info.canon), Some(Variant::Color3uint8(_))),
                };
            if uint8 {
                Variant::Color3uint8(pack_color3uint8(c3))
            } else {
                Variant::Color3(c3)
            }
        }
        (VariantType::UDim, FastVal::UDim(x)) => Variant::UDim(UDim::new(x.scale, x.offset)),
        (VariantType::UDim2, FastVal::UDim2(x)) => {
            Variant::UDim2(UDim2::new(UDim::new(x.x.scale, x.x.offset), UDim::new(x.y.scale, x.y.offset)))
        }
        _ => return None,
    };
    rustudio_core::telemetry::touch(class, key);
    if !info.acts {
        crate::lua_bridge::plain_write(lua, class, canon);
    }
    let no_dispatch = w.now.is_empty();
    if instance.properties.get(&info.canon) == Some(&value) {
        return Some(Ok(no_dispatch));
    }
    // A part's CFrame carries its assembly (carry_assembly, after the store).
    let carry = if canon == "CFrame" && w.mode != RunMode::Edit && doc::is_a(class, "BasePart") {
        match instance.properties.get(&info.canon) {
            Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(cf)),
            _ => return None,
        }
    } else {
        None
    };
    let quiet = no_dispatch && !w.listening.contains(&r) && !w.item_changed && !rustudio_core::telemetry::on();
    // Nothing to fire or record for the change history: the value goes
    // into its slot in place (store_prop_in_world's changed path with no
    // listener, outside Edit; ScreenGui's twinned inset properties excepted).
    let mut w = w;
    if quiet && w.mode != RunMode::Edit && class_u != "ScreenGui"
        && canon != "CanvasPosition"
        && !joint_pose_property(class, canon) && store_in_place(&mut w, r, info.canon, &value)
    {
        if carry.is_some() {
            drop(w);
            return Some(carry_assembly(lua, r, carry).map(|_| true));
        }
        return Some(Ok(true));
    }
    let done = store_prop_in_world(lua, w, r, info.canon, value).map(|_| quiet);
    if done.is_ok() && carry.is_some() {
        return Some(carry_assembly(lua, r, carry).map(|_| quiet));
    }
    Some(done)
}

/// Replaces a property's stored (or class-shared) value with `value`,
/// known to differ, and marks it as `store_prop_in_world` would; false
/// (nothing done) when the instance has no value for it at all.
fn store_in_place(w: &mut World, r: Ref, key: rbx_dom_weak::Ustr, value: &Variant) -> bool {
    let d = w.dm();
    let Some(slot) = d.dom.get_by_ref_mut(r).and_then(|i| i.properties.get_mut(&key)) else {
        return false;
    };
    *slot = value.clone();
    d.mark_prop(r, key);
    if w.side == Side::Server && w.has_peer {
        w.record(RepOp::Prop(r, key.as_str().to_string()));
    }
    true
}

/// `inst.key = value` from a script (the Instance `__newindex`): the
/// capability gates for members a rule names, then `newindex`.
fn newindex_entry(lua: &Lua, r: Ref, key: &str, value: Value, atom: i32) -> mlua::Result<()> {
    let (class_u, _) = inst_class(lua, r)?;
    let rt = routes::route(class_u, key, atom);
    if rt.restricted {
        security::newindex_hook(lua, r, key)?;
    }
    // A plugin writing a script's source asks the store's permission check
    // (Studio's script-injection permission for installed plugins).
    // (Unanswered: the write is held until the prompt's answer.)
    if key == "Source" && security::current(lua) == security::PLUGIN_IDENT {
        if domains::hold_source_write(lua, r, &value)? {
            return Ok(());
        }
    }
    newindex_routed(lua, r, key, value, class_u, rt)
}

fn index(lua: &Lua, r: Ref, key: &str) -> mlua::Result<Value> {
    let (class_u, parent) = inst_class(lua, r)?;
    index_routed(lua, r, key, class_u, parent, routes::route(class_u, key, -1))
}

fn index_routed(
    lua: &Lua,
    r: Ref,
    key: &str,
    class_u: rbx_dom_weak::Ustr,
    parent: Ref,
    rt: routes::Route,
) -> mlua::Result<Value> {
    let class: &str = class_u.as_str();
    rustudio_core::telemetry::touch(class, key);
    match rt.special {
        routes::Special::None => {}
        routes::Special::Name => {
            let w = world(lua)?;
            let name = w.d().dom.get_by_ref(r).map(|i| i.name.as_str()).unwrap_or("");
            return Ok(Value::String(lua.create_string(name)?));
        }
        routes::Special::ClassName => return Ok(Value::String(lua.create_string(class)?)),
        routes::Special::Parent => return inst_value(lua, parent),
        routes::Special::ScriptEnabled => return Ok(Value::Boolean(script_enabled_here(lua, r)?)),
    }
    match rt.disp {
        // Callbacks can only be set by game scripts: Studio's __index (Luau
        // bridge) refuses to read one, set or not. CoreScripts
        // (RobloxScriptSecurity) may read callbacks like TextChatService's
        // OnChatWindowAdded / OnBubbleAdded to invoke developer hooks.
        Disp::Callback => {
            if security::current(lua) >= security::CORE {
                return Ok(cache(lua)?
                    .callbacks
                    .get(&(r, key.to_string()))
                    .map(|(f, _)| Value::Function(f.clone()))
                    .unwrap_or(Value::Nil));
            }
            return Err(rt_err(format!(
                "{key} is a callback member of {class}; you can only set the callback value, get is not available"
            )));
        }
        Disp::LuaMethod(method, base) => return Ok(Value::Function(lua_method_fn(lua, method, base)?)),
        Disp::Method(method, base) => return Ok(Value::Function(method_fn(lua, method, base)?)),
        Disp::Event => {
            crate::lua_bridge::event_used(lua, class, key);
            return signal(lua, r, key);
        }
        Disp::Other => {}
    }
    // Only the handlers that can answer this member (runtime_routes.rs).
    if rt.domains != 0 || domains::route_check_on() {
        if let Some(v) = domains::prop_get_masked(lua, r, class, key, rt.domains)? {
            return Ok(v);
        }
    }
    if rt.ext {
        if let Some(v) = ext::prop_get(lua, r, class, key)? {
            return Ok(v);
        }
    } else if domains::route_check_on() {
        if let Ok(Some(_)) | Err(_) = ext::prop_get(lua, r, class, key) {
            eprintln!("ROUTE MISS ext answers {class}.{key}");
        }
    }
    if let Some(v) = prop_get_routed(lua, r, class, key, rt.core_special, rt.prop)? {
        return Ok(v);
    }
    // Old lowercase spellings (sound:play, player:isFriendsWith) reach the
    // real capitalised member before any stub of the deprecated name; only
    // the ones Roblox has (in the API dump): `frame.transparency` is a child
    // named "transparency" there (VFX Suite's buttons), not Transparency.
    let api_alias = |c: &str| rustudio_core::api::get().map_or(true, |a| a.lookup(c, key).is_some());
    if let Some(cap) = capitalized(key).filter(|c| real_member(&class, c) && api_alias(&class)) {
        return index(lua, r, &cap);
    }
    // A member rustudio has no behaviour for yet still exists (crates/rustudio-engine/src/stubs.rs);
    // members come before children, as in Roblox.
    if let Some(v) = stubs::index(lua, r, &class, key)? {
        return Ok(v);
    }
    if let Some(c) = find_named_child(lua, r, key)? {
        return inst_value(lua, c);
    }
    // Old scripts use lowercase names (script.parent, game.workspace).
    if let Some(cap) = capitalized(key) {
        if let Ok(v) = index(lua, r, &cap) {
            return Ok(v);
        }
    }
    crate::lua_bridge::gap(lua, "missing", &class, key);
    Err(not_member(lua, r, &class, key))
}

/// Whether rustudio itself implements `name` on `class` (a method, an event
/// or a reflected property), as opposed to answering it with a stub.
fn real_member(class: &str, name: &str) -> bool {
    let domain_lua_methods = domains::lua_methods();
    let domain_methods = domains::methods();
    LUA_METHODS
        .iter()
        .chain(ext::LUA_METHODS)
        .chain(domain_lua_methods)
        .chain(METHODS.iter())
        .chain(ext::METHODS)
        .chain(editable::METHODS).chain(domain_methods)
        .any(|(m, base)| *m == name && doc::is_a(class, base))
        || event_exists(class, name)
        || ext::event_exists(class, name)
        || domains::event_exists(class, name)
        || prop_desc(class, name).is_some()
}

/// "parent" → "Parent" for keys starting lowercase.
fn capitalized(key: &str) -> Option<String> {
    let mut c = key.chars();
    let first = c.next()?;
    first
        .is_ascii_lowercase()
        .then(|| first.to_ascii_uppercase().to_string() + c.as_str())
}

fn part_xform(lua: &Lua, r: Ref) -> mlua::Result<Xform> {
    Ok(match world(lua)?.d().prop(r, "CFrame") {
        Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
        _ => Xform {
            pos: Vec3::ZERO,
            rot: glam::Mat3::IDENTITY,
        },
    })
}

/// An Attachment's frame in its part (CFrame) and the part's frame in the
/// world (identity when it isn't on a part yet, as in Roblox).
fn attachment_frames(lua: &Lua, r: Ref) -> mlua::Result<(Xform, Xform)> {
    let w = world(lua)?;
    let d = w.d();
    let local = match d.prop(r, "CFrame") {
        Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
        _ => Xform {
            pos: Vec3::ZERO,
            rot: glam::Mat3::IDENTITY,
        },
    };
    let part = rustudio_core::attachment::parent(d, r, true).unwrap_or(Xform {
        pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY,
    });
    Ok((local, part))
}

fn compose(a: &Xform, b: &Xform) -> Xform {
    Xform {
        pos: a.apply(b.pos),
        rot: a.rot * b.rot,
    }
}

fn prop_get(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    let rt = routes::route(ustr(class), key, -1);
    prop_get_routed(lua, r, class, key, rt.core_special, rt.prop)
}

/// The core read: computed frames and the like (`special`), else the
/// stored or default value of the reflected property `prop`.
fn prop_get_routed(
    lua: &Lua,
    r: Ref,
    class: &str,
    key: &str,
    special: bool,
    prop: Option<routes::PropInfo>,
) -> mlua::Result<Option<Value>> {
    if special {
        if let Some(v) = prop_get_special(lua, r, class, key)? {
            return Ok(Some(v));
        }
    }
    prop_get_plain(lua, r, class, key, prop)
}

fn prop_get_special(lua: &Lua, r: Ref, class: &str, key: &str) -> mlua::Result<Option<Value>> {
    // A text object's Font is its FontFace's Enum.Font item (font_enum.rs);
    // a file from before FontFace keeps only Font.
    if key == "Font" && crate::font_enum::has_font_view(class) {
        let v = {
            let w = world(lua)?;
            let d = w.d();
            let stored = |n: &str| {
                d.dom.get_by_ref(r).and_then(|i| i.properties.get(&rbx_dom_weak::ustr(n)).cloned())
            };
            match (stored("FontFace"), stored("Font")) {
                (Some(Variant::Font(f)), _) => crate::font_enum::enum_of_face(&f),
                (_, Some(Variant::Enum(e))) => e.to_u32(),
                _ => match d.prop(r, "FontFace") {
                    Some(Variant::Font(f)) => crate::font_enum::enum_of_face(&f),
                    _ => 0,
                },
            }
        };
        return Ok(Some(variant_to_lua(lua, &Variant::Enum(Enum::from_u32(v)), Some("Font"))?));
    }
    if class == "Bone" && matches!(key, "TransformedCFrame" | "TransformedWorldCFrame") {
        let d = world(lua)?;
        let xf = if key == "TransformedCFrame" { rustudio_core::attachment::local(d.d(), r, true) }
            else { rustudio_core::attachment::world(d.d(), r, true).unwrap_or(rustudio_core::attachment::local(d.d(), r, true)) };
        return Ok(Some(LCFrame(xf).into_lua(lua)?));
    }
    if doc::is_a(class, "Attachment")
        && matches!(
            key,
            "WorldCFrame" | "WorldPosition" | "WorldOrientation" | "Position" | "Orientation"
            | "Axis" | "SecondaryAxis" | "WorldAxis" | "WorldSecondaryAxis"
        )
    {
        let (local, part) = attachment_frames(lua, r)?;
        let worldf = compose(&part, &local);
        let v = match key {
            "WorldCFrame" => {
                return Ok(Some(crate::ltypes::LCFrame(worldf).into_lua(lua)?))
            }
            "WorldPosition" => worldf.pos,
            "WorldOrientation" => worldf.orientation_deg(),
            "Position" => local.pos,
            "Axis" => local.rot.x_axis,
            "SecondaryAxis" => local.rot.y_axis,
            "WorldAxis" => worldf.rot.x_axis,
            "WorldSecondaryAxis" => worldf.rot.y_axis,
            _ => local.orientation_deg(),
        };
        return Ok(Some(LVector3(v).into_value()));
    }
    if doc::is_a(class, "BasePart") {
        match key {
            "Position" | "Orientation" | "Rotation" => {
                let xf = part_xform(lua, r)?;
                let v = if key == "Position" {
                    xf.pos
                } else {
                    xf.orientation_deg()
                };
                return Ok(Some(LVector3(v).into_value()));
            }
            "BrickColor" => {
                let rgb = match world(lua)?.d().prop(r, "Color") {
                    Some(Variant::Color3uint8(c)) => [c.r, c.g, c.b],
                    Some(Variant::Color3(c)) => {
                        let c = Color3uint8::from(c);
                        [c.r, c.g, c.b]
                    }
                    _ => [163, 162, 165],
                };
                return Ok(Some(Value::UserData(
                    lua.create_userdata(LBrickColor(nearest_brick(rgb)))?,
                )));
            }
            "AssemblyLinearVelocity" => {
                let v = world(lua)?
                    .velocities
                    .get(&r)
                    .copied()
                    .unwrap_or(Vec3::ZERO);
                return Ok(Some(LVector3(v).into_value()));
            }
            "Velocity" => {
                let (v, doc_id) = {
                    let w = world(lua)?;
                    (
                        w.velocities.get(&r).copied().unwrap_or(Vec3::ZERO),
                        w.d().id,
                    )
                };
                let (spin, com) = rustudio_physics::physics::live::with_stats(doc_id, |s| {
                    s.spin.get(&r).copied()
                })
                .flatten()
                .unwrap_or((Vec3::ZERO, Vec3::ZERO));
                let pt_v = if spin != Vec3::ZERO {
                    let pos = part_xform(lua, r)?.pos;
                    v + spin.cross(pos - com)
                } else {
                    v
                };
                return Ok(Some(LVector3(pt_v).into_value()));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn prop_get_plain(
    lua: &Lua,
    r: Ref,
    class: &str,
    key: &str,
    prop: Option<routes::PropInfo>,
) -> mlua::Result<Option<Value>> {
    // (set_callback keeps only these)
    if matches!(key, "OnInvoke" | "OnServerInvoke" | "OnClientInvoke") {
        if let Some((f, _)) = cache(lua)?.callbacks.get(&(r, key.to_string())) {
            return Ok(Some(Value::Function(f.clone())));
        }
    }
    // A property scripts can't see (reflection NotScriptable, e.g.
    // Instance.Attributes): a child of that name answers, as in Studio.
    // A game's `require(Packages.Attributes)` got nil here and its
    // whole player setup failed (2026-11-21).
    if prop.is_some() && prop_hidden_from_scripts(class, key) && find_named_child(lua, r, key)?.is_some() {
        return Ok(None);
    }
    let (enum_name, value, defaulted) = match prop {
        Some(p) => {
            let (stored, v) = {
                let w = world(lua)?;
                let d = w.d();
                // (ScreenGui's IgnoreGuiInset / ScreenInsets are one value: doc.rs)
                let derived = |i: &rbx_dom_weak::Instance| {
                    i.class == "ScreenGui" && matches!(p.canon.as_str(), "IgnoreGuiInset" | "ScreenInsets")
                };
                match d.dom.get_by_ref(r).filter(|i| !derived(i)).and_then(|i| i.properties.get(&p.canon)) {
                    Some(v) => (true, Some(v.clone())),
                    None => (false, d.prop_key(r, p.canon)),
                }
            };
            (p.enum_name, v, (!stored).then_some(p.canon.as_str()))
        }
        None => {
            let v = world(lua)?
                .d()
                .dom
                .get_by_ref(r)
                // (A name never interned is no instance's property: don't
                // intern every child name a script indexes.)
                .and_then(|i| i.properties.get(&rbx_dom_weak::Ustr::from_existing(key)?).cloned());
            match v {
                // A stored non-reflected value (the file's own data) never
                // hides a child of that name (see `prop_hidden_from_scripts`).
                Some(_) if find_named_child(lua, r, key)?.is_some() => return Ok(None),
                Some(v) => (None, Some(v), None),
                // A class newer than the reflection database: Studio's
                // measured constructor value where there is one
                // (doc::constructor_default), else the stub's.
                None => match doc::class_default(class, key) {
                    Some(v) => (None, Some(v), None),
                    None => return Ok(None),
                },
            }
        }
    };
    // An unset object property (game.Workspace, workspace.Terrain, …) yields
    // to a child of the same name, which is what those properties point at.
    if matches!(value, None | Some(Variant::Ref(_)))
        && value
            .as_ref()
            .is_none_or(|v| matches!(v, Variant::Ref(x) if x.is_none()))
    {
        let child = {
            let w = world(lua)?;
            let d = w.d();
            let cap = capitalized(key);
            d.dom.get_by_ref(r).and_then(|i| {
                i.children().iter().copied().find(|c| {
                    d.dom
                        .get_by_ref(*c)
                        .is_some_and(|ci| ci.name == key || Some(&ci.name) == cap.as_ref())
                })
            })
        };
        if let Some(c) = child {
            return Ok(Some(inst_value(lua, c)?));
        }
    }
    // Answered from the reflection default: a gap if Roblox computes it.
    if let Some(canon) = defaulted {
        crate::lua_bridge::default_read(lua, class, canon);
    }
    match value {
        Some(v) => Ok(Some(variant_to_lua(lua, &v, enum_name)?)),
        // Known to the reflection database but with no default there: the
        // stub's typed default (false, 0, Enum.X.Default…) rather than nil.
        // (An unset object property is nil in Roblox too: not a stub.)
        None if stubs::typed_default(class, key) => match stubs::default_value(lua, r, class, key)? {
            Some(v) => Ok(Some(v)),
            None => Ok(Some(Value::Nil)),
        },
        None => Ok(Some(Value::Nil)),
    }
}

pub fn set_parent(lua: &Lua, r: Ref, new_parent: Ref) -> mlua::Result<()> {
    set_parent_opts(lua, r, new_parent, true, true, domains::world::part::EnterPose::Full)
}

fn set_parent_opts(lua: &Lua, r: Ref, new_parent: Ref, replicate: bool, stop_scripts: bool, pose: domains::world::part::EnterPose) -> mlua::Result<()> {
    let (old, descendants, lost, snaps, enters_ws, stopped, gripped, ungripped, moved_sounds) = {
        let mut w = world(lua)?;
        let old = w
            .d()
            .dom
            .get_by_ref(r)
            .map(|i| i.parent())
            .unwrap_or_else(Ref::none);
        // Assigning the existing parent is a no-op, including nil after
        // Destroy(). Index cleanup repeats Parent=nil on stripped book parts.
        if old == new_parent {
            return Ok(());
        }
        if w.destroyed.contains(&r) {
            let name = w
                .d()
                .dom
                .get_by_ref(r)
                .map(|i| i.name.clone())
                .unwrap_or_default();
            return Err(rt_err(format!(
                "The Parent property of {name} is locked, current parent: NULL, new parent {}",
                w.full_name(new_parent)
            )));
        }
        if r == w.d().dom.root_ref() {
            return Err(rt_err("The Parent property of game is locked"));
        }
        if new_parent.is_some() {
            if new_parent == r || w.d().is_ancestor(r, new_parent) {
                return Err(rt_err(
                    "Attempt to set parent of an Instance to itself or one of its descendants",
                ));
            }
            if w.d().dom.get_by_ref(new_parent).is_none() {
                return Err(rt_err("The new parent no longer exists"));
            }
        }
        let was_in = w.mode == RunMode::Edit && w.d().records_change(r);
        let in_ws = |d: &Document, p: Ref| p.is_some() && d.workspace().is_some_and(|ws| p == ws || d.is_ancestor(ws, p));
        let enters_ws = w.mode != RunMode::Edit && !in_ws(w.d(), old) && in_ws(w.d(), new_parent);
        let lost = primary_parts_leaving(&w, r, old, new_parent);
        let root = w.d().dom.root_ref();
        if replicate && w.mode != RunMode::Edit && (w.side == Side::Client || !w.has_peer)
            && w.d().is_ancestor(root, r) && new_parent != root && !w.d().is_ancestor(root, new_parent)
        {
            let removed: Vec<_> = w.d().dom.descendants_of(r)
                .filter_map(|instance| crate::audio_scene::on_remove(w.d(), instance.referent(), w.input.sound_listener))
                .collect();
            w.requests.removed_sounds.extend(removed);
        }
        if old.is_some() && w.d().class_of(r).is_some_and(|c| doc::is_a(&c, "GuiBase2d") || doc::is_a(&c, "UIComponent")) {
            ext::invalidate_layout_parent(&mut w, old);
        }
        w.dm().dom.transfer_within(r, new_parent);
        if new_parent.is_none() {
            w.orphans.insert(r);
        }
        if w.mode == RunMode::Edit {
            let is_in = w.d().records_change(r);
            if was_in || is_in {
                let c = if is_in && !was_in {
                    doc::Change::Enter { r, old, new: new_parent }
                } else {
                    doc::Change::Parent { r, old, new: new_parent }
                };
                w.dm().note_script_change(c);
            }
        }
        if replicate {
            w.record(RepOp::Parent(r));
        }
        w.dm().mark_tree(r);
        // An Explosion going into the Workspace goes off.
        if w.d().class_of(r).is_some_and(|c| c == "Explosion")
            && w.d().workspace().is_some_and(|ws| w.d().is_ancestor(ws, r))
        {
            w.requests.explosions.push(r);
        }
        w.structure_changed = true;
        let descendants: Vec<Ref> = w.d().dom.descendants_of(r).map(|i| i.referent()).collect();
        // Scripts arriving where they run start (next dispatch), as in Roblox.
        if new_parent.is_some() {
            w.queue_subtree_scripts(&descendants);
        }
        let snaps = if enters_ws {
            let (snaps, activated) = weld_snaps(w.d(), &descendants);
            for (weld, rel) in activated {
                set_weld_offset(w.dm(), weld, Some(rel));
            }
            snaps
        } else {
            Vec::new()
        };
        // A legacy Script moved from Workspace into ReplicatedStorage
        // stops. LocalScripts also stop on leaving a runnable container.
        let stopped = if stop_scripts && w.mode != RunMode::Edit {
            descendants.iter().copied().filter(|d| {
                w.started.contains(d) && !w.runs_here(*d)
            }).collect()
        } else {
            HashSet::new()
        };
        // Sounds that moved: Studio loads a Sound (or a Clone of one) as it
        // enters the DataModel, its descendants too, and unloads it on
        // leaving, in Edit and in Play alike (checks/sound-clone-load).
        let moved_sounds: Vec<Ref> = {
            let d = w.d();
            std::iter::once(r)
                .chain(descendants.iter().copied())
                .filter(|s| d.dom.get_by_ref(*s).is_some_and(|i| i.class == "Sound"))
                .collect()
        };
        let gripped = if w.mode != RunMode::Edit { crate::tool_grip::gripped_tools(w.d(), r, enters_ws) } else { Vec::new() };
        let ungripped = if w.mode != RunMode::Edit { crate::tool_grip::ungripped_welds(w.d(), r, old) } else { Vec::new() };
        (old, descendants, lost, snaps, enters_ws, stopped, gripped, ungripped, moved_sounds)
    };
    for (model, pivot) in lost {
        store_prop(lua, model, "PrimaryPart", Variant::Ref(Ref::none()))?;
        store_prop(lua, model, "WorldPivotData", Variant::OptionalCFrame(Some(pivot.to_cframe())))?;
    }
    for (part, xf) in snaps {
        store_prop(lua, part, "CFrame", Variant::CFrame(xf.to_cframe()))?;
    }
    if enters_ws {
        let mut w = world(lua)?;
        let moves = domains::world::part::entering_joint_moves(w.d(), &descendants, pose);
        apply_assembly_pose(&mut w, moves);
    }
    fire_parent_signals(lua, r, old, new_parent, descendants)?;
    for s in moved_sounds {
        domains::media::touch_sound(lua, s)?;
    }
    // After the hierarchy signals (Studio: an AncestryChanged handler on the Tool still
    // reads CanCollide true; EquipTool has returned with it false): the queued
    // Immediate fires run first.
    if !gripped.is_empty() || !ungripped.is_empty() {
        signal_timing::flush_immediate(lua);
    }
    for weld in ungripped {
        let still_exists = world(lua).is_ok_and(|w| w.d().exists(weld));
        if still_exists {
            set_parent_opts(lua, weld, Ref::none(), replicate, false, domains::world::part::EnterPose::Full)?;
        }
    }
    for g in gripped {
        if g.turn_off_collide {
            store_prop(lua, g.handle, "CanCollide", Variant::Bool(false))?;
        }
        let existing = {
            let w = world(lua)?;
            let d = w.d();
            d.dom.get_by_ref(g.hand).and_then(|h| {
                h.children().iter().copied().find(|c| {
                    d.dom.get_by_ref(*c).is_some_and(|i| {
                        i.name == "RightGrip"
                            && doc::is_a(&i.class, "Weld")
                            && matches!(d.prop(*c, "Part1"), Some(Variant::Ref(p1)) if p1 == g.handle)
                    })
                })
            })
        };
        if let Some(weld) = existing {
            store_prop(lua, weld, "Part0", Variant::Ref(g.hand))?;
            store_prop(lua, weld, "C0", Variant::CFrame(g.c0))?;
            store_prop(lua, weld, "C1", Variant::CFrame(g.c1))?;
        } else {
            let weld = {
                let mut w = world(lua)?;
                let b = rbx_dom_weak::InstanceBuilder::new("Weld")
                    .with_name("RightGrip")
                    .with_property("Part0", Variant::Ref(g.hand))
                    .with_property("Part1", Variant::Ref(g.handle))
                    .with_property("C0", Variant::CFrame(g.c0))
                    .with_property("C1", Variant::CFrame(g.c1));
                let root = w.d().dom.root_ref();
                let wref = w.dm().dom.insert(root, b);
                w.dm().dom.transfer_within(wref, Ref::none());
                w.orphans.insert(wref);
                wref
            };
            set_parent_opts(lua, weld, g.hand, replicate, false, domains::world::part::EnterPose::Full)?;
        }
    }
    if !stopped.is_empty() {
        signal_timing::flush_immediate(lua);
        stop_code_of(lua, &stopped);
    }
    Ok(())
}


/// Parts of `r`'s tree that its saved WeldConstraints place as it enters the
/// Workspace, and the welds of the tree that take their offset now. A
/// WeldConstraint keeps its offset (the hidden CFrame0 = Part0⁻¹ · Part1,
/// saved and cloned with it) and the assembly is built from it: a part a
/// script moved while the tree was out of the Workspace goes back where its
/// weld holds it. A game's lantern clones its
/// LanternAssets, sets every part's CFrame to the tool's Handle and only
/// then parents it; in Roblox the lantern still comes out whole. Parts
/// outside the tree stay put and the tree's parts are placed outward from
/// them (else from the first weld's Part0).
///
/// The offset is only there once the weld has been active (`weld_offset`):
/// a weld made outside the Workspace (a game's hunter rigs, built in
/// ReplicatedStorage) is saved with State 1 and an identity CFrame0, and
/// takes its parts' offset as it enters the Workspace (Studio 0.747,
/// measured in Studio). Snapping those to identity pulled every welded
/// trim of a cloned rig into its limb.
fn weld_snaps(d: &Document, subtree: &[Ref]) -> (Vec<(Ref, Xform)>, Vec<(Ref, Xform)>) {
    let cf = |x: Ref| match d.prop(x, "CFrame") {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(&c)),
        _ => None,
    };
    let part = |x: Option<Variant>| match x {
        Some(Variant::Ref(p)) if p.is_some() && d.class_of(p).is_some_and(|c| doc::is_a(&c, "BasePart")) => Some(p),
        _ => None,
    };
    // (Part0, Part1, Part0⁻¹ · Part1) for each enabled weld between live parts.
    let mut welds: Vec<(Ref, Ref, Xform)> = Vec::new();
    let mut activated: Vec<(Ref, Xform)> = Vec::new();
    for x in subtree {
        let Some(i) = d.dom.get_by_ref(*x) else { continue };
        if i.class != "WeldConstraint" || !rustudio_core::body::joint_enabled(d, *x) {
            continue;
        }
        if let Some((p0, p1)) = rustudio_core::body::joint_parts(d, *x) {
            // (a weld to a part outside the Workspace holds nothing: a clone's weld to
            // its template's part, still in ReplicatedStorage)
            let live = |p: Ref| part(Some(Variant::Ref(p))).is_some() && d.workspace().is_some_and(|ws| d.is_ancestor(ws, p));
            if p0 == p1 || !live(p0) || !live(p1) {
                continue;
            }
            match weld_offset(d, *x) {
                Some(off) => welds.push((p0, p1, off)),
                None => {
                    let (Some(a), Some(b)) = (cf(p0), cf(p1)) else { continue };
                    let rel = compose(&a.inverse(), &b);
                    activated.push((*x, rel));
                    welds.push((p0, p1, rel));
                }
            }
        }
    }
    if welds.is_empty() {
        return (Vec::new(), activated);
    }
    let inside: HashSet<Ref> = subtree.iter().copied().collect();
    welds.sort_by_key(|(a, b, _)| (format!("{a:?}"), format!("{b:?}")));
    let mut at: HashMap<Ref, Xform> = HashMap::new();
    let mut queue: VecDeque<Ref> = VecDeque::new();
    for (p0, p1, _) in &welds {
        for p in [*p0, *p1] {
            if !inside.contains(&p) && !at.contains_key(&p) {
                if let Some(x) = cf(p) {
                    at.insert(p, x);
                    queue.push_back(p);
                }
            }
        }
    }
    let mut out = Vec::new();
    let inv = |x: &Xform| Xform { rot: x.rot.transpose(), pos: -(x.rot.transpose() * x.pos) };
    loop {
        while let Some(p) = queue.pop_front() {
            let px = at[&p];
            for (p0, p1, off) in &welds {
                let (q, step) = if *p0 == p {
                    (*p1, *off)
                } else if *p1 == p {
                    (*p0, inv(off))
                } else {
                    continue;
                };
                if at.contains_key(&q) {
                    continue;
                }
                let want = compose(&px, &step);
                at.insert(q, want);
                queue.push_back(q);
                if let Some(cur) = cf(q) {
                    if (cur.pos - want.pos).length() > 1e-3 || !cur.rot.abs_diff_eq(want.rot, 1e-4) {
                        out.push((q, want));
                    }
                }
            }
        }
        // A tree of its own: placed from its first weld's Part0, which stays.
        let Some(start) = welds.iter().map(|w| w.0).find(|p| !at.contains_key(p)) else { break };
        let Some(x) = cf(start) else { break };
        at.insert(start, x);
        queue.push_back(start);
    }
    (out, activated)
}

/// A WeldConstraint's offset (Part0⁻¹ · Part1) if it has one: once a weld
/// has been active Studio keeps the offset in CFrame0 and sets State bit 2
/// (saved: 3 enabled, 0 disabled; a weld that was never in the Workspace
/// saves State 1 and an identity CFrame0, which holds nothing). A stored
/// CFrame0 that isn't identity counts too (the live wire carries State 1).
pub(crate) fn weld_offset(d: &Document, j: Ref) -> Option<Xform> {
    let i = d.dom.get_by_ref(j)?;
    let stored = match i.properties.get(&ustr("CFrame0")) {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(c)),
        _ => None,
    };
    let active = matches!(i.properties.get(&ustr("State")), Some(Variant::Int32(s)) if s & 2 != 0);
    let ident = Xform::from_cframe(&rbx_dom_weak::types::CFrame::identity());
    match stored {
        Some(x) if active => Some(x),
        None if active => Some(ident),
        Some(x) if !(x.pos.length() < 1e-6 && x.rot.abs_diff_eq(ident.rot, 1e-6)) => Some(x),
        _ => None,
    }
}

/// Records a WeldConstraint's offset (`Some`: CFrame0 and State bit 2, as
/// Studio does when the weld becomes active) or forgets it (`None`: the
/// weld takes a new one when it next becomes active).
fn set_weld_offset(d: &mut Document, j: Ref, rel: Option<Xform>) {
    let enabled = rustudio_core::body::joint_enabled(d, j);
    let Some(i) = d.dom.get_by_ref_mut(j) else { return };
    let bit0 = i32::from(enabled);
    match rel {
        Some(x) => {
            i.properties.insert(ustr("CFrame0"), Variant::CFrame(x.to_cframe()));
            i.properties.insert(ustr("State"), Variant::Int32(bit0 | 2));
        }
        None => {
            i.properties.insert(ustr("State"), Variant::Int32(bit0));
            i.properties.remove(&ustr("CFrame0"));
        }
    }
}

/// A script set a WeldConstraint's Part0, Part1 or Enabled: Studio takes the
/// offset again from where its parts are if the weld is active now, and
/// otherwise when it next enters the Workspace (a weld re-enabled while out
/// of the Workspace took the parts' new offset there).
fn weld_rejoined(w: &mut World, j: Ref) {
    let d = w.d();
    let live = |p: Ref| d.workspace().is_some_and(|ws| d.is_ancestor(ws, p));
    let cf = |x: Ref| match d.prop(x, "CFrame") {
        Some(Variant::CFrame(c)) => Some(Xform::from_cframe(&c)),
        _ => None,
    };
    let rel = if w.mode != RunMode::Edit && live(j) && rustudio_core::body::joint_enabled(d, j) {
        rustudio_core::body::joint_parts(d, j)
            .filter(|(a, b)| a != b && live(*a) && live(*b))
            .and_then(|(a, b)| Some(compose(&cf(a)?.inverse(), &cf(b)?)))
    } else {
        None
    };
    set_weld_offset(w.dm(), j, rel);
}

/// The Models (among `old` and its ancestors) whose PrimaryPart leaves them
/// when `r` moves from `old` to `new_parent`, each with the pivot it had.
/// Studio (checked in a Studio-11-15, execute_luau in Edit):
/// destroying the PrimaryPart, or parenting it out of the model, sets
/// Model.PrimaryPart to nil at once (the model in the DataModel or not), and
/// the model's pivot stays where the part's pivot was: GetPivot and
/// WorldPivot both read the old PrimaryPart.CFrame · PivotOffset after, so a
/// later PivotTo measures from there. A game's teleport menu
/// clones its sky box, destroys the box's Center (its PrimaryPart) and then
/// PivotTo's the box onto the camera every frame.
fn primary_parts_leaving(w: &World, r: Ref, old: Ref, new_parent: Ref) -> Vec<(Ref, Xform)> {
    let d = w.d();
    let mut out = Vec::new();
    let mut cur = old;
    while cur.is_some() {
        let Some(i) = d.dom.get_by_ref(cur) else { break };
        let pp = if doc::is_a(&i.class, "Model") {
            match d.prop(cur, "PrimaryPart") {
                Some(Variant::Ref(pp)) => pp,
                _ => Ref::none(),
            }
        } else {
            Ref::none()
        };
        {
            if pp.is_some()
                && (pp == r || d.is_ancestor(r, pp))
                && !(new_parent == cur || d.is_ancestor(cur, new_parent))
            {
                let pivot = match d.prop(pp, "CFrame") {
                    Some(Variant::CFrame(cf)) => {
                        let off = match d.prop(pp, "PivotOffset") {
                            Some(Variant::CFrame(o)) => Xform::from_cframe(&o),
                            _ => Xform { pos: Vec3::ZERO, rot: glam::Mat3::IDENTITY },
                        };
                        compose(&Xform::from_cframe(&cf), &off)
                    }
                    _ => model_pivot(w, cur),
                };
                out.push((cur, pivot));
            }
        }
        cur = i.parent();
    }
    out
}

/// The signals of `r` having moved from `old` to `new_parent` (already done
/// in the DOM): ChildRemoved / ChildAdded, DescendantRemoving / Added on
/// the ancestors that listen, CollectionService's tag signals, Changed
/// ("Parent") and AncestryChanged. Also used for edits made outside Lua
/// (the editor, undo, Team Create) that the plugin host passes on.
pub fn fire_parent_signals(
    lua: &Lua,
    r: Ref,
    old: Ref,
    new_parent: Ref,
    descendants: Vec<Ref>,
) -> mlua::Result<()> {
    fire_parent_signals_opts(lua, r, old, new_parent, descendants, true)
}

/// DescendantRemoving alone, on `old`'s listening ancestors for `r` and its
/// `descendants` (`r` still in place): the plugin host fires it before it
/// takes an instance the editor removed out of the tree again, so handlers
/// read its Parent as Studio's do (Immediate signals).
pub fn fire_descendant_removing(lua: &Lua, old: Ref, descendants: &[Ref]) -> mlua::Result<()> {
    let chain: Vec<Ref> = {
        let w = world(lua)?;
        let d = w.d();
        let mut out = Vec::new();
        let mut cur = old;
        while cur.is_some() {
            out.push(cur);
            cur = d.dom.get_by_ref(cur).map(|i| i.parent()).unwrap_or_else(Ref::none);
        }
        out.into_iter().filter(|a| w.listening.contains(a) && hears(lua, *a, "DescendantRemoving")).collect()
    };
    for a in chain {
        for d in descendants {
            let v = inst_value(lua, *d)?;
            world(lua)?.fire(a, "DescendantRemoving", vec![v]);
        }
    }
    Ok(())
}

/// `fire_parent_signals`, DescendantRemoving left out when `removing` is
/// false (already fired by `fire_descendant_removing`).
pub fn fire_parent_signals_opts(
    lua: &Lua,
    r: Ref,
    old: Ref,
    new_parent: Ref,
    descendants: Vec<Ref>,
    removing: bool,
) -> mlua::Result<()> {
    // The ancestor chains the subtree left and joined, and whether each
    // reaches the DataModel: tagged instances entering or leaving it fire
    // CollectionService's InstanceAdded / InstanceRemoved signals (Roblox
    // does this for parenting, cloning in and replication, not only AddTag).
    let chain = |w: &World, from: Ref| -> (Vec<Ref>, bool) {
        let d = w.d();
        let root = d.dom.root_ref();
        let mut out = Vec::new();
        let mut cur = from;
        while cur.is_some() {
            out.push(cur);
            if cur == root {
                return (out, true);
            }
            cur = d.dom.get_by_ref(cur).map(|i| i.parent()).unwrap_or_else(Ref::none);
        }
        (out, false)
    };
    let (old_chain, was_in) = chain(&*world(lua)?, old);
    let (new_chain, now_in) = chain(&*world(lua)?, new_parent);
    // Tagged instances in the moved subtree, when it crossed the DataModel's edge.
    let tagged_moved: Vec<(Ref, Vec<String>)> = if was_in != now_in {
        let w = world(lua)?;
        let key = ustr("Tags");
        descendants
            .iter()
            .filter_map(|d| match w.d().dom.get_by_ref(*d)?.properties.get(&key) {
                Some(Variant::Tags(t)) if t.iter().next().is_some() => {
                    Some((*d, t.iter().map(|s| s.to_string()).collect()))
                }
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let cs = if tagged_moved.is_empty() {
        None
    } else {
        get_service(lua, "CollectionService", false)?
    };
    // DescendantAdded / DescendantRemoving fire on every ancestor for the
    // moved instance and each of its descendants, as in Roblox. Values are
    // only made for ancestors someone listens to (maps move thousands).
    let mut values: HashMap<Ref, Value> = HashMap::new();
    let mut value_of = |d: Ref| -> mlua::Result<Value> {
        if let Some(v) = values.get(&d) {
            return Ok(v.clone());
        }
        let v = inst_value(lua, d)?;
        values.insert(d, v.clone());
        Ok(v)
    };
    // Only ancestors with a connection or waiter on that very event: `game`
    // always has some (engine domains), and a 20k-part model made 20k
    // DescendantRemoving fires there for nobody.
    let listening = |chain: &[Ref], event: &str| -> mlua::Result<Vec<Ref>> {
        let w = world(lua)?;
        Ok(chain
            .iter()
            .copied()
            .filter(|a| w.listening.contains(a) && hears(lua, *a, event))
            .collect())
    };
    let old_listen = if removing { listening(&old_chain, "DescendantRemoving")? } else { Vec::new() };
    let new_listen = listening(&new_chain, "DescendantAdded")?;
    let mut fires: Vec<(Ref, &'static str, Value)> = Vec::new();
    for a in &old_listen {
        for d in &descendants {
            fires.push((*a, "DescendantRemoving", value_of(*d)?));
        }
    }
    let mut add_fires: Vec<(Ref, &'static str, Value)> = Vec::new();
    for a in &new_listen {
        for d in &descendants {
            add_fires.push((*a, "DescendantAdded", value_of(*d)?));
        }
    }
    let mut tag_fires: Vec<(Ref, String, Value)> = Vec::new();
    if let Some(cs) = cs {
        let ev = if now_in { "TagAdded:" } else { "TagRemoved:" };
        for (d, tags) in &tagged_moved {
            for t in tags {
                tag_fires.push((cs, format!("{ev}{t}"), value_of(*d)?));
            }
        }
    }
    // Class watches (`class_signals`): DescendantRemoving / Added on game
    // for the instances of the watched classes only.
    let mut watch_out: Vec<(Ref, String, Value)> = Vec::new();
    let mut watch_in: Vec<(Ref, String, Value)> = Vec::new();
    let watched = !world(lua)?.class_watches.is_empty();
    if watched && (was_in || now_in) {
        let root = world(lua)?.d().dom.root_ref();
        for d in &descendants {
            let hits = class_watch_hits(&mut *world(lua)?, *d);
            for i in hits {
                if was_in {
                    watch_out.push((root, format!("ClassRemoving:{i}"), value_of(*d)?));
                }
                if now_in {
                    watch_in.push((root, format!("ClassAdded:{i}"), value_of(*d)?));
                }
            }
        }
    }
    // AncestryChanged runs children first, with siblings in tree order.
    // Skip the traversal entirely when the subtree has no listeners.
    let ancestry = {
        let w = world(lua)?;
        if descendants.iter().any(|d| w.listening.contains(d) && hears(lua, *d, "AncestryChanged")) {
            let mut targets = Vec::new();
            let mut stack = vec![(r, false)];
            while let Some((d, visited)) = stack.pop() {
                if visited {
                    if w.listening.contains(&d) && hears(lua, d, "AncestryChanged") {
                        targets.push(d);
                    }
                } else {
                    stack.push((d, true));
                    if let Some(i) = w.d().dom.get_by_ref(d) {
                        stack.extend(i.children().iter().rev().map(|&c| (c, false)));
                    }
                }
            }
            targets
        } else {
            Vec::new()
        }
    };
    let (needs_child, needs_property) = {
        let w = world(lua)?;
        (
            !ancestry.is_empty() || w.listening.contains(&old) || w.listening.contains(&new_parent),
            w.item_changed || w.listening.contains(&r),
        )
    };
    let me = if needs_child { value_of(r)? } else { Value::Nil };
    let pname = if needs_property { Value::String(lua.create_string("Parent")?) } else { Value::Nil };
    let parent = if ancestry.is_empty() { Value::Nil } else { inst_value(lua, new_parent)? };
    let mut w = world(lua)?;
    if old.is_some() {
        w.fire(old, "ChildRemoved", vec![me.clone()]);
        for (a, name, v) in fires {
            w.fire(a, name, vec![v]);
        }
        for (a, name, v) in watch_out {
            w.fire(a, &name, vec![v]);
        }
    }
    if new_parent.is_some() {
        w.fire(new_parent, "ChildAdded", vec![me.clone()]);
        for (a, name, v) in add_fires {
            w.fire(a, name, vec![v]);
        }
        for (a, name, v) in watch_in {
            w.fire(a, &name, vec![v]);
        }
    }
    for (cs, name, v) in tag_fires {
        w.fire(cs, &name, vec![v]);
    }
    w.fire(r, "Changed", vec![pname]);
    w.fire(r, "Changed:Parent", vec![]);
    for d in ancestry {
        w.fire(d, "AncestryChanged", vec![me.clone(), parent.clone()]);
    }
    Ok(())
}

pub(crate) fn store_prop(lua: &Lua, r: Ref, name: &str, v: Variant) -> mlua::Result<()> {
    store_prop_key(lua, r, ustr(name), v)
}

/// `store_prop` by an interned name.
fn store_prop_key(lua: &Lua, r: Ref, key: rbx_dom_weak::Ustr, v: Variant) -> mlua::Result<()> {
    store_prop_in_world(lua, world(lua)?, r, key, v)
}

fn store_prop_in_world(
    lua: &Lua,
    mut w: mlua::AppDataRefMut<'_, World>,
    r: Ref,
    key: rbx_dom_weak::Ustr,
    v: Variant,
) -> mlua::Result<()> {
    let name = key.as_str();
    if name == "Contents" {
        domains::players_chat::loc::bump_contents_gen();
    }
    // A plugin's change of the edit DataModel goes into ChangeHistoryService
    // (doc_history.rs; Source is the script editor's own undo, as Studio's).
    let before = (w.mode == RunMode::Edit && name != "Source" && w.d().records_change(r))
        .then(|| w.d().dom.get_by_ref(r).map(|i| i.properties.clone()))
        .flatten();
    let old_frame = if w.mode != RunMode::Edit && matches!(name, "C0" | "C1") {
        w.d().prop_key(r, key)
    } else {
        None
    };
    let mut v = v;
    if name == "CanvasPosition" {
        if let Variant::Vector2(cp) = v {
            let is_sf = w.d().class_of(r).is_some_and(|c| c == "ScrollingFrame");
            if is_sf {
                let (cs, acs) = {
                    let d = w.d();
                    let cs = match d.prop(r, "CanvasSize") {
                        Some(Variant::UDim2(u)) => {
                            [u.x.scale, u.x.offset as f32, u.y.scale, u.y.offset as f32]
                        }
                        _ => [0.0, 0.0, 2.0, 0.0],
                    };
                    let acs = rustudio_render::scene::enum_u32(d.prop(r, "AutomaticCanvasSize"))
                        .unwrap_or(0);
                    (cs, acs)
                };
                let clamped = if let Some(b) = w.input.gui_layout.get(&r).copied() {
                    b.canvas_for(cs).or(b.canvas).map(|canvas| {
                        let win = b.window.unwrap_or(b.size);
                        let max_x = (canvas[0] - win[0]).max(0.0);
                        let max_y = (canvas[1] - win[1]).max(0.0);
                        rbx_dom_weak::types::Vector2::new(
                            cp.x.clamp(0.0, max_x),
                            cp.y.clamp(0.0, max_y),
                        )
                    })
                } else {
                    let ab = ext::abs_now(&mut w, r);
                    if ab.size[0] > 0.0 || ab.size[1] > 0.0 {
                        let nx = if acs & 1 != 0 {
                            cp.x.max(0.0)
                        } else {
                            let max_x = ((cs[0] * ab.size[0] + cs[1]).max(ab.size[0]) - ab.size[0]).max(0.0);
                            cp.x.clamp(0.0, max_x)
                        };
                        let ny = if acs & 2 != 0 {
                            cp.y.max(0.0)
                        } else {
                            let max_y = ((cs[2] * ab.size[1] + cs[3]).max(ab.size[1]) - ab.size[1]).max(0.0);
                            cp.y.clamp(0.0, max_y)
                        };
                        Some(rbx_dom_weak::types::Vector2::new(nx, ny))
                    } else {
                        None
                    }
                };
                if let Some(c) = clamped {
                    v = Variant::Vector2(c);
                }
            }
        }
    }
    let changed = {
        let Some(inst) = w.dm().dom.get_by_ref_mut(r) else {
            return Err(rt_err("the Instance no longer exists"));
        };
        let old = inst.properties.insert(key, v.clone());
        let unset = old.is_none();
        if let Some((twin, tv)) = doc::screen_insets_twin(inst.class.as_str(), name, &v) {
            inst.properties.insert(ustr(twin), tv);
        }
        // A legacy asset id and its Content twin are one property in Roblox
        // (Texture / TextureContent …): keep the twin in step, or readers that
        // take the twin first see a stale value or the class default (every
        // Place A Vfx emitter, made with Instance.new and then given Texture, drew
        // the default sparkle from TextureContent's default).
        if let Some(twin) = doc::content_twin(name) {
            let uri = rustudio_render::scene::content_string(Some(v.clone())).unwrap_or_default();
            let c = if uri.is_empty() {
                rbx_dom_weak::types::Content::none()
            } else {
                rbx_dom_weak::types::Content::from_uri(uri)
            };
            inst.properties.insert(ustr(twin), Variant::Content(c));
        } else if name.ends_with("Content") {
            for legacy in doc::content_legacy_names(name) {
                inst.properties.remove(&ustr(legacy));
            }
        } else if name == "FontFace" && crate::font_enum::has_font_view(inst.class.as_str()) {
            // The legacy Font enum is a view of FontFace (font_enum.rs): a
            // stored one would be stale now.
            inst.properties.remove(&ustr("Font"));
        }
        (old.as_ref() != Some(&v), unset)
    };
    let target = match &v {
        Variant::Ref(target) if target.is_some() => Some(*target),
        _ => None,
    };
    if let Some(edges) = w.prop_ref_edges.get_mut(&r) {
        if let Some(index) = edges.iter().position(|(prop, _)| *prop == key) {
            let (_, old_target) = edges.swap_remove(index);
            if edges.is_empty() {
                w.prop_ref_edges.remove(&r);
            }
            release_prop_ref(&mut w.prop_ref_counts, old_target);
        }
    }
    if let Some(target) = target {
        w.prop_ref_edges.entry(r).or_default().push((key, target));
        *w.prop_ref_counts.entry(target).or_insert(0) += 1;
    }
    // A property never set reads its default: writing that same value
    // changes nothing and fires nothing (Studio: Anchored = false on a new
    // Part fires no Changed, captures/misc2.txt).
    let changed = match changed {
        (true, true) => {
            let d = w.d();
            let class = d.class_of(r);
            let dflt = class.and_then(|c| doc::studio_default(c.as_str(), name).or_else(|| {
                let db = doc::db();
                db.classes.get(c.as_str()).and_then(|cd| db.find_default_property(cd, name).cloned())
            }));
            dflt.as_ref() != Some(&v)
        }
        (c, _) => c,
    };
    if let Some(before) = before {
        note_prop_changes(&mut w, r, &before);
    }
    if changed {
        w.dm().mark_prop(r, key);
        // (The record only when a peer takes it: no name copied per write
        // on a client or in Edit.)
        if w.side == Side::Server && w.has_peer {
            w.record(RepOp::Prop(r, name.to_string()));
        }
        if matches!(name, "Part0" | "Part1" | "Enabled") && w.d().class_of(r).is_some_and(|c| c == "WeldConstraint") {
            weld_rejoined(&mut w, r);
        }
        if w.mode != RunMode::Edit && matches!(name, "Anchored" | "C0" | "C1" | "Enabled" | "Part0" | "Part1")
            && w.d().class_of(r).is_some_and(|class| joint_pose_property(class.as_str(), name))
        {
            let moves = domains::world::part::joint_property_moves(w.d(), r, name, old_frame.as_ref());
            apply_assembly_pose(&mut w, moves);
        }
        // (Physics sees the change through the document's marks.)
        // A part's Size changes its mass: Studio fires Changed for Size,
        // then AssemblyMass, then Mass (checks/plugins/services/captures/
        // misc2.txt `part.size.changed`).
        let class = if name == "Size" || name == "Value" { w.d().class_of(r) } else { None };
        let mass_changed = name == "Size" && class.is_some_and(|c| doc::is_a(&c, "BasePart"));
        // Nobody listening to this instance or to game.ItemChanged
        // (services' on_fire), telemetry off: both fires below would do nothing.
        if !w.listening.contains(&r) && !w.item_changed && !rustudio_core::telemetry::on() {
            return Ok(());
        }
        // Changed's argument: the property name, or a ValueBase's new value.
        let value_changed = name == "Value" && class.is_some_and(|c| doc::is_a(&c, "ValueBase"));
        drop(w);
        let pname = if value_changed {
            variant_to_lua(lua, &v, None)?
        } else {
            Value::String(lua.create_string(name)?)
        };
        let mut w = world(lua)?;
        w.fire(r, "Changed", vec![pname]);
        w.fire(r, &format!("Changed:{name}"), vec![]);
        if mass_changed {
            for k in ["AssemblyMass", "Mass"] {
                w.fire(r, "Changed", vec![Value::String(lua.create_string(k)?)]);
                w.fire(r, &format!("Changed:{k}"), vec![]);
            }
        }
    }
    Ok(())
}

/// Changed(key) and Changed:key on `r` for a property set through another
/// (Position through CFrame), made only when something could hear them:
/// a connection on `r`, game.ItemChanged, or telemetry.
fn fire_changed(lua: &Lua, r: Ref, key: &str) -> mlua::Result<()> {
    {
        let w = world(lua)?;
        if !w.listening.contains(&r) && !w.item_changed && !rustudio_core::telemetry::on() {
            return Ok(());
        }
    }
    let pname = Value::String(lua.create_string(key)?);
    let mut w = world(lua)?;
    w.fire(r, "Changed", vec![pname]);
    w.fire(r, &format!("Changed:{key}"), vec![]);
    Ok(())
}

/// After a script moved part `r` from `before` (CFrame, PivotTo): the rest
/// of its assembly moves with it at once, keeping its offsets, as in
/// Studio, where a read of a welded part right after the write already
/// sees it moved (an FPS view model sets its AnimBase from the camera,
/// then reads the Grip and AimPart it carries to aim down the sights; a
/// read a frame stale left the sights half way). The offsets are the
/// parts' current ones: a Motor6D.Transform set since the last step
/// isn't in them yet (Studio's neither). Play / Run only; parts outside
/// Workspace aren't joined (Studio leaves them where they are).
fn carry_assembly(lua: &Lua, r: Ref, before: Option<Xform>) -> mlua::Result<()> {
    let Some(before) = before else { return Ok(()) };
    let mut w = world(lua)?;
    if w.mode == RunMode::Edit {
        return Ok(());
    }
    let moves = {
        let d = w.d();
        let after = match d.prop(r, "CFrame") {
            Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf),
            _ => return Ok(()),
        };
        if after == before {
            return Ok(());
        }
        let carried = domains::world::part::with_graph(d, |g| g.carried_by_move(d, r));
        if carried.is_empty() {
            return Ok(());
        }
        let delta = cf_mul(&after, &cf_inv(&before));
        let moves: Vec<(Ref, Xform)> = carried
            .into_iter()
            .filter_map(|q| match d.prop(q, "CFrame") {
                Some(Variant::CFrame(cf)) => Some((
                    q,
                    cf_mul(&delta, &Xform::from_cframe(&cf)).orthonormalized(),
                )),
                _ => None,
            })
            .collect();
        moves
    };
    apply_assembly_pose(&mut w, moves);
    Ok(())
}

fn apply_assembly_pose(w: &mut World, moves: Vec<(Ref, Xform)>) {
    static CFRAME: std::sync::LazyLock<rbx_dom_weak::Ustr> = std::sync::LazyLock::new(|| ustr("CFrame"));
    for (q, pose) in moves {
        let v = Variant::CFrame(pose.to_cframe());
        if !store_in_place(w, q, *CFRAME, &v) {
            if let Some(i) = w.dm().dom.get_by_ref_mut(q) {
                i.properties.insert(*CFRAME, v);
            }
            w.dm().mark_prop(q, *CFRAME);
            if w.side == Side::Server && w.has_peer {
                w.record(RepOp::Prop(q, "CFrame".to_string()));
            }
        }
    }
}

/// The change history's record of a script's property write: every
/// property of `r` that differs from `before` (the written one and its
/// twins).
pub(crate) fn note_prop_changes(w: &mut World, r: Ref, before: &rbx_dom_weak::PropMap) {
    let Some(inst) = w.d().dom.get_by_ref(r) else { return };
    let mut changes = Vec::new();
    for (k, nv) in inst.properties.iter() {
        if before.get(k) != Some(nv) {
            changes.push(doc::Change::Prop { r, name: *k, old: before.get(k).cloned(), new: Some(nv.clone()) });
        }
    }
    for (k, ov) in before.iter() {
        if !inst.properties.contains_key(k) {
            changes.push(doc::Change::Prop { r, name: *k, old: Some(ov.clone()), new: None });
        }
    }
    for c in changes {
        w.dm().note_script_change(c);
    }
}

/// A script's write of one property the runtime makes directly (Tags…),
/// `old` its value before: into the change history as `store_prop` does.
pub(crate) fn note_script_prop(w: &mut World, r: Ref, key: rbx_dom_weak::Ustr, old: Option<Variant>) {
    if w.mode != RunMode::Edit || !w.d().records_change(r) {
        return;
    }
    let new = w.d().dom.get_by_ref(r).and_then(|i| i.properties.get(&key).cloned());
    if new != old {
        w.dm().note_script_change(doc::Change::Prop { r, name: key, old, new });
    }
}

/// Sets or clears one attribute on `r`, preserving all other attributes,
/// firing `AttributeChanged` / `AttributeChanged:{name}`, and recording a
/// per-attribute replication op on the server.
fn set_attr(lua: &Lua, r: Ref, name: &str, var: Option<Variant>) -> mlua::Result<()> {
    let mut w = world(lua)?;
    let key = ustr("Attributes");
    let old_prop = {
        let Some(inst) = w.dm().dom.get_by_ref_mut(r) else {
            return Err(rt_err("the Instance no longer exists"));
        };
        let empty = Attributes::new();
        let old = match inst.properties.get(&key) {
            Some(Variant::Attributes(a)) => a,
            _ => &empty,
        };
        // The value it already has: nothing changes and nothing fires
        // (Roblox fires AttributeChanged only on a change).
        if old.get(name) == var.as_ref() {
            return Ok(());
        }
        // Rebuild without the key, then add the new value (nil clears it).
        let mut attrs = Attributes::new();
        for (k, v) in old.iter() {
            if k.as_str() != name {
                attrs.insert(k.clone(), v.clone());
            }
        }
        if let Some(var) = var {
            attrs.insert(name.to_string(), var);
        }
        inst.properties.insert(key, Variant::Attributes(attrs))
    };
    note_script_prop(&mut w, r, key, old_prop);
    w.dm().mark_prop(r, key);
    w.record(RepOp::Attr(r, name.to_string()));
    if !w.listening.contains(&r) && !rustudio_core::telemetry::on() {
        return Ok(());
    }
    drop(w);
    let an = Value::String(lua.create_string(name)?);
    let mut w = world(lua)?;
    w.fire(r, "AttributeChanged", vec![an]);
    w.fire(r, &format!("AttributeChanged:{name}"), vec![]);
    Ok(())
}

pub(crate) fn set_callback(lua: &Lua, r: Ref, key: &str, value: Value) -> mlua::Result<()> {
    let ident = security::current(lua);
    let mut c = cache(lua)?;
    match value {
        Value::Function(f) => {
            c.callbacks.insert((r, key.to_string()), (f, ident));
        }
        _ => {
            c.callbacks.retain(|k, _| !(k.0 == r && k.1 == key));
        }
    }
    Ok(())
}

fn newindex(lua: &Lua, r: Ref, key: &str, value: Value) -> mlua::Result<()> {
    let (class_u, _) = inst_class(lua, r)?;
    newindex_routed(lua, r, key, value, class_u, routes::route(class_u, key, -1))
}

/// `inst.key = value` with the member's route (runtime_routes.rs): only
/// the domains that can take the write are asked.
fn newindex_routed(
    lua: &Lua,
    r: Ref,
    key: &str,
    value: Value,
    class_u: rbx_dom_weak::Ustr,
    rt: routes::Route,
) -> mlua::Result<()> {
    let class: &str = class_u.as_str();
    rustudio_core::telemetry::touch(class, key);
    if (rt.ni_domains != 0 || domains::route_check_on())
        && domains::newindex_masked(lua, r, class, key, &value, rt.ni_domains)?
    {
        return Ok(());
    }
    if (rt.ni_ext || domains::route_check_on()) && ext::newindex(lua, r, class, key, &value)? {
        if !rt.ni_ext {
            eprintln!("ROUTE MISS ext takes {class}.{key} =");
        }
        return Ok(());
    }
    // The old FontSize enum ("Size28", Enum.FontSize.Size28) is TextSize in
    // Roblox: setting one sets the other.
    if key == "FontSize" && matches!(class, "TextLabel" | "TextButton" | "TextBox") {
        let name = match &value {
            Value::String(s) => s.to_str()?.to_string(),
            v => opt_ud::<LEnumItem>(v)
                .map(|e| e.name)
                .ok_or_else(|| bad_assign("FontSize", "EnumItem", v))?,
        };
        let px = name
            .strip_prefix("Size")
            .and_then(|n| n.parse::<f64>().ok())
            .ok_or_else(|| rt_err(&format!("Invalid value \"{name}\" for enum FontSize")))?;
        return newindex(lua, r, "TextSize", Value::Number(px));
    }
    if matches!(rt.disp, Disp::Callback) {
        return set_callback(lua, r, key, value);
    }
    match key {
        "Name" => {
            let s = match &value {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Number(n) => n.to_string(),
                _ => return Err(bad_assign("Name", "string", &value)),
            };
            let pname = Value::String(lua.create_string("Name")?);
            let mut w = world(lua)?;
            let rec = w.mode == RunMode::Edit && w.d().records_change(r);
            if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                let old = std::mem::replace(&mut i.name, s.clone());
                if rec && old != s {
                    w.dm().note_script_change(doc::Change::Name { r, old, new: s });
                }
            }
            w.dm().mark_prop(r, ustr("Name"));
            w.fire(r, "Changed", vec![pname]);
            w.fire(r, "Changed:Name", vec![]);
            w.record(RepOp::Name(r));
            return Ok(());
        }
        "Parent" => {
            let p = match &value {
                Value::Nil => Ref::none(),
                // Luau's argument error at the assigning line.
                v => opt_inst(v).ok_or_else(|| {
                    crate::cmeta::located(format!(
                        "invalid argument #3 (Instance expected, got {})",
                        crate::cmeta::type_name(v)
                    ))
                })?,
            };
            return script_set_parent(lua, r, p);
        }
        "ClassName" => {
            return Err(rt_err(
                "Unable to assign property ClassName. Property is read only",
            ))
        }
        "OnInvoke" | "OnServerInvoke" | "OnClientInvoke" => {
            return set_callback(lua, r, key, value)
        }
        // Script.Enabled is Disabled turned round: places save Disabled, and
        // that is what decides whether it runs. Place B Newer's
        // MusicSystemInitializer parents ModernMusicGui and then sets its
        // Handler's Enabled (saved Disabled); rustudio kept that in a stub
        // and the Handler never ran.
        "Enabled" if matches!(class, "Script" | "LocalScript") => {
            let on = !matches!(value, Value::Nil | Value::Boolean(false));
            store_prop(lua, r, "Disabled", Variant::Bool(!on))?;
            if !on {
                stop_script(lua, r);
            }
            {
                let mut w = world(lua)?;
                if let Some(i) = w.dm().dom.get_by_ref_mut(r) {
                    i.properties.remove(&ustr("Enabled"));
                }
                w.fire(r, "Changed", vec![Value::String(lua.create_string("Enabled")?)]);
                w.fire(r, "Changed:Enabled", vec![]);
            }
            if on {
                start_if_enabled(lua, r)?;
            }
            return Ok(());
        }
        _ => {}
    }
    // Attachment world placement: stored as CFrame relative to the part (or
    // as given while it isn't on a part, like Roblox).
    if rt.set_attachment && key == "Position" {
        // (Only its own frame's turn is kept: the part's frame isn't needed.)
        let v = opt_ud::<LVector3>(&value).ok_or_else(|| bad_assign(key, "Vector3", &value))?.0;
        let rot = match world(lua)?.d().prop(r, "CFrame") {
            Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf).rot,
            _ => glam::Mat3::IDENTITY,
        };
        store_prop(lua, r, "CFrame", Variant::CFrame(Xform { pos: v, rot }.to_cframe()))?;
        return fire_changed(lua, r, key);
    }
    if rt.set_attachment {
        let (local, part) = attachment_frames(lua, r)?;
        let worldf = compose(&part, &local);
        let new_local = match key {
            "WorldCFrame" => {
                let cf =
                    opt_ud::<LCFrame>(&value).ok_or_else(|| bad_assign(key, "CFrame", &value))?;
                compose(&part.inverse(), &cf.0)
            }
            _ => {
                let v = opt_ud::<LVector3>(&value)
                    .ok_or_else(|| bad_assign(key, "Vector3", &value))?
                    .0;
                match key {
                    "WorldPosition" => compose(
                        &part.inverse(),
                        &Xform {
                            pos: v,
                            rot: worldf.rot,
                        },
                    ),
                    "WorldOrientation" => {
                        compose(&part.inverse(), &Xform::from_orientation_deg(worldf.pos, v))
                    }
                    "Position" => Xform {
                        pos: v,
                        rot: local.rot,
                    },
                    // Axis / SecondaryAxis turn the attachment (Studio 0.747,
                    // checks/humanoid-physics/measure/attachment-axis.md):
                    // Axis becomes the X axis, the old secondary axis is
                    // made perpendicular to it (the old Z axis kept when
                    // they are parallel); SecondaryAxis keeps X and takes
                    // its part perpendicular to X (ignored when parallel);
                    // Z = X × Y. A zero vector changes nothing. They were
                    // read as an Orientation here, so a PrismaticConstraint
                    // a script aimed with Axis = (0, 1, 0) slid along X.
                    "Axis" | "WorldAxis" | "SecondaryAxis" | "WorldSecondaryAxis" => {
                        let world_key = key.starts_with("World");
                        let v = if world_key { part.rot.transpose() * v } else { v };
                        let (x0, y0, z0) = (local.rot.x_axis, local.rot.y_axis, local.rot.z_axis);
                        let rot = if v.length_squared() < 1e-12 {
                            local.rot
                        } else if key.ends_with("SecondaryAxis") {
                            let y = v - x0 * v.dot(x0);
                            if y.length_squared() < 1e-10 {
                                local.rot
                            } else {
                                let y = y.normalize();
                                glam::Mat3::from_cols(x0, y, x0.cross(y))
                            }
                        } else {
                            let x = v.normalize();
                            let y = y0 - x * y0.dot(x);
                            let y = if y.length_squared() < 1e-10 { z0.cross(x) } else { y };
                            let y = y.normalize();
                            glam::Mat3::from_cols(x, y, x.cross(y))
                        };
                        Xform { pos: local.pos, rot }
                    }
                    _ => Xform::from_orientation_deg(local.pos, v),
                }
            }
        };
        store_prop(lua, r, "CFrame", Variant::CFrame(new_local.to_cframe()))?;
        return fire_changed(lua, r, key);
    }
    if rt.set_part {
        match key {
            "Position" | "Orientation" | "Rotation" => {
                let v =
                    opt_ud::<LVector3>(&value).ok_or_else(|| bad_assign(key, "Vector3", &value))?;
                let xf = part_xform(lua, r)?;
                let nx = if key == "Position" {
                    Xform {
                        pos: v.0,
                        rot: xf.rot,
                    }
                } else {
                    Xform::from_orientation_deg(xf.pos, v.0)
                };
                store_prop(lua, r, "CFrame", Variant::CFrame(nx.to_cframe()))?;
                return fire_changed(lua, r, key);
            }
            "BrickColor" => {
                let b = opt_ud::<LBrickColor>(&value)
                    .ok_or_else(|| bad_assign(key, "BrickColor", &value))?;
                return store_prop(lua, r, "Color", Variant::Color3uint8(b.0.to_color3uint8()));
            }
            "AssemblyLinearVelocity" | "Velocity" => {
                let v =
                    opt_ud::<LVector3>(&value).ok_or_else(|| bad_assign(key, "Vector3", &value))?;
                let mut w = world(lua)?;
                w.velocities.insert(r, v.0);
                w.velocity_requests.push((r, v.0));
                return Ok(());
            }
            _ => {}
        }
    }
    let Some(info) = rt.set else {
        if let Some(cap) = capitalized(key)
            .filter(|c| prop_desc(class, c).is_some() || c == "Parent" || c == "Name")
        {
            return newindex(lua, r, &cap, value);
        }
        if stubs::assign(lua, r, class, key, &value)? {
            return Ok(());
        }
        crate::lua_bridge::gap(lua, "missing", class, key);
        let full = world(lua)?.full_name(r);
        return Err(rt_err(format!(
            "{key} is not a valid member of {class} \"{full}\""
        )));
    };
    let canon = info.canon.as_str();
    if info.read_only {
        // Properties that are only NotScriptable (Scriptability::None, not
        // PropertyTag::ReadOnly — e.g. TextBox.OverlayNativeInput used by
        // CoreScripts' ReportAbuse page) are writable by RobloxScript.
        let core_ok = security::current(lua) >= security::CORE && {
            let db = doc::db();
            db.classes
                .get(class)
                .and_then(|c| db.superclasses_iter(c).find_map(|cls| cls.properties.get(canon)))
                .is_some_and(|p| {
                    !p.tags.contains(&PropertyTag::ReadOnly)
                        && matches!(p.scriptability, Scriptability::None)
                })
        };
        if !core_ok {
            return Err(rt_err(format!(
                "Unable to assign property {key}. Property is read only"
            )));
        }
    }
    if !info.acts {
        crate::lua_bridge::plain_write(lua, class, canon);
    }
    let Some((ty, enum_name)) = info.ty else {
        return Err(rt_err(format!("Unable to assign property {key}")));
    };
    // (Only a Color3 conversion looks at what is stored now.)
    let existing = if matches!(ty, VariantType::Color3 | VariantType::Color3uint8) {
        world(lua)?.d().prop_key(r, info.canon)
    } else {
        None
    };
    let mut v = lua_to_variant(key, &value, ty, enum_name, existing.as_ref())?;
    // Studio's own setters clamp these (mech::setter_clamp, with addresses).
    if let Variant::Float32(f) = v {
        if let Some(c) = mech::setter_clamp(class, canon, f) {
            v = Variant::Float32(c);
        }
    }
    // A write to either zoom limit is bounded by the other limit.
    if let (Some(opposite), Variant::Float32(distance)) = (camera_zoom_opposite(class, canon), &v) {
        let w = world(lua)?;
        if let Some(bound) = rustudio_render::scene::f32_of(w.d().prop(r, opposite)) {
            v = Variant::Float32(if canon == "CameraMinZoomDistance" {
                distance.min(bound)
            } else {
                distance.max(bound)
            });
        }
    }
    if class == "Humanoid" && canon == "Health" {
        let max = match world(lua)?.d().prop(r, "MaxHealth") {
            Some(Variant::Float32(m)) => m,
            _ => 100.0,
        };
        let old = match existing {
            Some(Variant::Float32(h)) => h,
            _ => 100.0,
        };
        let new = match v {
            Variant::Float32(h) => h.clamp(0.0, max),
            _ => old,
        };
        store_prop(lua, r, canon, Variant::Float32(new))?;
        let mut w = world(lua)?;
        w.fire(r, "HealthChanged", vec![Value::Number(new as f64)]);
        if new <= 0.0 && old > 0.0 {
            w.fire(r, "Died", vec![]);
            w.died.push(r);
        }
        return Ok(());
    }
    // A text object's Font writes its FontFace (font_enum.rs, Studio's
    // faces); Enum.Font.Unknown writes nothing.
    if canon == "Font" && crate::font_enum::has_font_view(class) {
        let Variant::Enum(e) = v else { return Ok(()) };
        let Some(face) = crate::font_enum::face_of_enum(e.to_u32()) else { return Ok(()) };
        let before = world(lua)?.d().prop(r, "FontFace");
        let changed = !matches!(&before, Some(Variant::Font(f))
            if f.family == face.family && f.weight == face.weight && f.style == face.style);
        store_prop(lua, r, "FontFace", Variant::Font(face))?;
        if changed {
            fire_changed(lua, r, "Font")?;
        }
        return Ok(());
    }
    // (and a FontFace write changes what Font reads: Changed for both, as
    // Studio fires them)
    if canon == "FontFace" && crate::font_enum::has_font_view(class) {
        let view = |v: Option<Variant>| match v {
            Some(Variant::Font(f)) => Some((f.family, f.weight, f.style)),
            _ => None,
        };
        let before = view(world(lua)?.d().prop(r, "FontFace"));
        let after = view(Some(v.clone()));
        store_prop_key(lua, r, info.canon, v)?;
        if before != after {
            fire_changed(lua, r, "Font")?;
        }
        return Ok(());
    }
    let enabling = canon == "Disabled"
        && matches!(v, Variant::Bool(false))
        && matches!(class, "Script" | "LocalScript");
    let disabling = canon == "Disabled"
        && matches!(v, Variant::Bool(true))
        && matches!(class, "Script" | "LocalScript");
    let carry = canon == "CFrame" && doc::is_a(class, "BasePart");
    let before = if carry { part_xform(lua, r).ok() } else { None };
    store_prop_key(lua, r, info.canon, v)?;
    if carry {
        carry_assembly(lua, r, before)?;
    }
    if enabling {
        start_if_enabled(lua, r)?;
    }
    if disabling {
        stop_script(lua, r);
    }
    Ok(())
}

/// The script a function was written in: its environment's `script`.
fn script_of_function(f: &Function) -> Option<Ref> {
    f.environment().and_then(|e| e.raw_get::<Value>("script").ok()).as_ref().and_then(opt_inst)
}

/// The script of the running code: the `script` of the nearest Lua
/// function on the current thread's stack that has one (plugin_host's
/// printidentity asset id).
pub fn running_script(lua: &Lua) -> Option<Ref> {
    for level in 0..32 {
        match lua.inspect_stack(level, |d| script_of_function(&d.function())) {
            Some(Some(r)) => return Some(r),
            Some(None) => {}
            None => break,
        }
    }
    None
}

/// A task's entry function precedes its first Lua stack frame. debug.info
/// on a task.delay or task.defer thread therefore sees no script until it runs.
fn unstarted_script(lua: &Lua, th: &Thread) -> Option<Ref> {
    if th.status() != ThreadStatus::Resumable {
        return None;
    }
    // SAFETY: the live Thread belongs to this Lua VM. A new Luau coroutine
    // has status OK and keeps its entry function in the first stack slot.
    let function = unsafe {
        let thread = th.state();
        if mlua::ffi::lua_status(thread) != mlua::ffi::LUA_OK
            || mlua::ffi::lua_type(thread, 1) != mlua::ffi::LUA_TFUNCTION
        {
            return None;
        }
        lua.exec_raw::<Function>((), |state| mlua::ffi::lua_xpush(thread, state, 1)).ok()?
    };
    script_of_function(&function)
}

/// A running Script / LocalScript is disabled: it ends, as in Roblox. Its
/// connections are disconnected and its threads (waiting on a signal, in
/// task.wait / task.delay / task.defer) never resume. Enabled again it runs
/// from the top. (A character's Animate script disabled by a game stops
/// playing its walk and run tracks: measured in Studio 0.747, the default
/// rig then stands in its joints' own pose while it moves.)
fn stop_script(lua: &Lua, r: Ref) {
    let was_running = world(lua).is_ok_and(|mut w| w.started.remove(&r));
    if !was_running {
        return;
    }
    // (lists taken out while Lua's debug.info runs)
    stop_script_callbacks(lua, &HashSet::from([r]), false);
}

/// Whether any Lua frame of a suspended thread matches `pred` on its script
/// ref (ModuleScripts included: a thread that yields inside a required
/// module's function has the module's frame on top and its caller below).
fn thread_runs(lua: &Lua, th: &Thread, mut pred: impl FnMut(Ref) -> bool) -> bool {
    use mlua::ffi;
    // Inspect the suspended stack directly. Calling debug.info, converting its
    // result into a Function and looking up its environment for every frame
    // dominated teardown in clients with many unrelated waiting scripts.
    let thread = th.state();
    let owners = unsafe {
        // Both states belong to this Lua VM. Protect stack growth just as
        // Luau's debug.info does for inspection of another coroutine.
        lua.exec_raw::<MultiValue>((), |state| {
            ffi::luaL_checkstack(state, 67, c"inspect suspended scripts".as_ptr());
            if thread != state {
                ffi::lua_rawcheckstack(thread, 1);
            }
            let mut frame = std::mem::zeroed::<ffi::lua_Debug>();
            for level in 0..64 {
                if ffi::lua_getinfo(thread, level, c"f".as_ptr(), &mut frame) == 0 {
                    break;
                }
                if thread != state {
                    ffi::lua_xmove(thread, state, 1);
                }
                if ffi::lua_iscfunction(state, -1) != 0 {
                    ffi::lua_pop(state, 1);
                    continue;
                }
                // Replace the frame function with its environment's script,
                // keeping only userdata for the normal Instance type check.
                ffi::lua_getfenv(state, -1);
                ffi::lua_rawgetfield(state, -1, c"script".as_ptr());
                if ffi::lua_type(state, -1) == ffi::LUA_TUSERDATA {
                    ffi::lua_replace(state, -3);
                    ffi::lua_pop(state, 1);
                } else {
                    ffi::lua_pop(state, 3);
                }
            }
        })
    }
    .unwrap_or_default();
    owners.iter().filter_map(opt_inst).any(&mut pred) || unstarted_script(lua, th).is_some_and(pred)
}

/// Whether any Lua frame of a suspended thread runs code of one of `set`'s
/// scripts.
fn thread_runs_any(lua: &Lua, th: &Thread, set: &HashSet<Ref>) -> bool {
    thread_runs(lua, th, |r| set.contains(&r))
}

/// Ends every thread and connection running code of `set`'s scripts,
/// wherever it yields (an unloaded plugin: Studio stops all of its code,
/// including threads parked inside its ModuleScripts).
fn stop_code_of(lua: &Lua, set: &HashSet<Ref>) {
    if set.is_empty() {
        return;
    }
    let _scope = rustudio_core::bench::scope("stop_script_subtree");
    if let Ok(mut w) = world(lua) {
        for r in set {
            w.started.remove(r);
        }
    }
    stop_script_callbacks(lua, set, true);
}

fn stop_script_callbacks(lua: &Lua, set: &HashSet<Ref>, include_retired: bool) {
    let (mut waiters, mut changed) = {
        let Ok(mut s) = signals(lua) else { return };
        let keys: rustc_hash::FxHashSet<_> = set.iter()
            .filter_map(|r| s.script_connections.get(r))
            .flat_map(|keys| keys.keys().cloned())
            .collect();
        let mut changed = Vec::new();
        for key in keys {
            let remove = |c: &Conn| c.owner.is_some_and(|r| set.contains(&r));
            if s.remove_connections(&key, false, remove) {
                changed.push(key.0);
            }
            if include_retired && key.1 == "AncestryChanged" {
                s.remove_connections(&key, true, remove);
            }
        }
        (std::mem::take(&mut s.waiters), changed)
    };
    for (key, list) in &mut waiters {
        let before = list.len();
        list.retain(|(_, th)| !thread_runs_any(lua, th, set));
        if before != 0 && list.is_empty() {
            changed.push(key.0);
        }
    }
    if let Some(mut s) = lua.app_data_mut::<Signals>() {
        for (k, mut v) in std::mem::take(&mut s.waiters) {
            waiters.entry(k).or_default().append(&mut v);
        }
        s.waiters = waiters;
    }
    for r in changed {
        prune_layout_listener(lua, r);
    }
    retain_scheduled(lua, |th| !thread_runs_any(lua, th, set));
    retain_invokes(lua, |th| !thread_runs_any(lua, th, set));
}

fn retain_invokes(lua: &Lua, mut keep: impl FnMut(&Thread) -> bool) {
    let mut waiting = match lua.app_data_mut::<Invokes>() {
        Some(mut inv) => std::mem::take(&mut inv.waiting),
        None => return,
    };
    waiting.retain(|_, th| keep(th));
    if let Some(mut inv) = lua.app_data_mut::<Invokes>() {
        for (k, v) in std::mem::take(&mut inv.waiting) {
            waiting.insert(k, v);
        }
        inv.waiting = waiting;
    }
}

fn retain_scheduled(lua: &Lua, mut keep: impl FnMut(&Thread) -> bool) {
    let (mut timers, mut ready, mut deferred) = match lua.app_data_mut::<Sched>() {
        Some(mut s) => (
            std::mem::take(&mut s.timers),
            std::mem::take(&mut s.ready),
            std::mem::take(&mut s.deferred),
        ),
        None => return,
    };
    timers.retain(|t| keep(&t.thread));
    ready.retain(|t| keep(&t.thread));
    deferred.retain(|(th, _)| keep(th));
    if let Some(mut s) = lua.app_data_mut::<Sched>() {
        timers.append(&mut s.timers);
        ready.append(&mut s.ready);
        deferred.append(&mut s.deferred);
        s.timers = timers;
        s.ready = ready;
        s.deferred = deferred;
    }
}

/// Whether a script is enabled: not Disabled (and no saved Enabled false).
fn script_enabled_here(lua: &Lua, r: Ref) -> mlua::Result<bool> {
    let w = world(lua)?;
    let d = w.d();
    Ok(!matches!(d.prop(r, "Disabled"), Some(Variant::Bool(true)))
        && !matches!(d.prop(r, "Enabled"), Some(Variant::Bool(false))))
}

/// A script that hasn't run starts (next dispatch) when it's enabled where
/// it runs, as in Roblox.
fn start_if_enabled(lua: &Lua, r: Ref) -> mlua::Result<()> {
    let mut w = world(lua)?;
    if !w.started.contains(&r) && w.runs_here(r) {
        w.to_start.push(r);
    }
    Ok(())
}

// ---------------------------------------------------------------- methods

fn arg(args: &MultiValue, i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Nil)
}

fn arg_str(args: &MultiValue, i: usize, what: &str) -> mlua::Result<String> {
    match arg(args, i) {
        // Studio's reflected string argument converts what it can
        // (GetService(true) is `'true' is not a valid Service name`,
        // GetService({}) `'{}' …`); only nil is missing.
        Value::String(s) => Ok(s.to_str()?.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        Value::Nil => Err(rt_err(format!("Argument {i} missing or nil"))),
        Value::Table(t) if t.is_empty() => Ok("{}".into()),
        v => {
            let _ = what;
            Err(rt_err(format!("Unable to cast {} to string", crate::cmeta::type_name(&v))))
        }
    }
}

/// Descendants of `root` (DataModel), skipping hidden core services (`CoreGui`,
/// `CorePackages`) and their subtrees so game scripts never see or walk them.
fn descendants_skip_hidden(d: &Document, root: Ref) -> Vec<Ref> {
    let mut out = Vec::new();
    let Some(top) = d.dom.get_by_ref(root) else { return out };
    for &c in top.children() {
        let Some(ci) = d.dom.get_by_ref(c) else { continue };
        if security::is_hidden_service_class(&ci.class) {
            continue;
        }
        for desc in d.dom.descendants_of(c) {
            out.push(desc.referent());
        }
    }
    out
}

fn children_of(lua: &Lua, r: Ref) -> mlua::Result<Vec<Ref>> {
    let w = world(lua)?;
    let d = w.d();
    let hide_core = d.dom.root_ref() == r && security::hides_core_services(lua);
    Ok(d.dom
        .get_by_ref(r)
        .map(|i| {
            if hide_core {
                i.children()
                    .iter()
                    .copied()
                    .filter(|c| d.dom.get_by_ref(*c).is_none_or(|ci| !security::is_hidden_service_class(&ci.class)))
                    .collect()
            } else {
                i.children().to_vec()
            }
        })
        .unwrap_or_default())
}

fn list(lua: &Lua, refs: Vec<Ref>) -> mlua::Result<Table> {
    if domains::assets_cloud::scene::any_access_blocked(lua, &refs) {
        return Err(rt_err("The current thread cannot access 'RealtimeMedia' (lacking capability InternalTest)"));
    }
    let t = lua.create_table_with_capacity(refs.len(), 0)?;
    let mut c = cache(lua)?;
    let weak = c.weak.clone();
    for (i, r) in refs.into_iter().enumerate() {
        if r.is_none() {
            t.raw_set(i + 1, Value::Nil)?;
            continue;
        }
        let slot = c.instance_slot(r);
        let val = match weak.as_ref() {
            Some(weak) => match weak.raw_get::<Value>(slot)? {
                Value::UserData(u) => Value::UserData(u),
                _ => {
                    let u = new_inst_ud(lua, r)?;
                    weak.raw_set(slot, u.clone())?;
                    c.fresh.push(r);
                    Value::UserData(u)
                }
            },
            None => Value::UserData(new_inst_ud(lua, r)?),
        };
        t.raw_set(i + 1, val)?;
    }
    Ok(t)
}

fn find_child(
    lua: &Lua,
    r: Ref,
    pred: impl Fn(&rbx_dom_weak::Instance) -> bool,
    recursive: bool,
) -> mlua::Result<Option<Ref>> {
    let w = world(lua)?;
    let d = w.d();
    let hide_core = d.dom.root_ref() == r && security::hides_core_services(lua);
    Ok(if recursive {
        if hide_core {
            let Some(top) = d.dom.get_by_ref(r) else { return Ok(None) };
            top.children().iter().copied().find_map(|c| {
                let ci = d.dom.get_by_ref(c)?;
                if security::is_hidden_service_class(&ci.class) {
                    return None;
                }
                d.dom.descendants_of(c).find(|i| pred(i)).map(|i| i.referent())
            })
        } else {
            d.dom
                .descendants_of(r)
                .filter(|i| i.referent() != r)
                .find(|i| pred(i))
                .map(|i| i.referent())
        }
    } else {
        d.dom.get_by_ref(r).and_then(|i| {
            i.children().iter().copied().find(|c| {
                d.dom
                    .get_by_ref(*c)
                    .is_some_and(|ci| !(hide_core && security::is_hidden_service_class(&ci.class)) && pred(ci))
            })
        })
    })
}

/// Children by name for parents with many children (Place B's Workspace:
/// FindFirstChild scanned thousands per call). Kept from the document's
/// journal: a child added, moved in or renamed is added to its parent's
/// lists; one that left is filtered out when looked up.
/// Open debug.profilebegin markers per thread (by pointer), for the current
/// resumption only: Studio's profiler stack doesn't survive a yield.
#[derive(Default)]
struct ProfileDepth(HashMap<usize, u32>);

#[derive(Default)]
struct ChildIndex {
    doc_id: u64,
    /// Parent → name → children that had that name there (maybe stale).
    by_parent: HashMap<Ref, HashMap<String, Vec<Ref>>>,
}

/// Parents with fewer children are just scanned.
const CHILD_INDEX_MIN: usize = 64;

/// A child of `r` by name (`inst.Name`, FindFirstChild, WaitForChild).
/// The DataModel's services answer to the Name Roblox's instance has
/// where that differs from the class, too: RunService is "Run Service"
/// there, and a game's modules read `game["Run Service"]` (rustudio names
/// its services by class: crates/rustudio-core/src/doc.rs
/// default_instance_name; mech::service_class_for_name lists the aliases).
fn find_named_child(lua: &Lua, r: Ref, name: &str) -> mlua::Result<Option<Ref>> {
    if let Some(c) = find_named_child_plain(lua, r, name)? {
        return Ok(Some(c));
    }
    let Some(service) = mech::service_class_for_name(name) else {
        return Ok(None);
    };
    let is_game = world(lua)?.d().dom.root_ref() == r;
    if !is_game || security::service_hidden(lua, service) {
        return Ok(None);
    }
    // (made if no script asked for it yet, as `game:GetService` does: the
    // service is always there on Roblox)
    get_service(lua, service, true)
}

fn find_named_child_plain(lua: &Lua, r: Ref, name: &str) -> mlua::Result<Option<Ref>> {
    let w = world(lua)?;
    let d = w.d();
    let Some(i) = d.dom.get_by_ref(r) else {
        return Ok(None);
    };
    let hide_core = d.dom.root_ref() == r && security::hides_core_services(lua);
    if i.children().len() < CHILD_INDEX_MIN {
        return Ok(i.children().iter().copied().find(|c| {
            d.dom
                .get_by_ref(*c)
                .is_some_and(|ci| ci.name == name && !(hide_core && security::is_hidden_service_class(&ci.class)))
        }));
    }
    drop(w);
    let mut w = world(lua)?;
    let marks = w.dm().marks_for(doc::Reader::Children);
    let d = w.d();
    let mut ix = lua
        .app_data_mut::<ChildIndex>()
        .ok_or_else(|| rt_err("child index missing"))?;
    match marks {
        doc::Marks::Some(m) if ix.doc_id == d.id => {
            for mark in m {
                let c = match mark {
                    doc::Mark::Tree(c) => c,
                    doc::Mark::Prop(c, n) if n == "Name" => c,
                    _ => continue,
                };
                let Some(ci) = d.dom.get_by_ref(c) else { continue };
                if let Some(names) = ix.by_parent.get_mut(&ci.parent()) {
                    let list = names.entry(ci.name.clone()).or_default();
                    if !list.contains(&c) {
                        list.push(c);
                    }
                }
            }
        }
        doc::Marks::Some(_) => {}
        doc::Marks::All => ix.by_parent.clear(),
    }
    if ix.doc_id != d.id {
        ix.doc_id = d.id;
        ix.by_parent.clear();
    }
    let Some(i) = d.dom.get_by_ref(r) else {
        return Ok(None);
    };
    let names = ix.by_parent.entry(r).or_insert_with(|| {
        let mut m: HashMap<String, Vec<Ref>> = HashMap::with_capacity(i.children().len());
        for c in i.children() {
            if let Some(ci) = d.dom.get_by_ref(*c) {
                m.entry(ci.name.clone()).or_default().push(*c);
            }
        }
        m
    });
    let Some(list) = names.get_mut(name) else {
        return Ok(None);
    };
    // Still here under that name; with several, the first in child order.
    list.retain(|c| {
        d.dom.get_by_ref(*c).is_some_and(|ci| {
            ci.parent() == r && ci.name == name && !(hide_core && security::is_hidden_service_class(&ci.class))
        })
    });
    Ok(match list.len() {
        0 => None,
        1 => Some(list[0]),
        _ => i.children().iter().copied().find(|c| list.contains(c)),
    })
}

fn find_ancestor(
    lua: &Lua,
    r: Ref,
    pred: impl Fn(&rbx_dom_weak::Instance) -> bool,
) -> mlua::Result<Option<Ref>> {
    let w = world(lua)?;
    let d = w.d();
    let mut cur = d
        .dom
        .get_by_ref(r)
        .map(|i| i.parent())
        .unwrap_or_else(Ref::none);
    while let Some(i) = d.dom.get_by_ref(cur) {
        if pred(i) {
            return Ok(Some(cur));
        }
        cur = i.parent();
    }
    Ok(None)
}

fn opt_ref_value(lua: &Lua, r: Option<Ref>) -> mlua::Result<Value> {
    match r {
        Some(r) => inst_value(lua, r),
        None => Ok(Value::Nil),
    }
}

/// The services `get_service` answers nil for whether or not they exist.
fn service_studio_only(lua: &Lua, class: &str) -> bool {
    // Studio-only services: GetService / FindService give game scripts nil
    // (measured in Studio, checks/parity/testing.md).
    // (Plugins see them: measured, StudioService's Parent is the edit
    // DataModel.)
    (matches!(
        class,
        "DebuggerManager" | "StudioService" | "DraggerService" | "LiveSyncService" | "TestCoordinatorService"
    ) && security::current(lua) == security::GAME)
    // Studio's edit DataModel hides StudioPublishService from user plugins and
    // game scripts, while
    // built-in Studio plugins (PublishPlaceAs, identity 6) read it directly.
        || (class == "StudioPublishService"
            && !matches!(security::current(lua), security::BUILTIN_PLUGIN | security::CORE))
}

fn get_service(lua: &Lua, class: &str, create: bool) -> mlua::Result<Option<Ref>> {
    if service_studio_only(lua, class) {
        return Ok(None);
    }
    let mut w = world(lua)?;
    let root = w.d().dom.root_ref();
    if let Some(r) = w.d().find_child(root, class) {
        return Ok(Some(r));
    }
    // Studio 0.744's classes (its API dump): a name it doesn't have is not a
    // valid Service name (FeatureMatchingService, measured), whatever the
    // reflection database (another version) says.
    let api_class = rustudio_core::api::get().map(|a| a.classes.get(class));
    if matches!(api_class, Some(None)) {
        return Err(rt_err(format!("'{class}' is not a valid Service name")));
    }
    if !doc::is_service(class) {
        // Any other class answers nil (Studio probes 6, every library row:
        // AnimationTrack, WorldRoot, Terrain, RealtimeMedia…); one that can
        // be created is created first, so Player wants WritePlayer
        // (library.Player.Move).
        let known = doc::db().classes.contains_key(class) || matches!(api_class, Some(Some(_)));
        if !known {
            return Err(rt_err(format!("'{class}' is not a valid Service name")));
        }
        if doc::is_script_creatable(class) {
            drop(w);
            security::check_create(lua, class)?;
        }
        return Ok(None);
    }
    if !create {
        return Ok(None);
    }
    let r = w
        .dm()
        .dom
        .insert(root, InstanceBuilder::new(class).with_name(class));
    w.dm().mark_tree(r);
    w.structure_changed = true;
    Ok(Some(r))
}

/// A model's pivot: its PrimaryPart, else the centre of its parts.
/// When this process started (os.clock's origin, as in Studio).
/// RUSTUDIO_GAME_CLOCK: virtual script time spent this frame (os.clock calls), and what
/// one call costs (10 µs).
static GAME_CLOCK_SPENT: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);
const GAME_CLOCK_CALL: f64 = 1e-5;

/// RUSTUDIO_GAME_CLOCK: the script time spent since the last call, in whole periods of a
/// `hz` display (a frame's time is whole display periods), and starts the next frame's
/// count. A frame whose scripts spun on os.clock for 0.083 s steps 5 periods more.
pub fn take_game_clock_spent(hz: f32) -> f32 {
    let mut e = GAME_CLOCK_SPENT.lock().unwrap_or_else(|e| e.into_inner());
    let n = (*e * hz as f64).round();
    *e = 0.0;
    (n / hz as f64) as f32
}

pub fn process_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn model_pivot(w: &World, r: Ref) -> Xform {
    // Roblox: the PrimaryPart's pivot (CFrame · PivotOffset), else the
    // model's own WorldPivotData, else the centre of its parts' bounds.
    let d = w.d();
    if let Some(Variant::Ref(pp)) = d.prop(r, "PrimaryPart") {
        if let Some(Variant::CFrame(cf)) = d.prop(pp, "CFrame") {
            let off = match d.prop(pp, "PivotOffset") {
                Some(Variant::CFrame(o)) => Xform::from_cframe(&o),
                _ => Xform {
                    pos: Vec3::ZERO,
                    rot: glam::Mat3::IDENTITY,
                },
            };
            return compose(&Xform::from_cframe(&cf), &off);
        }
    }
    if let Some(Variant::OptionalCFrame(Some(cf))) = d
        .dom
        .get_by_ref(r)
        .and_then(|i| i.properties.get(&ustr("WorldPivotData")).cloned())
    {
        return Xform::from_cframe(&cf);
    }
    // Studio's fallback: the centre
    // of the parts' boxes (each part's half size through |R|), measured in
    // the world frame when there is no PrimaryPart (identity rotation).
    let (mut mn, mut mx) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let mut any = false;
    for i in d.dom.descendants_of(r) {
        if doc::is_a(&i.class, "BasePart") {
            if let Some(Variant::CFrame(cf)) = d.prop(i.referent(), "CFrame") {
                let xf = Xform::from_cframe(&cf);
                let half = match d.prop(i.referent(), "Size") {
                    Some(Variant::Vector3(s)) => Vec3::new(s.x, s.y, s.z) * 0.5,
                    _ => Vec3::ZERO,
                };
                let m = xf.rot;
                let ext = Vec3::new(
                    m.x_axis.x.abs() * half.x + m.y_axis.x.abs() * half.y + m.z_axis.x.abs() * half.z,
                    m.x_axis.y.abs() * half.x + m.y_axis.y.abs() * half.y + m.z_axis.y.abs() * half.z,
                    m.x_axis.z.abs() * half.x + m.y_axis.z.abs() * half.y + m.z_axis.z.abs() * half.z,
                );
                mn = mn.min(xf.pos - ext);
                mx = mx.max(xf.pos + ext);
                any = true;
            }
        }
    }
    Xform {
        pos: if any { (mn + mx) * 0.5 } else { Vec3::ZERO },
        rot: glam::Mat3::IDENTITY,
    }
}

fn primary_part_frame(lua: &Lua, model: Ref, method: &str) -> mlua::Result<(Ref, Xform)> {
    let w = world(lua)?;
    let d = w.d();
    if let Some(Variant::Ref(part)) = d.prop(model, "PrimaryPart") {
        if let Some(Variant::CFrame(frame)) = d.prop(part, "CFrame") {
            return Ok((part, Xform::from_cframe(&frame)));
        }
    }
    Err(rt_err(format!(
        "Model:{method}() failed because no PrimaryPart has been set, or the PrimaryPart no longer exists. Please set Model.PrimaryPart before using this."
    )))
}

fn pivot_to(lua: &Lua, r: Ref, target: Xform) -> mlua::Result<()> {
    let (class, _, _) = inst_meta(lua, r)?;
    if doc::is_a(&class, "BasePart") {
        let before = part_xform(lua, r).ok();
        store_prop(lua, r, "CFrame", Variant::CFrame(target.to_cframe()))?;
        return carry_assembly(lua, r, before);
    }
    move_model(lua, r, target, None, doc::is_a(&class, "Model"))
}

fn move_model(lua: &Lua, r: Ref, target: Xform, primary: Option<(Ref, Xform)>, is_model: bool) -> mlua::Result<()> {
    let (pivot, parts) = {
        let w = world(lua)?;
        let pivot = primary.map_or_else(|| model_pivot(&w, r), |(_, frame)| frame);
        let parts: Vec<(Ref, Xform)> = w
            .d()
            .dom
            .descendants_of(r)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .filter_map(|i| match w.d().prop(i.referent(), "CFrame") {
                Some(Variant::CFrame(cf)) => Some((i.referent(), Xform::from_cframe(&cf))),
                _ => None,
            })
            .collect();
        (pivot, parts)
    };
    let delta = cf_mul(&target, &cf_inv(&pivot));
    for (part, frame) in parts {
        let moved = if primary.is_some_and(|(root, _)| part == root) {
            // The legacy method targets the part's CFrame directly.
            // Composing a delta and its inverse perturbs the target and
            // makes repeated exact spawn-frame checks fail.
            target
        } else {
            cf_mul(&delta, &frame).orthonormalized()
        };
        store_prop(lua, part, "CFrame", Variant::CFrame(moved.to_cframe()))?;
    }
    // Roblox: after PivotTo the model's pivot *is* the target (GetPivot
    // returns it). With a PrimaryPart that follows from the part having
    // moved; without one the pivot is the model's own WorldPivotData, so it
    // moves too (a model that had none, pivoting on its parts' bounds, gets
    // one). Left behind, every PivotTo measured its delta from the stale
    // pivot: a game's teleport menu PivotTo's its sky box (a
    // Model, no PrimaryPart) onto the camera every RenderStepped and the
    // box ran off by the same offset each frame (-89600 studs in 6 s), so
    // its Beam sky never showed.
    let primary_moves = {
        let w = world(lua)?;
        let d = w.d();
        matches!(d.prop(r, "PrimaryPart"), Some(Variant::Ref(pp))
            if pp.is_some() && matches!(d.prop(pp, "CFrame"), Some(Variant::CFrame(_))))
    };
    if !primary_moves && is_model {
        store_prop(lua, r, "WorldPivotData", Variant::OptionalCFrame(Some(target.to_cframe())))?;
    }
    Ok(())
}

/// Instance:Destroy(), as Studio's `Instance::destroy`: the
/// instance's Parent goes nil (and locks), then each descendant's in turn,
/// a parent before its children, then the connections go. An immediate
/// ancestry handler can inspect the intermediate tree. Otherwise the
/// descendants are detached in bulk, with fires made only for listeners.
fn destroy_inst(lua: &Lua, r: Ref) -> mlua::Result<()> {
    if r == world(lua)?.d().dom.root_ref() {
        return Err(rt_err("game cannot be destroyed"));
    }
    // Destroying what is already destroyed (a descendant of a destroyed
    // instance, as Place D's preview strip loop over GetDescendants does) is a
    // no-op in Roblox, not the locked-Parent error.
    if world(lua)?.destroyed.contains(&r) {
        return Ok(());
    }
    {
        let mut w = world(lua)?;
        w.fire(r, "Destroying", vec![]);
        // One replicated op for the subtree: Studio's server sends one
        // DestroyInstance item and the client runs Destroy on its copy
        // (DeserializedDestroyInstanceItem),
        // not an unparent per descendant. Recorded before the Parent op so
        // the client still has the pair when it gets here.
        w.record(RepOp::Destroy(r));
    }
    set_parent_opts(lua, r, Ref::none(), true, false, domains::world::part::EnterPose::Full)?;
    let immediate = {
        let mut w = world(lua)?;
        if w.mode == RunMode::Edit {
            w.dm().note_script_destroyed(r);
        }
        w.signal_mode == signal_timing::SignalMode::Immediate
            && w.d().dom.descendants_of(r).any(|i| {
                let d = i.referent();
                w.listening.contains(&d) && hears(lua, d, "AncestryChanged")
            })
    };
    if immediate {
        // Let handlers inspect the root's Parent change while its children
        // are still attached, even if the root is the only listener.
        world(lua)?.destroyed.insert(r);
        signal_timing::flush_immediate(lua);
    }
    let observable_descendants = immediate && {
        let w = world(lua)?;
        w.d().dom.descendants_of(r).skip(1).any(|i| {
            let d = i.referent();
            w.listening.contains(&d) && hears(lua, d, "AncestryChanged")
        })
    };
    let order = if observable_descendants {
        let mut order = Vec::new();
        let mut stack: Vec<_> = children_of(lua, r)?.into_iter().rev().map(|d| (d, r)).collect();
        while let Some((d, p)) = stack.pop() {
            let mut w = world(lua)?;
            if !w.d().dom.get_by_ref(d).is_some_and(|i| i.parent() == p) {
                continue;
            }
            if let Some(quiet) = detach_quiet_leaf_prefix(lua, &mut w, p, d) {
                order.extend(quiet.into_iter().map(|child| (child, p)));
                continue;
            }
            w.fire(d, "Destroying", vec![]);
            drop(w);
            set_parent_opts(lua, d, Ref::none(), false, false, domains::world::part::EnterPose::Full)?;
            world(lua)?.destroyed.insert(d);
            signal_timing::flush_immediate(lua);
            order.push((d, p));
            stack.extend(children_of(lua, d)?.into_iter().rev().map(|c| (c, d)));
        }
        order
    } else {
        // Descendant Destroying is queued before replaying the detach signals.
        {
            let mut w = world(lua)?;
            let heard: Vec<Ref> = w
                .d()
                .dom
                .descendants_of(r)
                .map(|i| i.referent())
                .filter(|d| *d != r && w.listening.contains(d) && hears(lua, *d, "Destroying"))
                .collect();
            for d in heard {
                w.fire(d, "Destroying", vec![]);
            }
        }
        detach_descendants(lua, r)?
    };
    // A destroyed MeshPart made from an EditableMesh leaves the link table.
    editable::forget_parts(std::iter::once(r).chain(order.iter().map(|x| x.0)));
    // Connections go only when something in the subtree has any (a walk of
    // every connection in the DataModel otherwise: 125 us per Destroy in
    // Place E, 1.6 ms in a live Place A match, whose effect parts and
    // tracers have none).
    let (listened, stopped_scripts) = {
        let mut w = world(lua)?;
        w.destroyed.insert(r);
        w.destroyed.extend(order.iter().map(|x| x.0));
        let stopped_scripts = std::iter::once(r)
            .chain(order.iter().map(|x| x.0))
            .filter(|x| w.started.contains(x))
            .collect();
        let listened: Vec<_> = std::iter::once(r)
            .chain(order.iter().map(|x| x.0))
            .filter(|x| w.listening.contains(x))
            .collect();
        for &x in &listened {
            w.forget_layout_listener(x);
            w.abs_seen.remove(&x);
            w.scroll_seen.remove(&x);
        }
        (listened, stopped_scripts)
    };
    if !listened.is_empty() {
        let mut s = signals(lua)?;
        let cutoff = s.next_id;
        let mut retired_ancestry = false;
        for &x in &listened {
            let Some(mut names) = s.conn_names.remove(&x) else { continue };
            names.retain(|name| {
                if name == "Destroying" {
                    return true;
                }
                let key = (x, name.clone());
                if let Some(v) = s.conns.remove(&key) {
                    if name == "AncestryChanged" && !v.is_empty() {
                        if let Some((_, old)) = s.destroyed_ancestry.insert(x, (cutoff, v)) {
                            for conn in old {
                                s.forget_connection(&key, conn.owner);
                            }
                        }
                        retired_ancestry = true;
                    } else {
                        for conn in v {
                            s.forget_connection(&key, conn.owner);
                        }
                    }
                }
                false
            });
            if !names.is_empty() {
                s.conn_names.insert(x, names);
            }
        }
        if retired_ancestry {
            s.next_id += 1;
            world(lua)?.conn_seq = s.next_id;
        }
    }
    if let Ok(mut s) = signals(lua) {
        if !s.remote_queue.is_empty() || !s.remote_queue_warned.is_empty() {
            for x in std::iter::once(r).chain(order.iter().map(|x| x.0)) {
                for ev in ["OnClientEvent", "OnServerEvent"] {
                    let key = (x, ev.to_string());
                    s.remote_queue.remove(&key);
                    s.remote_queue_warned.remove(&key);
                }
            }
        }
    }
    // A running Script destroyed with its model also loses its connections
    // on surviving objects, such as RunService, and its scheduled threads.
    // A ModuleScript may be shared by other live scripts.
    stop_code_of(lua, &stopped_scripts);
    Ok(())
}

/// Batches silent siblings between immediate callbacks. Removing each leaf
/// from the front of a large child list would repeatedly shift the list.
fn detach_quiet_leaf_prefix(lua: &Lua, w: &mut World, parent: Ref, first: Ref) -> Option<Vec<Ref>> {
    if w.item_changed
        || w.listening.contains(&first)
        || w.listening.contains(&parent)
            && (hears(lua, parent, "ChildRemoved") || hears(lua, parent, "DescendantRemoving"))
    {
        return None;
    }
    let d = w.d();
    let children = d.dom.get_by_ref(parent)?.children();
    if children.first() != Some(&first) {
        return None;
    }
    let primary = d.prop(parent, "PrimaryPart");
    let count = children.iter().take_while(|&&child| {
        !w.listening.contains(&child)
            && !matches!(&primary, Some(Variant::Ref(r)) if *r == child)
            && d.dom.get_by_ref(child).is_some_and(|i| i.children().is_empty())
    }).count();
    if count == 0 {
        return None;
    }
    ext::invalidate_layout_parent(w, parent);
    let quiet = w.dm().dom.detach_children_prefix(parent, count);
    for &child in &quiet {
        w.orphans.insert(child);
        w.destroyed.insert(child);
        w.dm().mark_tree(child);
    }
    w.structure_changed = true;
    Some(quiet)
}

/// Cuts every descendant of the (already unparented) `r` loose, as
/// Studio's destroy does one `setParent(nil)` at a time, parent before
/// children: each one's ChildRemoved / DescendantRemoving go to its old
/// parent only (already out of the tree), its Changed("Parent") and the
/// AncestryChanged of it and everything under it to themselves. Fires are
/// made only where someone listens (or, for Changed, game.ItemChanged
/// does). Returns the descendants in pre-order with their old parents.
fn detach_descendants(lua: &Lua, r: Ref) -> mlua::Result<Vec<(Ref, Ref)>> {
    let (order, listens, item_changed) = {
        let mut w = world(lua)?;
        let order = w.dm().dom.detach_descendants(r);
        for (d, _) in &order {
            w.orphans.insert(*d);
            // Each one is a root of its own now: readers (scene, physics,
            // GUI) see it leave through its own mark.
            w.dm().mark_tree(*d);
        }
        if !order.is_empty() {
            w.structure_changed = true;
        }
        let listens: Vec<bool> = order
            .iter()
            .map(|(d, p)| w.listening.contains(d) || w.listening.contains(p))
            .collect();
        let item_changed = w.item_changed;
        (order, listens, item_changed)
    };
    if !item_changed && !listens.iter().any(|x| *x) {
        return Ok(order);
    }
    // Pre-order: each subtree is a contiguous run order[i..end[i]].
    let n = order.len();
    let mut end = vec![n; n];
    {
        let mut stack: Vec<usize> = Vec::new();
        for (i, (_, p)) in order.iter().enumerate() {
            while let Some(&top) = stack.last() {
                if order[top].0 == *p {
                    break;
                }
                end[top] = i;
                stack.pop();
            }
            stack.push(i);
        }
    }
    // Index the ancestry listeners in pre-order so a subtree can select
    // its listeners without scanning quiet descendants for every detach.
    let ancestry: Vec<usize> = {
        let w = world(lua)?;
        (0..n)
            .filter(|&i| w.listening.contains(&order[i].0) && hears(lua, order[i].0, "AncestryChanged"))
            .collect()
    };
    let pname = Value::String(lua.create_string("Parent")?);
    let mut values: HashMap<Ref, Value> = HashMap::new();
    let mut value_of = |d: Ref| -> mlua::Result<Value> {
        if let Some(v) = values.get(&d) {
            return Ok(v.clone());
        }
        let v = inst_value(lua, d)?;
        values.insert(d, v.clone());
        Ok(v)
    };
    for i in 0..n {
        let (d, p) = order[i];
        let start = ancestry.partition_point(|&j| j < i);
        let stop = ancestry.partition_point(|&j| j < end[i]);
        let mut targets = ancestry[start..stop].to_vec();
        if !listens[i] && !item_changed && targets.is_empty() {
            continue;
        }
        // A parent and its last child share an end index.
        targets.sort_unstable_by_key(|&j| (end[j], std::cmp::Reverse(j)));
        let child = if targets.is_empty() { Value::Nil } else { value_of(d)? };
        let (p_hears, d_hears) = {
            let w = world(lua)?;
            (w.listening.contains(&p), w.listening.contains(&d))
        };
        if p_hears && hears(lua, p, "ChildRemoved") {
            let me = value_of(d)?;
            world(lua)?.fire(p, "ChildRemoved", vec![me]);
        }
        if p_hears && hears(lua, p, "DescendantRemoving") {
            let subtree: Vec<Value> = (i..end[i]).map(|j| value_of(order[j].0)).collect::<mlua::Result<_>>()?;
            let mut w = world(lua)?;
            for v in subtree {
                w.fire(p, "DescendantRemoving", vec![v]);
            }
        }
        let mut w = world(lua)?;
        if d_hears || item_changed {
            w.fire(d, "Changed", vec![pname.clone()]);
            w.fire(d, "Changed:Parent", vec![]);
        }
        for j in targets {
            w.fire(order[j].0, "AncestryChanged", vec![child.clone(), Value::Nil]);
        }
    }
    Ok(order)
}

fn part_size(lua: &Lua, r: Ref) -> mlua::Result<Vec3> {
    Ok(match world(lua)?.d().prop(r, "Size") {
        Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
        _ => Vec3::ONE,
    })
}

/// Studio's attribute-name check: not empty, at most 100
/// bytes (0x64), and only ASCII letters, digits, '_', '-', '.' and '/'.
fn check_attribute_name(n: &str) -> mlua::Result<()> {
    if n.is_empty() {
        return Err(rt_err("Attribute name cannot be empty"));
    }
    if n.len() > mech::ATTRIBUTE_NAME_MAX {
        return Err(rt_err(format!(
            "Attribute name exceeds {} character limit",
            mech::ATTRIBUTE_NAME_MAX
        )));
    }
    if let Some(c) = n
        .bytes()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'/')))
    {
        return Err(rt_err(format!(
            "Attribute name contains illegal character '{}'",
            c as char
        )));
    }
    Ok(())
}

fn attribute_value(v: &Value) -> mlua::Result<Option<Variant>> {
    Ok(match v {
        Value::Nil => None,
        Value::Boolean(b) => Some(Variant::Bool(*b)),
        Value::Number(x) => Some(Variant::Float64(*x)),
        Value::Integer(i) => Some(Variant::Float64(*i as f64)),
        Value::String(s) => Some(Variant::String(s.to_str()?.to_string())),
        _ => {
            if let Some(e) = opt_ud::<LEnumItem>(v) {
                Some(Variant::EnumItem(rbx_dom_weak::types::EnumItem {
                    ty: e.enum_type.clone(),
                    value: e.value,
                }))
            } else if let Some(p) = opt_ud::<LVector2>(v) {
                Some(Variant::Vector2(Vector2::new(p.0.x, p.0.y)))
            } else if let Some(b) = opt_ud::<LBrickColor>(v) {
                Some(Variant::BrickColor(b.0))
            } else if let Some(u) = opt_ud::<LUDim>(v) {
                Some(Variant::UDim(UDim::new(u.scale, u.offset)))
            } else if let Some(u) = opt_ud::<LUDim2>(v) {
                Some(Variant::UDim2(UDim2::new(
                    UDim::new(u.x.scale, u.x.offset),
                    UDim::new(u.y.scale, u.y.offset),
                )))
            } else if let Some(f) = opt_ud::<LFont>(v) {
                Some(Variant::Font(f.0))
            } else if let Some(x) = crate::ltypes::datatype_to_variant(v) {
                Some(x)
            } else if let Some(p) = opt_ud::<LVector3>(v) {
                Some(Variant::Vector3(Vector3::new(p.0.x, p.0.y, p.0.z)))
            } else if let Some(c) = opt_ud::<LColor3>(v) {
                Some(Variant::Color3(Color3::new(c.0[0], c.0[1], c.0[2])))
            } else if let Some(c) = opt_ud::<LCFrame>(v) {
                Some(Variant::CFrame(c.0.to_cframe()))
            } else {
                return Err(rt_err(format!(
                    "{} is not a supported attribute type",
                    v.type_name()
                )));
            }
        }
    })
}

fn call_method(lua: &Lua, name: &str, args: MultiValue) -> mlua::Result<MultiValue> {
    let this = opt_inst(&arg(&args, 0)).ok_or_else(|| {
        rt_err(format!(
            "Expected ':' not '.' calling member function {name}"
        ))
    })?;
    let class_u = inst_class(lua, this)?.0;
    let m = routes::method_route(class_u, name);
    call_method_routed(lua, name, args, this, class_u, &m)
}

/// `call_method` with the method's route worked out (runtime_routes.rs).
fn call_method_routed(
    lua: &Lua,
    name: &str,
    args: MultiValue,
    this: Ref,
    class_u: rbx_dom_weak::Ustr,
    m: &routes::MethodRoute,
) -> mlua::Result<MultiValue> {
    // Same name, different class (Team:GetPlayers, Humanoid:MoveTo…): the extension handles it.
    let (core, extended) = (m.core, m.extended);
    let class: &str = class_u.as_str();
    // A method taken from one class and called on an instance that hasn't
    // it fails as in Roblox (an admin script's anti-kick check calls
    // `Player.Kick(workspace, …)` and expects exactly this message).
    if !core && !extended {
        return Err(rt_err(format!(
            "Expected ':' not '.' calling member function {name}"
        )));
    }
    // Domains (src/domains) may take over a core method too.
    if core && (m.domains != 0 || domains::route_check_on()) {
        if let Some(r) = domains::call_masked_check(lua, name, this, &args, m.domains, m.shadowed) {
            return r;
        }
    }
    if !core && extended {
        return Some(ext::call_routed(lua, name, this, &args, m.domains))
            .unwrap_or_else(|| stubs::unimplemented_call(lua, this, class, name));
    }
    let one = |v: Value| -> mlua::Result<MultiValue> { Ok(MultiValue::from_iter([v])) };
    let none = || -> mlua::Result<MultiValue> { Ok(MultiValue::new()) };
    match name {
        "GetAxis" | "GetSecondaryAxis" => {
            let key = if name == "GetAxis" { "Axis" } else { "SecondaryAxis" };
            one(prop_get(lua, this, &class, key)?.unwrap_or(Value::Nil))
        }
        "FindFirstChild" => {
            let n = arg_str(&args, 1, "string")?;
            let rec = matches!(arg(&args, 2), Value::Boolean(true));
            let r = if rec {
                find_child(lua, this, |i| i.name == n, rec)?
            } else {
                find_named_child(lua, this, &n)?
            };
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstChildOfClass" => {
            let c = arg_str(&args, 1, "string")?;
            let r = find_child(lua, this, |i| i.class == c.as_str(), false)?;
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstChildWhichIsA" => {
            let c = arg_str(&args, 1, "string")?;
            let rec = matches!(arg(&args, 2), Value::Boolean(true));
            let r = find_child(lua, this, |i| doc::is_a(&i.class, &c), rec)?;
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstDescendant" => {
            let n = arg_str(&args, 1, "string")?;
            let r = find_child(lua, this, |i| i.name == n, true)?;
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstAncestor" => {
            let n = arg_str(&args, 1, "string")?;
            let r = find_ancestor(lua, this, |i| i.name == n)?;
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstAncestorOfClass" => {
            let c = arg_str(&args, 1, "string")?;
            let r = find_ancestor(lua, this, |i| i.class == c.as_str())?;
            one(opt_ref_value(lua, r)?)
        }
        "FindFirstAncestorWhichIsA" => {
            let c = arg_str(&args, 1, "string")?;
            let r = find_ancestor(lua, this, |i| doc::is_a(&i.class, &c))?;
            one(opt_ref_value(lua, r)?)
        }
        "GetChildren" => {
            let kids = children_of(lua, this)?;
            one(Value::Table(list(lua, kids)?))
        }
        "GetDescendants" => {
            let w = world(lua)?;
            let d = w.d();
            let refs: Vec<Ref> = if d.dom.root_ref() == this && security::hides_core_services(lua) {
                descendants_skip_hidden(d, this)
            } else {
                d.dom
                    .descendants_of(this)
                    .map(|i| i.referent())
                    .filter(|r| *r != this)
                    .collect()
            };
            drop(w);
            one(Value::Table(list(lua, refs)?))
        }
        "IsA" => {
            let c = arg_str(&args, 1, "string")?;
            one(Value::Boolean(doc::is_a(&class, &c)))
        }
        "IsDescendantOf" => {
            let other = opt_inst(&arg(&args, 1));
            let w = world(lua)?;
            one(Value::Boolean(
                other.is_some_and(|o| w.d().is_ancestor(o, this)),
            ))
        }
        "IsAncestorOf" => {
            let other = opt_inst(&arg(&args, 1));
            let w = world(lua)?;
            one(Value::Boolean(
                other.is_some_and(|o| w.d().is_ancestor(this, o)),
            ))
        }
        "Clone" => {
            if matches!(
                world(lua)?.d().prop(this, "Archivable"),
                Some(Variant::Bool(false))
            ) {
                return one(Value::Nil);
            }
            // clone_within leaves the copy unparented, like Instance:Clone().
            let r = world(lua)?.dm().dom.clone_within(this);
            domains::assets_cloud::scene::track_instance(lua,r)?;
            // Descendants with Archivable = false aren't copied (Roblox).
            {
                let mut w = world(lua)?;
                let skip: Vec<Ref> = w
                    .d()
                    .dom
                    .descendants_of(r)
                    .filter(|i| {
                        i.referent() != r
                            && matches!(
                                i.properties.get(&ustr("Archivable")),
                                Some(Variant::Bool(false))
                            )
                    })
                    .map(|i| i.referent())
                    .collect();
                for x in skip {
                    if w.d().dom.get_by_ref(x).is_some() {
                        w.dm().dom.destroy(x);
                    }
                }
                w.structure_changed = true;
            }
            one(inst_value(lua, r)?)
        }
        "Destroy" => {
            destroy_inst(lua, this)?;
            none()
        }
        "Remove" => {
            set_parent(lua, this, Ref::none())?;
            none()
        }
        "ClearAllChildren" => {
            // Studio's Instance::clearAllChildren destroys the
            // last child until none is left: ChildRemoved comes newest first,
            // and each child leaves from the end of the list (no shift).
            for c in children_of(lua, this)?.into_iter().rev() {
                destroy_inst(lua, c)?;
            }
            none()
        }
        "GetFullName" => {
            // (game:GetFullName() is its Name: "Place1" in Studio's edit DataModel.)
            let s = {
                let w = world(lua)?;
                if this == w.d().dom.root_ref() {
                    w.d().dom.root().name.clone()
                } else {
                    w.full_name(this)
                }
            };
            one(Value::String(lua.create_string(&s)?))
        }
        "GetAttribute" => {
            let n = arg_str(&args, 1, "string")?;
            // Borrowed: only the one value is cloned (round 27: cloning the
            // whole map per call made GetAttribute 3.5 µs in Place A's nav build).
            let v = {
                let w = world(lua)?;
                let d = w.d();
                match d.dom.get_by_ref(this).and_then(|i| i.properties.get(&ustr("Attributes"))) {
                    Some(Variant::Attributes(a)) => a.get(n.as_str()).cloned(),
                    _ => None,
                }
            };
            one(match v {
                Some(v) => variant_to_lua(lua, &v, None)?,
                None => Value::Nil,
            })
        }
        "GetAttributes" => {
            let t = lua.create_table()?;
            if let Some(Variant::Attributes(a)) = world(lua)?.d().prop(this, "Attributes") {
                for (k, v) in a.iter() {
                    t.set(k.as_str(), variant_to_lua(lua, v, None)?)?;
                }
            }
            one(Value::Table(t))
        }
        "SetAttribute" => {
            let n = arg_str(&args, 1, "string")?;
            check_attribute_name(&n)?;
            let var = attribute_value(&arg(&args, 2))?;
            set_attr(lua, this, &n, var)?;
            none()
        }
        "GetPropertyChangedSignal" => {
            let p = arg_str(&args, 1, "string")?;
            let (class, _, _) = inst_meta(lua, this)?;
            let canon = prop_desc(&class, &p).map_or(p.as_str(), |(c, _, _)| c);
            one(signal(lua, this, &format!("Changed:{canon}"))?)
        }
        "GetAttributeChangedSignal" => {
            let p = arg_str(&args, 1, "string")?;
            one(signal(lua, this, &format!("AttributeChanged:{p}"))?)
        }
        "GetService" | "FindService" => {
            let c = arg_str(&args, 1, "string")?;
            if security::service_hidden(lua, &c) {
                return one(Value::Nil);
            }
            let r = get_service(lua, &c, name == "GetService")?;
            one(opt_ref_value(lua, r)?)
        }
        "GetPrimaryPartCFrame" => {
            let (_, frame) = primary_part_frame(lua, this, name)?;
            one(LCFrame(frame).into_lua(lua)?)
        }
        "GetPivot" => {
            let (class, _, _) = inst_meta(lua, this)?;
            let xf = if doc::is_a(&class, "BasePart") {
                part_xform(lua, this)?
            } else {
                model_pivot(&*world(lua)?, this)
            };
            one(LCFrame(xf).into_lua(lua)?)
        }
        "PivotTo" | "SetPrimaryPartCFrame" => {
            let cf = opt_ud::<LCFrame>(&arg(&args, 1)).ok_or_else(|| rt_err("CFrame expected"))?;
            if name == "SetPrimaryPartCFrame" {
                let primary = primary_part_frame(lua, this, name)?;
                move_model(lua, this, cf.0, Some(primary), true)?;
            } else {
                pivot_to(lua, this, cf.0)?;
            }
            none()
        }
        "MoveTo" => {
            let v = opt_ud::<LVector3>(&arg(&args, 1)).ok_or_else(|| rt_err("Vector3 expected"))?;
            let pivot = model_pivot(&*world(lua)?, this);
            pivot_to(
                lua,
                this,
                Xform {
                    pos: v.0,
                    rot: pivot.rot,
                },
            )?;
            none()
        }
        "GetMass" => {
            let s = part_size(lua, this)?;
            one(Value::Number((s.x * s.y * s.z * mech::PART_DENSITY) as f64))
        }
        "ApplyImpulse" => {
            let imp =
                opt_ud::<LVector3>(&arg(&args, 1)).ok_or_else(|| rt_err("Vector3 expected"))?;
            let s = part_size(lua, this)?;
            let mass = (s.x * s.y * s.z * mech::PART_DENSITY).max(1e-4);
            let mut w = world(lua)?;
            let v = w.velocities.get(&this).copied().unwrap_or(Vec3::ZERO);
            w.velocity_requests.push((this, v + imp.0 / mass));
            none()
        }
        "GetState" => {
            let name = world(lua)?
                .humanoid_states
                .get(&this)
                .copied()
                .unwrap_or("Running");
            match LEnumItem::from_name("HumanoidStateType", name) {
                Some(item) => one(crate::ltypes::enum_value(lua, item)?),
                None => one(Value::Nil),
            }
        }
        "TakeDamage" => {
            let dmg = num(&arg(&args, 1)).unwrap_or(0.0);
            let shielded = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .get_by_ref(this)
                    .map(|i| i.parent())
                    .is_some_and(|p| d.find_child(p, "ForceField").is_some())
            };
            if !shielded {
                let h = match world(lua)?.d().prop(this, "Health") {
                    Some(Variant::Float32(h)) => h as f64,
                    _ => 100.0,
                };
                newindex(lua, this, "Health", Value::Number(h - dmg))?;
            }
            none()
        }
        "Fire" => {
            let rest: Vec<Value> = args.iter().skip(1).cloned().collect();
            world(lua)?.fire(this, "Event", rest);
            none()
        }
        "FireServer" => {
            if world(lua)?.side != Side::Client {
                return Err(rt_err("FireServer can only be called from the client"));
            }
            let payload: Vec<Xfer> = args.iter().skip(1).map(|v| export(v, 0)).collect();
            let mut w = world(lua)?;
            if w.has_peer {
                w.outbox.push(RemoteMsg {
                    inst: this,
                    kind: RemoteKind::Event,
                    args: payload,
                });
            }
            none()
        }
        "FireClient" | "FireAllClients" => {
            if world(lua)?.side != Side::Server {
                return Err(rt_err(format!("{name} can only be called from the server")));
            }
            if name == "FireClient" && opt_inst(&arg(&args, 1)).is_none() {
                return Err(rt_err("FireClient: player argument must be a Player"));
            }
            // One client: FireClient(player, …) and FireAllClients(…) both reach it,
            // except FireAllClients before any Player has joined has no recipients
            // (Roblox does not replay pre-join FireAllClients to future players).
            let skip = if name == "FireClient" { 2 } else { 1 };
            let payload: Vec<Xfer> = args.iter().skip(skip).map(|v| export(v, 0)).collect();
            let mut w = world(lua)?;
            let has_recipient = if name == "FireAllClients" {
                let d = w.d();
                d.find_child(d.dom.root_ref(), "Players").is_some_and(|players| {
                    d.dom.get_by_ref(players).is_some_and(|i| {
                        i.children()
                            .iter()
                            .any(|c| d.class_of(*c).is_some_and(|k| k == "Player"))
                    })
                })
            } else {
                true
            };
            if w.has_peer && has_recipient {
                w.outbox.push(RemoteMsg {
                    inst: this,
                    kind: RemoteKind::Event,
                    args: payload,
                });
            }
            none()
        }
        "GetPlayers" => {
            let kids: Vec<Ref> = {
                let w = world(lua)?;
                let d = w.d();
                d.dom
                    .get_by_ref(this)
                    .map(|i| {
                        i.children()
                            .iter()
                            .copied()
                            .filter(|c| d.class_of(*c).is_some_and(|k| k == "Player"))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            one(Value::Table(list(lua, kids)?))
        }
        "GetPlayerFromCharacter" => {
            let ch = opt_inst(&arg(&args, 1));
            let found = {
                let w = world(lua)?;
                let d = w.d();
                ch.and_then(|ch| {
                    d.dom.get_by_ref(this)?.children().iter().copied().find(
                        |p| matches!(d.prop(*p, "Character"), Some(Variant::Ref(c)) if c == ch),
                    )
                })
            };
            one(opt_ref_value(lua, found)?)
        }
        "LoadCharacter" => {
            world(lua)?.load_character.push(this);
            none()
        }
        // Studio's edit DataModel (measured with a plugin, 0.747): IsEdit,
        // IsClient, IsServer and IsStudio true, IsRunning and IsRunMode false.
        "IsClient" => {
            let w = world(lua)?;
            one(Value::Boolean(w.side == Side::Client || w.mode == RunMode::Edit))
        }
        "IsServer" => one(Value::Boolean(world(lua)?.side == Side::Server)),
        "IsStudio" => one(Value::Boolean(true)),
        "IsRunning" => one(Value::Boolean(world(lua)?.mode != RunMode::Edit)),
        "IsRunMode" => one(Value::Boolean(world(lua)?.mode == RunMode::Run)),
        "IsEdit" => one(Value::Boolean(world(lua)?.mode == RunMode::Edit)),
        "IsKeyDown" => {
            let k = opt_ud::<LEnumItem>(&arg(&args, 1))
                .ok_or_else(|| rt_err("Enum.KeyCode expected"))?;
            one(Value::Boolean(world(lua)?.keys_down.contains(&k.name)))
        }
        _ => ext::call_routed(lua, name, this, &args, m.domains),
    }
}

// ---------------------------------------------------------------- prelude

/// Lua side of the runtime: everything that yields, plus the signal type.
const PRELUDE: &str = r##"
local rs_wait, rs_wait_signal, rs_output, rs_connect, rs_load_module, rs_index, rs_newindex = ...
local yield, running = coroutine.yield, coroutine.running

-- Engine calls return (ok, ...) and errors are rethrown here without a
-- position, like Roblox's own: pcall gets "X is not a valid member of …"
-- alone (a test place warns "DataStore unavailable: You must
-- publish …"), and Output puts the script and line in the stack after it.
local function check(ok, ...)
    if ok then return ... end
    error((...), 0)
end
local function inst_index(self, key) return check(rs_index(self, key)) end
local function inst_newindex(self, key, value) check(rs_newindex(self, key, value)) end
local function wrap(f) return function(...) return check(f(...)) end end

-- A signal is a userdata over { __inst, __name } (ltypes LSignal), its
-- methods these.
local Signal = {}
-- Events whose Connect / Wait want a capability (API dump Security /
-- Capabilities, checked for the calling thread: src/security.rs).
local rs_identity, sec_event, sec_names = __rs_identity, __rs_sec_event, __rs_sec_event_names
local function gate(self, verb)
    if sec_names[self.__name] then
        local ok, e = sec_event(self.__inst, self.__name, verb)
        if not ok then error(e, 0) end
    end
end
-- Studio: a non-function still answers a (dead) Connection; the failure
-- goes to Output as an error, not to the caller (RE-DATATYPES signal.*).
local function noop() end
local function connect(self, fn, once)
    gate(self, "connect")
    if type(fn) ~= "function" then
        rs_output("error", "Attempt to connect failed: Passed value is not a function")
        local c = rs_connect(self.__inst, self.__name, noop, once, true)
        c:Disconnect()
        return c
    end
    return rs_connect(self.__inst, self.__name, fn, once, false)
end
function Signal:Connect(fn) return connect(self, fn, false) end
function Signal:Once(fn) return connect(self, fn, true) end
Signal.connect = Signal.Connect
-- The calling script (level 3: past this function and the named C method
-- function, cmeta) must sit under an Actor; checked before anything else.
local getfenv, rawget_, typeof_ = getfenv, rawget, typeof
local function under_actor(level)
    local ok, env = pcall(getfenv, level + 1)
    local s = ok and type(env) == "table" and env.script
    if typeof_(s) ~= "Instance" then return false end
    local ok2, a = pcall(function() return s:FindFirstAncestorWhichIsA("Actor") end)
    return ok2 and a ~= nil
end
function Signal:ConnectParallel(fn)
    if not under_actor(2) then error("Scripts that connect in parallel must be rooted under an Actor.", 0) end
    return connect(self, fn, false)
end
function Signal:Wait()
    gate(self, "wait")
    rs_wait_signal(self.__inst, self.__name, running())
    return yield()
end
Signal.wait = Signal.Wait
function __rs_new_signal(inst, name)
    return __rs_make_signal({ __inst = inst, __name = name }, Signal)
end

function task.wait(t)
    rs_wait(running(), tonumber(t) or 0, false)
    return yield()
end
function wait(t)
    rs_wait(running(), tonumber(t) or 0, true)
    local elapsed = yield()
    return elapsed, time()
end

local function fmt(...)
    local n = select("#", ...)
    local parts = table.create(n)
    for i = 1, n do parts[i] = tostring((select(i, ...))) end
    return table.concat(parts, " ")
end
function print(...)
    local src, line = debug.info(2, "sl")
    rs_output("print", fmt(...), src, line)
end
function warn(...)
    local src, line = debug.info(2, "sl")
    rs_output("warn", fmt(...), src, line)
end

-- One module cache per identity: a command-bar require (identity 1) must
-- not hand game scripts its result, and Studio runs the module again.
local loaded_by, loading_by = {}, {}
local asset_modules, asset_loading = {}, {}
local NIL_RESULT = newproxy(false) -- a module that returned nil, cached
local FAILED = newproxy(false) -- a module whose code raised
-- Require by string (Luau's require-by-string, as Studio's own messages
-- read): "./X" is a sibling of the requiring script, "../X" one level up,
-- "@self/X" its child, "@game/Service/X" from the DataModel (estimated); ".." inside a
-- path goes to the parent. Children are found, not waited for.
local function require_path(path, ctx)
    -- Studio's require raises without a position (RE-GLOBALS.md require.*).
    local function fail(why) error(string.format('error requiring "%s": %s', path, why), 0) end
    if typeof(ctx) ~= "Instance" then fail("could not get requiring context") end
    local node, rest
    if path:sub(1, 2) == "./" then
        node, rest = ctx.Parent, path:sub(3)
    elseif path:sub(1, 3) == "../" then
        node, rest = ctx.Parent and ctx.Parent.Parent, path:sub(4)
    elseif path:sub(1, 1) == "@" then
        local alias
        alias, rest = path:match("^@([^/]*)/?(.*)$")
        if alias == "self" then node = ctx
        elseif alias == "game" then node = game
        else fail('could not resolve alias "' .. tostring(alias) .. '"') end
    else
        fail("require path must start with a valid prefix: ./, ../, or @")
    end
    if not node then fail("could not get parent of requiring context") end
    for comp in rest:gmatch("[^/]+") do
        if comp == ".." then
            node = node.Parent
            if not node then fail('could not get parent of component "' .. comp .. '"') end
        elseif comp ~= "." then
            local c = node:FindFirstChild(comp)
            if not c then fail('could not resolve child component "' .. comp .. '"') end
            node = c
        end
    end
    return node
end
local function require_impl(m, script)
    if type(m) == "string" then
        -- The requiring script: the calling function's environment's `script`.
        m = require_path(m, script or __rs_caller_script())
    end
    -- false is asset id 0 (measured); the client may not load assets.
    if type(m) == "boolean" then m = 0 end
    -- (The edit DataModel is client and server at once: plugins require
    -- asset modules there, as F3X does; Studio measured "function".)
    -- (IsEdit needs PluginOrOpenCloud: a game script that can't ask is not
    -- a plugin in the edit DataModel.)
    local function in_edit()
        local ok, edit = pcall(function() return game:GetService("RunService"):IsEdit() end)
        return ok and edit
    end
    if type(m) == "number" and game:GetService("RunService"):IsClient() and not in_edit() then
        error(string.format("require(assetId) cannot be called from a client.  assetId = %d", m), 0)
    end
    if type(m) == "number" then
        -- An asset id: cache the loaded MainModule per id (Studio loads and
        -- executes each asset module once per VM so multiple scripts requiring
        -- the same id share its state, even across concurrent yields).
        local id = m
        local main = asset_modules[id]
        if not main then
            if asset_loading[id] then
                while asset_loading[id] do task.wait() end
                main = asset_modules[id]
            else
                asset_loading[id] = true
                local ok, model = pcall(function()
                    return game:GetService("InsertService"):LoadAsset(id)
                end)
                local found = ok and model and model:FindFirstChild("MainModule", true)
                if found and found:IsA("ModuleScript") then
                    asset_modules[id] = found
                    main = found
                end
                asset_loading[id] = nil
            end
        end
        if not (main and main:IsA("ModuleScript")) then
            error(string.format('Downloading asset failed for asset id %d.  Is the asset id correct and is the asset type "Model"?', id), 0)
        end
        m = main
    end
    if typeof(m) ~= "Instance" or not m:IsA("ModuleScript") then
        error("Attempted to call require with invalid argument(s).", 0)
    end
    local ident = rs_identity()
    local loaded, loading = loaded_by[ident], loading_by[ident]
    if not loaded then
        loaded, loading = {}, {}
        loaded_by[ident], loading_by[ident] = loaded, loading
    end
    local v = loaded[m]
    if v == FAILED then error("Requested module experienced an error while loading", 0) end
    if v ~= nil then return (if v == NIL_RESULT then nil else v) end
    if loading[m] then
        -- Another script is still loading it (the module yielded): wait for
        -- that, like Roblox. The same thread asking again is a real cycle.
        if loading[m] == running() then error("Requested module was required recursively", 0) end
        while loading[m] do task.wait() end
        v = loaded[m]
        if v ~= nil and v ~= FAILED then return (if v == NIL_RESULT then nil else v) end
        error("Requested module experienced an error while loading", 0)
    end
    loading[m] = running()
    local f = rs_load_module(m)
    local memoryCategory = __ac_scene_mem_begin(m)
    local mod_tb = nil
    local res = table.pack(xpcall(f, function(err)
        mod_tb = debug.traceback(nil, 2)
        return err
    end))
    __ac_scene_mem_restore(memoryCategory)
    loading[m] = nil
    local ok, r = res[1], res[2]
    if not ok then
        -- The module's own error and stack trace go to Output; the requirer gets
        -- Studio's generic message, and so does every later require of it.
        loaded[m] = FAILED
        rs_output("module_error", tostring(r), mod_tb)
        error("Requested module experienced an error while loading", 0)
    end
    -- One value, which may be nil (a types-only module ends `return nil`);
    -- no return at all, or two values, is the error.
    if res.n ~= 2 then error("Module code did not return exactly one value", 0) end
    loaded[m] = if r == nil then NIL_RESULT else r
    return r
end
-- One require for every script, as Studio's: the requiring script for a
-- path is read from the calling function's environment by
-- __rs_caller_script (src/globals_parity.rs), not getfenv, which would clear
-- the caller's safeenv flag and with it Luau's fast paths.
function require(m) return require_impl(m, nil) end

__rs_lua_methods = {}
-- A Lua-written method called on the wrong thing fails as Roblox's own do
-- (Adonis: `RemoteFunction.InvokeServer(workspace)` must say exactly this).
-- typeof only: a script's proxy userdata must not see an __index.
function __rs_member_check(self, class, name)
    if typeof(self) ~= "Instance" or not self:IsA(class) then
        error("Expected ':' not '.' calling member function " .. name, 0)
    end
end
local member = __rs_member_check
function __rs_lua_methods.WaitForChild(self, name, timeout)
    local c = self:FindFirstChild(name)
    if c then return c end
    -- Studio names the instance as it was at the call (a test place's
    -- CharacterAdded handler: 'Player:WaitForChild("Torso")', unparented).
    local start, warned, full = time(), false, self:GetFullName()
    -- Studio's warning names the script line that waits, not this prelude.
    local lvl, src, line = 2, nil, nil
    repeat
        src, line = debug.info(lvl, "sl")
        lvl += 1
    until src == nil or (src ~= "[C]" and not string.find(src, "rustudio.", 1, true))
    while true do
        task.wait()
        c = self:FindFirstChild(name)
        if c then return c end
        if timeout and time() - start >= timeout then return nil end
        if not timeout and not warned and time() - start >= 5 then
            warned = true
            rs_output("warn_stack", "Infinite yield possible on '" .. full .. ":WaitForChild(\"" .. tostring(name) .. "\")'", debug.traceback(nil, 2), line)
        end
    end
end
-- Invoke before the callback exists yields until it is set: a
-- BindableFunction's Invoke "yields until the corresponding callback is
-- found" (and never resumes if none is ever set), and a RemoteFunction
-- queues invocations until OnServerInvoke / OnClientInvoke is assigned
-- (both class pages). Erroring "OnInvoke is not set" instead broke every
-- LocalScript that starts before the one that sets the callback (a
-- character's Controls script invoking a RemoteFunction-style
-- BindableFunction that a 70k-line renderer LocalScript sets up).
local function __rs_wait_callback(self, key)
    local cb = __rs_get_callback(self, key)
    while not cb do
        task.wait()
        cb = __rs_get_callback(self, key)
    end
    return cb
end
function __rs_lua_methods.Invoke(self, ...)
    member(self, "BindableFunction", "Invoke")
    local cb = __rs_wait_callback(self, "OnInvoke")
    return cb(...)
end
function __rs_lua_methods.InvokeServer(self, ...)
    member(self, "RemoteFunction", "InvokeServer")
    local cb = __rs_wait_callback(self, "OnServerInvoke")
    return cb(game:GetService("Players").LocalPlayer, ...)
end
function __rs_lua_methods.InvokeClient(self, player, ...)
    member(self, "RemoteFunction", "InvokeClient")
    error("InvokeClient can only be called from the server", 2)
end

return inst_index, inst_newindex, wrap
"##;

// ---------------------------------------------------------------- runtime

/// `getmetatable` of every bridge object answers "The metatable is locked",
/// as in Studio: one throwaway object of each type.
fn lock_bridge(lua: &Lua) -> mlua::Result<()> {
    use crate::cmeta::lock;
    lock(lua, &lua.create_userdata(LInst(Ref::none()))?)?;
    lock(lua, &lua.create_userdata(LObj(Ref::none()))?)?;
    lock(lua, &lua.create_userdata(LConn { key: (Ref::none(), String::new()), id: 0 })?)?;
    let sig: AnyUserData = lua
        .globals()
        .get::<Function>("__rs_make_signal")?
        .call((lua.create_table()?, lua.create_table()?))?;
    lock(lua, &sig)?;
    crate::ltypes::lock_enums(lua)
}

/// Turns a Lua error into one Output line: `Script.Path:line: message`.
pub fn describe_error(e: &mlua::Error) -> String {
    fn cause_msg(e: &mlua::Error) -> String {
        match e {
            mlua::Error::RuntimeError(m) => m.lines().next().unwrap_or("").to_string(),
            mlua::Error::SyntaxError { message, .. } => {
                message.lines().next().unwrap_or("").to_string()
            }
            mlua::Error::CallbackError { cause, .. } => cause_msg(cause),
            mlua::Error::WithContext { cause, .. } => cause_msg(cause),
            other => other.to_string().lines().next().unwrap_or("").to_string(),
        }
    }
    // Errors raised from Rust callbacks are engine errors: the message
    // alone, as Studio prints "Downloading asset failed for asset id …"
    // (round 26); where it happened is the stack after it (callback_origin).
    let m = cause_msg(e);
    match m.strip_prefix(crate::cmeta::LOCATED) {
        Some(rest) => rest.to_string(),
        None => m,
    }
}

/// Where a Rust callback's error was raised: the first Lua frame of its
/// traceback.
fn callback_origin(e: &mlua::Error) -> Option<(String, u32)> {
    let mlua::Error::CallbackError { traceback, .. } = e else {
        return None;
    };
    for line in traceback.lines() {
        let tok = line.split_whitespace().next().unwrap_or("");
        if tok.is_empty() || tok.starts_with('[') || tok.starts_with("stack") {
            continue;
        }
        if let Some((src, ln)) = tok.trim_end_matches(':').rsplit_once(':') {
            if let Ok(ln) = ln.parse::<u32>() {
                if !src.starts_with("rustudio.") {
                    return Some((src.to_string(), ln));
                }
            }
        }
    }
    None
}

fn thread_arg(lua: &Lua, v: Option<Value>, what: &str) -> mlua::Result<Thread> {
    match v {
        Some(Value::Function(f)) => domains::assets_cloud::scene::create_thread(lua,f),
        Some(Value::Thread(t)) => Ok(t),
        _ => Err(rt_err(format!("{what} expects a function or thread"))),
    }
}

/// Luau feature flags Roblox ships on. The compiler flags reuse a dead
/// local's register (Luau 745); without them a big script like Place A'
/// Weapons client fails with "Out of local registers". Flags are global and
/// must be set before any VM exists or compiles, so this runs once.
pub fn luau_flags() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for f in ["LuauCompileReuseLocalRegs", "LuauCompileReuseLowersLimit"] {
            if Lua::set_fflag(f, true).is_err() {
                eprintln!("rustudio: Luau flag {f} missing");
            }
        }
    });
}

impl Runtime {
    pub fn new() -> Result<Self, String> {
        domains::plugin_api::freeze();
        luau_flags();
        let lua = Lua::new();
        // Scripts marked --!native (and @native functions) run as native
        // code, as in Roblox (vendor mlua state/raw.rs: other chunks are
        // interpreted). RUSTUDIO_NO_NATIVE=1: everything interpreted.
        let native_on = !std::env::var_os("RUSTUDIO_NO_NATIVE").is_some_and(|v| v != "0");
        if !native_on {
            lua.enable_jit(false);
        }
        // Their native code is built in the frames after they load
        // (native_drain), not while Play starts. RUSTUDIO_NATIVE_NOW=1: at
        // the load, as before.
        lua.set_app_data(NativeQueue {
            on: native_on && !std::env::var_os("RUSTUDIO_NATIVE_NOW").is_some_and(|v| v != "0"),
            ..Default::default()
        });
        rustudio_core::load_trace::mark("runtime: Lua made");
        // RUSTUDIO_DETERMINISTIC (test runs): Luau seeds math.random from the time and the
        // state's address; the default Animate script rolls its weighted variants with it,
        // and the chosen pose shapes the character. Seeded by creation order instead.
        if rbx_dom_weak::deterministic() {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let _ = lua.load(format!("math.randomseed({n})")).exec();
        }
        routes::install_atoms(&lua);
        rustudio_core::load_trace::mark("runtime: atoms");
        crate::globals_parity::init_vm(&lua);
        rustudio_core::load_trace::mark("runtime: globals parity");
        // Studio compiles scripts at Luau -O1 (no inlining), so Output stacks
        // keep every frame; -O2 inlined local functions out of error stacks.
        // Studio's other compile options:
        // `Vector3.new` is Luau's vector constructor (the LBF_VECTOR fastcall:
        // no call into the engine for `Vector3.new(x, y, z)` with numbers),
        // and these globals stay mutable (not cached as imports).
        // (bytecode_cache::compiler: the same settings for its cached scripts)
        lua.set_compiler(crate::bytecode_cache::compiler());
        lua.set_app_data(Sched::default());
        lua.set_app_data(Signals::default());
        lua.set_app_data(signal_timing::Reentry::default());
        let weak = lua.create_table().map_err(|e| e.to_string())?;
        let mt = lua.create_table().map_err(|e| e.to_string())?;
        mt.raw_set("__mode", "v").map_err(|e| e.to_string())?;
        weak.set_metatable(Some(mt)).map_err(|e| e.to_string())?;
        let sigs = lua.create_table().map_err(|e| e.to_string())?;
        let sig_mt = lua.create_table().map_err(|e| e.to_string())?;
        // Luau's 's' mode shrinks sparse weak tables after collection. A
        // burst of Connect calls leaves few live signal wrappers, so later
        // GC cycles should only scan the signal states still held by Lua.
        sig_mt.raw_set("__mode", "vs").map_err(|e| e.to_string())?;
        sigs.set_metatable(Some(sig_mt)).map_err(|e| e.to_string())?;
        lua.set_app_data(Cache { weak: Some(weak), sigs: Some(sigs), ..Default::default() });
        lua.set_app_data(Budget { start: None });
        lua.set_app_data(ChildIndex::default());
        lua.set_app_data(ProfileDepth::default());
        security::install_thread_hook(&lua).map_err(|e| e.to_string())?;
        let mut callbacks = 0usize;
        unsafe {
            lua.exec_raw::<()>((), |state| {
                callbacks = mlua::ffi::lua_callbacks(state) as usize;
            })
        }
        .map_err(|e| e.to_string())?;
        lua.set_interrupt(move |lua| {
            // Armed by the watchdog (interrupt_watchdog): one check, then off.
            interrupt_watchdog::disarm(callbacks);
            if let Some(b) = lua.app_data_ref::<Budget>() {
                if let Some(s) = b.start {
                    // Studio Settings > Debugger > Script Timeout Length
                    // (0 or less: no limit; the default without the store).
                    let limit = domains::settings_host::num(
                        "Studio",
                        "ScriptTimeoutLength",
                        mech::SCRIPT_TIMEOUT_S as f64,
                    );
                    if limit > 0.0 && s.elapsed() > Duration::from_secs_f64(limit.min(1e9)) {
                        // Name the script and line that ran too long.
                        let at = (0..32)
                            .map_while(|level| {
                                lua.inspect_stack(level, |d| {
                                    let src = d
                                        .source()
                                        .short_src
                                        .map(|s| s.trim_start_matches(['=', '@']).to_string())
                                        .unwrap_or_default();
                                    (src, d.current_line())
                                })
                            })
                            .find(|(s, _)| {
                                !s.is_empty() && !s.starts_with("rustudio.") && s != "[C]"
                            });
                        return Err(rt_err(match at {
                            Some((s, Some(l))) => {
                                format!("{s}:{l}: Script timeout: exhausted allowed execution time")
                            }
                            Some((s, None)) => {
                                format!("{s}: Script timeout: exhausted allowed execution time")
                            }
                            None => "Script timeout: exhausted allowed execution time".to_string(),
                        }));
                    }
                }
            }
            Ok(mlua::VmState::Continue)
        });
        let shared = lua.create_table().map_err(|e| e.to_string())?;
        crate::script_profiler::attach(&lua, callbacks as *mut mlua::ffi::lua_Callbacks);
        interrupt_watchdog::register(callbacks as *mut mlua::ffi::lua_Callbacks);
        let rt = Runtime { lua, shared, callbacks };
        rustudio_core::load_trace::mark("runtime: hooks");
        rt.install().map_err(|e| describe_error(&e))?;
        rustudio_core::load_trace::mark("runtime: installed");
        Ok(rt)
    }

    fn install(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        let g = lua.globals();
        install(lua, &g)?;

        let inst = lua.create_table()?;
        inst.set(
            "new",
            lua.create_function(|lua, (class, parent): (String, Value)| {
                security::check_create(lua, &class)?;
                if !doc::is_script_creatable(&class) {
                    return Err(rt_err(format!(
                        "Unable to create an Instance of type \"{class}\""
                    )));
                }
                let r = {
                    let mut w = world(lua)?;
                    // The constructor's Name (BindableEvent "Event", BloomEffect "Bloom" …).
                    let r = w.dm().dom.insert(
                        Ref::none(),
                        InstanceBuilder::new(class.as_str()).with_name(doc::default_instance_name(&class)),
                    );
                    w.structure_changed = true;
                    r
                };
                domains::assets_cloud::scene::track_instance(lua,r)?;
                if let Some(p) = opt_inst(&parent) {
                    set_parent(lua, r, p)?;
                }
                inst_value(lua, r)
            })?,
        )?;
        // Instance.fromExisting(x): a new instance of x's class with x's
        // properties (not its children), parentless (Studio: Name and
        // values kept, Parent nil).
        inst.set(
            "fromExisting",
            lua.create_function(|lua, v: Value| {
                let src = opt_inst(&v).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
                let class = world(lua)?.d().class_of(src).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
                security::check_create(lua, &class)?;
                if !doc::is_script_creatable(&class) {
                    return Err(rt_err(format!("Unable to create an Instance of type \"{class}\"")));
                }
                let r = {
                    let mut w = world(lua)?;
                    let (name, props) = {
                        let i = w.d().dom.get_by_ref(src).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
                        (i.name.clone(), i.properties.clone())
                    };
                    let b = InstanceBuilder::new(class.as_str()).with_name(name).with_prop_map(props);
                    let r = w.dm().dom.insert(Ref::none(), b);
                    w.structure_changed = true;
                    r
                };
                domains::assets_cloud::scene::track_instance(lua, r)?;
                inst_value(lua, r)
            })?,
        )?;
        g.set("Instance", inst)?;

        let task = lua.create_table()?;
        task.set(
            "spawn",
            lua.create_function(|lua, args: Variadic<Value>| {
                let mut it = args.into_iter();
                let th = thread_arg(lua, it.next(), "task.spawn")?;
                crate::script_profiler::with_entry(
                    crate::script_profiler::EntryKind::Spawn,
                    || (None, None),
                    || resume(lua, &th, it.collect()),
                );
                Ok(th)
            })?,
        )?;
        task.set(
            "defer",
            lua.create_function(|lua, args: Variadic<Value>| {
                let mut it = args.into_iter();
                let th = thread_arg(lua, it.next(), "task.defer")?;
                sched(lua)?.deferred.push_back((th.clone(), it.collect()));
                Ok(th)
            })?,
        )?;
        task.set(
            "delay",
            lua.create_function(|lua, args: Variadic<Value>| {
                let mut it = args.into_iter();
                let t = it.next().and_then(|v| num(&v)).unwrap_or(0.0);
                let th = thread_arg(lua, it.next(), "task.delay")?;
                let now = world(lua)?.time;
                sched(lua)?.schedule(now, t, th.clone(), Some(it.collect()), false);
                Ok(th)
            })?,
        )?;
        task.set(
            "cancel",
            lua.create_function(|lua, th: Thread| {
                let mut s = sched(lua)?;
                s.timers.retain(|t| t.thread != th);
                s.ready.retain(|t| t.thread != th);
                s.deferred.retain(|(t, _)| *t != th);
                Ok(())
            })?,
        )?;
        g.set("task", task)?;
        g.set(
            "spawn",
            lua.create_function(|lua, f: Function| {
                let th = domains::assets_cloud::scene::create_thread(lua, f)?;
                let now = world(lua)?.time;
                sched(lua)?.schedule(now, 0.0, th, Some(Vec::new()), true);
                Ok(())
            })?,
        )?;
        g.set(
            "delay",
            lua.create_function(|lua, (t, f): (f64, Function)| {
                let th = domains::assets_cloud::scene::create_thread(lua,f)?;
                let now = world(lua)?.time;
                sched(lua)?.schedule(now, t, th, Some(Vec::new()), true);
                Ok(())
            })?,
        )?;
        // (time() stays 0 in Studio's edit DataModel: .)
        g.set(
            "time",
            lua.create_function(|lua, ()| {
                let w = world(lua)?;
                Ok(if w.mode == RunMode::Edit { 0.0 } else { w.time })
            })?,
        )?;
        g.set(
            "elapsedTime",
            lua.create_function(|lua, ()| Ok(world(lua)?.time))?,
        )?;
        g.set(
            "tick",
            lua.create_function(|lua, ()| {
                // RUSTUDIO_FIXED_DT runs: game time from a fixed epoch; else
                // --fixed-dt's simulated clock or the wall clock (sim_clock).
                if let Some(t) = fixed_epoch_time(lua) {
                    return Ok(t);
                }
                Ok(rustudio_core::sim_clock::epoch_secs())
            })?,
        )?;
        g.set("shared", lua.create_table()?)?;

        let rs_wait = lua.create_function(|lua, (th, t, legacy): (Thread, f64, bool)| {
            let now = world(lua)?.time;
            sched(lua)?.schedule(now, t, th, None, legacy);
            Ok(())
        })?;
        let rs_wait_signal =
            lua.create_function(|lua, (inst, name, th): (LInst, String, Thread)| {
                let (id, item_changed, touch, pending) = {
                    let mut s = signals(lua)?;
                    s.next_id += 1;
                    let id = s.next_id;
                    let item_changed = name == "ItemChanged";
                    let touch = crate::runtime::TOUCH_EVENTS.contains(&name.as_str());
                    let key = (inst.0, name.clone());
                    s.note_name(inst.0, &name);
                    s.waiters.entry(key.clone()).or_default().push((id, th));
                    let pending = if is_remote_event_signal(&name) {
                        let item = s.remote_queue.get_mut(&key).and_then(VecDeque::pop_front);
                        if s.remote_queue.get(&key).is_some_and(VecDeque::is_empty) {
                            s.remote_queue.remove(&key);
                            s.remote_queue_warned.remove(&key);
                        }
                        item
                    } else {
                        None
                    };
                    (id, item_changed, touch, pending)
                };
                let mut w = world(lua)?;
                w.item_changed |= item_changed;
                w.conn_seq = id;
                if touch {
                    w.note_touch_interest(inst.0, "Touched");
                }
                w.note_listener(inst.0, false);
                if let Some(vals) = pending {
                    w.events.push((inst.0, name, vals, id, 0));
                }
                Ok(())
            })?;
        let rs_output = lua.create_function(
            |lua, (kind, msg, src, line): (String, String, Option<String>, Option<i64>)| {
                if kind == "module_error" {
                    let tb = src.unwrap_or_default();
                    let mut frames = Vec::new();
                    for raw in tb.lines() {
                        let t = raw.trim();
                        if t.starts_with("rustudio.") {
                            break;
                        }
                        if let Some(f) = parse_frame(t) {
                            frames.push(f);
                        }
                    }
                    report_frames(lua, &mlua::Error::RuntimeError(msg), frames);
                    return Ok(());
                }
                if kind == "warn_stack" {
                    let tb = src.unwrap_or_default();
                    let frames: Vec<_> = tb.lines().filter_map(parse_frame).collect();
                    let mut w = world(lua)?;
                    let loc = frames
                        .first()
                        .map(|(p, l, _)| format!("{p}:{l}"))
                        .unwrap_or_default();
                    let line = if loc.is_empty() { msg } else { format!("{msg}  —  {loc}") };
                    w.output.push((Level::Warn, line));
                    if !frames.is_empty() {
                        let mark = STACK_MARK;
                        w.output.push((Level::Info, format!("{mark}Stack Begin")));
                        for (path, ln, func) in &frames {
                            let text = match func {
                                Some(f) => format!("Script '{path}', Line {ln} - function {f}"),
                                None => format!("Script '{path}', Line {ln}"),
                            };
                            w.output.push((Level::Info, format!("{mark}{text}  —  {path}:{ln}")));
                        }
                        w.output.push((Level::Info, format!("{mark}Stack End")));
                    }
                    return Ok(());
                }
                let src = src.unwrap_or_default();
                let src = src.trim_start_matches(['=', '@']);
                let loc = match line {
                    Some(l) if l > 0 => format!("{src}:{l}"),
                    _ => src.to_string(),
                };
                let level = match kind.as_str() {
                    "warn" => Level::Warn,
                    "error" => Level::Error,
                    _ => Level::Info,
                };
                let line = if loc.is_empty() { msg } else { format!("{msg}  —  {loc}") };
                world(lua)?.output.push((level, line));
                Ok(())
            },
        )?;
        let rs_connect = lua.create_function(
            |lua, (inst, name, f, once, dead): (LInst, String, Function, bool, Option<bool>)| {
                let dead = dead.unwrap_or(false);
                let (id, pending) = {
                    let mut s = signals(lua)?;
                    s.next_id += 1;
                    let id = s.next_id;
                    let key = (inst.0, name.clone());
                    let list = s.conns.entry(key.clone()).or_default();
                    let first = list.is_empty();
                    let owner = script_of_function(&f);
                    list.push(Conn { id, func: f, owner, once, ident: security::current(lua) });
                    s.note_connection(&key, owner);
                    if first {
                        s.note_name(inst.0, &name);
                    }
                    let pending: Vec<Vec<Value>> = if !dead && is_remote_event_signal(&name) {
                        if once {
                            let item = s.remote_queue.get_mut(&key).and_then(VecDeque::pop_front);
                            if s.remote_queue.get(&key).is_some_and(VecDeque::is_empty) {
                                s.remote_queue.remove(&key);
                                s.remote_queue_warned.remove(&key);
                            }
                            item.into_iter().collect()
                        } else {
                            s.remote_queue_warned.remove(&key);
                            s.remote_queue
                                .remove(&key)
                                .map(|q| q.into_iter().collect())
                                .unwrap_or_default()
                        }
                    } else {
                        Vec::new()
                    };
                    (id, pending)
                };
                if let Some(mut w) = lua.app_data_mut::<World>() {
                    w.conn_seq = id;
                    w.item_changed |= name == "ItemChanged";
                    w.note_touch_interest(inst.0, &name);
                    w.note_listener(inst.0, true);
                    for vals in pending {
                        w.events.push((inst.0, name.clone(), vals, id, 0));
                    }
                }
                Ok(LConn {
                    key: (inst.0, name),
                    id,
                })
            },
        )?;
        let rs_load_module = lua.create_function(|lua, m: LInst| {
            let shared: Table = lua.named_registry_value("rustudio_shared")?;
            compile_script(lua, m.0, &shared)
        })?;
        let rs_index = lua.create_function(|lua, (this, key, atom): (LInst, mlua::LuaString, Option<i32>)| {
            let key = key.to_str()?;
            let key: &str = &key;
            crate::lua_bridge::site(lua, "index", key);
            rustudio_core::bench::api_timed("index", key, || {
                checked1(lua, index_entry(lua, this.0, key, atom.unwrap_or(-1)))
            })
            .inspect(|_| signal_timing::flush_immediate(lua))
        })?;
        let rs_newindex =
            lua.create_function(|lua, (this, key, value, atom): (LInst, mlua::LuaString, Value, Option<i32>)| {
                let key = key.to_str()?;
                let key: &str = &key;
                crate::lua_bridge::site(lua, "newindex", key);
                rustudio_core::bench::api_timed("newindex", key, || {
                    checked1(lua, newindex_entry(lua, this.0, key, value, atom.unwrap_or(-1)).map(|_| Value::Nil))
                })
                .inspect(|_| signal_timing::flush_immediate(lua))
            })?;
        lua.set_named_registry_value("rustudio_shared", self.shared.clone())?;
        let (rs_index_raw, rs_newindex_raw) = (rs_index.clone(), rs_newindex.clone());
        security::install_globals(lua)?;
        let (inst_index, inst_newindex, wrap): (Function, Function, Function) =
            lua.load(PRELUDE).set_name("=rustudio.prelude").call((
                rs_wait,
                rs_wait_signal,
                rs_output,
                rs_connect,
                rs_load_module,
                rs_index,
                rs_newindex,
            ))?;
        // Roblox's are C functions, and Adonis checks (crates/rustudio-engine/src/cmeta.rs); the
        // prelude's Lua shims stay for anything that calls them directly.
        let _ = (inst_index, inst_newindex);
        let _ = wrap;
        use crate::cmeta::INSTANCE;
        lua.set_named_registry_value(
            "rs_inst_index",
            crate::cmeta::wrap_meta_atom(lua, rs_index_raw, true, &INSTANCE, None)?,
        )?;
        lua.set_named_registry_value("rs_inst_newindex", crate::cmeta::wrap_meta_atom(lua, rs_newindex_raw, true, &INSTANCE, None)?)?;
        let rs_namecall = lua.create_function(|lua, mut args: MultiValue| {
            // The method name: a C string from lua_namecallatom, read now
            // (cmeta::namecall_fn), before anything else can run.
            let name = match args.pop_front() {
                Some(Value::LightUserData(p)) if !p.0.is_null() => {
                    unsafe { std::ffi::CStr::from_ptr(p.0 as *const std::ffi::c_char) }
                }
                _ => return checked(lua, Err(rt_err("attempt to call a nil value"))),
            };
            let atom = match args.pop_front() {
                Some(Value::Integer(a)) => a as i32,
                _ => -1,
            };
            let name = name.to_str().unwrap_or("");
            namecall(lua, name, atom, args)
        })?;
        lua.set_named_registry_value("rs_inst_namecall", crate::cmeta::namecall(lua, rs_namecall)?)?;
        lua.set_named_registry_value("rs_lua_methods", lua.globals().get::<Table>("__rs_lua_methods")?)?;
        lock_bridge(lua)?;
        // Studio's fast member reads (runtime_routes.rs), once the bridge
        // objects exist.
        if let Some((fast, data)) = routes::fast_index_fn(lua) {
            let f: Function = lua.named_registry_value("rs_inst_index")?;
            crate::cmeta::set_fast(lua, &f, fast, data)?;
        }
        if let Some((fast, data)) = routes::fast_newindex_fn(lua) {
            let f: Function = lua.named_registry_value("rs_inst_newindex")?;
            crate::cmeta::set_fast(lua, &f, fast, data)?;
        }
        if let Some((fast, data)) = routes::fast_namecall_fn(lua) {
            let f: Function = lua.named_registry_value("rs_inst_namecall")?;
            crate::cmeta::set_fast_call(lua, &f, fast, data)?;
        }

        // Engine services written in Lua (TweenService, Debris, DataStores…).
        let rs_create = lua.create_function(|lua, class: String| {
            let r = ext::create_any(lua, &class)?;
            inst_value(lua, r)
        })?;
        let rs_fire = lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let target = it
                .next()
                .and_then(|v| opt_inst(&v))
                .ok_or_else(|| rt_err("__rs_fire: Instance expected"))?;
            let name = match it.next() {
                Some(Value::String(s)) => s.to_str()?.to_string(),
                _ => return Err(rt_err("__rs_fire: event name expected")),
            };
            world(lua)?.fire(target, &name, it.collect());
            signal_timing::flush_immediate(lua);
            Ok(())
        })?;
        // A KeyframeSequence's poses and keyframes as library.luau's
        // parse_sequence reads them, straight from the tree: walking 84k
        // poses through the API took Place B's first spawn 1.4 s. Returns
        // (joints: name → {t, cf, style, dir} in tree order, frames:
        // {t, name, markers = {{name, value}}}, length); the caller sorts.
        g.set(
            "__rs_kfs_parts",
            lua.create_function(|lua, kfs: LInst| {
                let enum_name = |v: Option<Variant>, e: &str, dflt: &str| match v {
                    Some(Variant::Enum(x)) => LEnumItem::from_value(e, x.to_u32())
                        .map(|i| i.name)
                        .unwrap_or_else(|| dflt.to_string()),
                    _ => dflt.to_string(),
                };
                let num = |v: Option<Variant>, dflt: f64| match v {
                    Some(Variant::Float32(f)) => f as f64,
                    Some(Variant::Float64(f)) => f,
                    Some(Variant::Int32(i)) => i as f64,
                    _ => dflt,
                };
                struct PoseRow {
                    parent: String,
                    name: String,
                    t: f64,
                    cf: Variant,
                    style: String,
                    dir: String,
                }
                let (poses, keyframes) = {
                    let w = world(lua)?;
                    let d = w.d();
                    let mut poses: Vec<PoseRow> = Vec::new();
                    // (time, name, markers)
                    let mut keyframes: Vec<(f64, String, Vec<(String, String)>)> = Vec::new();
                    fn walk(
                        d: &Document,
                        parent: Ref,
                        parent_name: &str,
                        t: f64,
                        out: &mut Vec<PoseRow>,
                        row: &dyn Fn(&Document, Ref, &str, f64) -> Option<PoseRow>,
                    ) {
                        let Some(i) = d.dom.get_by_ref(parent) else { return };
                        for c in i.children() {
                            let Some(ci) = d.dom.get_by_ref(*c) else { continue };
                            if !doc::is_a(&ci.class, "Pose") {
                                continue;
                            }
                            if let Some(r) = row(d, *c, parent_name, t) {
                                out.push(r);
                            }
                            walk(d, *c, &ci.name, t, out, row);
                        }
                    }
                    let row = |d: &Document, p: Ref, parent: &str, t: f64| -> Option<PoseRow> {
                        if num(d.prop(p, "Weight"), 1.0) <= 0.0 {
                            return None;
                        }
                        Some(PoseRow {
                            parent: parent.into(),
                            name: d.dom.get_by_ref(p)?.name.clone(),
                            t,
                            cf: d
                                .prop(p, "CFrame")
                                .unwrap_or(Variant::CFrame(rbx_dom_weak::types::CFrame::identity())),
                            style: enum_name(d.prop(p, "EasingStyle"), "PoseEasingStyle", "Linear"),
                            dir: enum_name(d.prop(p, "EasingDirection"), "PoseEasingDirection", "In"),
                        })
                    };
                    if let Some(ki) = d.dom.get_by_ref(kfs.0) {
                        for k in ki.children() {
                            let Some(kk) = d.dom.get_by_ref(*k) else { continue };
                            if !doc::is_a(&kk.class, "Keyframe") {
                                continue;
                            }
                            let t = num(d.prop(*k, "Time"), 0.0);
                            walk(d, *k, "", t, &mut poses, &row);
                            let markers = kk
                                .children()
                                .iter()
                                .filter_map(|m| d.dom.get_by_ref(*m))
                                .filter(|m| doc::is_a(&m.class, "KeyframeMarker"))
                                .map(|m| {
                                    let v = match d.prop(m.referent(), "Value") {
                                        Some(Variant::String(s)) => s,
                                        _ => String::new(),
                                    };
                                    (m.name.clone(), v)
                                })
                                .collect();
                            keyframes.push((t, kk.name.clone(), markers));
                        }
                    }
                    (poses, keyframes)
                };
                let joints = lua.create_table()?;
                for p in poses {
                    let children: Table = match joints.raw_get::<Option<Table>>(p.parent.as_str())? {
                        Some(children) => children,
                        None => {
                            let children = lua.create_table()?;
                            joints.raw_set(p.parent, children.clone())?;
                            children
                        }
                    };
                    let list: Table = match children.raw_get::<Option<Table>>(p.name.as_str())? {
                        Some(l) => l,
                        None => {
                            let l = lua.create_table()?;
                            children.raw_set(p.name.as_str(), l.clone())?;
                            l
                        }
                    };
                    let e = lua.create_table()?;
                    e.raw_set("t", p.t)?;
                    e.raw_set("cf", variant_to_lua(lua, &p.cf, None)?)?;
                    e.raw_set("style", p.style)?;
                    e.raw_set("dir", p.dir)?;
                    list.raw_push(e)?;
                }
                let frames = lua.create_table()?;
                let mut length = 0.0f64;
                for (t, name, markers) in keyframes {
                    let ms = lua.create_table()?;
                    for (n, v) in markers {
                        let m = lua.create_table()?;
                        m.raw_push(n)?;
                        m.raw_push(v)?;
                        ms.raw_push(m)?;
                    }
                    let f = lua.create_table()?;
                    f.raw_set("t", t)?;
                    f.raw_set("name", name)?;
                    f.raw_set("markers", ms)?;
                    frames.raw_push(f)?;
                    length = length.max(t);
                }
                Ok((joints, frames, length))
            })?,
        )?;
        // A public asset's download state: true (on disk), false (downloading
        // in the background) or the reason it can't be had.
        g.set(
            "__rs_asset_ready",
            lua.create_function(|lua, id: f64| {
                Ok(match rustudio_core::assets::public_prefetch(id as u64) {
                    rustudio_core::assets::Fetch::Ready(_) => Value::Boolean(true),
                    rustudio_core::assets::Fetch::Pending => Value::Boolean(false),
                    rustudio_core::assets::Fetch::Unavailable(e) => Value::String(lua.create_string(&e)?),
                })
            })?,
        )?;
        // debug.profilebegin (+1) / profileend (-1) for the running thread;
        // false for an end with nothing open (library.luau warns).
        g.set(
            "__rs_profile",
            lua.create_function(|lua, (delta, label): (i32, Value)| {
                if crate::script_profiler::on() {
                    if delta > 0 {
                        let label = match &label {
                            Value::String(s) => s.to_string_lossy(),
                            _ => "(unnamed)".to_string(),
                        };
                        crate::script_profiler::marker_begin(&label);
                    } else {
                        crate::script_profiler::marker_end(lua);
                    }
                }
                let th = lua.current_thread().to_pointer() as usize;
                let Some(mut p) = lua.app_data_mut::<ProfileDepth>() else {
                    return Ok(true);
                };
                let d = p.0.get(&th).copied().unwrap_or(0);
                if delta < 0 && d == 0 {
                    return Ok(false);
                }
                // Kept only while open, so the map stays small.
                match (d as i32 + delta).max(0) as u32 {
                    0 => p.0.remove(&th),
                    n => p.0.insert(th, n),
                };
                Ok(true)
            })?,
        )?;
        // MarketplaceService:GetProductInfo's record (library.luau): the
        // JSON text, false while it's being fetched, or nil and the error.
        g.set(
            "__rs_product_info",
            lua.create_function(|lua, (id, kind): (f64, String)| {
                Ok(match rustudio_core::assets::product_info(id as u64, &kind) {
                    rustudio_core::assets::Fetch::Ready(b) => (
                        Value::String(lua.create_string(&b[..])?),
                        Value::Nil,
                    ),
                    rustudio_core::assets::Fetch::Pending => (Value::Boolean(false), Value::Nil),
                    rustudio_core::assets::Fetch::Unavailable(e) => {
                        (Value::Nil, Value::String(lua.create_string(&e)?))
                    }
                })
            })?,
        )?;
        // ParticleEmitter:Emit(n) / :Clear() (n −1) for the viewport's
        // particles (crates/rustudio-render/src/effects.rs), on this DataModel.
        g.set(
            "__rs_particles",
            lua.create_function(|lua, (inst, n): (LInst, f64)| {
                let Some(id) = world(lua)?.doc.as_ref().map(|d| d.id) else {
                    return Ok(());
                };
                if n < 0.0 {
                    rustudio_render::effects::clear(id, inst.0);
                } else {
                    rustudio_render::effects::emit(id, inst.0, n.min(100_000.0) as u32);
                }
                Ok(())
            })?,
        )?;
        // PhysicsService collision groups (library.luau), in the session's
        // shared table.
        g.set(
            "__rs_cg",
            lua.create_function(|lua, (op, a, b, on): (String, Option<String>, Option<String>, Option<bool>)| {
                let shared = world(lua)?.groups.clone();
                let mut cg = shared.lock().map_err(|_| rt_err("collision groups poisoned"))?;
                let a = a.unwrap_or_default();
                let b = b.unwrap_or_default();
                Ok(match op.as_str() {
                    "register" => {
                        cg.register(&a).map_err(rt_err)?;
                        Value::Number((cg.names().len() - 1) as f64)
                    }
                    "unregister" => {
                        rustudio_core::collision_groups::check_name(&a).map_err(rt_err)?;
                        cg.unregister(&a);
                        Value::Nil
                    }
                    "rename" => {
                        rustudio_core::collision_groups::check_name(&a).map_err(rt_err)?;
                        rustudio_core::collision_groups::check_name(&b).map_err(rt_err)?;
                        cg.rename(&a, &b);
                        Value::Nil
                    }
                    "set" => {
                        // Server only while the game runs.
                        if world(lua)?.side == Side::Client {
                            return Err(rt_err("This API can only be used on the server!"));
                        }
                        cg.set_collidable_checked(&a, &b, on.unwrap_or(true)).map_err(rt_err)?;
                        Value::Nil
                    }
                    "id" => match cg.id(&a) {
                        Some(i) => Value::Number(i as f64),
                        // GetCollisionGroupId.
                        None => return Err(rt_err("Collision group not found.")),
                    },
                    "name" => {
                        // GetCollisionGroupName: ids 0..31.
                        let id = a.parse::<i64>().unwrap_or(-1);
                        if !(0..rustudio_core::collision_groups::MAX_GROUPS as i64).contains(&id) {
                            return Err(rt_err(format!(
                                "The Collision group Id must be between 0 and {}",
                                rustudio_core::collision_groups::MAX_GROUPS
                            )));
                        }
                        let n = cg.names().get(id as usize).cloned().unwrap_or_default();
                        Value::String(lua.create_string(&n)?)
                    }
                    "collidable" => Value::Boolean(cg.collidable(&a, &b)),
                    "registered" => Value::Boolean(cg.is_registered(&a)),
                    "list" => {
                        let t = lua.create_table()?;
                        for (i, n) in cg.names().iter().enumerate() {
                            let e = lua.create_table()?;
                            e.set("name", n.as_str())?;
                            e.set("id", i)?;
                            e.set("mask", (cg.bits(n).1) as f64)?;
                            t.set(i + 1, e)?;
                        }
                        Value::Table(t)
                    }
                    _ => Value::Nil,
                })
            })?,
        )?;
        // A callback's function, for the engine's own invoke code (scripts
        // can't read callbacks: Studio's __index refuses, see `index`).
        g.set(
            "__rs_get_callback",
            lua.create_function(|lua, (inst, key): (LInst, String)| {
                Ok(cache(lua)?
                    .callbacks
                    .get(&(inst.0, key))
                    .map(|(f, _)| Value::Function(f.clone()))
                    .unwrap_or(Value::Nil))
            })?,
        )?;
        // Invokes a stored callback with the thread identity that assigned it,
        // returning (true, true, result) on success, (true, false, err) on error,
        // or (false) if no callback is assigned.
        g.set(
            "__rs_call_callback",
            lua.create_function(|lua, args: MultiValue| {
                let mut it = args.into_iter();
                let inst = it.next().and_then(|v| opt_inst(&v)).ok_or_else(|| rt_err("instance expected"))?;
                let key = match it.next() {
                    Some(Value::String(s)) => s.to_str()?.to_string(),
                    _ => return Err(rt_err("callback name expected")),
                };
                let entry = cache(lua)?.callbacks.get(&(inst, key)).cloned();
                let Some((f, ident)) = entry else {
                    return Ok(MultiValue::from_iter([Value::Boolean(false)]));
                };
                let call_args: MultiValue = it.collect();
                let prev = security::current(lua);
                security::set_current(lua, ident);
                let res = f.call::<Value>(call_args);
                security::set_current(lua, prev);
                match res {
                    Ok(v) => Ok(MultiValue::from_iter([Value::Boolean(true), Value::Boolean(true), v])),
                    Err(e) => Ok(MultiValue::from_iter([
                        Value::Boolean(true),
                        Value::Boolean(false),
                        Value::String(lua.create_string(error_message(&e))?),
                    ])),
                }
            })?,
        )?;
        // Context actions resume on a coroutine with the binder's identity.
        // The library captures this function before it is removed from globals.
        g.set(
            "__rs_call_action",
            lua.create_function(|lua, (ident, f, call_args): (u8, Function, MultiValue)| {
                let th = lua.create_thread(f)?;
                security::set_thread(&th, ident);
                match th.resume::<Value>(call_args) {
                    Ok(v) => Ok(MultiValue::from_iter([Value::Boolean(true), v])),
                    Err(e) => Ok(MultiValue::from_iter([
                        Value::Boolean(false),
                        Value::String(lua.create_string(error_message(&e))?),
                    ])),
                }
            })?,
        )?;
        g.set(
            "__rs_get_identity",
            lua.create_function(|lua, ()| Ok(security::current(lua) as i64))?,
        )?;
        g.set(
            "__rs_spawn_with_identity",
            lua.create_function(|lua, (name, args): (Value, MultiValue)| {
                let mut it = args.into_iter();
                let ident = match it.next() {
                    Some(Value::Integer(n)) => n as u8,
                    Some(Value::Number(n)) => n as u8,
                    _ => security::GAME,
                };
                let Some(Value::Function(f)) = it.next() else {
                    return Err(rt_err("__rs_spawn_with_identity: function expected"));
                };
                let call_args: Vec<Value> = it.collect();
                let th = lua.create_thread(f)?;
                security::set_thread(&th, ident);
                crate::script_profiler::with_entry(
                    crate::script_profiler::EntryKind::Event,
                    || {
                        let name = match name {
                            Value::String(s) => Some(format!("BindToRenderStep:{}", s.to_string_lossy())),
                            _ => Some("BindToRenderStep".to_owned()),
                        };
                        (None, name)
                    },
                    || resume(lua, &th, call_args),
                );
                Ok(())
            })?,
        )?;
        // Engine-owned values that scripts may only read (Humanoid.MoveDirection
        // from Humanoid:Move): set from the library, read-only or not.
        g.set(
            "__rs_set_raw",
            lua.create_function(|lua, (inst, key, value): (LInst, String, Value)| {
                let (class, _, _) = inst_meta(lua, inst.0)?;
                let Some((canon, dt, _)) = prop_desc(&class, &key) else {
                    return Err(rt_err(format!("{key} is not a valid member of {class}")));
                };
                let existing = world(lua)?.d().prop(inst.0, canon);
                let (ty, en) = match &dt {
                    DataType::Value(t) => (*t, None),
                    DataType::Enum(e) => (VariantType::Enum, Some(e.to_string())),
                    _ => return Err(rt_err(format!("Unable to assign property {key}"))),
                };
                let v = lua_to_variant(&key, &value, ty, en.as_deref(), existing.as_ref())?;
                store_prop(lua, inst.0, canon, v)
            })?,
        )?;
        lua.load(include_str!("library.luau"))
            .set_name("=rustudio.library")
            .call::<()>((rs_create, rs_fire))?;
        g.set("__rs_call_action", Value::Nil)?;
        // UserGameSettings' values, for its property hooks (domains/services).
        lua.set_named_registry_value("rs_user_game_settings", g.get::<Table>("__rs_user_game_settings")?)?;
        editable::install(lua)?;
        domains::install(lua)?;
        crate::dtmeta::install_all(lua)?;
        security::seal(lua)?;

        // Script environments read the public globals through `shared`
        // (crates/rustudio-engine/src/globals_parity.rs: rustudio's own stay out of reach).
        let _ = g;
        crate::globals_parity::install(lua, &self.shared)?;
        Ok(())
    }

    /// Hands the play document (inside its World) to Lua for this frame.
    pub fn enter(&self, world: World) {
        self.lua.set_app_data(world);
        domains::services::on_enter(&self.lua);
    }

    pub fn leave(&self) -> World {
        domains::services::on_leave(&self.lua);
        self.lua.remove_app_data::<World>().expect("world entered")
    }

    pub fn world_mut(&self) -> Option<mlua::AppDataRefMut<'_, World>> {
        self.lua.app_data_mut::<World>()
    }

    /// Defines `game` and `workspace` once the DataModel is in place.
    pub fn bind_globals(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        domains::data::bind_context(lua)?;
        let (root, ws) = {
            let w = world(lua)?;
            let d = w.d();
            (d.dom.root_ref(), d.workspace().unwrap_or_else(Ref::none))
        };
        domains::assets_cloud::scene::scan_access(lua,root)?;
        let g = lua.globals();
        g.set("game", inst_value(lua, root)?)?;
        g.set("Game", inst_value(lua, root)?)?;
        g.set("workspace", inst_value(lua, ws)?)?;
        g.set("Workspace", inst_value(lua, ws)?)?;
        // os.clock: Studio's counts from its own process start (2633 s into a
        // long session, measured; Studio clock, not the
        // machine's uptime that Luau's built-in gives on Linux). The fidelity
        // harness sets RUSTUDIO_CLOCK_BASE to Studio's reading at Play start
        // so time-driven game code (Place A's menu camera sway) runs in phase.
        let base = std::env::var("RUSTUDIO_CLOCK_BASE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok());
        let since = process_start();
        // (Phase taken once: Studio's reading at Play start + time() at the
        // first call; from there it runs on the wall clock like Studio's, so
        // it advances inside a frame. Frozen at the frame's time(), Place A's
        // time-sliced map loader never yielded and hit the script timeout.)
        let offset: std::cell::Cell<Option<f64>> = std::cell::Cell::new(None);
        // RUSTUDIO_CPU_CLOCK=1 (benchmarks only): the calling thread's CPU
        // time instead, so per-call costs can be timed on a loaded machine.
        let cpu_clock = std::env::var_os("RUSTUDIO_CPU_CLOCK").is_some();
        // RUSTUDIO_INSTR_CLOCK=1 (benchmarks only): the calling thread's
        // user-space instructions retired, in billions (a loop's time per
        // call in ns reads as instructions per call): stable under load
        // (checks/call-costs).
        let instr_clock = std::env::var_os("RUSTUDIO_INSTR_CLOCK").is_some();
        // RUSTUDIO_GAME_CLOCK=1 (tests only, with RUSTUDIO_FIXED_DT): the
        // game's time, so wall-clock probes are deterministic on a loaded machine.
        let game_clock = std::env::var_os("RUSTUDIO_GAME_CLOCK").is_some() || rustudio_core::sim_clock::on();
        let clock = lua.create_function(move |lua, ()| {
            if game_clock {
                // Each call costs GAME_CLOCK_CALL of virtual script time, so a script that
                // waits by spinning on os.clock (the probes' frame cap) ends its wait, and
                // the frame it spent goes into the next frame's step (`take_game_clock_spent`).
                let mut e = GAME_CLOCK_SPENT.lock().unwrap_or_else(|e| e.into_inner());
                *e += GAME_CLOCK_CALL;
                return Ok(world(lua).map(|w| w.time).unwrap_or(0.0) + 1000.0 + *e);
            }
            if instr_clock {
                return Ok(rustudio_core::bench::instructions() as f64 * 1e-9);
            }
            // (Unix only; elsewhere the wall clock below.)
            #[cfg(unix)]
            if cpu_clock {
                let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
                return Ok(ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9);
            }
            #[cfg(not(unix))]
            let _ = cpu_clock;
            let now = since.elapsed().as_secs_f64();
            Ok(match base {
                Some(b) => {
                    let off = offset.get().unwrap_or_else(|| {
                        let o = b + world(lua).map(|w| w.time).unwrap_or(0.0) - now;
                        offset.set(Some(o));
                        o
                    });
                    now + off
                }
                None => now,
            })
        })?;
        if let Ok(os) = g.get::<Table>("os") {
            os.set("clock", clock)?;
            // --fixed-dt: os.time() (no table) and os.date(fmt) (no time)
            // on the simulated clock.
            if rustudio_core::sim_clock::on() {
                let date: Function = os.get("date")?;
                os.set(
                    "date",
                    lua.create_function(move |_, (fmt, t): (Value, Option<f64>)| {
                        let t = t.unwrap_or_else(|| rustudio_core::sim_clock::epoch_secs().floor());
                        date.call::<mlua::MultiValue>((fmt, t))
                    })?,
                )?;
                let orig: Function = os.get("time")?;
                os.set(
                    "time",
                    lua.create_function(move |_, t: mlua::MultiValue| {
                        if t.is_empty() || matches!(t.front(), Some(Value::Nil)) {
                            Ok(Value::Number(rustudio_core::sim_clock::epoch_secs().floor()))
                        } else {
                            orig.call::<Value>(t)
                        }
                    })?,
                )?;
            }
        }
        crate::globals_parity::sync(lua)?;
        Ok(())
    }

    /// Takes the CoreScripts core_scripts::install put in this side's
    /// DataModel and starts the engine's entry CoreScript, StarterScript
    /// (core_scripts.rs), which adds the others.
    pub fn start_core_scripts(&self, installed: rustudio_core::core_scripts::Installed) -> Result<(), String> {
        if let Ok(mut w) = world(&self.lua) {
            w.core_scripts = installed;
        }
        match start_core_script(&self.lua, "StarterScript", Ref::none()) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err("the data model patch has no StarterScript".into()),
            Err(e) => Err(describe_error(&e)),
        }
    }

    /// Starts a script Play collected, if it is still somewhere it runs, as
    /// in Roblox: Play gathers scripts first and starts them one by one, and
    /// an earlier one can unparent or clone a later one's GUI in between
    /// (Place B' button scripts then ran with script.Parent nil).
    /// One left out stays unstarted, so moving it somewhere it runs starts it.
    pub fn start_collected(&self, r: Ref) {
        if world(&self.lua).is_ok_and(|w| w.runs_here(r)) {
            self.run_script(r);
        }
    }

    /// Starts a Script or LocalScript as a new coroutine.
    pub fn run_script(&self, r: Ref) {
        self.run_script_as(r, security::GAME)
    }

    /// `run_script` at identity `ident` (plugins run at security::PLUGIN).
    pub fn run_script_as(&self, r: Ref, ident: u8) {
        let lua = &self.lua;
        if let Ok(mut w) = world(lua) {
            if !w.started.insert(r) {
                return;
            }
        }
        // Like Studio, the classic free-model "Virus" scripts don't run.
        let flagged = world(lua).ok().and_then(|w| {
            let name = w.d().dom.get_by_ref(r)?.name.to_lowercase();
            name.contains("virus").then(|| w.full_name(r))
        });
        if let Some(full) = flagged {
            if let Ok(mut w) = world(lua) {
                w.output.push((
                    Level::Warn,
                    format!("Script {full} detected as malicious.  Script will not run."),
                ));
            }
            return;
        }
        let started = compile_script(lua, r, &self.shared)
            .and_then(|f| domains::assets_cloud::scene::create_thread(lua,f))
            .inspect(|th| security::set(th, ident));
        match started {
            Ok(th) if rustudio_core::bench::lua_prof_on() => rustudio_core::bench::lua_timed(
                th.to_pointer() as usize,
                || format!("(start) {}", world(lua).map(|w| w.full_name(r)).unwrap_or_default()),
                || resume(lua, &th, vec![]),
            ),
            Ok(th) => crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Start,
                || (Some("(main chunk)".into()), None),
                || resume(lua, &th, vec![]),
            ),
            Err(e) => report(lua, &e),
        }
    }

    /// Builds this side's world-query cache (rays, region queries) ahead of
    /// the first script query.
    pub fn warm_queries(&self) {
        ext::warm_queries(&self.lua);
    }

    pub fn fire(&self, r: Ref, name: &str, args: Vec<Value>) {
        if let Some(mut w) = self.lua.app_data_mut::<World>() {
            // A native frame step that follows this RunService event
            // (TweenService, Animator: domains::anim::hook_after).
            let hook = domains::anim::hook_after(w.side, name).map(|h| (h, args.first().cloned()));
            w.fire(r, name, args);
            if let Some((h, dt)) = hook {
                w.queue_hook(r, name, h, dt.unwrap_or(Value::Nil));
            }
        }
    }

    /// Fires a RunService event without its native frame step: Studio runs
    /// stepHumanoid between PreAnimation and the Animator (gameStep
    /// Studio), so Play queues the Animator itself (`queue_frame_hook`).
    pub fn fire_bare(&self, r: Ref, name: &str, args: Vec<Value>) {
        if let Some(mut w) = self.lua.app_data_mut::<World>() {
            w.fire(r, name, args);
        }
    }

    /// Queues the native frame step that follows `after` (the Animator after
    /// PreAnimation) at this point of the event queue.
    pub fn queue_frame_hook(&self, r: Ref, after: &str, dt: f64) {
        if let Some(mut w) = self.lua.app_data_mut::<World>() {
            if let Some(h) = domains::anim::hook_after(w.side, after) {
                w.queue_hook(r, after, h, Value::Number(dt));
            }
        }
    }

    pub fn inst(&self, r: Ref) -> Value {
        inst_value(&self.lua, r).unwrap_or(Value::Nil)
    }

    /// Re-anchors pending timers' wall-clock timestamps to now so synchronous
    /// Play bootstrap work (client CoreScripts) before frame 1 does not count
    /// as elapsed wall time against server `task.wait` / `task.delay` timers.
    pub fn reset_timer_walls(&self) {
        if let Ok(mut s) = sched(&self.lua) {
            let now = rustudio_core::sim_clock::now();
            for t in &mut s.timers {
                t.wall = now;
            }
        }
    }

    /// Advances time: resumes due waits and delays, then dispatches.
    pub fn step(&self, dt: f64) {
        let lua = &self.lua;
        let (now, realtime) = {
            let Some(mut w) = lua.app_data_mut::<World>() else {
                return;
            };
            w.time += dt;
            (w.time, w.realtime)
        };
        // RUSTUDIO_FIXED_DT Play (test runs): no wall clock in what resumes when.
        let fixed_step = !realtime && fixed_dt_env();
        if let Ok(mut s) = sched(lua) {
            s.step += 1;
        }
        native_drain(lua);
        crate::script_profiler::vm_frame(lua);
        {
            let _s = rustudio_core::bench::scope("rt_domains_step");
            let _ = domains::step(lua, dt);
        }
        // Frame signals (including the camera's RenderStepped callback) must
        // run before task.wait resumes at Heartbeat. Otherwise a script can
        // switch Custom back to Scriptable before the camera gets its frame.
        {
            let _s = rustudio_core::bench::scope("rt_dispatch1");
            self.dispatch();
        }
        let _st = rustudio_core::bench::scope("rt_timers");
        {
            let Ok(mut s) = sched(lua) else { return };
            let step = s.step;
            let wall_now = realtime.then(rustudio_core::sim_clock::now);
            let Sched { timers, ready, .. } = &mut *s;
            ready.clear();
            ready.extend(timers.extract_if(.., |t| {
                t.scheduled_step < step
                    && (t.at <= now + 1e-9
                        || wall_now.is_some_and(|wall| wall.saturating_duration_since(t.wall).as_secs_f64() >= t.at - t.started))
            }));
            // The legacy resumption point has its own budget. Run it before
            // the modern task queue, which still drains in this frame.
            ready.make_contiguous().sort_by(|a, b| b.legacy.cmp(&a.legacy).then_with(|| a.at.total_cmp(&b.at)));
        }
        let started = Instant::now();
        let budgeted = !fixed_step && !rustudio_core::sim_clock::on();
        while let Some(t) = sched(lua).ok().and_then(|mut s| s.ready.pop_front()) {
            if t.legacy && budgeted && started.elapsed() >= LEGACY_TIMER_BUDGET {
                // Keep the rest for the next frame. Modern tasks are
                // in the same ready list, so cancellation can still see them.
                if let Ok(mut s) = sched(lua) {
                    s.timers.push(t);
                    while s.ready.front().is_some_and(|t| t.legacy) {
                        let next = s.ready.pop_front().unwrap();
                        s.timers.push(next);
                    }
                }
                continue;
            }
            let args = t
                .args
                .unwrap_or_else(|| {
                    // (RUSTUDIO_FIXED_DT runs: game time only; the wall clock would make
                    // the result depend on the machine's load. --fixed-dt: its simulated clock.)
                    let real = if fixed_step { 0.0 } else { rustudio_core::sim_clock::since(t.wall).as_secs_f64() };
                    vec![Value::Number((now - t.started).max(real))]
                });
            crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Timer,
                || (None, None),
                || resume_thread_timed(lua, &t.thread, args),
            );
        }
        drop(_st);
        let _s2 = rustudio_core::bench::scope("rt_dispatch2");
        self.dispatch();
        drop(_s2);
        ext::flush_terrain(lua);
    }

    /// Frees unparented instances no script can reach any more, as Roblox's
    /// garbage collector does: an instance out of the DataModel goes away
    /// with its subtree once no Lua value of it or of any descendant is left
    /// (Destroy only unparents, locks Parent and drops the connections).
    /// Before this, everything ever made stayed in the DOM: the Death
    /// Machine's tracers, bullet holes and impact parts piled up for as long
    /// as Play ran (a live place, RSS past 24 GB). `pinned` keeps instances the
    /// play session still needs (replication pairs). Looks at no more than
    /// `budget` instances; returns the ones freed, after marking each in the
    /// document's journal so every reader drops it.
    pub fn collect(&self, pinned: &dyn Fn(Ref) -> bool, budget: usize) -> Vec<Ref> {
        let lua = &self.lua;
        let Some(mut c) = lua.app_data_mut::<Cache>() else {
            return Vec::new();
        };
        let Some(mut w) = lua.app_data_mut::<World>() else {
            return Vec::new();
        };
        if w.doc.is_none() {
            return Vec::new();
        }
        for r in c.fresh.drain(..) {
            if w.d().dom.get_by_ref(r).is_some_and(|i| i.parent().is_none()) {
                w.orphans.insert(r);
            }
        }
        // Many new unparented instances since the last pass: garbage is
        // being made, so the Luau collector is stepped at the fast rate
        // (`gc_step`). A trickle (a GUI script swapping a few labels) isn't.
        let arrived = w.orphans.len().saturating_sub(w.orphans_seen) >= mech::GC_BUSY_ORPHANS;
        if w.orphans.is_empty() {
            w.orphans_seen = 0;
            w.gc_busy = false;
            return Vec::new();
        }
        let root = w.d().dom.root_ref();
        let candidates: Vec<Ref> = w.orphans.iter().copied().collect();
        let mut looked = 0usize;
        let mut freed: Vec<Ref> = Vec::new();
        let mut subtree = Vec::new();
        for r in candidates {
            if looked >= budget {
                break;
            }
            let d = w.d();
            let Some(i) = d.dom.get_by_ref(r) else {
                w.orphans.remove(&r);
                continue;
            };
            if i.parent().is_some() || r == root {
                // Back in a tree (its new root is checked if that leaves).
                w.orphans.remove(&r);
                continue;
            }
            // Stops at the first instance something still holds: usually
            // the root itself (a template a script keeps unparented), so a
            // held subtree costs one lookup, not a walk of all of it. A
            // live Place A match keeps whole map pieces this way: walking them
            // every pass was 45-70 ms twice a second.
            let held = |x: Ref| {
                pinned(x)
                    || w.prop_ref_counts.get(&x).is_some_and(|&n| n > 0)
                    || c.slots.get(&x).is_some_and(|slot| {
                        // Slots contain only userdata or nil. Luau's
                        // truth test avoids rooting each checked userdata.
                        c.weak.as_ref().is_some_and(|t| t.raw_get::<bool>(*slot).unwrap_or(false))
                    })
            };
            looked += 1;
            if held(r) {
                continue;
            }
            // The same list also queues the next nodes in breadth-first
            // order. Held roots and most transient leaf instances need
            // no traversal or per-candidate allocation.
            subtree.clear();
            subtree.push(r);
            subtree.extend_from_slice(i.children());
            let mut next = 1;
            while let Some(&x) = subtree.get(next) {
                let Some(child) = d.dom.get_by_ref(x) else {
                    subtree.truncate(next);
                    break;
                };
                looked += 1;
                if held(x) {
                    break;
                }
                subtree.extend_from_slice(child.children());
                next += 1;
            }
            if next != subtree.len() {
                continue;
            }
            w.orphans.remove(&r);
            let dm = w.dm();
            for x in &subtree {
                dm.mark_tree(*x);
            }
            dm.dom.destroy(r);
            for x in &subtree {
                if let Some(slot) = c.slots.remove(x) {
                    if let Some(t) = c.weak.as_ref() {
                        let _ = t.raw_set(slot, Value::Nil);
                    }
                }
                w.destroyed.remove(x);
                w.velocities.remove(x);
                w.humanoid_states.remove(x);
                w.states_disabled.remove(x);
                w.avatar_descs.remove(x);
                w.listening.remove(x);
                w.forget_layout_listener(*x);
                w.abs_seen.remove(x);
                w.scroll_seen.remove(x);
                w.started.remove(x);
                w.orphans.remove(x);
                w.input.sound_pos.remove(x);
                if let Some(edges) = w.prop_ref_edges.remove(x) {
                    for (_, target) in edges {
                        release_prop_ref(&mut w.prop_ref_counts, target);
                    }
                }
                w.input.sound_len.remove(x);
                w.input.sound_playing.remove(x);
                w.input.sound_paused.remove(x);
                w.input.gui_layout.remove(x);
            }
            freed.extend_from_slice(&subtree);
        }
        w.orphans_seen = w.orphans.len();
        w.gc_busy = arrived || freed.len() >= mech::GC_BUSY_ORPHANS;
        if freed.is_empty() {
            return freed;
        }
        w.freed += freed.len() as u64;
        w.structure_changed = true;
        let gone: HashSet<Ref> = freed.iter().copied().collect();
        c.callbacks.retain(|(r, _), _| !gone.contains(r));
        drop(c);
        drop(w);
        if let Some(mut s) = lua.app_data_mut::<Signals>() {
            for &r in &freed {
                let Some(names) = s.conn_names.remove(&r) else { continue };
                for name in names {
                    let key = (r, name);
                    for conn in s.conns.remove(&key).into_iter().flatten() {
                        s.forget_connection(&key, conn.owner);
                    }
                }
            }
            s.waiters.retain(|(r, _), _| !gone.contains(r));
            s.remote_queue.retain(|(r, _), _| !gone.contains(r));
            s.remote_queue_warned.retain(|(r, _)| !gone.contains(r));
        }
        if let Some(mut ix) = lua.app_data_mut::<ChildIndex>() {
            for r in &freed {
                ix.by_parent.remove(r);
            }
        }
        freed
    }

    /// Steps the Luau collector by this frame's share of a cycle. Roblox's
    /// collector runs all the time; Luau's runs as scripts allocate, so a
    /// quiet script could keep dead values (and the unparented instances
    /// behind them, which `collect` frees) for long. While many unparented
    /// instances keep arriving or going (`World::gc_busy`), a cycle (and
    /// with it the weak cache's dead entries) takes GC_CYCLE_PASSES
    /// collector intervals whatever the heap's size; while the only ones
    /// watched are those scripts still hold (cached templates: nothing to
    /// free), GC_IDLE_CYCLE_S. The work is spread over the frames, and no
    /// frame does more than Studio's LuauGc job does in one
    /// (GC_FRAME_MAX_KB; a huge heap's cycle takes longer instead): one
    /// step of heap / 2 (up to 16 MB) per pass was a 10-14 ms frame twice a
    /// second in Place E, 16 ms (89 ms at worst) in a Place A match.
    /// Nothing to do while no unparented instance is watched.
    pub fn gc_step(&self, dt: f64) {
        let lua = &self.lua;
        let Some(busy) = lua
            .app_data_ref::<World>()
            .filter(|w| w.doc.is_some() && !w.orphans.is_empty())
            .map(|w| w.gc_busy)
        else {
            return;
        };
        let heap_kb = (lua.used_memory() / 1024) as f64;
        let kb = if busy {
            let pass_kb = (heap_kb / mech::GC_CYCLE_PASSES as f64)
                .clamp(mech::GC_STEP_MIN_KB as f64, mech::GC_STEP_MAX_KB as f64);
            pass_kb * (dt / mech::GC_INTERVAL).clamp(0.0, 1.0)
        } else {
            heap_kb * (dt / mech::GC_IDLE_CYCLE_S).clamp(0.0, 1.0)
        };
        let kb = kb.ceil().clamp(1.0, mech::GC_FRAME_MAX_KB);
        let _ = unsafe {
            lua.exec_raw::<()>((), |state| {
                mlua::ffi::lua_gc(state, mlua::ffi::LUA_GCSTEP, kb as std::ffi::c_int);
            })
        };
    }

    /// Instances with a slot in the Lua value cache (bounded by the ones
    /// alive: a freed instance gives its slot back).
    pub fn lua_slots(&self) -> usize {
        self.lua.app_data_ref::<Cache>().map(|c| c.slots.len()).unwrap_or(0)
    }

    /// An instance the engine itself took out of the tree (a fallen part):
    /// the collector frees it once scripts can't reach it.
    pub fn note_orphan(&self, r: Ref) {
        if let Some(mut c) = self.lua.app_data_mut::<Cache>() {
            c.fresh.push(r);
        }
    }

    /// Lua-side counts for RUSTUDIO_MEMSTATS: (DOM instances reachable from
    /// the root, unparented instances being watched, instances freed so far,
    /// Lua values cached, connections, waiters, timers, Lua heap KB).
    pub fn mem_stats(&self) -> String {
        let lua = &self.lua;
        let (tree, orphans, freed, destroyed) = lua
            .app_data_ref::<World>()
            .and_then(|w| {
                let d = w.doc.as_ref()?;
                Some((d.dom.descendants().count(), w.orphans.len(), w.freed, w.destroyed.len()))
            })
            .unwrap_or_default();
        let (slots, _, cbs) = lua
            .app_data_ref::<Cache>()
            .map(|c| (c.slots.len(), 0usize, c.callbacks.len()))
            .unwrap_or_default();
        let (conns, waiters) = lua
            .app_data_ref::<Signals>()
            .map(|s| (s.conns.values().map(|v| v.len()).sum::<usize>(), s.waiters.values().map(|v| v.len()).sum::<usize>()))
            .unwrap_or_default();
        let (timers, deferred) = lua
            .app_data_ref::<Sched>()
            .map(|s| (s.timers.len(), s.deferred.len()))
            .unwrap_or_default();
        format!(
            "tree={tree} orphans={orphans} freed={freed} destroyed={destroyed} luavals={slots} cbs={cbs} conns={conns} waiters={waiters} timers={timers} deferred={deferred} luakb={}",
            lua.used_memory() / 1024
        )
    }

    /// Runs deferred threads and queued signal fires until both are empty.
    pub fn dispatch(&self) {
        let lua = &self.lua;
        for _ in 0..mech::MAX_DISPATCH_ROUNDS {
            let starting: Vec<Ref> = lua
                .app_data_mut::<World>()
                .map(|mut w| std::mem::take(&mut w.to_start))
                .unwrap_or_default();
            {
                let _sc = rustudio_core::bench::scope("dis_start");
                for s in starting {
                    let still = world(lua).map(|w| w.runs_here(s)).unwrap_or(false);
                    if still {
                        self.run_script(s);
                    }
                }
            }
            let deferred: Vec<(Thread, Vec<Value>)> = sched(lua)
                .map(|mut s| s.deferred.drain(..).collect())
                .unwrap_or_default();
            // Fires made outside any script call (engine steps, input) that
            // the place runs immediately: run them first, at this point.
            {
                let _sc = rustudio_core::bench::scope("dis_immediate");
                signal_timing::flush_immediate(lua);
            }
            let events: Vec<(Ref, String, Vec<Value>, u64, u32)> = lua
                .app_data_mut::<World>()
                .map(|mut w| std::mem::take(&mut w.events))
                .unwrap_or_default();
            // Threads the immediate fires just deferred (a handler's
            // task.defer) run in this cycle too, as Roblox drains its
            // deferred queue before the invocation point ends.
            let more = || {
                sched(lua).is_ok_and(|s| !s.deferred.is_empty())
                    || lua.app_data_ref::<World>().is_some_and(|w| !w.to_start.is_empty())
            };
            if deferred.is_empty() && events.is_empty() && !more() {
                if let Ok(mut s) = signals(lua) {
                    for (r, (_, list)) in std::mem::take(&mut s.destroyed_ancestry) {
                        let key = (r, "AncestryChanged".to_string());
                        for conn in list {
                            s.forget_connection(&key, conn.owner);
                        }
                    }
                }
                return;
            }
            let _sc = rustudio_core::bench::scope("dis_deferred");
            for (th, args) in deferred {
                crate::script_profiler::with_entry(
                    crate::script_profiler::EntryKind::Deferred,
                    || (None, None),
                    || resume_thread_timed(lua, &th, args),
                );
            }
            drop(_sc);
            let _sc = rustudio_core::bench::scope("dis_events");
            for (r, name, args, seq, depth) in events {
                if name.starts_with('\0') {
                    domains::anim::run_hook(lua, &name, &args);
                    continue;
                }
                signal_timing::deliver_deferred(lua, r, name, args, seq, depth);
            }
        }
        if let Some(mut w) = lua.app_data_mut::<World>() {
            w.output.push((
                Level::Warn,
                "Too many chained signal fires in one frame; the rest run next frame".into(),
            ));
        }
    }
}

/// The Luau frames of a thread stopped by an error, innermost first:
/// (chunk name, line, function name), read with Luau's own
/// `debug.traceback(thread)` ("Name:3 function f" / "Name:6" / "[C] function
/// error"). C frames and rustudio's own chunks are left out, as Studio's
/// stack printer skips frames without a line.
fn thread_frames(lua: &Lua, th: &Thread) -> Vec<(String, u32, Option<String>)> {
    // debug.traceback(thread) uses this VM function with level zero for
    // a stopped thread. Read it directly so reporting an error never calls
    // a Lua global or converts the thread through a second Lua invocation.
    let _scope = rustudio_core::bench::scope("script_traceback");
    // SAFETY: exec_raw puts the supplied Thread on the protected call's
    // stack. The native formatter leaves its result above that argument.
    let tb = unsafe {
        lua.exec_raw::<String>(th.clone(), |state| {
            let thread = mlua::ffi::lua_tothread(state, 1);
            let level = i32::from(state == thread);
            mlua::ffi::luaL_traceback_(state, thread, std::ptr::null(), level);
            mlua::ffi::lua_remove(state, 1);
        })
    };
    tb.map(|s| s.lines().filter_map(parse_frame).collect()).unwrap_or_default()
}

fn parse_frame(l: &str) -> Option<(String, u32, Option<String>)> {
    let l = l.trim();
    let (loc, func) = match l.split_once(" function ") {
        Some((a, f)) => (a, Some(f.trim().to_string()).filter(|f| !f.is_empty())),
        None => (l, None),
    };
    let (path, line) = loc.rsplit_once(':')?;
    let line = line.trim().parse().ok()?;
    (!path.starts_with('[') && !path.starts_with("rustudio.")).then(|| (path.to_string(), line, func))
}

fn report(lua: &Lua, e: &mlua::Error) {
    report_frames(lua, e, Vec::new());
}

/// Output for a script error, as Studio prints it: the error,
/// then in blue "Stack Begin", one "Script '{}', Line {} - function {}" (or
/// "Script '{}', Line {}" when the frame has no function name) per Luau
/// frame, "Stack End". With no frames known, the error's own "Name:line".
fn report_frames(lua: &Lua, e: &mlua::Error, mut frames: Vec<(String, u32, Option<String>)>) {
    let _scope = rustudio_core::bench::scope("script_error_report");
    let msg = describe_error(e);
    if frames.is_empty() {
        if let Some((path, line)) = error_origin(&msg) {
            frames.push((path.to_string(), line, None));
        } else if let Some((path, line)) = callback_origin(e) {
            frames.push((path, line, None));
        }
    }
    let target = {
        let Some(mut w) = lua.app_data_mut::<World>() else { return };
        w.output.push((Level::Error, msg.clone()));
        if !frames.is_empty() {
            let mark = STACK_MARK;
            w.output.push((Level::Info, format!("{mark}Stack Begin")));
            for (path, line, func) in &frames {
                let text = match func {
                    Some(f) => format!("Script '{path}', Line {line} - function {f}"),
                    None => format!("Script '{path}', Line {line}"),
                };
                w.output.push((Level::Info, format!("{mark}{text}  —  {path}:{line}")));
            }
            w.output.push((Level::Info, format!("{mark}Stack End")));
        }
        // ScriptContext.Error(message, stackTrace, script): the script from
        // the message's "Full.Name:line:" prefix when it resolves.
        w.doc.as_ref().and_then(|d| {
            let root = d.dom.root_ref();
            let sc = d.find_child(root, "ScriptContext")?;
            if !w.listening.contains(&sc) {
                rustudio_core::telemetry::fired("Error");
                return None;
            }
            // An engine error has no prefix: the innermost frame's script.
            let path = match error_origin(&msg) {
                Some((p, _)) => p,
                None => frames.first().map(|f| f.0.as_str()).unwrap_or(""),
            };
            let mut cur = root;
            let mut found = !path.is_empty();
            for name in path.split('.') {
                match d.find_child(cur, name) {
                    Some(c) => cur = c,
                    None => {
                        found = false;
                        break;
                    }
                }
            }
            Some((sc, found.then_some(cur)))
        })
    };
    if let Some((sc, script)) = target {
        // The stack trace as Studio passes it: "Full.Name, line N\n" per
        // frame (" - function f" when named).
        let trace: String = frames
            .iter()
            .map(|(path, line, func)| match func {
                Some(f) => format!("{path}, line {line} - function {f}\n"),
                None => format!("{path}, line {line}\n"),
            })
            .collect();
        let args = (|| -> mlua::Result<Vec<Value>> {
            Ok(vec![
                Value::String(lua.create_string(&msg)?),
                Value::String(lua.create_string(&trace)?),
                match script {
                    Some(s) => inst_value(lua, s)?,
                    None => Value::Nil,
                },
            ])
        })();
        if let (Ok(args), Some(mut w)) = (args, lua.app_data_mut::<World>()) {
            w.fire(sc, "Error", args);
        }
    }
}

/// A bench key for a name (event, global).
fn name_key(name: &str) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    h.finish() as usize
}

/// "Chunk:line" where a function is defined (bench labels).
fn function_label(f: &Function) -> String {
    let i = f.info();
    format!(
        "{}:{}",
        i.short_src.or(i.source).unwrap_or_default().trim_start_matches('='),
        i.line_defined.unwrap_or(0)
    )
}

/// "Chunk:line" where a suspended thread is waiting (bench labels): the
/// first frame outside rustudio's own chunks (wait / task.wait).
fn thread_label(lua: &Lua, th: &Thread) -> String {
    let Ok(info) = lua
        .globals()
        .get::<Table>("debug")
        .and_then(|d| d.get::<Function>("info"))
    else {
        return "(resume) ?".to_string();
    };
    for level in 1..6 {
        match info.call::<(Option<String>, Option<i64>)>((th.clone(), level, "sl")) {
            Ok((Some(src), line))
                if !src.trim_start_matches('=').starts_with("rustudio.") && !src.starts_with('[') =>
            {
                return format!("(resume) {}:{}", src.trim_start_matches('='), line.unwrap_or(0));
            }
            Ok((Some(_), _)) => continue,
            _ => break,
        }
    }
    "(resume) ?".to_string()
}

/// `resume`, timed per thread while a bench runs.
fn resume_thread_timed(lua: &Lua, th: &Thread, args: Vec<Value>) {
    if rustudio_core::bench::lua_prof_on() && th.status() == ThreadStatus::Resumable {
        rustudio_core::bench::lua_timed(th.to_pointer() as usize, || thread_label(lua, th), || resume(lua, th, args))
    } else {
        resume(lua, th, args)
    }
}

/// Resumes a coroutine under the script timeout, routing errors to Output.
pub fn resume(lua: &Lua, th: &Thread, args: Vec<Value>) {
    if th.status() != ThreadStatus::Resumable {
        return;
    }
    let outermost = lua
        .app_data_ref::<Budget>()
        .is_some_and(|b| b.start.is_none());
    if outermost {
        if let Some(mut b) = lua.app_data_mut::<Budget>() {
            b.start = Some(Instant::now());
        }
    }
    let hitch = rustudio_core::bench::hitch_on().then(Instant::now);
    let label = hitch.map(|_| thread_label(lua, th));
    if hitch.is_some() && outermost {
        rustudio_core::bench::hitch_api_take();
    }
    if let Some(mut p) = lua.app_data_mut::<ProfileDepth>() {
        if !p.0.is_empty() {
            p.0.remove(&(th.to_pointer() as usize));
        }
    }
    let prof = crate::script_profiler::begin_resume(lua, th);
    // Engine-scheduled coroutines have no caller waiting for their results.
    let r = th.resume::<()>(MultiValue::from_vec(args));
    if let Some(p) = prof {
        crate::script_profiler::end_resume(p, th.status() != ThreadStatus::Resumable);
    }
    if let (Some(t0), Some(l)) = (hitch, label) {
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if ms > 10.0 {
            let after = thread_label(lua, th);
            let err = r.as_ref().err().map(|e| describe_error(e)).unwrap_or_default();
            eprintln!("HITCH resume {ms:.1} ms {l} / after {after} {err:.200}");
            if outermost && ms > 300.0 {
                for (k, t, n) in rustudio_core::bench::hitch_api_take().into_iter().take(12) {
                    eprintln!("HITCH   {t:.1} ms {n}x {k}");
                }
            }
        }
    }
    if outermost {
        if let Some(mut b) = lua.app_data_mut::<Budget>() {
            b.start = None;
        }
    }
    if let Err(e) = r {
        report_frames(lua, &e, thread_frames(lua, th));
    }
}

/// Releases one tracked Instance property reference.
fn release_prop_ref(counts: &mut HashMap<Ref, usize>, target: Ref) {
    if let std::collections::hash_map::Entry::Occupied(mut count) = counts.entry(target) {
        if *count.get() == 1 {
            count.remove();
        } else {
            *count.get_mut() = count.get().saturating_sub(1);
        }
    }
}

/// A place script's chunk name: `@` + its full name. Instance names are
/// place-controlled, so they must never land in the `=` namespace that
/// security::internal_chunk trusts as rustudio's own (a ModuleScript named
/// `rustudio.x` parented to nil). Luau shows `@` and `=` names alike
/// (short_src: errors, tracebacks, debug.info "s") up to LUA_IDSIZE; past it
/// `@` would keep the tail, so cut to the 255 bytes `=` shows.
pub(crate) fn place_chunk_name(full_name: &str) -> String {
    let mut end = full_name.len().min(255);
    while !full_name.is_char_boundary(end) {
        end -= 1;
    }
    format!("@{}", &full_name[..end])
}

/// Whether a source asks for native code (`--!native` among the hot
/// comments at its top; a rough look at the first lines, for the
/// `[startup]` summary only).
fn native_marked(source: &str) -> bool {
    let mut end = source.len().min(512);
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    source[..end]
        .lines().take_while(|l| l.trim_start().is_empty() || l.trim_start().starts_with("--")).any(|l| l.contains("--!native"))
}

/// The --!native scripts loaded without their native code yet: a big
/// game's 67 native modules (1.4 MB of Luau) went through the code
/// generator inside Play's start; they run interpreted for their first
/// frames instead and native_drain builds them a few milliseconds a
/// frame (Luau attaches the code to the protos, so the closures made at
/// the load take it up at their next call).
#[derive(Default)]
struct NativeQueue {
    on: bool,
    items: Vec<(Function, usize)>,
    built: usize,
    bytes: usize,
    took: std::time::Duration,
    frames: u32,
}

/// Native code for queued scripts, within a frame's budget (at least one
/// a frame): called at the start of every Runtime::step.
fn native_drain(lua: &Lua) {
    const BUDGET: std::time::Duration = std::time::Duration::from_millis(3);
    let Some(mut q) = lua.app_data_mut::<NativeQueue>() else { return };
    if q.items.is_empty() {
        return;
    }
    let t0 = std::time::Instant::now();
    q.frames += 1;
    while let Some((f, bytes)) = q.items.pop() {
        lua.compile_native(&f);
        q.built += 1;
        q.bytes += bytes;
        if t0.elapsed() >= BUDGET {
            break;
        }
    }
    q.took += t0.elapsed();
    if q.items.is_empty() {
        rustudio_core::load_trace::native_built(q.built, q.bytes, q.took, q.frames);
        q.built = 0;
        q.bytes = 0;
        q.took = std::time::Duration::ZERO;
        q.frames = 0;
    }
}

/// Compiles a script's Source with `script` bound in its own environment.
fn compile_script(lua: &Lua, r: Ref, shared: &Table) -> mlua::Result<Function> {
    let (source, name, core) = {
        let w = world(lua)?;
        let src = match w.d().prop(r, "Source") {
            Some(Variant::String(s)) => s,
            _ => String::new(),
        };
        // A name with a NUL in it (an Place B script) can't be a chunk
        // name; Studio runs the script all the same.
        (src, w.full_name(r).replace('\0', ""), w.core_scripts.modules.get(&r).cloned())
    };
    let memory_category=domains::assets_cloud::scene::begin(lua,r)?;
    let result=(|| {
    // `_G`, `shared`, `script`, locked, reading through to `shared` (the
    // public globals) and flagged safe for Luau's fast paths (fastcall
    // builtins, pairs/ipairs loops, pcall), as Roblox's sandboxed script
    // envs are; Luau clears it itself if the script calls getfenv/setfenv.
    let _ = shared;
    let env = crate::globals_parity::new_env(lua, Some(inst_value(lua, r)?))?;
    // A Script inside a loaded plugin's tree also gets `plugin`.
    domains::script_env(lua, r, &env)?;
    // A module from Studio's data model patch (CorePackages, RobloxGui's
    // Modules): Roblox's own bytecode.
    if let Some(b) = core {
        return lua.load(&b[..]).set_mode(mlua::chunk::ChunkMode::Binary)
            .set_name(place_chunk_name(&name))
            .set_environment(env)
            .into_function();
    }
    // Big sources compile once (the other side of Play, the next run):
    // bytecode_cache.rs.
    let t0 = std::time::Instant::now();
    let cached = crate::bytecode_cache::get(&source);
    let (bytes, was_cached, native) = (source.len(), cached.is_some(), native_marked(&source));
    // (NativeQueue: a --!native script loads interpreted and is built in
    // the frames after)
    let defer = native && lua.app_data_ref::<NativeQueue>().is_some_and(|q| q.on);
    if defer {
        lua.enable_jit(false);
    }
    let f = match cached {
        Some(b) => lua.load(&b[..]).set_mode(mlua::chunk::ChunkMode::Binary)
            .set_name(place_chunk_name(&name))
            .set_environment(env)
            .into_function(),
        None => lua.load(source)
            .set_name(place_chunk_name(&name))
            .set_environment(env)
            .into_function(),
    };
    if defer {
        lua.enable_jit(true);
        if let (Ok(f), Some(mut q)) = (&f, lua.app_data_mut::<NativeQueue>()) {
            q.items.push((f.clone(), bytes));
        }
    }
    // (the `[startup]` summary of a Play's compiling: load_trace.rs)
    rustudio_core::load_trace::note_compile(bytes, was_cached, native, t0.elapsed());
    f
    })();
    domains::assets_cloud::scene::restore(lua,memory_category)?;
    result
}

/// Starts the CoreScript `name` of Studio's data model patch
/// (`PatchRoot.CoreScripts`, core_scripts.rs) as a CoreScript instance under
/// `parent` (none: parentless, as the engine's own StarterScript), at the
/// CoreScript identity. Its chunk is `=` + its full name, the names Roblox
/// reports (`StarterScript`, `CoreGui.RobloxGui.CoreScripts/<Name>`) and
/// security::internal_chunk knows. None when the patch has no such script.
pub(crate) fn start_core_script(lua: &Lua, name: &str, parent: Ref) -> mlua::Result<Option<Ref>> {
    let (r, code, full) = {
        let mut w = world(lua)?;
        let Some(code) = w.core_scripts.scripts.get(name).cloned() else { return Ok(None) };
        let r = w.dm().dom.insert(parent, InstanceBuilder::new("CoreScript").with_name(name));
        if parent.is_none() {
            w.orphans.insert(r);
        } else {
            w.dm().mark_tree(r);
        }
        w.structure_changed = true;
        w.started.insert(r);
        let full = w.full_name(r);
        (r, code, full)
    };
    let env = crate::globals_parity::new_env(lua, Some(inst_value(lua, r)?))?;
    let f = lua.load(&code[..]).set_mode(mlua::chunk::ChunkMode::Binary).set_name(format!("={full}")).set_environment(env).into_function()?;
    let th = domains::assets_cloud::scene::create_thread(lua, f)?;
    security::set(&th, security::CORE);
    resume(lua, &th, vec![]);
    Ok(Some(r))
}

// ------------------------------------------------------- client ↔ server

/// Converts a Lua value for the other VM. Functions, coroutines and other
/// non-transferable values become nil, as with Roblox remotes.
pub fn export(v: &Value, depth: usize) -> Xfer {
    match v {
        Value::Nil => Xfer::Nil,
        Value::Boolean(b) => Xfer::Bool(*b),
        Value::Number(n) => Xfer::Num(*n),
        Value::Integer(i) => Xfer::Num(*i as f64),
        Value::String(s) => Xfer::Str(s.as_bytes().to_vec()),
        Value::Buffer(b) => Xfer::Buffer(b.to_vec()),
        Value::Vector(_) => Xfer::V3(crate::ltypes::v3_opt(v).map(|x| x.0).unwrap_or_default()),
        Value::Table(t) if depth < 32 => {
            let mut out = Vec::new();
            for (k, v) in t.pairs::<Value, Value>().flatten() {
                out.push((export(&k, depth + 1), export(&v, depth + 1)));
            }
            Xfer::Table(out)
        }
        Value::UserData(_) => {
            if let Some(r) = opt_inst(v) {
                Xfer::Inst(r)
            } else if let Some(x) = opt_ud::<LVector3>(v) {
                Xfer::V3(x.0)
            } else if let Some(x) = opt_ud::<LVector2>(v) {
                Xfer::V2(x.0)
            } else if let Some(x) = opt_ud::<LCFrame>(v) {
                Xfer::CF(x.0)
            } else if let Some(x) = opt_ud::<LColor3>(v) {
                Xfer::C3(x.0)
            } else if let Some(x) = opt_ud::<LBrickColor>(v) {
                Xfer::Brick(x.0)
            } else if let Some(x) = opt_ud::<LUDim>(v) {
                Xfer::UDim(x)
            } else if let Some(x) = opt_ud::<LUDim2>(v) {
                Xfer::UDim2(x)
            } else if let Some(x) = opt_ud::<LEnumItem>(v) {
                Xfer::EnumItem(x)
            } else {
                Xfer::Nil
            }
        }
        _ => Xfer::Nil,
    }
}

/// Rebuilds a transferred value in this VM; `map` turns the sender's
/// instance refs into ours (unknown instances arrive as nil).
pub fn import(lua: &Lua, x: &Xfer, map: &dyn Fn(Ref) -> Option<Ref>) -> mlua::Result<Value> {
    let ud = |u: AnyUserData| Value::UserData(u);
    Ok(match x {
        Xfer::Nil => Value::Nil,
        Xfer::Bool(b) => Value::Boolean(*b),
        Xfer::Num(n) => Value::Number(*n),
        Xfer::Str(s) => Value::String(lua.create_string(s)?),
        Xfer::Buffer(b) => Value::Buffer(lua.create_buffer(b)?),
        Xfer::Table(items) => {
            let t = lua.create_table()?;
            for (k, v) in items {
                let k = import(lua, k, map)?;
                if !k.is_nil() {
                    t.raw_set(k, import(lua, v, map)?)?;
                }
            }
            Value::Table(t)
        }
        Xfer::Inst(r) => match map(*r) {
            Some(m) => inst_value(lua, m)?,
            None => Value::Nil,
        },
        Xfer::V3(v) => LVector3(*v).into_value(),
        Xfer::V2(v) => ud(lua.create_userdata(LVector2(*v))?),
        Xfer::CF(c) => LCFrame(*c).into_lua(lua)?,
        Xfer::C3(c) => ud(lua.create_userdata(LColor3(*c))?),
        Xfer::Brick(b) => ud(lua.create_userdata(LBrickColor(*b))?),
        Xfer::UDim(u) => ud(lua.create_userdata(*u)?),
        Xfer::UDim2(u) => ud(lua.create_userdata(*u)?),
        Xfer::EnumItem(e) => ud(lua.create_userdata(e.clone())?),
    })
}

/// Coroutines waiting in InvokeServer, by request id.
#[derive(Default)]
struct Invokes {
    waiting: HashMap<u64, Thread>,
    next: u64,
}

const INVOKE_PRELUDE: &str = r##"
local yield, running = coroutine.yield, coroutine.running
function __rs_lua_methods.InvokeServer(self, ...)
    __rs_member_check(self, "RemoteFunction", "InvokeServer")
    if __rs_has_peer() then
        __rs_invoke_server(self, running(), ...)
        return yield()
    end
    local cb = __rs_get_callback(self, "OnServerInvoke")
    while not cb do
        task.wait()
        cb = __rs_get_callback(self, "OnServerInvoke")
    end
    return cb(game:GetService("Players").LocalPlayer, ...)
end
function __rs_lua_methods.InvokeClient(self, player, ...)
    __rs_member_check(self, "RemoteFunction", "InvokeClient")
    if typeof(player) ~= "Instance" or not player:IsA("Player") then
        error("Unable to cast value to Object", 2)
    end
    if not __rs_has_peer() then
        error("InvokeClient can only be called from the server", 2)
    end
    __rs_invoke_server(self, running(), ...)
    return yield()
end
-- Client side of InvokeClient: like Roblox, the call is held until
-- OnClientInvoke is set.
function __rs_serve_client_invoke(remote, id, ...)
    local cb = __rs_get_callback(remote, "OnClientInvoke")
    while not cb do
        task.wait()
        if remote.Parent == nil then return end
        cb = __rs_get_callback(remote, "OnClientInvoke")
    end
    local res = table.pack(pcall(cb, ...))
    if res[1] then
        __rs_invoke_reply(id, true, table.unpack(res, 2, res.n))
    else
        __rs_invoke_reply(id, false, tostring(res[2]))
    end
end
-- Server side of InvokeServer: held until OnServerInvoke is set, as Roblox
-- queues it (the client's call stays yielded meanwhile).
function __rs_serve_invoke(remote, player, id, ...)
    local cb = __rs_get_callback(remote, "OnServerInvoke")
    while not cb do
        task.wait()
        if remote.Parent == nil then return end
        cb = __rs_get_callback(remote, "OnServerInvoke")
    end
    local res = table.pack(pcall(cb, player, ...))
    if res[1] then
        __rs_invoke_reply(id, true, table.unpack(res, 2, res.n))
    else
        __rs_invoke_reply(id, false, tostring(res[2]))
    end
end
"##;

impl Runtime {
    /// Marks this runtime as one side of a two-DataModel Play session.
    pub fn install_side(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        lua.set_app_data(Invokes::default());
        let g = lua.globals();
        g.set(
            "__rs_has_peer",
            lua.create_function(|lua, ()| Ok(world(lua)?.has_peer))?,
        )?;
        g.set(
            "__rs_invoke_server",
            lua.create_function(|lua, args: MultiValue| {
                let mut it = args.into_iter();
                let remote = it
                    .next()
                    .and_then(|v| opt_inst(&v))
                    .ok_or_else(|| rt_err("InvokeServer: bad remote"))?;
                let th = match it.next() {
                    Some(Value::Thread(t)) => t,
                    _ => return Err(rt_err("InvokeServer must be called from a script thread")),
                };
                let payload: Vec<Xfer> = it.map(|v| export(&v, 0)).collect();
                let id = {
                    let mut inv = lua
                        .app_data_mut::<Invokes>()
                        .ok_or_else(|| rt_err("invokes missing"))?;
                    inv.next += 1;
                    let id = inv.next;
                    inv.waiting.insert(id, th);
                    id
                };
                world(lua)?.outbox.push(RemoteMsg {
                    inst: remote,
                    kind: RemoteKind::Invoke(id),
                    args: payload,
                });
                Ok(())
            })?,
        )?;
        g.set(
            "__rs_invoke_reply",
            lua.create_function(|lua, args: MultiValue| {
                let mut it = args.into_iter();
                let id = it.next().and_then(|v| num(&v)).unwrap_or(0.0) as u64;
                let ok = matches!(it.next(), Some(Value::Boolean(true)));
                let payload: Vec<Xfer> = it.map(|v| export(&v, 0)).collect();
                world(lua)?.outbox.push(RemoteMsg {
                    inst: Ref::none(),
                    kind: RemoteKind::Reply(id, ok),
                    args: payload,
                });
                Ok(())
            })?,
        )?;
        lua.load(INVOKE_PRELUDE).set_name("=rustudio.invoke").exec()
    }

    /// Fires `event` on a remote here, with arguments from the other side.
    pub fn deliver_event(
        &self,
        remote: Ref,
        event: &str,
        player: Option<Ref>,
        args: &[Xfer],
        map: &dyn Fn(Ref) -> Option<Ref>,
    ) {
        let lua = &self.lua;
        let mut vals = Vec::new();
        if let Some(p) = player {
            vals.push(self.inst(p));
        }
        for a in args {
            match import(lua, a, map) {
                Ok(v) => vals.push(v),
                Err(e) => report(lua, &e),
            }
        }
        // Roblox queues reliable RemoteEvent invocations arriving before any
        // script connects to OnClientEvent / OnServerEvent (up to 256 per
        // remote), flushing them FIFO when a listener attaches.
        // UnreliableRemoteEvent never queues.
        if is_remote_event_signal(event) {
            let is_reliable = lua
                .app_data_ref::<World>()
                .is_some_and(|w| w.d().dom.get_by_ref(remote).is_some_and(|i| i.class == "RemoteEvent"));
            if is_reliable {
                let key = (remote, event.to_string());
                let mut warn_overflow = false;
                let mut queued = false;
                if let Some(mut s) = lua.app_data_mut::<Signals>() {
                    let has_listener = s.conns.get(&key).is_some_and(|v| !v.is_empty())
                        || s.waiters.get(&key).is_some_and(|v| !v.is_empty());
                    let has_pending = s.remote_queue.get(&key).is_some_and(|q| !q.is_empty());
                    if !has_listener || has_pending {
                        let q = s.remote_queue.entry(key.clone()).or_default();
                        if q.len() < REMOTE_QUEUE_LIMIT {
                            q.push_back(std::mem::take(&mut vals));
                        } else if s.remote_queue_warned.insert(key) {
                            warn_overflow = true;
                        }
                        queued = true;
                    }
                }
                if warn_overflow {
                    if let Some(mut w) = lua.app_data_mut::<World>() {
                        let full = w.full_name(remote);
                        w.output.push((
                            Level::Warn,
                            format!(
                                "Remote event invocation queue exhausted for {full}; did you forget to implement {event}?"
                            ),
                        ));
                    }
                }
                if queued {
                    return;
                }
            }
        }
        self.fire(remote, event, vals);
    }

    /// Runs OnServerInvoke for a client's InvokeServer.
    pub fn serve_invoke(
        &self,
        remote: Ref,
        player: Ref,
        id: u64,
        args: &[Xfer],
        map: &dyn Fn(Ref) -> Option<Ref>,
    ) {
        let lua = &self.lua;
        let run = || -> mlua::Result<()> {
            let f: Function = lua.globals().get("__rs_serve_invoke")?;
            let th = domains::assets_cloud::scene::create_thread(lua,f)?;
            let mut vals = vec![
                self.inst(remote),
                self.inst(player),
                Value::Number(id as f64),
            ];
            for a in args {
                vals.push(import(lua, a, map)?);
            }
            crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Remote,
                || (None, None),
                || resume(lua, &th, vals),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report(lua, &e);
        }
    }

    /// Runs OnClientInvoke for the server's InvokeClient (the reply waits
    /// until a callback is set).
    pub fn serve_client_invoke(
        &self,
        remote: Ref,
        id: u64,
        args: &[Xfer],
        map: &dyn Fn(Ref) -> Option<Ref>,
    ) {
        let lua = &self.lua;
        let run = || -> mlua::Result<()> {
            let f: Function = lua.globals().get("__rs_serve_client_invoke")?;
            let th = domains::assets_cloud::scene::create_thread(lua,f)?;
            let mut vals = vec![self.inst(remote), Value::Number(id as f64)];
            for a in args {
                vals.push(import(lua, a, map)?);
            }
            crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Remote,
                || (None, None),
                || resume(lua, &th, vals),
            );
            Ok(())
        };
        if let Err(e) = run() {
            report(lua, &e);
        }
    }

    /// Resumes the InvokeServer / InvokeClient caller with the other side's answer.
    pub fn finish_invoke(
        &self,
        id: u64,
        ok: bool,
        args: &[Xfer],
        map: &dyn Fn(Ref) -> Option<Ref>,
    ) {
        let lua = &self.lua;
        let Some(th) = lua.app_data_mut::<Invokes>().and_then(|mut i| {
            let t = i.waiting.get(&id).cloned();
            i.waiting.retain(|k, _| *k != id);
            t
        }) else {
            return;
        };
        if ok {
            let vals: Vec<Value> = args
                .iter()
                .filter_map(|a| import(lua, a, map).ok())
                .collect();
            crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Remote,
                || (None, None),
                || resume(lua, &th, vals),
            );
        } else {
            let msg = match args.first() {
                Some(Xfer::Str(s)) => String::from_utf8_lossy(s).into_owned(),
                _ => "remote invoke failed".into(),
            };
            if th.status() == ThreadStatus::Resumable {
                if let Err(e) = th.resume_error::<MultiValue>(msg) {
                    report(lua, &e);
                }
            }
        }
    }

    /// Sets a property the way a script would (Humanoid.Health → Died etc.).
    /// Runs a library global (`__rs_…`) in a new thread, like task.spawn:
    /// it may yield; errors go to Output.
    pub fn spawn_global(&self, name: &str, args: Vec<Value>) {
        let Ok(f) = self.lua.globals().get::<Function>(name) else {
            return;
        };
        match domains::assets_cloud::scene::create_thread(&self.lua,f) {
            Ok(th) if rustudio_core::bench::lua_prof_on() => rustudio_core::bench::lua_timed(
                name_key(name),
                || format!("(rustudio) {name}"),
                || resume(&self.lua, &th, args),
            ),
            Ok(th) => resume(&self.lua, &th, args),
            Err(e) => report(&self.lua, &e),
        }
    }

    pub fn set(&self, r: Ref, key: &str, v: Value) {
        if let Err(e) = newindex(&self.lua, r, key, v) {
            report(&self.lua, &e);
        }
    }

    /// Replication: sets a property as the server did (fires Changed here).
    pub fn apply_prop(&self, r: Ref, name: &str, v: Variant) {
        // A Humanoid's Health from the server fires HealthChanged here, and
        // Died when it reaches 0 (Roblox fires both on the clients too).
        let health = (name == "Health")
            .then(|| {
                let w = world(&self.lua).ok()?;
                let d = w.d();
                if d.class_of(r).as_deref() != Some("Humanoid") {
                    return None;
                }
                let old = match d.prop(r, "Health") {
                    Some(Variant::Float32(h)) => h,
                    _ => 100.0,
                };
                match v {
                    Variant::Float32(h) => Some((old, h)),
                    _ => None,
                }
            })
            .flatten();
        let character = match (name, &v) {
            ("Character", Variant::Ref(c)) if c.is_some() => Some(*c),
            _ => None,
        };
        if let Err(e) = store_prop(&self.lua, r, name, v) {
            report(&self.lua, &e);
        }
        // The LocalPlayer's Character set: the LocalScripts already in that
        // model start now, as in Roblox (a server may stream the character
        // in before it sets the property); ones that arrive later start as
        // they arrive (set_parent).
        if let (Some(c), Ok(mut w)) = (character, world(&self.lua)) {
            if w.d().exists(c) {
                w.queue_scripts(c);
            }
        }
        if let (Some((old, new)), Ok(mut w)) = (health, world(&self.lua)) {
            if new != old {
                w.fire(r, "HealthChanged", vec![Value::Number(new as f64)]);
                if new <= 0.0 && old > 0.0 {
                    w.fire(r, "Died", vec![]);
                }
            }
        }
    }

    /// Replication: sets or clears one attribute as the server did, preserving
    /// every other attribute (including client-local ones).
    pub fn apply_attr(&self, r: Ref, name: &str, v: Option<Variant>) {
        if world(&self.lua).ok().is_none_or(|w| !w.d().exists(r)) {
            return;
        }
        if let Err(e) = set_attr(&self.lua, r, name, v) {
            report(&self.lua, &e);
        }
    }

    /// Replication: adds or removes one CollectionService tag as the server
    /// did, preserving every other tag (including client-local ones).
    pub fn apply_tag(&self, r: Ref, tag: &str, present: bool) {
        if world(&self.lua).ok().is_none_or(|w| !w.d().exists(r)) {
            return;
        }
        let res = if present {
            ext::add_tag(&self.lua, r, tag)
        } else {
            ext::remove_tag(&self.lua, r, tag)
        };
        if let Err(e) = res {
            report(&self.lua, &e);
        }
    }

    /// Replication: renames as the server did.
    pub fn apply_name(&self, r: Ref, name: &str) {
        let lua = &self.lua;
        let changed = world(lua).ok().and_then(|mut w| {
            let i = w.dm().dom.get_by_ref_mut(r)?;
            if i.name == name {
                return None;
            }
            i.name = name.to_string();
            w.dm().mark_prop(r, ustr("Name"));
            Some(())
        });
        if changed.is_some() {
            if let (Ok(s), Ok(mut w)) = (lua.create_string("Name"), world(lua)) {
                w.fire(r, "Changed", vec![Value::String(s)]);
                w.fire(r, "Changed:Name", vec![]);
            }
        }
    }

    /// Replication: reparents as the server did (fires ChildAdded etc.).
    pub fn apply_parent(&self, r: Ref, parent: Ref) {
        if let Err(e) = set_parent(&self.lua, r, parent) {
            report(&self.lua, &e);
        }
    }

    pub(crate) fn apply_parent_replicated(&self, r: Ref, parent: Ref) {
        if let Err(e) = set_parent_opts(&self.lua, r, parent, true, true, domains::world::part::EnterPose::Replicated) {
            report(&self.lua, &e);
        }
    }

    /// Detaches a batch of streamed leaf parts directly under Workspace.
    /// Their hierarchy signals still queue in replication order.
    pub(crate) fn stream_out_workspace_parts(&self, parts: &[Ref]) -> bool {
        let Some(mut world) = self.world_mut() else {
            return false;
        };
        let document = world.d();
        let Some(workspace) = document.workspace() else {
            return false;
        };
        // A primary part loss, a tool, or a script under a departing part can
        // require work between individual parent changes.
        if matches!(document.prop(workspace, "PrimaryPart"), Some(Variant::Ref(r)) if r.is_some())
            || parts.iter().any(|&r| {
                document.dom.get_by_ref(r).is_none_or(|instance| {
                    instance.parent() != workspace
                        || !instance.children().is_empty()
                        || !doc::is_a(&instance.class, "BasePart")
                })
            })
        {
            return false;
        }
        let departing: rustc_hash::FxHashSet<_> = parts.iter().copied().collect();
        world.dm().dom.detach_matching_children(workspace, |r| departing.contains(&r));
        for &r in parts {
            world.orphans.insert(r);
            world.record(RepOp::Parent(r));
            world.dm().mark_tree(r);
        }
        world.structure_changed = true;
        drop(world);

        for &r in parts {
            if let Err(e) = fire_parent_signals(&self.lua, r, workspace, Ref::none(), vec![r]) {
                report(&self.lua, &e);
            }
        }
        true
    }

    /// A property the editor (or undo, Team Create, a file sync) changed
    /// outside Lua, already in the document: Changed (a ValueBase's new
    /// value for Value) and its GetPropertyChangedSignal fire, as for a
    /// script's write (the plugin host's edit sync).
    pub fn notify_prop(&self, r: Ref, name: &str) {
        notify_prop_in(&self.lua, r, name)
    }

    /// An instance the editor moved, created (`old` none) or removed (`new`
    /// none) outside Lua, already done in the document: the hierarchy
    /// signals fire as for a script's Parent write. `descendants`: the
    /// moved subtree below `r`.
    pub fn notify_parent(&self, r: Ref, old: Ref, new: Ref, descendants: Vec<Ref>) {
        if let Ok(mut w) = world(&self.lua) {
            if new.is_none() {
                w.orphans.remove(&r);
            }
        }
        if let Err(e) = fire_parent_signals(&self.lua, r, old, new, descendants) {
            report(&self.lua, &e);
        }
    }

    /// Ends a running script as disabling it does: its connections go and
    /// its threads never resume (plugin unload).
    pub fn stop_script(&self, r: Ref) {
        stop_script(&self.lua, r);
    }

    /// Ends every thread and connection running code of these scripts
    /// (ModuleScripts too), wherever on its stack (the plugin host's unload).
    pub fn stop_code_of(&self, scripts: &HashSet<Ref>) {
        stop_code_of(&self.lua, scripts);
    }

    /// Reports an error to Output as a script error is (the plugin host).
    pub fn report_error(&self, e: &mlua::Error) {
        report(&self.lua, e);
    }

    /// Replication: the server destroyed `r` (Destroying fires here too,
    /// Parent locks, connections go), as Studio's client does on a
    /// DestroyInstance item.
    pub fn apply_destroy(&self, r: Ref) {
        if let Err(e) = destroy_inst(&self.lua, r) {
            report(&self.lua, &e);
        }
    }
}

/// Compile-only check for the script editor: (line, message) of the first
/// syntax error.
pub fn check_syntax(source: &str, name: &str) -> Option<(usize, String)> {
    luau_flags();
    let lua = Lua::new();
    let err = lua
        .load(source)
        .set_name(format!("={name}"))
        .into_function()
        .err()?;
    let msg = describe_error(&err);
    let line = msg
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(':'))
        .and_then(|rest| rest.split(':').next())
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(1);
    Some((line, msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Vec<(Level, String)> {
        run_in_place(Document::new_place(), src)
    }

    fn run_in_place(mut d: Document, src: &str) -> Vec<(Level, String)> {
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("T")
                .with_property("Source", src.to_string()),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..120 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output
    }

    #[test]
    fn camera_zoom_limits_clamp_the_property_being_set() {
        let mut doc = Document::new_place();
        let players = doc.find_child(doc.dom.root_ref(), "Players").unwrap();
        doc.dom.insert(players, InstanceBuilder::new("Player").with_name("TestPlayer"));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(doc, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            for _, instance in {game:GetService("StarterPlayer"), game.Players.TestPlayer} do
                instance.CameraMinZoomDistance = 1
                instance.CameraMaxZoomDistance = 50
                instance.CameraMinZoomDistance = 75
                assert(instance.CameraMinZoomDistance == 50 and instance.CameraMaxZoomDistance == 50)
                instance.CameraMaxZoomDistance = 25
                assert(instance.CameraMinZoomDistance == 50 and instance.CameraMaxZoomDistance == 50)
                instance.CameraMinZoomDistance = 10
                instance.CameraMaxZoomDistance = "25"
                assert(instance.CameraMinZoomDistance == 10 and instance.CameraMaxZoomDistance == 25)
            end
        "#).exec().unwrap();
    }

    #[test]
    fn input_actions_keep_gamepad_sticks_and_other_held_bindings() {
        let rt = Runtime::new().unwrap();
        let mut w = World::new(Document::new_place(), RunMode::Play);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            local ctx = Instance.new("InputContext", workspace)
            ctx.Name = "TestInputContext"
            local move = Instance.new("InputAction", ctx)
            move.Name = "Move"
            move.Type = Enum.InputActionType.Direction2D
            local stick = Instance.new("InputBinding", move)
            stick.KeyCode = Enum.KeyCode.Thumbstick1
            stick.Deadzone = NumberRange.new(0, 1)
            stick.ResponseCurve = 2
            local keyboard = Instance.new("InputBinding", move)
            keyboard.Up = Enum.KeyCode.W

            local jump = Instance.new("InputAction", ctx)
            jump.Name = "Jump"
            local button = Instance.new("InputBinding", jump)
            button.KeyCode = Enum.KeyCode.ButtonA
        "#).exec().unwrap();
        rt.step(1.0 / 60.0);
        rt.lua.load(r#"
            local move = workspace.TestInputContext.Move
            local jump = workspace.TestInputContext.Jump
            local presses, releases = 0, 0
            jump.Pressed:Connect(function() presses += 1 end)
            jump.Released:Connect(function() releases += 1 end)

            local function input(state, device, key, x)
                __rs_input_event(state, device, key, x or 0, 0, 0, 0, 0, false)
            end
            input("Change", "Gamepad1", "Thumbstick1", .5)
            assert(move:GetState() == Vector2.new(.25, 0), tostring(move:GetState()))
            __rs_input_frame_end(1/60)
            input("Begin", "Keyboard", "W")
            assert(move:GetState() == Vector2.new(.25, 1))
            input("Change", "Gamepad1", "Thumbstick1", 0)
            assert(move:GetState() == Vector2.new(0, 1))
            input("End", "Keyboard", "W")
            assert(move:GetState() == Vector2.zero)
            input("Begin", "Gamepad1", "ButtonA")
            input("Begin", "Gamepad2", "ButtonA")
            input("End", "Gamepad1", "ButtonA")
            assert(jump:GetState())
            input("End", "Gamepad2", "ButtonA")
            assert(not jump:GetState())
            task.defer(function() assert(presses == 1 and releases == 1) end)
        "#).exec().unwrap();
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn input_actions_follow_ui_button_presses() {
        let rt = Runtime::new().unwrap();
        let mut w = World::new(Document::new_place(), RunMode::Play);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            local gui = Instance.new("ScreenGui", workspace)
            gui.Name = "TestGui"
            local text = Instance.new("TextButton", gui)
            text.Name = "Text"
            local image = Instance.new("ImageButton", gui)
            image.Name = "Image"
            local ctx = Instance.new("InputContext", workspace)
            ctx.Name = "TestInputContext"
            local closed = Instance.new("InputContext", workspace)
            closed.Name = "Closed"
            closed.Enabled = false
            counts = {}
            local function action(parent, name, button, key)
                local a = Instance.new("InputAction", parent)
                a.Name = name
                local b = Instance.new("InputBinding", a)
                b.UIButton = button
                if key then b.KeyCode = key end
                counts[name] = { 0, 0 }
                a.Pressed:Connect(function() counts[name][1] += 1 end)
                a.Released:Connect(function() counts[name][2] += 1 end)
                return a
            end
            action(ctx, "Open", text, Enum.KeyCode.E)
            action(ctx, "Fire", image)
            action(ctx, "Off", text).Enabled = false
            action(closed, "Shut", text)
            action(ctx, "Jump", nil, Enum.KeyCode.Space)
            function state(name)
                return (workspace.TestInputContext:FindFirstChild(name) or workspace.Closed:FindFirstChild(name)):GetState()
            end
            function input(st, device, key)
                __rs_input_event(st, device, key, 0, 0, 0, 0, 0, device ~= "Keyboard")
            end
        "#).exec().unwrap();
        rt.step(1.0 / 60.0);
        let button = |name: &str| rt.lua.load(format!("return workspace.TestGui.{name}")).eval::<Value>().unwrap();
        let ui = |name: &str, down: bool| domains::gui_input::ui_button(&rt.lua, button(name), down);
        let check = |src: &str| rt.lua.load(format!("assert({src})")).exec().unwrap();

        ui("Text", true);
        check(r#"state("Open") and not state("Fire") and not state("Off") and not state("Shut") and not state("Jump")"#);
        // The key bound with it neither presses nor lets go again.
        check(r#"(function() input("Begin", "Keyboard", "E") input("End", "Keyboard", "E") return state("Open") end)()"#);
        ui("Text", false);
        check(r#"not state("Open")"#);
        // Held while the mouse is down; let go with no MouseButton1Up (the
        // button was hidden) when the left button comes up.
        ui("Image", true);
        check(r#"state("Fire") and not state("Open")"#);
        check(r#"(function() input("End", "MouseButton1", "Unknown") return not state("Fire") end)()"#);
        ui("Image", false);
        // A button destroyed mid-press lets its action go.
        ui("Image", true);
        check(r#"state("Fire")"#);
        rt.lua.load("workspace.TestGui.Image:Destroy()").exec().unwrap();
        rt.step(1.0 / 60.0);
        check(r#"not workspace.TestInputContext.Fire:GetState()"#);
        rt.step(1.0 / 60.0);
        check(r#"counts.Open[1] == 1 and counts.Open[2] == 1"#);
        check(r#"counts.Fire[1] == 2 and counts.Fire[2] == 2"#);
        check(r#"counts.Off[1] == 0 and counts.Shut[1] == 0 and counts.Jump[1] == 0"#);
        let out = rt.leave().output;
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn render_callbacks_run_before_waiting_tasks_resume() {
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(Document::new_place(), RunMode::Run));
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            local seen = false
            game:GetService("RunService").RenderStepped:Connect(function() seen = true end)
            task.spawn(function() task.wait(); assert(seen, "render callback ran after task.wait") end)
        "#).exec().unwrap();
        let rs = { let w = world(&rt.lua).unwrap(); w.d().find_child(w.d().dom.root_ref(), "RunService").unwrap() };
        rt.fire(rs, "RenderStepped", vec![Value::Number(1.0 / 60.0)]);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn timers_keep_legacy_cadence_and_can_cancel_another_due_task() {
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(Document::new_place(), RunMode::Run));
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            result = {}
            local function mark(name)
                table.insert(result, name)
            end
            local cancelled
            task.delay(0, function()
                task.cancel(cancelled)
                mark("task")
                task.wait()
                mark("again")
            end)
            cancelled = task.delay(0, function() mark("cancelled") end)
            spawn(function() mark("spawn") end)
            task.spawn(function()
                wait()
                mark("wait")
            end)
        "#).exec().unwrap();
        let state = || rt.lua.load("return table.concat(result, ',')").eval::<String>().unwrap();
        rt.step(1.0 / 60.0);
        assert_eq!(state(), "task");
        rt.step(1.0 / 60.0);
        assert_eq!(state(), "task,spawn,wait,again");
    }

    #[test]
    fn playing_an_unloaded_sound_keeps_its_play_request() {
        let probe = r#"
            local sound = Instance.new("Sound", workspace)
            local missingId = "rbxasset://sounds/rustudio-test-file-does-not-exist.ogg"
            local played, loaded = 0, 0
            sound.Played:Connect(function(id)
                played += 1
                assert(id == missingId)
                assert(sound.Playing == (not immediate))
                assert(not sound.IsLoaded and sound.TimeLength == 0)
            end)
            sound.Loaded:Connect(function() loaded += 1 end)
            sound.SoundId = missingId
            assert(not sound.Playing and not sound.IsLoaded and sound.TimeLength == 0)
            sound:Play()
            assert(sound.Playing and not sound.IsLoaded and sound.TimeLength == 0)
            assert(played == (if immediate then 1 else 0) and loaded == 0)
            task.wait()
            assert(sound.Playing and not sound.IsLoaded and sound.TimeLength == 0)
            assert(played == 1 and loaded == 0)
            task.wait(0.25)
            assert(sound.Playing and not sound.IsLoaded and sound.TimeLength == 0)
            assert(played == 1 and loaded == 0)
            print("sound request checked")
        "#;
        for (signal_behavior, immediate) in [(1, true), (2, false)] {
            let mut d = Document::new_place();
            let workspace = d.workspace().unwrap();
            d.dom.get_by_ref_mut(workspace).unwrap().properties.insert(
                ustr("SignalBehavior2"),
                Variant::Enum(rbx_dom_weak::types::Enum::from_u32(signal_behavior)),
            );
            let out = run_in_place(d, &format!("local immediate = {immediate}\n{probe}"));
            assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
            assert!(out.iter().any(|(_, msg)| msg.starts_with("sound request checked")), "{out:?}");
        }
    }

    #[test]
    fn controlled_textbox_observes_committed_typing_without_echoing_the_caret() {
        use signal_timing::SignalMode;

        for signal_mode in [SignalMode::Immediate, SignalMode::Deferred] {
            let rt = Runtime::new().unwrap();
            let mut w = World::new(Document::new_place(), RunMode::Play);
            w.side = Side::Client;
            w.signal_mode = signal_mode;
            rt.enter(w);
            rt.bind_globals().unwrap();
            rt.step(1.0 / 60.0);
            let r: Ref = opt_inst(&rt.lua.load(r#"
                local gui = Instance.new("ScreenGui", game:GetService("StarterGui"))
                box = Instance.new("TextBox", gui)
                box.ClearTextOnFocus = true
                box.TextEditable = true
                box.Text = "old"
                box:CaptureFocus()
                assert(box:IsFocused(), "CaptureFocus did not focus the TextBox")
                assert(box.Text == "" and box.CursorPosition == 1, "CaptureFocus did not clear Text")
                events = {}
                for _, prop in {"Text", "CursorPosition", "SelectionStart"} do
                    box:GetPropertyChangedSignal(prop):Connect(function()
                        table.insert(events, string.format("%s:%s:%d:%d",
                            prop, box.Text, box.CursorPosition, box.SelectionStart))
                    end)
                end
                box:GetPropertyChangedSignal("Text"):Connect(function()
                    -- A controlled component reads the edit and renders those props back.
                    local text, caret, selection = box.Text, box.CursorPosition, box.SelectionStart
                    box.Text = text
                    box.CursorPosition = caret
                    box.SelectionStart = selection
                end)
                return box
            "#).eval::<Value>().unwrap()).unwrap();
            rt.dispatch();
            crate::gui::share().tb_script_sel.retain(|(box_ref, ..)| *box_ref != r);

            domains::gui_input::apply_textbox_edit(&rt.lua, r, Some("a".into()), Some(2), None);
            rt.dispatch();
            rt.lua.load(r#"
                assert(table.concat(events, ",") == "CursorPosition:a:2:-1,Text:a:2:-1")
            "#).exec().unwrap();
            assert!(!crate::gui::share().tb_script_sel.iter().any(|(box_ref, ..)| *box_ref == r));

            rt.lua.load(r#"
                box:GetPropertyChangedSignal("Text"):Connect(function()
                    box.CursorPosition = 1
                end)
            "#).exec().unwrap();
            domains::gui_input::apply_textbox_edit(&rt.lua, r, Some("ab".into()), Some(3), None);
            rt.dispatch();
            rt.lua.load("assert(box.Text == 'ab' and box.CursorPosition == 1)").exec().unwrap();
            let mut gui = crate::gui::share();
            assert!(gui.tb_script_sel.contains(&(r, Some(1), None)));
            gui.focus = None;
            gui.focus_req.retain(|req| *req != Some(r));
            gui.tb_cursor.remove(&r);
            gui.tb_sel.remove(&r);
            gui.tb_script_sel.retain(|(box_ref, ..)| *box_ref != r);
        }
    }

    #[test]
    fn same_brightness_leaves_the_journal_and_signals_untouched() {
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(Document::new_place(), RunMode::Run));
        rt.bind_globals().unwrap();
        let exec = |source| rt.lua.load(source).exec().unwrap();
        let journal_end = || world(&rt.lua).unwrap().d().journal_end();
        exec(r#"
            light = Instance.new("PointLight", workspace)
            changes = 0
            light.Changed:Connect(function(name)
                if name == "Brightness" then changes += 1 end
            end)
        "#);
        let before = journal_end();
        exec("light.Brightness = light.Brightness");
        rt.dispatch();
        exec("assert(changes == 0)");
        assert_eq!(before, journal_end());
        exec("light.Brightness = light.Brightness + 0.25");
        rt.dispatch();
        exec("assert(changes == 1)");
        assert_eq!(before + 1, journal_end());
        exec("light.Brightness = light.Brightness");
        rt.dispatch();
        exec("assert(changes == 1)");
        assert_eq!(before + 1, journal_end());
    }

    #[test]
    fn children_arrays_are_independent_and_keep_instance_identity() {
        let out = run(r#"
            local folder = Instance.new("Folder")
            local empty = folder:GetChildren()
            empty[1] = folder
            assert(#folder:GetChildren() == 0)
            local child = Instance.new("Part", folder)
            assert(folder:GetChildren()[1] == child and empty[1] == folder)
            local singleton = folder:GetChildren()
            singleton[1] = nil
            assert(folder:GetChildren()[1] == child and #singleton == 0)
            local other = Instance.new("Model", folder)
            local children = folder:GetChildren()
            assert(#children == 2 and children[1] == child and children[2] == other)
            assert(child:IsA("BasePart") and not other:IsA("BasePart"))
            assert(not child:IsA("a class that doesn't exist"))
        "#);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn native_attribute_reads_keep_luau_types() {
        let out = run(r#"
            local light = Instance.new("PointLight")
            light:SetAttribute("BaseBrightness", 0.625)
            light:SetAttribute("Enabled", false)
            local seed = Vector3.new(1, 2, 3)
            light:SetAttribute("FlickerSeed", seed)
            assert(light:GetAttribute("BaseBrightness") == 0.625)
            assert(light:GetAttribute("Enabled") == false)
            assert(light:GetAttribute("FlickerSeed") == seed)
            light:SetAttribute("BaseBrightness", nil)
            assert(light:GetAttribute("BaseBrightness") == nil)
        "#);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn value_changed_delivers_typed_value_and_property_signal_has_no_arguments() {
        let out = run(r##"
            local n = Instance.new("NumberValue")
            n.Changed:Connect(function(v) assert(type(v) == "number"); print("value", 1-v) end)
            n:GetPropertyChangedSignal("Value"):Connect(function(...) assert(select("#", ...) == 0); print("property") end)
            n.Value = .25
        "##);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
        assert!(out.iter().any(|(_, s)| s.starts_with("value 0.75 ")), "{out:?}");
        assert!(out.iter().any(|(_, s)| s.starts_with("property ")), "{out:?}");
    }

    #[test]
    fn basics() {
        let out = run("local p = Instance.new(\"Part\")\n\
            p.Name = \"X\"\n\
            p.Size = Vector3.new(2, 3, 4)\n\
            p.Parent = workspace\n\
            local hits = 0\n\
            p.Changed:Connect(function(prop) hits += 1 end)\n\
            p.Transparency = 0.5\n\
            print(workspace:FindFirstChild(\"X\") == p, p.Size.Y, typeof(p), p:IsA(\"BasePart\"))\n\
            local t = task.wait(0.5)\n\
            print(\"waited\", t >= 0.49, hits)\n\
            print(tostring(CFrame.new(1, 2, 3) * Vector3.new(1, 0, 0)), Enum.Material.Neon.Value)\n\
            local ok, err = pcall(function() return p.Nope end)\n\
            print(ok, err)\n\
            error(\"boom\")\n");
        let text: Vec<&str> = out.iter().map(|(_, s)| s.as_str()).collect();
        assert!(text[0].starts_with("true 3 Instance true"), "{text:?}");
        assert!(text[0].contains("ServerScriptService.T:8"), "{text:?}");
        assert!(text[1].starts_with("waited true 1"), "{text:?}");
        assert!(text[2].starts_with("2, 2, 3 288"), "{text:?}");
        assert!(
            text[3].contains("Nope is not a valid member of Part"),
            "{text:?}"
        );
        assert!(
            out[4].0 == Level::Error && out[4].1.contains("ServerScriptService.T:14: boom"),
            "{out:?}"
        );
    }

    /// Model pivots as a Studio gave them (2026-11-15, the same
    /// script in execute_luau): a PrimaryPart destroyed or parented out of
    /// its model is cleared at once and the pivot stays where the part's
    /// was; PivotTo then moves the WorldPivot with the parts.
    #[test]
    fn pivot_follows_pivot_to_and_lost_primary_part() {
        let out = run("local m = Instance.new(\"Model\")\n\
            local c = Instance.new(\"Part\") c.Name = \"Center\" c.Anchored = true c.Position = Vector3.new(0, 900, 0) c.Parent = m\n\
            local a = Instance.new(\"Part\") a.Anchored = true a.Position = Vector3.new(0, 900, 60) a.Parent = m\n\
            m.PrimaryPart = c m.Parent = workspace\n\
            local k = m:Clone() k.Parent = workspace\n\
            k.Center:Destroy()\n\
            print(\"A\", k.PrimaryPart, k:GetPivot().Position)\n\
            k:PivotTo(CFrame.new(0, 400, 300))\n\
            k:PivotTo(CFrame.new(10, 400, 300))\n\
            print(\"B\", k:GetPivot().Position, k.Part.Position)\n\
            local k2 = m:Clone() k2.Parent = workspace\n\
            k2.Center.Parent = workspace\n\
            print(\"C\", k2.PrimaryPart, k2:GetPivot().Position)\n\
            local k3 = m:Clone() k3.Center:Destroy()\n\
            print(\"D\", k3.PrimaryPart)\n");
        let text: Vec<&str> = out.iter().map(|(_, s)| s.as_str()).collect();
        assert!(text[0].starts_with("A nil 0, 900, 0"), "{text:?}");
        assert!(text[1].starts_with("B 10, 400, 300 10, 400, 360"), "{text:?}");
        assert!(text[2].starts_with("C nil 0, 900, 0"), "{text:?}");
        assert!(text[3].starts_with("D nil"), "{text:?}");
    }

    #[test]
    fn legacy_primary_part_methods_use_the_part_cframe() {
        let out = run(r#"
            local model = Instance.new("Model", workspace)
            local root = Instance.new("Part", model)
            local sibling = Instance.new("Part", model)
            local spawn = CFrame.new(-305.21, 182.035, 222.9)
            local ok, err = pcall(model.GetPrimaryPartCFrame, model)
            assert(not ok and string.find(err, "no PrimaryPart has been set", 1, true))
            ok, err = pcall(model.SetPrimaryPartCFrame, model, spawn)
            assert(not ok and string.find(err, "no PrimaryPart has been set", 1, true))

            root.CFrame = CFrame.new(305.367, 122.1, 99.9) * CFrame.Angles(0.45, 0.584, -0.83)
            local offset = Vector3.new(0, 3, 2)
            sibling.CFrame = root.CFrame * CFrame.new(offset)
            root.PivotOffset = CFrame.new(5, 2, -1)
            model.PrimaryPart = root
            model:SetPrimaryPartCFrame(spawn)
            assert(root.CFrame == spawn, "primary did not land at the spawn CFrame")
            assert(model:GetPrimaryPartCFrame() == root.CFrame)
            assert((sibling.Position - spawn:PointToWorldSpace(offset)).Magnitude < 0.001)
            assert(model:GetPivot() == spawn * root.PivotOffset)
            print("legacy primary part frame")
        "#);
        assert!(
            out.iter().any(|(level, line)| *level == Level::Info && line.starts_with("legacy primary part frame")),
            "{out:?}"
        );
    }

    /// An error lists its Luau frames the way Studio's Output does.
    #[test]
    fn error_stack_lines() {
        let out = run("local function inner()\n\terror(\"deep\")\nend\nlocal function outer()\n\tinner()\nend\nouter()\n");
        let text: Vec<String> = out.iter().map(|(_, s)| s.replace(STACK_MARK, "")).collect();
        let at = text.iter().position(|t| t == "Stack Begin").expect("stack");
        assert_eq!(
            &text[at..at + 5],
            &[
                "Stack Begin".to_string(),
                "Script 'ServerScriptService.T', Line 2 - function inner  —  ServerScriptService.T:2".to_string(),
                "Script 'ServerScriptService.T', Line 5 - function outer  —  ServerScriptService.T:5".to_string(),
                "Script 'ServerScriptService.T', Line 7  —  ServerScriptService.T:7".to_string(),
                "Stack End".to_string(),
            ],
            "{text:?}"
        );
    }

    #[test]
    fn modules_and_signals() {
        let mut d = Document::new_place();
        let rs = d.find_child(d.dom.root_ref(), "ReplicatedStorage").unwrap();
        d.dom.insert(
            rs,
            InstanceBuilder::new("ModuleScript")
                .with_name("Lib")
                .with_property(
                    "Source",
                    "return { add = function(a, b) return a + b end }".to_string(),
                ),
        );
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property(
                "Source",
                "local Lib = require(game:GetService('ReplicatedStorage'):WaitForChild('Lib'))\n\
                 local ev = Instance.new('BindableEvent')\n\
                 task.delay(0.1, function() ev:Fire('hi', 7) end)\n\
                 local a, b = ev.Event:Wait()\n\
                 print(Lib.add(2, 3), a, b)"
                    .to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..30 {
            rt.step(1.0 / 60.0);
        }
        let out = rt.leave().output;
        assert!(out.iter().any(|(_, s)| s.starts_with("5 hi 7")), "{out:?}");
    }

    #[test]
    fn timeout() {
        let out = run("while true do end");
        assert!(
            out.iter().any(|(_, s)| s.contains("Script timeout")),
            "{out:?}"
        );
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;

    fn run(src: &str) -> Vec<(Level, String)> {
        run_in(Document::new_place(), src)
    }

    /// `run` with a Player in Players (scripts can't create one).
    fn run_with_player(src: &str) -> Vec<(Level, String)> {
        let mut d = Document::new_place();
        let root = d.dom.root_ref();
        let players = match d.find_child(root, "Players") {
            Some(p) => p,
            None => d.dom.insert(root, InstanceBuilder::new("Players").with_name("Players")),
        };
        d.dom.insert(players, InstanceBuilder::new("Player").with_name("Player"));
        run_in(d, src)
    }

    fn run_in(mut d: Document, src: &str) -> Vec<(Level, String)> {
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("T")
                .with_property("Source", src.to_string()),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..120 {
            // Tweens advance on Heartbeat, as in a play session.
            let rs = rt
                .world_mut()
                .and_then(|w| w.d().find_child(w.d().dom.root_ref(), "RunService"));
            if let Some(rs) = rs {
                rt.fire(rs, "Heartbeat", vec![Value::Number(1.0 / 60.0)]);
            }
            rt.step(1.0 / 60.0);
        }
        rt.leave().output
    }

    /// Round 12's members, against Roblox's documented results.
    #[test]
    fn round12_api_members() {
        let out = run("local r = workspace:Spherecast(Vector3.new(50, 20, 50), 2, Vector3.new(0, -50, 0))\n\
            print('sphere', r.Instance.Name, math.floor(r.Position.Y * 100 + 0.5) / 100, r.Normal.Y, math.floor(r.Distance * 100 + 0.5) / 100)\n\
            local b = workspace:Blockcast(CFrame.new(50, 20, 50), Vector3.new(4, 4, 4), Vector3.new(0, -50, 0))\n\
            print('block', b.Instance.Name, math.floor(b.Distance * 100 + 0.5) / 100)\n\
            local p = Instance.new('Part'); p.Anchored = true; p.Size = Vector3.new(4, 4, 4); p.Position = Vector3.new(-50, 2, 0); p.Parent = workspace\n\
            local c = p:GetClosestPointOnSurface(Vector3.new(-40, 3, 0))\n\
            print('closest', c.X, c.Y, c.Z)\n\
            local m = Instance.new('Model'); p.Parent = m; m.Parent = workspace\n\
            print('pivot', m.WorldPivot.Position.X)\n\
            local f = Instance.new('Frame')\n\
            print('tween', f:TweenSize(UDim2.new(0, 100, 0, 50), 'Out', 'Quad', 0.1), f:TweenSize(UDim2.new(0, 10, 0, 5), 'Out', 'Quad', 0.1, false))\n\
            local r15h = Instance.new('Humanoid'); local ra = Instance.new('Part'); ra.Name = 'RightHand'; local ut = Instance.new('Part'); ut.Name = 'UpperTorso'\n\
            print('limb', r15h:GetLimb(Instance.new('Part')).Name, r15h:GetLimb(ra).Name, r15h:GetLimb(ut).Name)\n\
            print('groups', workspace:GetMaxCollisionGroups(), game:GetService('PhysicsService'):GetCollisionGroupName(0))\n\
            print('sun', math.floor(game.Lighting:GetSunDirection().Magnitude * 100 + 0.5) / 100)\n\
            print('job', game.JobId == '', game.PlaceId)\n\
            local keep = Instance.new('Folder'); local drop = Instance.new('Part', keep); drop.Archivable = false; Instance.new('Part', keep)\n\
            print('archivable', #keep:Clone():GetChildren())\n\
            print('limits', select(2, pcall(workspace.Spherecast, workspace, Vector3.zero, 300, Vector3.new(0, -1, 0))))\n\
            print('limits', select(2, pcall(workspace.Spherecast, workspace, Vector3.zero, 1, Vector3.new(0, -2000, 0))))\n\
            print('limits', select(2, pcall(workspace.Spherecast, workspace, Vector3.zero, -1, Vector3.new(0, -1, 0))))\n");
        let text: Vec<&str> = out.iter().map(|(_, t)| t.as_str()).collect();
        let has = |s: &str| text.iter().any(|t| t.starts_with(s));
        assert!(has("sphere Baseplate 0 1 18"), "{text:?}");
        assert!(has("block Baseplate 18"), "{text:?}");
        assert!(has("closest -48 3 0"), "{text:?}");
        assert!(has("pivot -50"), "{text:?}");
        assert!(has("tween true false"), "{text:?}");
        assert!(has("limb Unknown RightArm Torso"), "{text:?}");
        assert!(has("groups 32 Default"), "{text:?}");
        assert!(has("sun 1"), "{text:?}");
        assert!(has("job true 0"), "{text:?}");
        assert!(has("archivable 1"), "{text:?}");
        // Studio's own messages: max radius 256 = ShapecastMaxSize 512 / 2.
        let lim = |s: &str| text.iter().any(|t| t.starts_with("limits") && t.contains(s));
        assert!(lim("The maximum radius is 256.000000."), "{text:?}");
        assert!(lim("Attempt to shapecast with distance 2000.000000. The maximum distance is 1024."), "{text:?}");
        assert!(lim("Invalid Radius. Must be greater than or equal to 0, got -1.000000."), "{text:?}");
    }

    #[test]
    fn humanoid_health_and_legacy_names() {
        let out = run("local h = Instance.new(\"Humanoid\")\n\
            print(h.Health, h.MaxHealth, h.WalkSpeed)\n\
            print(script.parent == script.Parent, game.workspace == workspace, workspace:findFirstChild(\"Baseplate\") ~= nil)\n\
            local p = Instance.new(\"Part\", workspace)\n\
            local c = p:clone()\n\
            print(c.className, #workspace:getChildren() > 0, p:isA(\"BasePart\"))\n\
            p:remove()\n\
            print(p.Parent)\n\
            local r = Ray.new(Vector3.new(0, 50, 0), Vector3.new(0, -100, 0))\n\
            local hit, pos = workspace:FindPartOnRay(r)\n\
            print(hit and hit.Name, math.floor(pos.Y + 0.5))\n\
            local res = workspace:Raycast(Vector3.new(0, 50, 0), Vector3.new(0, -100, 0))\n\
            print(res.Instance.Name, res.Normal.Y)\n\
            local cs = game:GetService(\"CollectionService\")\n\
            cs:AddTag(workspace.Baseplate, \"Floor\")\n\
            print(#cs:GetTagged(\"Floor\"), workspace.Baseplate:HasTag(\"Floor\"))\n\
            local hs = game:GetService(\"HttpService\")\n\
            print(hs:JSONEncode({a = 1}), hs:JSONDecode('[1,2,3]')[3])\n\
            local t = game:GetService(\"TweenService\"):Create(workspace.Baseplate, TweenInfo.new(0.2), {Transparency = 1})\n\
            t.Completed:Connect(function(state) print(\"tween\", state.Name, workspace.Baseplate.Transparency) end)\n\
            t:Play()\n\
            local part = Instance.new(\"Part\", workspace)\n\
            game:GetService(\"Debris\"):AddItem(part, 0.5)\n\
            delay(1, function() print(\"debris\", part.Parent) end)\n\
            local okds, errds = pcall(function() return game:GetService(\"DataStoreService\"):GetDataStore(\"x\") end)\n\
            print(okds, string.find(tostring(errds), \"publish this place\") ~= nil)\n\
            print(typeof(NumberSequence.new(1)), typeof(Region3.new(Vector3.new(), Vector3.one)), ypcall ~= nil, version() ~= nil)\n");
        let text: Vec<String> = out
            .iter()
            .map(|(_, t)| t.split("  —").next().unwrap_or("").to_string())
            .collect();
        let errors: Vec<&(Level, String)> =
            out.iter().filter(|(l, _)| *l == Level::Error).collect();
        assert!(errors.is_empty(), "{out:?}");
        assert_eq!(text[0], "100 100 16", "{text:?}");
        assert_eq!(text[1], "true true true", "{text:?}");
        assert_eq!(text[2], "Part true true", "{text:?}");
        assert_eq!(text[3], "nil", "{text:?}");
        assert_eq!(text[4], "SpawnLocation 1", "{text:?}");
        assert_eq!(text[5], "SpawnLocation 1", "{text:?}");
        assert_eq!(text[6], "1 true", "{text:?}");
        assert_eq!(text[7], "{\"a\":1} 3", "{text:?}");
        assert!(
            text.iter().any(|t| t == "false true"),
            "unpublished DataStore errors like Studio: {text:?}"
        );
        assert!(
            text.iter().any(|t| t == "NumberSequence Region3 true true"),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t == "tween Completed 1"), "{text:?}");
        assert!(text.iter().any(|t| t == "debris nil"), "{text:?}");
    }

    /// Require by string (another admin script's `require("./String")`) and a
    /// types-only module that ends `return nil` (round 25).
    #[test]
    fn require_by_string_and_nil_modules() {
        let module = |name: &str, src: &str| {
            InstanceBuilder::new("ModuleScript")
                .with_name(name)
                .with_property("Source", src.to_string())
        };
        let mut d = Document::new_place();
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let src = "local a = require('@self/A')\n\
            print('req', a.s, a.b, a.t == nil, a.t2 == nil)\n\
            print('missing', select(2, pcall(function() return require('@self/Missing') end)))\n\
            print('noret', select(2, pcall(function() return require('@self/NoRet') end)))\n\
            print('prefix', select(2, pcall(function() return require('A') end)))";
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("T")
                .with_property("Source", src.to_string())
                .with_child(
                    module("A", "return { s = require('./S'), b = require('@self/B'), t = require('@self/Types'), t2 = require('@self/Types') }")
                        .with_child(module("B", "return require('../S') .. '+b'"))
                        .with_child(module("Types", "export type X = number\nreturn nil")),
                )
                .with_child(module("S", "return 'sib'"))
                .with_child(module("NoRet", "local x = 1")),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        let text: Vec<&str> = out.iter().map(|(_, t)| t.as_str()).collect();
        assert!(text.iter().any(|t| t.starts_with("req sib sib+b true true")), "{text:?}");
        assert!(
            text.iter().any(|t| t.starts_with("missing") && t.contains("could not resolve child component \"Missing\"")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|t| t.starts_with("noret") && t.contains("did not return exactly one value")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|t| t.starts_with("prefix") && t.contains("must start with a valid prefix")),
            "{text:?}"
        );
    }

    /// A disabled script starts when a script enables it (round 26:
    /// Place B Newer's music GUI Handler), once.
    #[test]
    fn enabling_a_script_starts_it() {
        let mut d = Document::new_place();
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("Later")
                .with_property("Disabled", true)
                .with_property("Source", "print('later ran')".to_string()),
        );
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property(
                "Source",
                "local l = script.Parent.Later\nprint('before', l.Enabled)\nl.Enabled = true\nl.Enabled = false\nl.Enabled = true\nprint('state', l.Enabled, l.Disabled)"
                    .to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        let ran = out.iter().filter(|(_, t)| t.starts_with("later ran")).count();
        assert_eq!(ran, 1, "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("before false")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("state true false")), "{out:?}");
    }

    /// An event is a userdata, typeof "RBXScriptSignal", and a bool property takes any
    /// value by truthiness, as in Roblox (round 26:
    /// another admin script's Flux connects FocusLost by typeof, binds Visible to a number).
    #[test]
    fn signal_typeof() {
        let mut d = Document::new_place();
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property(
                "Source",
                "local b = Instance.new('TextBox')\n\
                 local p = {FocusLost = function() end, Text = 'x'}\n\
                 for k, v in p do\n\
                     if typeof(b[k]) == 'RBXScriptSignal' then b[k]:Connect(v) else b[k] = v end\n\
                 end\n\
                 print('types', type(b.FocusLost), tostring(b.FocusLost), b.FocusLost == b.FocusLost, typeof(b.FocusLost), typeof(b.FocusLost:Connect(print)), typeof({}), typeof(b), b.Text)\n\
                 b.Visible = 0; local a = b.Visible; b.Visible = nil\n\
                 print('bool', a, b.Visible)"
                    .to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        assert!(
            out.iter()
                .any(|(_, t)| t.starts_with("types userdata Signal FocusLost true RBXScriptSignal RBXScriptConnection table Instance x")),
            "{out:?}"
        );
        assert!(out.iter().any(|(_, t)| t.starts_with("bool true false")), "{out:?}");
    }

    /// A legacy Script in a player's PlayerGui runs on the server, as in
    /// Studio (round 26: Place B Newer's "server version" label).
    #[test]
    fn player_gui_scripts_run() {
        let mut d = Document::new_place();
        let root = d.dom.root_ref();
        let players = match d.find_child(root, "Players") {
            Some(p) => p,
            None => d.dom.insert(root, InstanceBuilder::new("Players").with_name("Players")),
        };
        let p = d.dom.insert(players, InstanceBuilder::new("Player").with_name("Player"));
        let gui = d.dom.insert(p, InstanceBuilder::new("PlayerGui").with_name("PlayerGui"));
        d.dom.insert(
            gui,
            InstanceBuilder::new("Script")
                .with_name("version")
                .with_property("Disabled", true)
                .with_property("Source", "print('gui script ran')".to_string()),
        );
        let sss = d.find_child(root, "ServerScriptService").unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property(
                "Source",
                "game.Players.Player.PlayerGui.version.Enabled = true".to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        assert!(out.iter().any(|(_, t)| t.starts_with("gui script ran")), "{out:?}");
    }

    #[test]
    fn replicated_character_starts_only_local_scripts_once_in_either_order() {
        for character_first in [false, true] {
            let mut d = Document::new_place();
            let root = d.dom.root_ref();
            let players = d.find_child(root, "Players").unwrap();
            let player = d.dom.insert(players, InstanceBuilder::new("Player"));
            let other = d.dom.insert(players, InstanceBuilder::new("Player"));
            d.dom.get_by_ref_mut(players).unwrap().properties
                .insert(ustr("LocalPlayer"), Variant::Ref(player));
            let workspace = d.workspace().unwrap();
            let character = d.dom.insert(workspace, InstanceBuilder::new("Model"));
            let script_parent = if character_first { root } else { character };
            let script = d.dom.insert(script_parent, InstanceBuilder::new("LocalScript")
                .with_property("Source", "print('character script ran')".to_string()));
            d.dom.insert(character, InstanceBuilder::new("LocalScript")
                .with_property("Disabled", true)
                .with_property("Source", "error('disabled character script ran')".to_string()));
            let rt = Runtime::new().unwrap();
            let mut w = World::new(d, RunMode::Play);
            w.side = Side::Client;
            rt.enter(w);
            rt.bind_globals().unwrap();
            rt.apply_prop(other, "Character", Variant::Ref(character));
            assert!(world(&rt.lua).unwrap().to_start.is_empty());
            rt.apply_prop(player, "Character", Variant::Ref(character));
            if character_first {
                rt.apply_parent(script, character);
            }
            // Repeated replication before dispatch must still execute once.
            rt.apply_prop(player, "Character", Variant::Ref(character));
            assert!(world(&rt.lua).unwrap().to_start.iter().all(|r| *r == script));
            rt.dispatch();
            rt.apply_prop(player, "Character", Variant::Ref(character));
            rt.dispatch();
            let out = rt.leave().output;
            assert_eq!(out.len(), 1, "{out:?}");
            assert!(out[0].1.starts_with("character script ran"), "{out:?}");
        }
    }

    /// Round 26's library gaps from Place D's second admin script and Place B's
    /// an admin script, on the client: settings() refused to a game script,
    /// TextService filtering refused on the client, a CoreGuiType by name,
    /// and a humanoid model with its Animator.
    #[test]
    fn script_library_gaps() {
        let mut d = Document::new_place();
        let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        let s = d.dom.insert(
            sps,
            InstanceBuilder::new("LocalScript").with_name("T").with_property(
                "Source",
                "print('settings', pcall(settings))\n\
                 print('filter', pcall(function() return game:GetService('TextService'):FilterStringAsync('hello', 1) end))\n\
                 local sg = game:GetService('StarterGui')\n\
                 sg:SetCoreGuiEnabled('Chat', false)\n\
                 print('coregui', sg:GetCoreGuiEnabled('Chat'), sg:GetCoreGuiEnabled(Enum.CoreGuiType.Chat), sg:GetCoreGuiEnabled(Enum.CoreGuiType.Backpack))\n\
                 local m = game:GetService('Players'):CreateHumanoidModelFromUserId(1)\n\
                 print('animator', m.Humanoid:FindFirstChild('Animator') ~= nil)"
                    .to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Run);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        let has = |p: &str| out.iter().any(|(_, t)| t.starts_with(p));
        assert!(
            has("settings false The current thread cannot call 'settings' (lacking capability Plugin)"),
            "{out:?}"
        );
        // Server-only in Roblox: a LocalScript gets the engine's error.
        assert!(has("filter false Must only be called from server scripts"), "{out:?}");
        assert!(has("coregui false false true"), "{out:?}");
        assert!(has("animator true"), "{out:?}");
    }

    /// an admin script's client anti-tamper, condensed (Client.Core.Anti and
    /// Shared.Service ReadOnly): settings() refused inside a metamethod
    /// (rt0001 kicked every Play), Instance/Enum metamethods are unnamed C
    /// functions raising the same string each time, and a Lua-written
    /// method called with the wrong self says "Expected ':' not '.'".
    #[test]
    fn adonis_tamper_checks() {
        let mut d = Document::new_place();
        let sp = d.find_child(d.dom.root_ref(), "StarterPlayer").unwrap();
        let sps = d.find_child(sp, "StarterPlayerScripts").unwrap();
        let src = r#"
            local ro = setmetatable({}, { __index = function() return pcall(settings) end })
            print("ro", ro.x)
            local function mm(f)
                local m
                xpcall(f, function() m = debug.info(2, "f") end)
                local ok1, e1 = pcall(m)
                local ok2, e2 = pcall(m)
                local parts = { debug.info(m, "s"), debug.info(m, "l"), debug.info(m, "n") == "",
                    debug.info(m, "a"), ok1, type(e1), rawequal(e1, e2) }
                for i, v in parts do parts[i] = tostring(v) end
                return table.concat(parts, " ")
            end
            print("idx", mm(function() local c = game.____________ end))
            print("nidx", mm(function() game.____________ = 5 end))
            print("ncall", mm(function() game:____________() end))
            print("eidx", mm(function() local c = Enum.HumanoidStateType.____________ end))
            print("rf", pcall(Instance.new("RemoteFunction").InvokeServer, workspace))
        "#;
        let s = d.dom.insert(
            sps,
            InstanceBuilder::new("LocalScript")
                .with_name("T")
                .with_property("Source", src.to_string()),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Run);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        let text: Vec<String> = out
            .iter()
            .map(|(_, t)| t.split("  —").next().unwrap_or("").to_string())
            .collect();
        for want in [
            "ro false",
            "idx [C] -1 true 0 false string true",
            "nidx [C] -1 true 0 false string true",
            "ncall [C] -1 true 0 false string true",
            "eidx [C] -1 true 0 false string true",
            "rf false Expected ':' not '.' calling member function InvokeServer",
        ] {
            assert!(text.iter().any(|t| t.starts_with(want)), "{want}: {text:?}");
        }
    }

    /// Engine errors caught by pcall are the engine's text without a
    /// position (round 26). (GetProductInfo, once tested here, is
    /// web_social_http's now: domains/web_social.rs tests it.)
    #[test]
    fn engine_error_text() {
        let mut d = Document::new_place();
        let sss = d
            .find_child(d.dom.root_ref(), "ServerScriptService")
            .unwrap();
        let src = "print('member', pcall(function() return workspace.Nope end))\n\
            print('dot', pcall(function() game:GetService('Players'):FindFirstChildOfClass('Player').Kick(workspace, 'x') end))\n\
            print('wskick', pcall(function() workspace:Kick('x') end))\n\
            local tr = game:GetService('LocalizationService'):GetTranslatorForLocaleAsync('en-us')\n\
            print('tr', tr.ClassName, tr.LocaleId, tr:Translate(game, 'hi'), tr:GetPropertyChangedSignal('LocaleId'):Connect(print).Connected)\n\
            print('ds', pcall(function() return game:GetService('DataStoreService'):GetDataStore('x') end))";
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("T")
                .with_property("Source", src.to_string()),
        );
        // Game scripts can't make a Player (WritePlayer); the test places one.
        let players = d.find_child(d.dom.root_ref(), "Players").unwrap();
        d.dom.insert(players, InstanceBuilder::new("Player").with_name("P"));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..10 {
            rt.step(1.0 / 60.0);
        }
        let out = rt.leave().output;
        let text: Vec<String> = out
            .iter()
            .map(|(_, t)| t.split("  —").next().unwrap_or("").to_string())
            .collect();
        assert!(
            text.iter().any(|t| t == "member false Nope is not a valid member of Workspace \"Workspace\""),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t == "tr Translator en-us hi true"), "{text:?}");
        // an admin script's anti-kick check, word for word.
        assert!(
            text.iter().any(|t| t == "dot false Expected ':' not '.' calling member function Kick"),
            "{text:?}"
        );
        assert!(
            text.iter().any(|t| t == "wskick false Kick is not a valid member of Workspace \"Workspace\""),
            "{text:?}"
        );
        assert!(
            text.iter().any(|t| t == "ds false You must publish this place to the web to access DataStore."),
            "{text:?}"
        );
    }
    /// ClearAllChildren destroys the last child first, as Studio's
    /// Instance::clearAllChildren; Destroy cuts the whole
    /// subtree loose, parent before children, and locks every Parent.
    #[test]
    fn clear_all_children_last_first_and_destroy_detaches_all() {
        let out = run("local f = Instance.new('Folder', workspace)\n\
            for _, n in {'A', 'B', 'C'} do local x = Instance.new('Folder') x.Name = n x.Parent = f end\n\
            local order = {}\n\
            workspace.DescendantRemoving:Connect(function(d) table.insert(order, d.Name) end)\n\
            f:ClearAllChildren()\n\
            task.wait()\n\
            print('order', table.concat(order, ','))\n\
            local m = Instance.new('Model')\n\
            local a = Instance.new('Folder', m) local b = Instance.new('Folder', a) local c = Instance.new('Part', b)\n\
            for i = 1, 50 do Instance.new('Part', m) end\n\
            m.Parent = workspace\n\
            m:Destroy()\n\
            print('detached', m.Parent == nil, a.Parent == nil, b.Parent == nil, c.Parent == nil, #m:GetChildren(), #a:GetChildren(), #b:GetChildren())\n\
            print('locked', pcall(function() c.Parent = workspace end))\n\
            print('again', pcall(function() b:Destroy() end))\n");
        assert!(out.iter().any(|(_, t)| t.starts_with("order C,B,A")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("detached true true true true 0 0 0")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("locked false")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("again true")), "{out:?}");
    }

    /// class_signals: DescendantAdded / Removing on game for the watched
    /// classes only (IsA), on parenting in, reparenting and Destroy.
    #[test]
    fn class_signals_fire_only_for_watched_classes() {
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(Document::new_place(), RunMode::Run));
        rt.bind_globals().unwrap();
        let (added, removing) = class_signals(&rt.lua, vec!["ProximityPrompt".into(), "HandleAdornment".into()]).unwrap();
        rt.lua.globals().set("added_sig", added).unwrap();
        rt.lua.globals().set("removing_sig", removing).unwrap();
        rt.lua.load("_G.log = {}\n\
            added_sig:Connect(function(i) table.insert(_G.log, '+' .. i.Name) end)\n\
            removing_sig:Connect(function(i) table.insert(_G.log, '-' .. i.Name) end)\n\
            local m = Instance.new('Model') m.Name = 'M'\n\
            for i = 1, 20 do Instance.new('Part', m) end\n\
            local pp = Instance.new('ProximityPrompt') pp.Name = 'PP' pp.Parent = m.Part\n\
            local box = Instance.new('BoxHandleAdornment') box.Name = 'Box' box.Parent = m\n\
            m.Parent = workspace\n\
            local f = Instance.new('Folder', workspace) m.Parent = f\n\
            f:Destroy()").exec().unwrap();
        rt.step(1.0 / 60.0);
        let log: String = rt.lua.load("table.sort(_G.log) return table.concat(_G.log, ',')").eval().unwrap();
        assert_eq!(log, "+Box,+Box,+PP,+PP,-Box,-Box,-PP,-PP");
    }

    #[test]
    fn destroyed_parent_nil_is_idempotent() {
        let out = run("local root = Instance.new('Folder', workspace)\n\
            local p = Instance.new('Part', root)\n\
            root:Destroy()\n\
            print('detached', p.Parent == nil)\n\
            print('nilparent', pcall(function() p.Parent = nil end))\n\
            print('reparent', pcall(function() p.Parent = workspace end))\n");
        assert!(out.iter().any(|(_, t)| t.starts_with("detached true")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("nilparent true")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("reparent false")), "{out:?}");
    }

    #[test]
    fn destroying_model_retires_its_scripts_from_surviving_signals() {
        assert_model_scripts_retire("workspace.Npc:Destroy()");
    }

    #[test]
    fn moving_model_to_replicated_storage_retires_legacy_scripts() {
        assert_model_scripts_retire("workspace.Npc.Parent = game:GetService('ReplicatedStorage')");
    }

    fn assert_model_scripts_retire(action: &str) {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let model = d.dom.insert(ws, InstanceBuilder::new("Model").with_name("Npc"));
        d.dom.insert(model, InstanceBuilder::new("Humanoid"));
        let worker = d.dom.insert(model, InstanceBuilder::new("Script").with_property("Source", r#"
            local model = script.Parent
            local run = game:GetService("RunService")
            _G.npc_connection = run.Heartbeat:Connect(function()
                assert(model.Humanoid)
            end)
            task.spawn(function()
                task.wait(1)
                warn("retired NPC resumed")
            end)
            task.delay(1, function() warn("retired NPC delay fired") end)
        "#.to_owned()));
        let observer_source = format!(r#"
            local run = game:GetService("RunService")
            local count = 0
            local live = run.Heartbeat:Connect(function() count += 1 end)
            task.wait(0.25)
            assert(_G.npc_connection.Connected)
            assert(count > 0)
            {action}
            assert(not _G.npc_connection.Connected)
            local before = count
            task.wait(1.25)
            assert(live.Connected and count > before)
            print("NPC script stopped")
        "#);
        let observer = d.dom.insert(ws, InstanceBuilder::new("Script").with_property("Source", observer_source));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(worker);
        rt.run_script(observer);
        for _ in 0..120 {
            let run = world(&rt.lua).ok().and_then(|w| w.d().find_child(w.d().dom.root_ref(), "RunService")).unwrap();
            rt.fire(run, "Heartbeat", vec![]);
            rt.step(1.0 / 60.0);
        }
        let out = rt.leave().output;
        assert!(out.iter().any(|(_, text)| text.starts_with("NPC script stopped")), "{out:?}");
        assert!(out.iter().all(|(level, _)| *level == Level::Info), "{out:?}");
    }

    /// Tagged instances entering / leaving the DataModel fire the tag's
    /// InstanceAdded / InstanceRemoved signals (Place A's box, upgrade machine and
    /// perk lights track replicated map pieces this way); AddTag outside
    /// the DataModel fires nothing until it is parented in. DescendantAdded
    /// fires for every descendant of the moved instance.
    #[test]
    fn tag_signals_follow_data_model_membership() {
        let out = run("local CS = game:GetService('CollectionService')\n\
            local added, removed, desc = {}, {}, {}\n\
            CS:GetInstanceAddedSignal('Box'):Connect(function(i) table.insert(added, i.Name) end)\n\
            CS:GetInstanceRemovedSignal('Box'):Connect(function(i) table.insert(removed, i.Name) end)\n\
            workspace.DescendantAdded:Connect(function(i) table.insert(desc, i.Name) end)\n\
            local m = Instance.new('Model'); m.Name = 'M'\n\
            local p = Instance.new('Part'); p.Name = 'Lid'; p.Parent = m\n\
            p:AddTag('Box')\n\
            print('before', #added)\n\
            m.Parent = workspace\n\
            task.wait()\n\
            print('added', table.concat(added, ','))\n\
            print('desc', table.concat(desc, ','))\n\
            local c = m:Clone(); c.Name = 'C'; c:FindFirstChild('Lid').Name = 'Lid2'; c.Parent = workspace\n\
            m.Parent = nil\n\
            task.wait()\n\
            print('added2', table.concat(added, ','))\n\
            print('removed', table.concat(removed, ','))\n");
        assert!(out.iter().any(|(_, t)| t.starts_with("before 0")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("added Lid")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("desc M,Lid")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("added2 Lid,Lid2")), "{out:?}");
        assert!(out.iter().any(|(_, t)| t.starts_with("removed Lid")), "{out:?}");
    }

    /// A profileend whose begin was before a yield warns, as in Studio
    /// (round 26); a balanced pair doesn't. Player:Move without a
    /// character warns once. Destroy twice is fine.
    #[test]
    fn profile_markers_and_move_warnings() {
        // (A game script can't Instance.new a Player: WritePlayer.)
        let out = run_with_player("debug.profilebegin('a')\n\
            debug.profileend()\n\
            debug.profilebegin('b')\n\
            task.wait()\n\
            debug.profileend()\n\
            local p = game:GetService('Players'):GetPlayers()[1]\n\
            p:Move(Vector3.new(1, 0, 0))\n\
            p:Move(Vector3.new(1, 0, 0))\n\
            local f = Instance.new('Folder')\n\
            local l = Instance.new('PointLight', f)\n\
            f:Destroy()\n\
            print('twice', pcall(l.Destroy, l))\n");
        // Destroying a destroyed instance's descendant is a no-op.
        assert!(out.iter().any(|(_, t)| t.starts_with("twice true")), "{out:?}");
        let warns: Vec<&String> = out.iter().filter(|(l, _)| *l == Level::Warn).map(|(_, t)| t).collect();
        assert_eq!(
            warns.iter().filter(|t| t.starts_with("debug.profileEnd() - No active profile annotation. At: Stack Begin")).count(),
            1,
            "{warns:?}"
        );
        assert_eq!(
            warns.iter().filter(|t| t.starts_with("Player:Move called, but player currently has no character.")).count(),
            1,
            "{warns:?}"
        );
    }
}

#[cfg(test)]
#[path = "selftest/attachment_runtime.rs"]
mod attachment_tests;

#[cfg(test)]
#[path = "selftest/bridge.rs"]
mod bridge_tests;

#[cfg(test)]
#[path = "selftest/text_font.rs"]
mod text_font_tests;

/// `Runtime::notify_prop` from inside Lua (ChangeHistoryService's undo,
/// src/plugin_host/api_history.rs): Changed for a property changed outside
/// the script's own write.
pub fn notify_prop_in(lua: &Lua, r: Ref, name: &str) {
    let Ok(mut w) = world(lua) else { return };
    let root = w.d().dom.root_ref();
    if !w.listening.contains(&r) && !w.listening.contains(&root) {
        return;
    }
    let class = w.d().class_of(r);
    let value_changed = name == "Value" && class.is_some_and(|c| doc::is_a(&c, "ValueBase"));
    let v = if value_changed {
        let v = w.d().prop(r, name);
        drop(w);
        v.and_then(|v| variant_to_lua(lua, &v, None).ok()).unwrap_or(Value::Nil)
    } else {
        drop(w);
        match lua.create_string(name) {
            Ok(s) => Value::String(s),
            Err(_) => return,
        }
    };
    if let Ok(mut w) = world(lua) {
        w.fire(r, "Changed", vec![v]);
        w.fire(r, &format!("Changed:{name}"), vec![]);
    }
}

#[cfg(test)]
mod weld_offset_tests {
    use super::*;

    /// Runs `src` as a server Script in Play, with `setup` adding to the place first.
    fn run(setup: impl FnOnce(&mut Document), src: &str) -> Vec<(Level, String)> {
        let mut d = Document::new_place();
        setup(&mut d);
        let root = d.dom.root_ref();
        let sss = d.find_child(root, "ServerScriptService").unwrap();
        let s = d.dom.insert(sss, InstanceBuilder::new("Script").with_name("T").with_property("Source", src.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..30 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output
    }

    fn line(out: &[(Level, String)], tag: &str) -> String {
        out.iter()
            .find_map(|(_, s)| s.strip_prefix(tag).map(|r| r.split("  —  ").next().unwrap_or(r).trim().to_string()))
            .unwrap_or_else(|| panic!("no {tag} in {out:?}"))
    }

    /// Studio 0.747 (a Studio server): a
    /// WeldConstraint takes its parts' offset when it becomes active (both
    /// parts in the Workspace) and keeps it while out of the Workspace; a
    /// weld made outside the Workspace holds its parts where they are when
    /// they enter it; Enabled or Part0/Part1 set again takes a new offset.
    /// rustudio snapped every such weld to an identity offset.
    #[test]
    fn weld_constraint_offset_is_taken_when_active() {
        let out = run(|_| {}, r#"
            local RS = game:GetService("ReplicatedStorage")
            local function mk(parent)
                local f = Instance.new("Model")
                local a = Instance.new("Part") a.Name = "A" a.Anchored = true a.CFrame = CFrame.new(0, 500, 0) a.Parent = f
                local b = Instance.new("Part") b.Name = "B" b.Anchored = true b.CFrame = CFrame.new(3, 500, 0) b.Parent = f
                f.Parent = parent
                return f, a, b
            end
            local function weld(a, b) local w = Instance.new("WeldConstraint") w.Part0 = a w.Part1 = b w.Parent = b return w end
            local function off(f) return string.format("%.2f", (f.A.CFrame:Inverse() * f.B.CFrame).Position.X) end
            local r = {}
            local f, a, b = mk(RS) weld(a, b) b.CFrame = CFrame.new(7, 500, 0) f.Parent = workspace
            table.insert(r, off(f))
            local t, ta, tb = mk(RS) weld(ta, tb) tb.CFrame = CFrame.new(9, 500, 0)
            local c = t:Clone() c.Parent = workspace
            table.insert(r, off(c))
            local g, ga, gb = mk(workspace) weld(ga, gb) g.Parent = RS gb.CFrame = CFrame.new(11, 500, 0) g.Parent = workspace
            table.insert(r, off(g))
            local h, ha, hb = mk(workspace) weld(ha, hb) h.Parent = RS
            local hc = h:Clone() hc.B.CFrame = CFrame.new(13, 500, 0) hc.Parent = workspace
            table.insert(r, off(hc))
            local k, ka, kb = mk(workspace) local kw = weld(ka, kb) k.Parent = RS kb.CFrame = CFrame.new(15, 500, 0)
            kw.Enabled = false kw.Enabled = true k.Parent = workspace
            table.insert(r, off(k))
            print("OFFSETS", table.concat(r, " "))
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "OFFSETS"), "7.00 9.00 3.00 3.00 15.00", "{out:?}");
    }

    /// a game's hunter rigs were built in ReplicatedStorage: their welds are
    /// saved with State 1 and an identity CFrame0 (what a Studio save writes
    /// for a weld that was never active). A clone parented into the
    /// Workspace keeps its trim where the template has it. A weld saved
    /// active (State 3) still puts a moved part back (a game's lantern).
    #[test]
    fn saved_never_active_weld_keeps_the_parts_where_they_are() {
        let cf = |x: f32, y: f32| Variant::CFrame(rbx_dom_weak::types::CFrame::new(rbx_dom_weak::types::Vector3::new(x, y, 0.0), rbx_dom_weak::types::Matrix3::identity()));
        let at = |x: f32| cf(x, 50.0);
        let out = run(|d| {
            let rs = d.find_child(d.dom.root_ref(), "ReplicatedStorage").unwrap();
            for (name, state, c0) in [("Never", 1i32, 0.0f32), ("Active", 3i32, 2.0f32)] {
                let m = d.dom.insert(rs, InstanceBuilder::new("Model").with_name(name));
                let a = d.dom.insert(m, InstanceBuilder::new("Part").with_name("Torso").with_property("Anchored", true).with_property("CFrame", at(0.0)));
                let b = d.dom.insert(m, InstanceBuilder::new("Part").with_name("Stain").with_property("Anchored", true).with_property("CFrame", at(0.45)));
                d.dom.insert(b, InstanceBuilder::new("WeldConstraint")
                    .with_property("Part0", Variant::Ref(a))
                    .with_property("Part1", Variant::Ref(b))
                    .with_property("State", state)
                    .with_property("CFrame0", cf(c0, 0.0)));
            }
        }, r#"
            local RS = game:GetService("ReplicatedStorage")
            local function off(m) return string.format("%.2f", (m.Torso.CFrame:Inverse() * m.Stain.CFrame).Position.X) end
            local never = RS.Never:Clone() never.Parent = workspace
            local active = RS.Active:Clone() active.Parent = workspace
            print("SAVED", off(never), off(active))
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "SAVED"), "0.45 2.00", "{out:?}");
    }

    #[test]
    fn run_service_resolves_by_roblox_name() {
        let out = run(|_| {}, r#"
            local a = game["Run Service"]
            local b = game:FindFirstChild("Run Service")
            local c = game:WaitForChild("Run Service")
            local d = game:GetService("RunService")
            local folder = Instance.new("Folder")
            assert(a == d and b == d and c == d, "Run Service alias")
            assert(folder:FindFirstChild("Run Service") == nil, "only on DataModel")
            print("RUN_SVC", a.ClassName)
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "RUN_SVC"), "RunService", "{out:?}");
    }

    #[test]
    fn legacy_datamodel_service_indexing() {
        let out = run(|_| {}, r#"
            local cp = game.ContentProvider
            assert(cp == game:GetService("ContentProvider"), "ContentProvider dot")
            assert(game:FindFirstChild("ContentProvider") == cp, "ContentProvider FindFirstChild")
            assert(game:WaitForChild("ContentProvider") == cp, "ContentProvider WaitForChild")
            assert(game.BadgeService == game:GetService("BadgeService"), "BadgeService dot")
            assert(game.TweenService == game:GetService("TweenService"), "TweenService dot")
            assert(game.JointsService == game:GetService("JointsService"), "JointsService dot")
            assert(game["Teleport Service"] == game:GetService("TeleportService"), "Teleport Service alias")
            assert(game["Script Context"] == game:GetService("ScriptContext"), "Script Context alias")
            assert(game:FindFirstChild("CoreGui") == nil, "CoreGui hidden")
            assert(game:FindFirstChild("StudioService") == nil, "StudioService hidden")
            assert(game:FindFirstChild("DebuggerManager") == nil, "DebuggerManager hidden")
            assert(game:FindFirstChild("ScriptEditorService") == nil, "ScriptEditorService hidden")
            assert(game:FindFirstChild("PluginGuiService") == nil, "PluginGuiService hidden")
            assert(game:FindFirstChild("DataStoreService") == nil, "DataStoreService on-demand")
            local ok_cg = pcall(function() return game.CoreGui end)
            local ok_ss = pcall(function() return game.StudioService end)
            local ok_dm = pcall(function() return game.DebuggerManager end)
            local ok_se = pcall(function() return game.ScriptEditorService end)
            local ok_ds = pcall(function() return game.DataStoreService end)
            assert(not ok_cg and not ok_ss and not ok_dm and not ok_se and not ok_ds, "restricted/on-demand services not indexable")
            local folder = Instance.new("Folder")
            assert(folder:FindFirstChild("ContentProvider") == nil, "only on DataModel")
            local ok_f = pcall(function() return folder.ContentProvider end)
            assert(not ok_f, "folder.ContentProvider errors")
            assert(UserSettings().GameSettings.SavedQualityLevel == Enum.SavedQualitySetting.QualityLevel10, "SavedQualityLevel default")
            local ok_setql, err_setql = pcall(function()
                UserSettings().GameSettings.SavedQualityLevel = Enum.SavedQualitySetting.Automatic
            end)
            assert(not ok_setql and string.find(err_setql, "lacking capability RobloxScript"),
                "SavedQualityLevel write denied from identity 2: " .. tostring(err_setql))
            local ok_set, err_set = pcall(settings)
            assert(not ok_set and string.find(err_set, "lacking capability Plugin"), "settings still denied")
            print("LEGACY_SVC", cp.ClassName)
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "LEGACY_SVC"), "ContentProvider", "{out:?}");
    }

    #[test]
    fn hidden_core_services_invisible_to_game_scripts() {
        let out = run(
            |d| {
                let root = d.dom.root_ref();
                let cg = d.dom.insert(root, InstanceBuilder::new("CoreGui").with_name("CoreGui"));
                d.dom.insert(cg, InstanceBuilder::new("ScreenGui").with_name("RobloxGui"));
                let cp = d.dom.insert(root, InstanceBuilder::new("CorePackages").with_name("CorePackages"));
                d.dom.insert(cp, InstanceBuilder::new("Folder").with_name("Packages"));
            },
            r#"
                assert(game:FindFirstChild("CoreGui") == nil, "CoreGui FFC hidden")
                assert(game:FindFirstChild("CorePackages") == nil, "CorePackages FFC hidden")
                assert(game:FindFirstChildOfClass("CoreGui") == nil, "CoreGui FFCoC hidden")
                assert(game:FindFirstChild("RobloxGui", true) == nil, "RobloxGui recursive hidden")
                assert(game:GetService("CoreGui") == nil, "CoreGui GetService hidden")
                assert(game:GetService("CorePackages") == nil, "CorePackages GetService hidden")
                assert(not pcall(function() return game.CoreGui end), "game.CoreGui hidden")
                assert(not pcall(function() return game.CorePackages end), "game.CorePackages hidden")
                for _, c in game:GetChildren() do
                    assert(c.ClassName ~= "CoreGui" and c.ClassName ~= "CorePackages", "GetChildren hidden")
                end
                for _, d in game:GetDescendants() do
                    assert(d.Name ~= "RobloxGui" and d.Name ~= "Packages" and d.ClassName ~= "CoreGui" and d.ClassName ~= "CorePackages", "GetDescendants hidden")
                end
                print("CORE_HIDDEN", "ok")
            "#,
        );
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "CORE_HIDDEN"), "ok", "{out:?}");
    }

    #[test]
    fn camera_coordinate_frame_alias() {
        let out = run(|_| {}, r#"
            local cam = Instance.new("Camera")
            local sig = 0
            cam:GetPropertyChangedSignal("CFrame"):Connect(function() sig += 1 end)
            cam:GetPropertyChangedSignal("CoordinateFrame"):Connect(function() sig += 10 end)
            local cf = CFrame.new(10, 20, 30)
            cam.CoordinateFrame = cf
            assert(cam.CFrame == cf, "CoordinateFrame write updates CFrame")
            assert(cam.CoordinateFrame == cf, "CoordinateFrame read matches CFrame")
            local cf2 = CFrame.new(1, 2, 3)
            cam.CFrame = cf2
            assert(cam.CoordinateFrame == cf2, "CFrame write updates CoordinateFrame")
            assert(sig == 22, "both CFrame and CoordinateFrame signals fired, got " .. tostring(sig))
            print("CAM_CF_OK", sig)
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "CAM_CF_OK"), "22", "{out:?}");
    }

    #[test]
    fn custom_physical_properties_accepts_nil() {
        let out = run(|_| {}, r#"
            local p = Instance.new("Part")
            p.CustomPhysicalProperties = PhysicalProperties.new(2, 0.4, 0.3)
            assert(p.CustomPhysicalProperties ~= nil)
            p.CustomPhysicalProperties = nil
            assert(p.CustomPhysicalProperties == nil, tostring(p.CustomPhysicalProperties))
            print("CPP", tostring(p.CustomPhysicalProperties))
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "CPP"), "nil", "{out:?}");
    }

    #[test]
    fn invoke_waits_for_the_callback() {
        let out = run(|_| {}, r#"
            local bf = Instance.new("BindableFunction")
            local rf = Instance.new("RemoteFunction")
            local got = {}
            task.spawn(function()
                got[1] = bf:Invoke("hi")
            end)
            task.spawn(function()
                got[2] = rf:InvokeServer(21)
            end)
            task.wait()
            bf.OnInvoke = function(x) return x .. "!" end
            rf.OnServerInvoke = function(_, n) return n * 2 end
            task.wait()
            task.wait()
            print("INV", tostring(got[1]), tostring(got[2]))
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "INV"), "hi! 42", "{out:?}");
    }

    #[test]
    fn stop_script_cancels_pending_invoke_through_module() {
        let mut d = Document::new_place();
        let rs = d.find_child(d.dom.root_ref(), "ReplicatedStorage").unwrap();
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        d.dom.insert(rs, InstanceBuilder::new("RemoteFunction").with_name("GetVal"));
        d.dom.insert(
            rs,
            InstanceBuilder::new("ModuleScript")
                .with_name("Helper")
                .with_property("Source", "return function() return game.ReplicatedStorage.GetVal:InvokeServer() end".to_string()),
        );
        let holder = d.dom.insert(sss, InstanceBuilder::new("Folder").with_name("Holder"));
        let scr = d.dom.insert(
            holder,
            InstanceBuilder::new("Script")
                .with_name("Caller")
                .with_property(
                    "Source",
                    "local helper = require(game.ReplicatedStorage.Helper)\nlocal v = helper()\nprint('RESUMED', tostring(v), tostring(script.Parent))\n".to_string(),
                ),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Run);
        w.has_peer = true;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.install_side().unwrap();
        rt.run_script(scr);
        let req_id = rt.lua.app_data_ref::<Invokes>().and_then(|i| i.waiting.keys().next().copied());
        assert!(req_id.is_some(), "InvokeServer should register a pending invoke");
        rt.apply_destroy(holder);
        rt.finish_invoke(req_id.unwrap(), true, &[Xfer::Str(b"ok".to_vec())], &|r| Some(r));
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert!(!out.iter().any(|(_, s)| s.contains("RESUMED")), "destroyed script should not resume after invoke: {out:?}");
    }

    #[test]
    fn native_script_loads_deferred_and_drains_on_step() {
        let mut d = Document::new_place();
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_property(
                "Source",
                "--!native\nlocal function sum(n) local t = 0 for i = 1, n do t += i end return t end\nprint('NAT', sum(10))\n".to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        let queued = rt.lua.app_data_ref::<NativeQueue>().map_or(0, |q| q.items.len());
        let on = rt.lua.app_data_ref::<NativeQueue>().is_some_and(|q| q.on);
        if on {
            assert_eq!(queued, 1, "--!native script queued for deferred native build");
        }
        rt.step(1.0 / 60.0);
        let remaining = rt.lua.app_data_ref::<NativeQueue>().map_or(0, |q| q.items.len());
        assert_eq!(remaining, 0, "native queue drained on step");
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "NAT"), "55", "{out:?}");
    }

    #[test]
    fn topbar_inset_safe_insets_and_set_core_topbar_enabled() {
        let mut d = Document::new_place();
        let players = d.find_child(d.dom.root_ref(), "Players").unwrap();
        let p = d.dom.insert(players, InstanceBuilder::new("Player").with_name("Local"));
        let pg = d.dom.insert(p, InstanceBuilder::new("PlayerGui").with_name("PlayerGui"));
        let sg = d.dom.insert(
            pg,
            InstanceBuilder::new("ScreenGui")
                .with_name("TopbarGui")
                .with_property("ScreenInsets", Variant::Enum(rbx_dom_weak::types::Enum::from_u32(3))),
        );
        let s = d.dom.insert(
            pg,
            InstanceBuilder::new("LocalScript").with_property(
                "Source",
                r#"
                    local gs = game:GetService("GuiService")
                    local sg = game:GetService("StarterGui")
                    local gui = script.Parent.TopbarGui
                    local changed = 0
                    gs:GetPropertyChangedSignal("TopbarInset"):Connect(function() changed += 1 end)
                    local r1 = gs.TopbarInset
                    assert(r1.Min.X == 128 and r1.Height == 58 and r1.Max.X == 1000, tostring(r1))
                    local area = gs:GetInsetArea(Enum.ScreenInsets.TopbarSafeInsets)
                    assert(area.Min.X == 128 and area.Min.Y == -58 and area.Max.Y == 0, tostring(area))
                    assert(gui.AbsolutePosition.X == 128 and gui.AbsolutePosition.Y == -58, tostring(gui.AbsolutePosition))
                    assert(gui.AbsoluteSize.X == 872 and gui.AbsoluteSize.Y == 58, tostring(gui.AbsoluteSize))
                    assert(sg:GetCore("TopbarEnabled") == true)
                    sg:SetCore("TopbarEnabled", false)
                    assert(sg:GetCore("TopbarEnabled") == false)
                    task.wait()
                    sg:SetCoreGuiEnabled(Enum.CoreGuiType.Chat, false)
                    task.wait()
                    local r2 = gs.TopbarInset
                    assert(r2.Min.X == 72 and r2.Height == 58 and changed >= 1, tostring(r2) .. " ch=" .. tostring(changed))
                    print("TOPBAR", r1.Min.X, r2.Min.X, changed)
                "#
                .to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Play);
        w.side = Side::Client;
        w.input.view_size = [1000.0, 600.0];
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..5 {
            rt.step(1.0 / 60.0);
        }
        let w = rt.leave();
        let _ = sg;
        assert!(w.topbar_off, "TopbarEnabled=false reflected in World::topbar_off");
        let out = w.output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert!(out.iter().any(|(_, s)| s.starts_with("TOPBAR 128 72")), "{out:?}");
    }

    #[test]
    fn get_last_input_type_defaults_to_device_kind() {
        let out = run(|_| {}, r#"
            local uis = game:GetService("UserInputService")
            print("UIT", tostring(uis:GetLastInputType()))
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "UIT"), "Enum.UserInputType.Keyboard", "{out:?}");
    }

    #[test]
    fn collection_service_only_indexes_live_datamodel_members() {
        let out = run(|_| {}, r#"
            local tags = game:GetService("CollectionService")
            local folder = Instance.new("Folder")
            local part = Instance.new("Part", folder)
            assert(#tags:GetTagged("LifecycleProbe") == 0)
            part:AddTag("LifecycleProbe")
            assert(#tags:GetTagged("LifecycleProbe") == 0)
            folder.Parent = workspace
            assert(tags:GetTagged("LifecycleProbe")[1] == part)
            folder.Parent = nil
            assert(#tags:GetTagged("LifecycleProbe") == 0)
            assert(part:HasTag("LifecycleProbe"))
            folder.Parent = workspace
            assert(#tags:GetTagged("LifecycleProbe") == 1)
            folder:Destroy()
            assert(#tags:GetTagged("LifecycleProbe") == 0)
        "#);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn hsv_extrapolation_and_packed_part_color_match_studio() {
        let out = run(|_| {}, r#"
            local part = Instance.new("Part", workspace)
            local function close(a, b)
                return math.abs(a.R - b.R) < 1e-5
                    and math.abs(a.G - b.G) < 1e-5
                    and math.abs(a.B - b.B) < 1e-5
            end
            assert(close(Color3.fromHSV(1, 1, 1), Color3.new(1, 0, 0)))
            assert(close(Color3.fromHSV(-0.25, 1, 1), Color3.new()))
            local c = Color3.fromHSV(2, 3, 4)
            assert(close(c, Color3.new(4, -8, -80)))
            part.Color = c
            assert(close(part.Color, Color3.fromRGB(252, 8, 80)))
            part.Color = Color3.fromHSV(0.8, 0.6, 0.9)
            assert(close(part.Color, Color3.fromRGB(201, 91, 229)))
            part.Color = Color3.fromHSV(66, 70, 248)
            assert(close(part.Color, Color3.fromRGB(8, 216, 128)))
        "#);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
    }

    #[test]
    fn legacy_collision_groups_warn_once_and_apply_changes() {
        let out = run(|_| {}, r#"
            local physics = game:GetService("PhysicsService")
            local part = Instance.new("Part", workspace)
            local first = physics:CreateCollisionGroup("LegacyFirst")
            local second = physics:CreateCollisionGroup("LegacySecond")
            assert(type(first) == "number" and first == physics:GetCollisionGroupId("LegacyFirst"))
            assert(second == first + 1 and second == physics:GetCollisionGroupId("LegacySecond"))
            assert(physics:RegisterCollisionGroup("Modern") == nil)
            assert(physics:GetCollisionGroupName(first) == "LegacyFirst")
            assert(physics:GetCollisionGroupName(second) == "LegacySecond")
            physics:SetPartCollisionGroup(part, "LegacyFirst")
            physics:SetPartCollisionGroup(part, "LegacySecond")
            assert(physics:IsCollisionGroupRegistered("LegacyFirst"))
            assert(part.CollisionGroup == "LegacySecond")
        "#);
        assert!(out.iter().all(|(level, _)| *level != Level::Error), "{out:?}");
        for warning in [
            "CreateCollisionGroup is deprecated, please use RegisterCollisionGroup instead.",
            "SetPartCollisionGroup is deprecated, please use BasePart.CollisionGroup instead.",
            "GetCollisionGroupName is deprecated.",
        ] {
            assert_eq!(
                out.iter()
                    .filter(|(level, msg)| *level == Level::Warn && msg.starts_with(warning))
                    .count(),
                1,
                "{out:?}"
            );
        }
    }

    #[test]
    fn get_humanoid_description_populates_clothing_and_accessories() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let sss = d.dom.insert(root, InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"));
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("S").with_property(
                "Source",
                r#"
                local Players = game:GetService("Players")
                local desc = Players:GetHumanoidDescriptionFromUserId(42)
                print("DESC", desc.Shirt, desc.Pants, desc.Face, desc.HatAccessory, desc.ShouldersAccessory, desc.BackAccessory)
                local c = desc:Clone()
                print("CLONE", c.Shirt, c.HatAccessory, c.BackAccessory)
                "#.to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Play);
        w.avatar = Some(crate::avatar::AvatarLook {
            user_id: 42,
            r15: true,
            colors: crate::rig::BodyColors::default(),
            scales: crate::rig::Scales::default(),
            proportion: 0.0,
            body_type: 0.0,
            shirt: None,
            pants: None,
            tshirt: None,
            face: None,
            shirt_id: Some(1001),
            pants_id: Some(1002),
            tshirt_id: None,
            face_id: Some(1003),
            head_id: None,
            body_part_ids: Vec::new(),
            accessory_ids: vec![
                ("Hat".into(), 2001),
                ("Hat".into(), 2002),
                ("ShoulderAccessory".into(), 2003),
                ("BackAccessory".into(), 2004),
            ],
            accessories: Vec::new(),
            body_parts: Vec::new(),
            head: Vec::new(),
            animations: Vec::new(),
            emotes: Vec::new(),
            skipped: Vec::new(),
        });
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script(s);
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "DESC"), "1001 1002 1003 2001,2002 2003 2004", "{out:?}");
        assert_eq!(line(&out, "CLONE"), "1001 2001,2002 2004", "{out:?}");
    }

    #[test]
    fn failed_animation_load_warns_with_sanitized_id() {
        let out = run(|_| {}, r#"
            local m = Instance.new("Model", workspace)
            local h = Instance.new("Humanoid", m)
            local animator = Instance.new("Animator", h)
            local anim = Instance.new("Animation")
            anim.AnimationId = "invalid://missing-clip"
            animator:LoadAnimation(anim)
        "#);
        assert!(
            out.iter().any(|(l, s)| *l == Level::Warn && s.contains("Failed to load animation - sanitized ID: invalid://missing-clip")),
            "{out:?}"
        );
    }

    #[test]
    fn part_velocity_includes_angular_lever_arm() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let part = d.dom.insert(
            ws,
            InstanceBuilder::new("Part")
                .with_name("Corner")
                .with_property("CFrame", Variant::CFrame(rbx_types::CFrame::new(
                    rbx_types::Vector3::new(10.0, 2.0, 0.0),
                    rbx_types::Matrix3::identity(),
                ))),
        );
        let sss = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("Probe")
                .with_property(
                    "Source",
                    r#"
                    local p = workspace:WaitForChild("Corner")
                    local v, av = p.Velocity, p.AssemblyLinearVelocity
                    print(string.format("VEL %.2f %.2f %.2f | %.2f %.2f %.2f", v.X, v.Y, v.Z, av.X, av.Y, av.Z))
                    "#
                    .to_string(),
                ),
        );
        let doc_id = d.id;
        let mut w = World::new(d, RunMode::Run);
        w.velocities.insert(part, Vec3::new(1.0, 2.0, 3.0));
        let mut stats = rustudio_physics::physics::live::Stats::default();
        // spin = (0, 0, 2) rad/s about COM (0, 2, 0), part at (10, 2, 0) => spin x r = (0, 20, 0)
        stats.spin.insert(part, (Vec3::new(0.0, 0.0, 2.0), Vec3::new(0.0, 2.0, 0.0)));
        rustudio_physics::physics::live::publish(doc_id, stats);
        let rt = Runtime::new().unwrap();
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script(s);
        rt.step(1.0 / 60.0);
        let out = rt.leave().output;
        rustudio_physics::physics::live::publish(doc_id, Default::default());
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "VEL"), "1.00 22.00 3.00 | 1.00 2.00 3.00", "{out:?}");
    }

    #[test]
    fn get_player_by_user_id_coerces_numeric_strings() {
        let out = run(
            |d| {
                let players = d.find_child(d.dom.root_ref(), "Players").unwrap();
                d.dom.insert(
                    players,
                    InstanceBuilder::new("Player")
                        .with_name("Alice")
                        .with_property("UserId", 111067068i64),
                );
            },
            r#"
            local Players = game:GetService("Players")
            local p1 = Players:GetPlayerByUserId(111067068)
            local p2 = Players:GetPlayerByUserId("111067068")
            local p3 = Players:GetPlayerByUserId("not-a-number")
            print("RES", p1 and p1.Name, p2 and p2.Name, tostring(p3))
            "#,
        );
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "RES"), "Alice Alice nil", "{out:?}");
    }

    #[test]
    fn collection_service_get_tagged_excludes_non_datamodel() {
        let out = run(
            |_| {},
            r#"
            local CS = game:GetService("CollectionService")
            local p = Instance.new("Part")
            CS:AddTag(p, "Coin")
            local n0 = #CS:GetTagged("Coin")
            p.Parent = workspace
            local n1 = #CS:GetTagged("Coin")
            p.Parent = nil
            local n2 = #CS:GetTagged("Coin")
            p.Parent = workspace
            local n3 = #CS:GetTagged("Coin")
            p:Destroy()
            local n4 = #CS:GetTagged("Coin")
            print("COUNTS", n0, n1, n2, n3, n4)
            "#,
        );
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "COUNTS"), "0 1 0 1 0", "{out:?}");
    }

    #[test]
    fn require_module_error_prints_stack_trace() {
        let out = run(
            |d| {
                let rs = d.find_child(d.dom.root_ref(), "ReplicatedStorage").unwrap();
                d.dom.insert(
                    rs,
                    InstanceBuilder::new("ModuleScript")
                        .with_name("Inner")
                        .with_property("Source", "local x = 1\nerror('boom inner')\nreturn x"),
                );
                d.dom.insert(
                    rs,
                    InstanceBuilder::new("ModuleScript")
                        .with_name("Outer")
                        .with_property("Source", "local i = require(script.Parent.Inner)\nreturn i"),
                );
            },
            r#"
            require(game.ReplicatedStorage.Outer)
            "#,
        );
        let texts: Vec<&str> = out.iter().map(|(_, s)| s.as_str()).collect();
        assert!(
            texts.iter().any(|s| s.contains("Script 'ReplicatedStorage.Inner', Line 2")),
            "missing inner module stack frame: {texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("Script 'ReplicatedStorage.Outer', Line 1")),
            "missing outer module stack frame: {texts:?}"
        );
    }

    #[test]
    fn remote_event_queues_invocations_before_connect_and_flushes_fifo() {
        let mut d = Document::new_place();
        let rs = d.find_child(d.dom.root_ref(), "ReplicatedStorage").unwrap();
        let reliable = d.dom.insert(
            rs,
            InstanceBuilder::new("RemoteEvent").with_name("Reliable"),
        );
        let unreliable = d.dom.insert(
            rs,
            InstanceBuilder::new("UnreliableRemoteEvent").with_name("Unreliable"),
        );
        let cap_re = d.dom.insert(
            rs,
            InstanceBuilder::new("RemoteEvent").with_name("Capped"),
        );
        let script = d.dom.insert(
            rs,
            InstanceBuilder::new("LocalScript")
                .with_name("Client")
                .with_property(
                    "Source",
                    r#"
                    local RS = game:GetService("ReplicatedStorage")
                    local reliable = RS.Reliable
                    local unreliable = RS.Unreliable
                    local capped = RS.Capped

                    local log = {}
                    reliable.OnClientEvent:Once(function(v)
                        table.insert(log, "once:" .. tostring(v))
                    end)
                    task.spawn(function()
                        local v = reliable.OnClientEvent:Wait()
                        table.insert(log, "wait:" .. tostring(v))
                    end)
                    local c1
                    c1 = reliable.OnClientEvent:Connect(function(v)
                        table.insert(log, "c1:" .. tostring(v))
                        assert(c1 ~= nil, "connection handle should be assigned before callback")
                    end)
                    reliable.OnClientEvent:Connect(function(v)
                        table.insert(log, "c2:" .. tostring(v))
                    end)

                    local uCount = 0
                    unreliable.OnClientEvent:Connect(function()
                        uCount += 1
                    end)

                    local capCount = 0
                    capped.OnClientEvent:Connect(function()
                        capCount += 1
                    end)

                    task.wait(0.05)
                    print("LOG", table.concat(log, ","))
                    print("UNRELIABLE", uCount)
                    print("CAPPED", capCount)
                    "#,
                ),
        );

        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Play);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();

        let nomap = |_: Ref| None;
        // Deliver 4 invocations to reliable before any listener connects.
        for n in 1..=4 {
            rt.deliver_event(reliable, "OnClientEvent", None, &[Xfer::Num(n as f64)], &nomap);
        }
        // UnreliableRemoteEvent must drop pre-connect invocations.
        rt.deliver_event(unreliable, "OnClientEvent", None, &[Xfer::Num(99.0)], &nomap);
        // Overflow queue past REMOTE_QUEUE_LIMIT (256) to check cap + single warning.
        for n in 0..(REMOTE_QUEUE_LIMIT + 10) {
            rt.deliver_event(cap_re, "OnClientEvent", None, &[Xfer::Num(n as f64)], &nomap);
        }

        rt.run_script(script);
        rt.dispatch();
        // Now deliver a live 5th invocation; both c1 and c2 should see it.
        rt.deliver_event(reliable, "OnClientEvent", None, &[Xfer::Num(5.0)], &nomap);
        rt.dispatch();
        for _ in 0..10 {
            rt.step(1.0 / 60.0);
        }
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(
            line(&out, "LOG"),
            "once:1,wait:2,c1:3,c1:4,c1:5,c2:5",
            "{out:?}"
        );
        assert_eq!(line(&out, "UNRELIABLE"), "0", "{out:?}");
        assert_eq!(line(&out, "CAPPED"), REMOTE_QUEUE_LIMIT.to_string(), "{out:?}");
        let warns: Vec<&str> = out
            .iter()
            .filter(|(l, _)| *l == Level::Warn)
            .map(|(_, s)| s.as_str())
            .collect();
        assert_eq!(warns.len(), 1, "expected exactly one overflow warning: {warns:?}");
        assert!(warns[0].contains("Remote event invocation queue exhausted"), "{warns:?}");
    }

    #[test]
    fn key_code_string_maps_symbols_and_letters() {
        let out = run(|_| {}, r#"
            local UIS = game:GetService("UserInputService")
            local cases = {
                { Enum.KeyCode.Slash, "/" },
                { Enum.KeyCode.BackSlash, "\\" },
                { Enum.KeyCode.Semicolon, ";" },
                { Enum.KeyCode.Quote, "'" },
                { Enum.KeyCode.Comma, "," },
                { Enum.KeyCode.Period, "." },
                { Enum.KeyCode.LeftBracket, "[" },
                { Enum.KeyCode.RightBracket, "]" },
                { Enum.KeyCode.Backquote, "`" },
                { Enum.KeyCode.Tilde, "~" },
                { Enum.KeyCode.Minus, "-" },
                { Enum.KeyCode.Equals, "=" },
                { Enum.KeyCode.A, "A" },
                { Enum.KeyCode.One, "1" },
                { Enum.KeyCode.KeypadPlus, "+" },
                { Enum.KeyCode.Escape, "" },
            }
            for _, c in cases do
                local got = UIS:GetStringForKeyCode(c[1])
                assert(got == c[2], c[1].Name .. " expected '" .. c[2] .. "' got '" .. tostring(got) .. "'")
            end
            print("OK")
        "#);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "OK"), "", "{out:?}");
    }

    #[test]
    fn prop_ref_keeps_unparented_instance_alive() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        d.dom.insert(root, InstanceBuilder::new("Workspace").with_name("Workspace"));
        let sss = d.dom.insert(root, InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"));
        let s1 = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("S1").with_property(
                "Source",
                r#"
                local btn = Instance.new("TextButton")
                btn.Name = "RefBtn"
                btn.Parent = workspace
                do
                    local sel = Instance.new("ImageLabel")
                    sel.Name = "VRSelector"
                    btn.SelectionImageObject = sel
                end
                "#.to_string(),
            ),
        );
        let s2 = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("S2").with_property(
                "Source",
                r#"
                local btn = workspace:FindFirstChild("RefBtn")
                local kept = btn and btn.SelectionImageObject
                assert(kept ~= nil and kept.Name == "VRSelector", "orphan ref collected")
                print("OK")
                "#.to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Play));
        rt.bind_globals().unwrap();
        rt.run_script(s1);
        rt.lua.gc_collect().unwrap();
        rt.lua.gc_collect().unwrap();
        rt.collect(&|_| false, 1000);
        rt.run_script(s2);
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "OK"), "", "{out:?}");
    }

    #[test]
    fn scrolling_frame_canvas_position_clamps_on_write() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let sg = d.dom.insert(root, InstanceBuilder::new("ScreenGui").with_name("G"));
        let sf = d.dom.insert(
            sg,
            InstanceBuilder::new("ScrollingFrame")
                .with_name("S")
                .with_property("Size", Variant::UDim2(rbx_dom_weak::types::UDim2::new(
                    rbx_dom_weak::types::UDim::new(0.0, 200),
                    rbx_dom_weak::types::UDim::new(0.0, 100),
                )))
                .with_property("CanvasSize", Variant::UDim2(rbx_dom_weak::types::UDim2::new(
                    rbx_dom_weak::types::UDim::new(0.0, 0),
                    rbx_dom_weak::types::UDim::new(0.0, 150),
                ))),
        );
        let s = d.dom.insert(
            sg,
            InstanceBuilder::new("LocalScript").with_name("T").with_property(
                "Source",
                r#"
                local sf = game:FindFirstChild("S", true)
                sf.CanvasPosition = Vector2.new(50, 999)
                assert(sf.CanvasPosition == Vector2.new(0, 50), tostring(sf.CanvasPosition))
                sf.CanvasSize = UDim2.fromOffset(0, 60)
                sf.CanvasPosition = Vector2.new(0, 500)
                assert(sf.CanvasPosition == Vector2.new(0, 0), tostring(sf.CanvasPosition))
                print("OK")
                "#.to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Play);
        w.input.gui_layout.insert(
            sf,
            crate::gui::GuiBox {
                pos: [0.0, 0.0],
                size: [200.0, 100.0],
                rot: 0.0,
                text: None,
                text_fits: false,
                canvas: Some([200.0, 150.0]),
                window: Some([200.0, 100.0]),
                canvas_fit: Some(crate::gui::CanvasFit {
                    basis: [200.0, 100.0],
                    floor: [200.0, 100.0],
                    scale: 1.0,
                }),
            },
        );
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.run_script_as(s, security::CORE);
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "OK"), "", "{out:?}");
    }

    #[test]
    fn stats_item_display_name_and_core_action_identity() {
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let stats = d.dom.insert(root, InstanceBuilder::new("Stats").with_name("Stats"));
        let perf = d.dom.insert(stats, InstanceBuilder::new("Folder").with_name("PerformanceStats"));
        d.dom.insert(perf, InstanceBuilder::new("StatsItem").with_name("Memory"));
        let s = d.dom.insert(
            root,
            InstanceBuilder::new("LocalScript").with_name("T").with_property(
                "Source",
                r#"
                local Stats = game:GetService("Stats")
                local CAS = game:GetService("ContextActionService")
                local mem = Stats.PerformanceStats.Memory
                assert(mem.DisplayName == "Memory", tostring(mem.DisplayName))
                local got = nil
                CAS:BindAction("CoreTestAction", function(_, st)
                    if st == Enum.UserInputState.Begin then
                        got = Stats:GetBrowserTrackerId()
                    end
                end, false, Enum.KeyCode.Period)
                __rs_input_event("Begin", "Keyboard", "Period", 0, 0, 0, 0, 0, false)
                assert(got == "0", "got=" .. tostring(got))
                print("OK")
                "#.to_string(),
            ),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Play));
        rt.bind_globals().unwrap();
        rt.run_script_as(s, security::CORE);
        let out = rt.leave().output;
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "OK"), "", "{out:?}");
    }
}
