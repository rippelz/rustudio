//! rustudio's engine: the Luau runtime, its API domains, replication, GUI
//! objects, audio and rigs. It sits on rustudio-core, -physics, -render and
//! -account. Studio and the player binary use the engine. Studio's plugin API
//! domain is registered into it at start-up (`runtime::domains::register`).
//!
//! What is not added here: anything that only the editor or the player needs,
//! and any reference to `crate::app`, `crate::plugin_host` or a player module
//! — those do not compile. Studio's own pieces arrive through hooks: the
//! plugin domain through `domains::register`, the settings store through
//! `domains::settings_host`, editable content through `editable_source`.
//! `cargo test -p rustudio-engine` builds without egui-wgpu's surface or the
//! editor.

pub mod lua_bridge;
pub mod bytecode_cache;
pub mod env_probe;
pub mod game_avatar;
pub mod cmeta;
pub mod dtmeta;
pub mod audio;
pub mod audio_dsp;
pub mod audio_scene;
pub mod audio_capture;
pub mod avatar;
pub mod gui;
pub mod gui_modal;
pub mod mouse_lock;
pub mod ltypes;
pub mod replicate;
mod replicate_streaming;
pub mod anim_replicate;
pub mod rig;
pub mod runtime;
pub mod play_teleport;
pub mod tool_grip;
pub mod script_profiler;
pub mod globals_parity;
pub mod sharedtable;
pub mod theme;
pub mod ti;

#[cfg(test)]
#[path = "selftest/attachment_runtime.rs"]
mod attachment_runtime;
#[cfg(test)]
#[path = "selftest/bridge.rs"]
mod bridge;
