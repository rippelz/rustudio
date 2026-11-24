//! The GPU renderer: the wgpu viewport renderer with its scene build,
//! lighting, shadow maps, skinning and effects. No mlua, rapier3d or parry3d
//! dependencies. The scene probe
//! that casts rays through the collision meshes is engine's (`env_probe`);
//! its GPU half is [`render::env_probe_gpu`] and stays here.
//!
//! What is not added here: anything that runs Luau or steps physics, editor
//! state, and any reference to `rustudio-engine` or the studio crate — those
//! do not compile. `cargo test -p rustudio-render` and
//! `cargo build -p rustudio-render --example dof_regression` build without
//! the editor.

pub mod quality;
pub mod postfx;
pub mod occlusion;
pub mod render;
pub mod gpu_timing;
pub mod gpu_health;
pub mod gpu_caps;
pub mod pipeline_cache;
pub mod gpu_compat;
pub mod world_gui;
pub mod sun_shadow;
pub mod highlight;
pub mod adorn;
pub mod water;
pub mod clouds;
pub mod skin;
pub mod skin_gpu;
pub mod wrap;
pub mod hsr;
pub mod lightgrid;
pub mod lightocc;
pub mod matpack;
pub mod grass_temporal;
pub mod effects;
pub mod lighting;
pub mod scene;
pub mod camera;
pub mod snapshot;
