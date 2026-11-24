//! Physics for rustudio: rapier rigid bodies and collision, the Studio solver
//! port, shape queries and humanoid physics. Plain data in, plain data out: no
//! egui, wgpu, mlua or renderer dependencies.
//! `cargo test -p rustudio-physics` builds without the editor.
//!
//! What is not added here: renderer types, Lua, editor or player state, and
//! any reference to `rustudio-render`, `rustudio-engine` or the studio crate —
//! those do not compile. The simulation facts the engine reads (`physics::live`
//! and the rest) are already here.

pub mod client_joints;
pub mod collision;
pub mod humanoid;
pub mod physics;
pub mod physics_mesh;
pub mod query_shapes;
pub mod solver;
pub mod solver_ticks;
pub mod throttle;
