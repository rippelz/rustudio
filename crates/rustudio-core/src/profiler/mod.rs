//! The profiler's capture engine: nested timed scopes per thread, counters,
//! instant marks and frame boundaries, gathered into a ring of the last few
//! seconds that the profiler window reads and that exports to Chrome trace
//! JSON or the native `.rsprof` format.
//!
//! Recording API (cheap, call from any thread):
//! - `scope(name, cat)` / `scope_dyn(label, cat)`: a guard timing until drop.
//! - `counter(name, cat, value)`: a sampled value (instances, draw calls, …).
//! - `mark(name, cat)`: an instant event.
//! - `frame(kind)`: a frame boundary (the render frame drains the threads).
//! - `thread_name(name)`: this thread's lane name.
//!
//! Everything costs one relaxed atomic load while `active()` is false. While
//! active, a scope reads the clock twice and appends one fixed-size record to
//! its thread's buffer (no allocation once the buffer has grown).
//!
//! Control and reading: `control` (enable, pause, capture next N frames,
//! spike capture, settings) and `with_capture` / `snapshot` hand out the
//! gathered `Capture`; `analysis` turns a capture into frame stats, call
//! trees and hot lists; `export` writes and reads files.

pub mod alloc;
pub mod analysis;
pub mod capture;
pub mod cli;
pub mod control;
pub mod export;
pub mod report;
mod record;

pub use capture::{Capture, CounterTrack, FrameRec, MarkRec, SpanRec, Track, TrackKind, Unit};
pub use control::{
    capture_next, clear, config, disable, enable, frame_at, frozen, is_enabled, lane_span, set_gpu_info, last_spike, load_saved_config, pause, set_and_save_config,
    register_extension, resume, set_build, set_config, set_context, snapshot, status, with_capture, Config, Frozen, Status,
};
pub use record::{
    active, counter, counter_unit, frame, intern, gpu_wanted, label_name, mark, mark_dyn, now_ns, rate, scope, scope_dyn,
    thread_name, Guard, Label,
};

use serde::{Deserialize, Serialize};

/// What a scope, counter or mark is about: one colour per category in the
/// profiler window, shared by every view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Cat {
    /// The frame itself (the outermost scope of a loop iteration).
    Frame = 0,
    /// CPU-side rendering: culling, encoding, uploads.
    Render = 1,
    /// GPU passes (timestamp queries).
    Gpu = 2,
    /// Editor panels and egui.
    Ui = 3,
    /// Physics: broadphase, contacts, solver, joints, humanoids.
    Physics = 4,
    /// Luau: script resumes, event handlers, RunService callbacks.
    Script = 5,
    /// Animation tracks, tweens, skinning poses.
    Animation = 6,
    /// Replication, Team Create, HTTP.
    Network = 7,
    /// Asset downloads, decodes, mesh and texture loading.
    Assets = 8,
    /// Sounds and the audio graph.
    Audio = 9,
    /// Device and user input handling.
    Input = 10,
    /// In-game GUI: layout, text, tessellation.
    Gui = 11,
    /// Pathfinding jobs.
    Pathfinding = 12,
    /// Waiting: vsync, locks, idle, a worker's reply.
    Wait = 13,
    Other = 14,
    /// Not chosen at the call site (the old `bench::scope` names): resolved
    /// from the name when the capture is gathered (`Cat::from_name`).
    Auto = 255,
}

impl Cat {
    /// Every real category, in display order.
    pub const ALL: [Cat; 15] = [
        Cat::Frame,
        Cat::Render,
        Cat::Gpu,
        Cat::Ui,
        Cat::Physics,
        Cat::Script,
        Cat::Animation,
        Cat::Network,
        Cat::Assets,
        Cat::Audio,
        Cat::Input,
        Cat::Gui,
        Cat::Pathfinding,
        Cat::Wait,
        Cat::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Cat::Frame => "Frame",
            Cat::Render => "Render",
            Cat::Gpu => "GPU",
            Cat::Ui => "UI",
            Cat::Physics => "Physics",
            Cat::Script => "Script",
            Cat::Animation => "Animation",
            Cat::Network => "Network",
            Cat::Assets => "Assets",
            Cat::Audio => "Audio",
            Cat::Input => "Input",
            Cat::Gui => "GUI",
            Cat::Pathfinding => "Pathfinding",
            Cat::Wait => "Wait",
            Cat::Other => "Other",
            Cat::Auto => "Other",
        }
    }

    pub fn from_u8(v: u8) -> Cat {
        Cat::ALL.get(v as usize).copied().unwrap_or(Cat::Other)
    }

    /// The default colour (sRGB 0xRRGGBB): muted, distinct, readable on the
    /// editor's dark panels. The window lets the user change each one.
    pub fn default_rgb(self) -> u32 {
        // (the profiler window's approved palette)
        match self {
            Cat::Frame => 0x6c7280,
            Cat::Render => 0x5b8def,
            Cat::Gpu => 0x8a76e8,
            Cat::Ui => 0xd474b0,
            Cat::Physics => 0xe0a43c,
            Cat::Script => 0x4fb87a,
            Cat::Animation => 0x3fb6c4,
            Cat::Network => 0xd0805e,
            Cat::Assets => 0xa7a663,
            Cat::Audio => 0xb48ad6,
            Cat::Input => 0x8fa3b5,
            Cat::Gui => 0xe7a0cf,
            Cat::Pathfinding => 0x7fae6b,
            Cat::Wait => 0x3a3d46,
            Cat::Other => 0x7d838c,
            Cat::Auto => 0x7d838c,
        }
    }

    /// The category of an engine section named without one (the names the
    /// bench's `scope` has always used).
    pub fn from_name(name: &str) -> Cat {
        const RULES: &[(&str, Cat)] = &[
            ("frame", Cat::Frame),
            // physics
            ("physics", Cat::Physics),
            ("ph_", Cat::Physics),
            ("sv_", Cat::Physics),
            ("pc_", Cat::Physics),
            ("pj_", Cat::Physics),
            ("pb_", Cat::Physics),
            ("pt_", Cat::Physics),
            ("ps_", Cat::Physics),
            ("p1_physics", Cat::Physics),
            ("p1b_", Cat::Physics),
            ("p3_physics", Cat::Physics),
            ("p3b_", Cat::Physics),
            ("p3a_", Cat::Physics),
            ("p6e_", Cat::Physics),
            ("p6d_", Cat::Physics),
            ("p2_", Cat::Physics),
            ("ray_", Cat::Physics),
            ("hum_", Cat::Physics),
            ("npc_", Cat::Physics),
            ("joint_graph", Cat::Physics),
            ("cg_", Cat::Physics),
            ("control_", Cat::Physics),
            ("collision", Cat::Physics),
            ("gameplay_publish_wait", Cat::Wait),
            ("physics_pose_wait", Cat::Wait),
            ("render_thread_wait", Cat::Wait),
            // scripts
            ("rt_", Cat::Script),
            ("dis_", Cat::Script),
            ("script", Cat::Script),
            ("p4_server_lua", Cat::Script),
            ("p6_client_lua", Cat::Script),
            ("p6b_lua", Cat::Script),
            ("p2a_lua", Cat::Script),
            ("p2b_lua", Cat::Script),
            ("p6b_rt_step", Cat::Script),
            ("p9_", Cat::Script),
            ("plugin", Cat::Script),
            ("lua", Cat::Script),
            ("dstep_gui_input", Cat::Input),
            ("dstep_media", Cat::Audio),
            ("dstep_anim", Cat::Animation),
            ("dstep_assets", Cat::Assets),
            ("dstep_web", Cat::Network),
            ("dstep_data", Cat::Network),
            ("dstep_", Cat::Script),
            // animation
            ("anim", Cat::Animation),
            ("tween", Cat::Animation),
            ("skin_", Cat::Animation),
            // network
            ("rep_", Cat::Network),
            ("p5_replicate", Cat::Network),
            ("p6c_", Cat::Network),
            ("p6b_mirror", Cat::Network),
            ("net", Cat::Network),
            ("tc_", Cat::Network),
            ("http", Cat::Network),
            // game gui
            ("gui_", Cat::Gui),
            ("p6b_layout", Cat::Gui),
            // audio
            ("media", Cat::Audio),
            ("audio", Cat::Audio),
            ("sound", Cat::Audio),
            // assets
            ("asset", Cat::Assets),
            ("tex_", Cat::Assets),
            ("mesh_", Cat::Assets),
            ("load_", Cat::Assets),
            // input
            ("loop_device_event", Cat::Input),
            ("loop_user_event", Cat::Input),
            ("input", Cat::Input),
            // editor ui
            ("explorer", Cat::Ui),
            ("properties", Cat::Ui),
            ("output", Cat::Ui),
            ("dock", Cat::Ui),
            ("menu", Cat::Ui),
            ("editor_", Cat::Ui),
            ("test_toolbar", Cat::Ui),
            ("panel", Cat::Ui),
            ("marquee", Cat::Ui),
            ("viewport_pick", Cat::Ui),
            ("viewport_overlays", Cat::Ui),
            ("egui", Cat::Ui),
            ("ribbon", Cat::Ui),
            ("native_redraw", Cat::Ui),
            ("loop_window_event", Cat::Input),
            ("ui_", Cat::Ui),
            // pathfinding
            ("path", Cat::Pathfinding),
            ("nav", Cat::Pathfinding),
            // render
            ("render", Cat::Render),
            ("r_", Cat::Render),
            ("vp_", Cat::Render),
            ("vf_", Cat::Render),
            ("viewport", Cat::Render),
            ("fx", Cat::Render),
            ("sd_", Cat::Render),
            ("lt_", Cat::Render),
            ("light", Cat::Render),
            ("ss_", Cat::Render),
            ("scene", Cat::Render),
            ("terrain", Cat::Render),
            ("env_probe", Cat::Render),
            ("pl_static", Cat::Render),
            ("present", Cat::Render),
            ("gpu", Cat::Gpu),
            ("wait", Cat::Wait),
            ("idle", Cat::Wait),
            ("loop_about_to_wait", Cat::Wait),
            ("loop_plugins_idle", Cat::Script),
            ("gameplay_step", Cat::Script),
            ("play_tick", Cat::Script),
            ("p6a_prep", Cat::Script),
        ];
        RULES.iter().find(|(p, _)| name.starts_with(p)).map(|r| r.1).unwrap_or(Cat::Other)
    }
}

/// Which loop a frame boundary closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum FrameKind {
    /// The display loop (editor or player). Closing one gathers every
    /// thread's records into the capture.
    Render = 0,
    /// One physics step.
    Physics = 1,
    /// One gameplay tick (Heartbeat).
    Heartbeat = 2,
    /// The GPU's work for one display frame (from timestamp queries).
    Gpu = 3,
}

impl FrameKind {
    pub const ALL: [FrameKind; 4] = [FrameKind::Render, FrameKind::Physics, FrameKind::Heartbeat, FrameKind::Gpu];

    pub fn name(self) -> &'static str {
        match self {
            FrameKind::Render => "Render frame",
            FrameKind::Physics => "Physics step",
            FrameKind::Heartbeat => "Heartbeat",
            FrameKind::Gpu => "GPU frame",
        }
    }
}

#[cfg(test)]
mod tests;
