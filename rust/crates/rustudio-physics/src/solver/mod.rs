//! The Studio solver port: `RUSTUDIO_SOLVER` unset / `studio` (the default) or
//! `studio-nextgen`; `RUSTUDIO_SOLVER=rapier` is the old physics.
//!
//! Two Studio solvers share this module (Studio observations 2026-11-13):
//! * `studio`: the legacy solver Studio 0.747 runs by default
//!   (workspace.NextGenerationPhysicsSolver = Default): "Solve Batch" Studio,
//!   dual LDL + PGS, rows from connector vtbl[10] (dual.rs; lanes DUAL, DUALROWS,
//!   JOINT slot 10). This is the port's first target.
//! * `studio-nextgen`: the primal solver being rolled out (rows from vtbl[11],
//!   LINSYS assembly + sparse LDL, ITER Newton loop; lanes CONTACT, LINSYS, ITER).
//! Shared: bodies, contact state (feature-id keyed connectors), joints, islands,
//! external forces, integration and sleep (STEP).
//!
//! What it replaces: rapier's island build, constraint solver and integration.
//! What it keeps: rapier / parry's broad and narrow phase (contact manifolds with
//! feature ids for every collider pair inside the prediction margin), run through
//! rapier's `CollisionPipeline` (no solver, no islands, no CCD).
//!
//! One world tick (`StudioSolver::step`, step.rs), every stage a fixed interface:
//!
//! ```text
//!  1 collide      rapier CollisionPipeline   -> NarrowPhase (pairs, manifolds, fid1/fid2)
//!  2 gather       BodySet::gather            -> SoA of every dynamic / kinematic body
//!  3 contacts     ContactStore::update       -> ContactList (one entry per manifold point,
//!                                               persistent state matched by pair + feature ids)
//!  4 joints       JointList::gather          -> one entry per rapier impulse joint
//!  5 islands      Islands::build             -> islands of awake bodies (union-find) + levels
//!  6 per island:  external (STEP)            -> velocities with the tick's forces
//!     studio:     rows::*_dual (DUALROWS, JOINT) -> RowBuffer
//!                 DualSolver::solve_batch (DUAL)  -> velocities, lambda
//!                 dual::write_back_dual (DUALROWS)
//!     nextgen:    rows::*_primal (CONTACT, JOINT) -> RowBuffer
//!                 LinSys::assemble / factor (LINSYS), iterate (ITER)
//!                 contact::write_back (CONTACT)
//!  7 integrate    step::integrate (STEP)     -> poses
//!  8 scatter      BodySet::scatter           -> rapier bodies + collider poses
//!  9 sleep        step::sleep (STEP)
//! ```
//!
//! Every solver decision that the reverse-engineering lanes have not specified yet
//! is a `todo_spec!("LANE §n …")` hook naming the spec section it waits for
//! (checks/humanoid-physics/solver/<LANE>.md). `RUSTUDIO_SOLVER_STRICT=1` makes the
//! first hook reached panic; otherwise each hook site prints `SOLVER-TODO` once.
//! `grep -rn todo_spec src/solver` lists what is left. See solver/IMPLEMENT.md.

#![allow(dead_code)] // the skeleton's interfaces are for the implementation round

pub mod body;
pub mod codec;
pub mod contact;
pub mod dual;
pub mod island;
pub mod joints;
pub mod linsys;
pub mod mass;
pub mod narrow;
pub mod narrow_poly;
pub mod pose;
pub mod rows;
pub mod sleep;
pub mod step;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

pub use step::StudioSolver;

pub use rustudio_core::rustudio_settings::SolverKind;

/// The solver of this Play session: `RUSTUDIO_SOLVER`, else the saved
/// rustudio option, else studio (crates/rustudio-core/src/rustudio_settings.rs), re-read at each
/// Play start.
pub fn kind() -> SolverKind {
    rustudio_core::rustudio_settings::solver()
}

pub fn strict() -> bool {
    static S: OnceLock<bool> = OnceLock::new();
    *S.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_STRICT").is_some())
}

/// A hook site reached: printed once per site (`SOLVER-TODO site spec`), a panic
/// under RUSTUDIO_SOLVER_STRICT.
#[cold]
pub fn hook_hit(seen: &AtomicBool, site: &'static str, spec: &'static str) {
    if strict() {
        panic!("solver hook reached at {site}: waits for {spec}");
    }
    if !seen.swap(true, Ordering::Relaxed) {
        eprintln!("SOLVER-TODO {site} {spec}");
    }
}

/// `todo_spec!("CONTACT §3 primal rows")` marks a solver decision that waits for a
/// spec section. As a statement it only reports the site (once). With a value,
/// `todo_spec!("STEP §5 integration", expr)` evaluates to `expr`, the harness's
/// stand-in, so the pipeline still runs end to end; the stand-in is NOT Studio's
/// behaviour and must be replaced from the spec.
#[macro_export]
macro_rules! todo_spec {
    ($spec:literal) => {{
        static SEEN: ::std::sync::atomic::AtomicBool = ::std::sync::atomic::AtomicBool::new(false);
        if !SEEN.load(::std::sync::atomic::Ordering::Relaxed) || $crate::solver::strict() {
            $crate::solver::hook_hit(&SEEN, concat!(file!(), ":", line!()), $spec);
        }
    }};
    ($spec:literal, $stand_in:expr) => {{
        $crate::todo_spec!($spec);
        $stand_in
    }};
}

/// Constants of the solver, one field per spec value (live flag or .rdata).
/// The legacy path's per-batch config (cfg, Studio rescaled by
/// Studio for the batch's level) is `dual::LevelCfg`.
#[derive(Debug, Clone)]
pub struct Config {
    /// World tick (s): Studio's 1/240 (Studio -> .rdata Studio = 0.0041666669).
    pub h: f32,
    /// Largest number of contact connectors per pair: FeaturePoly keeps up to 8
    /// (CONTACT §2.1 / §2.2 step 9).
    pub max_points: usize,
    /// maxLevel = Studio() = 4 (8 only with SimAdaptiveEnableEightDt, live
    /// false; STEP terms).
    pub max_level: u8,
    /// SimTimestepMultiplierDebounceCount, live 60 (STEP §9; compiled 100).
    pub debounce_count: i32,
    /// SimArbiterPositionCorrectionFactorThresholdThou live 300 -> 0.3 (STEP §3.3 step 1).
    pub pcf_threshold: f32,
    /// TimestepArbiterConditionNumberThresholdThou live 50000 -> 50.0 (STEP §3.3a).
    pub cond_threshold: f32,
    /// SimPositionalCorrectionScaleThou 50 -> 0.05 (STEP §4.2b step 4).
    pub pos_corr_scale: f32,
    /// SimArbiterCFLLengthScaleThou 5000 -> 5.0 (STEP §4.2b step 6).
    pub cfl_length_scale: f32,
    /// SimAdaptiveEnable60HzParts live on: slot caps 4 instead of 2 (STEP §3.3).
    pub enable_60hz: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            h: 1.0 / 240.0,
            max_points: contact::MAX_POINTS,
            max_level: 4,
            debounce_count: 60,
            pcf_threshold: 0.3,
            cond_threshold: 50.0,
            pos_corr_scale: 0.05,
            cfl_length_scale: 5.0,
            enable_60hz: true,
        }
    }
}

/// What one step saw: printed by the perf script, checked by the harness.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepStats {
    pub bodies: u32,
    pub awake: u32,
    pub kinematic: u32,
    pub contact_pairs: u32,
    pub manifolds: u32,
    pub points: u32,
    pub joints: u32,
    pub islands: u32,
    pub largest_island: u32,
    pub rows: u32,
}

/// RUSTUDIO_SOLVER_STATS=1: what the world holds after each step, counted the
/// same way under either solver (the perf budget's denominators), printed as a
/// `SOLVERSTATS` line (averages per step) every 240 steps.
pub struct Census {
    /// Worlds in creation order (Play: the server's, then the client's).
    id: usize,
    bodies: body::BodySet,
    contacts: contact::ContactStore,
    joints: joints::JointList,
    islands: island::Islands,
    no_names: rustc_hash::FxHashMap<rapier3d::prelude::ImpulseJointHandle, rbx_dom_weak::types::Ref>,
    steps: u64,
    sum: [f64; 8],
    hist: [u64; 12],
}

impl Default for Census {
    fn default() -> Self {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        Census {
            id: N.fetch_add(1, Ordering::Relaxed),
            bodies: Default::default(),
            contacts: Default::default(),
            joints: Default::default(),
            islands: Default::default(),
            no_names: Default::default(),
            steps: 0,
            sum: [0.0; 8],
            hist: [0; 12],
        }
    }
}

impl Census {
    /// The bench section of this world's steps.
    pub fn section(&self) -> &'static str {
        ["ph_step_w0", "ph_step_w1", "ph_step_w2", "ph_step_w3"][self.id.min(3)]
    }

    pub fn observe(&mut self, world: &rapier3d::prelude::PhysicsWorld, studio: Option<&body::BodySet>) {
        self.bodies.gather(&world.bodies, None);
        self.contacts.update(&self.bodies, &world.bodies, &world.colliders, &world.narrow_phase, &Default::default());
        self.joints.gather(&world.impulse_joints, &self.bodies, &self.no_names);
        self.islands.build(&mut self.bodies, &self.contacts.list, &self.joints);
        let both = body::flag::AWAKE | body::flag::DYNAMIC;
        let awake = match studio {
            // the Studio solver's own slots: AWAKE = taking part in its last solve (sleep.rs)
            Some(b) => (0..b.len()).filter(|&s| b.flags[s] & both == both).count(),
            None => (0..self.bodies.len()).filter(|&s| self.bodies.flags[s] & both == both).count(),
        };
        let largest = (0..self.islands.len()).map(|i| self.islands.start[i + 1] - self.islands.start[i]).max().unwrap_or(0);
        let v = [
            self.bodies.len() as f64,
            awake as f64,
            self.contacts.list.pairs as f64,
            self.contacts.list.manifold_count() as f64,
            self.contacts.list.len() as f64,
            self.joints.len() as f64,
            self.islands.len() as f64,
            largest as f64,
        ];
        for (s, x) in self.sum.iter_mut().zip(v) {
            *s += x;
        }
        for (h, x) in self.hist.iter_mut().zip(self.islands.histogram()) {
            *h += x as u64;
        }
        self.steps += 1;
        if self.steps % 240 == 0 {
            let n = self.steps as f64;
            let a: Vec<String> = self.sum.iter().map(|x| format!("{:.1}", x / n)).collect();
            let h: Vec<String> = self.hist.iter().map(|x| format!("{:.1}", *x as f64 / n)).collect();
            eprintln!(
                "SOLVERSTATS w{} steps {} bodies {} awake {} pairs {} manifolds {} points {} joints {} islands {} largest {} hist {}",
                self.id, self.steps, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], h.join(",")
            );
        }
    }
}
