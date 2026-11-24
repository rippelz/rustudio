//! The world tick under `RUSTUDIO_SOLVER=studio` (see mod.rs for the stages; the
//! order is STEP.md §1.2 / §1.4, the legacy batch §1.4 step 5 [STEP2]).

use glam::{Mat3, Quat, Vec3};
use rapier3d::prelude as rp;
use rbx_dom_weak::types::Ref;
use rustc_hash::FxHashMap;

use super::body::{flag, BodySet, NONE};
use super::contact::{self, ContactStore};
use super::dual::{self, BodyRec, ConnSrc, DualBatch, LevelCfg, MassRec};
use super::island::Islands;
use super::joints::JointList;
use super::linsys::LinSys;
use super::rows::{self, LocalIndex, RowBuffer};
use super::{Config, SolverKind, StepStats};

/// A queued level change {body slot, type (1 island level, 3 arbiter), L}
/// (STEP §2.3, §3.2; applied by Studio, §3.6).
#[derive(Debug, Clone, Copy)]
struct LevelChange {
    idx: u32,
    gen: u32,
    ty: u8,
    level: u8,
}

pub struct StudioSolver {
    /// Which Studio solver: `Studio` = the legacy one (dual LDL + PGS, what 0.747
    /// runs at NextGenerationPhysicsSolver = Default), `StudioNextGen` = primal.
    pub path: SolverKind,
    /// This tick belongs to a throttled 60 Hz frame (Workspace.PhysicsEnvironmentalThrottle,
    /// throttle.rs): only the bodies in `throttle_exempt` (Humanoid assemblies) step.
    pub throttled: bool,
    pub throttle_exempt: rustc_hash::FxHashSet<rp::RigidBodyHandle>,
    /// An environment catch-up tick at this phase (physics.rs `catch_up_environment`): the
    /// reverse of a throttled tick. The bodies in `throttle_exempt` (Humanoid assemblies,
    /// the client's stand-ins) are held still and everything else steps the tick a
    /// hurried frame left it; `ticks` (the Humanoids' phase) does not advance.
    pub env_phase: Option<u8>,
    pub cfg: Config,
    pub bodies: BodySet,
    pub contacts: ContactStore,
    pub joints: JointList,
    pub islands: Islands,
    pub rows: RowBuffer,
    pub local: LocalIndex,
    /// Primal (next-gen) system.
    pub sys: LinSys,
    /// Legacy path: one working set per worker, the due islands, the split.
    workers: Vec<Worker>,
    due: Vec<u32>,
    cuts: Vec<usize>,
    pub stats: StepStats,
    /// `stats` of the last tick that was not throttled (the pool decision's input).
    pub full_stats: StepStats,
    /// Which Roblox constraint each rapier joint was made for (physics_joints.rs).
    pub joint_instances: FxHashMap<rp::ImpulseJointHandle, Ref>,
    /// Velocities scripts gave anchored parts, by collider: (linear, angular, part
    /// centre) — the non-sim record of a conveyor (STEP §7a step 5). Filled by
    /// physics.rs every tick.
    pub anchored_vel: FxHashMap<rp::ColliderHandle, (Vec3, Vec3, Vec3)>,
    /// World ticks stepped (the phase p = ticks mod maxLevel, world).
    pub ticks: u64,
    /// Calls of `step` (harness skips count here, not in `ticks`).
    calls: u64,
    queue: Vec<LevelChange>,
    /// Assembly sleep (sleep.rs, SLEEP.md).
    pub sleep: super::sleep::SleepStage,
    /// Part serials (DOM order at load, then creation order) and each collider's: the
    /// stand-in for the primitive id at prim that orders a contact's A / B (GEOM §1.2).
    part_serial: FxHashMap<Ref, u64>,
    pub prim_id: FxHashMap<rp::ColliderHandle, u64>,
    /// Collider index → (generation, narrow.rs class or 0): the `external_contacts` hook's test.
    shape_class: Vec<(u32, u8)>,
    /// Cost of each stage of the last ticks (bench::measure units: ms, or M
    /// instructions under RUSTUDIO_BENCH_INSTR), taken by `take_costs` on the
    /// calling thread (bench sections are per thread). Order: STAGES.
    pub costs: [f64; 5],
}

impl StudioSolver {
    pub fn new(path: SolverKind) -> Self {
        StudioSolver {
            path,
            throttled: false,
            throttle_exempt: Default::default(),
            env_phase: None,
            cfg: Default::default(),
            bodies: Default::default(),
            contacts: Default::default(),
            joints: Default::default(),
            islands: Default::default(),
            rows: Default::default(),
            local: Default::default(),
            sys: Default::default(),
            workers: Vec::new(),
            due: Vec::new(),
            cuts: Vec::new(),
            stats: Default::default(),
            full_stats: Default::default(),
            joint_instances: Default::default(),
            anchored_vel: Default::default(),
            ticks: 0,
            calls: 0,
            queue: Vec::new(),
            sleep: Default::default(),
            part_serial: Default::default(),
            prim_id: Default::default(),
            shape_class: Vec::new(),
            costs: [0.0; 5],
        }
    }
}

struct Harness {
    skip: Vec<u64>,
    phase: u64,
}

fn harness() -> &'static Harness {
    static H: std::sync::OnceLock<Harness> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        let mut skip: Vec<u64> = std::env::var("RUSTUDIO_SOLVER_SKIP").unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
        skip.sort_unstable();
        let phase = std::env::var("RUSTUDIO_SOLVER_PHASE").ok().and_then(|x| x.parse().ok()).unwrap_or(0);
        Harness { skip, phase }
    })
}

/// RUSTUDIO_SOLVER_HASH=1 (determinism check): after every tick, one line per solver
/// instance: `SVH <instance> <tick> <bodies> <hash in slot order> <hash of the sorted
/// states>`; RUSTUDIO_SOLVER_HASH=T (or T1-T2) also lists every body at those ticks.
fn state_hash(s: &StudioSolver) {
    static H: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    let Some((at, to)) = *H.get_or_init(|| {
        std::env::var("RUSTUDIO_SOLVER_HASH").ok().map(|v| match v.split_once('-') {
            Some((a, b)) => (a.parse().unwrap_or(u64::MAX), b.parse().unwrap_or(0)),
            None => {
                let a = v.parse().unwrap_or(u64::MAX);
                (a, a)
            }
        })
    }) else {
        return;
    };
    use std::hash::{Hash, Hasher};
    let b = &s.bodies;
    let mut each: Vec<(u64, usize)> = Vec::with_capacity(b.len());
    for i in 0..b.len() {
        let mut h = rustc_hash::FxHasher::default();
        for x in [b.pos[i], b.linvel[i], b.angvel[i]] {
            x.to_array().map(f32::to_bits).hash(&mut h);
        }
        b.rot[i].to_array().map(f32::to_bits).hash(&mut h);
        each.push((h.finish(), i));
    }
    let mut ord = rustc_hash::FxHasher::default();
    each.iter().for_each(|e| e.0.hash(&mut ord));
    let mut sorted: Vec<u64> = each.iter().map(|e| e.0).collect();
    sorted.sort_unstable();
    let mut un = rustc_hash::FxHasher::default();
    sorted.hash(&mut un);
    let id = s as *const _ as usize % 100_000;
    eprintln!("SVH {id} {} {} {:016x} {:016x}", s.ticks, b.len(), ord.finish(), un.finish());
    if (at..=to).contains(&s.ticks) {
        for i in 0..b.len() {
            eprintln!("SVB {id} {} {i} {:?} {:?} {:?} {:?} lvl {} im {}", s.ticks, b.pos[i], b.linvel[i], b.angvel[i], b.rot[i], b.persist[b.handle[i].into_raw_parts().0 as usize].level, b.inv_mass[i]);
        }
    }
}

/// RUSTUDIO_SOLVER_HASH (lane DEATH): before each tick, one `SVI <instance> <tick> <colliders>
/// <hash of every collider's pose and enabled flag> <hash of every body's pose, velocities
/// and external force / torque>` line: the tick's inputs, so a run-to-run split can be told
/// from the inputs (a collider an Animator moved, a Humanoid's force) or from the step.
fn input_hash(s: &StudioSolver, world: &rp::PhysicsWorld) {
    if std::env::var_os("RUSTUDIO_SOLVER_HASH").is_none() {
        return;
    }
    use std::hash::{Hash, Hasher};
    let mut hc = rustc_hash::FxHasher::default();
    let mut n = 0usize;
    let list = {
        static R: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
        let (a, b) = *R.get_or_init(|| match std::env::var("RUSTUDIO_SOLVER_HASH").ok().as_deref().and_then(|v| v.split_once('-')) {
            Some((a, b)) => (a.parse().unwrap_or(u64::MAX), b.parse().unwrap_or(0)),
            None => (u64::MAX, 0),
        });
        (a..=b).contains(&s.ticks)
    };
    let id = s as *const _ as usize % 100_000;
    for (h, c) in world.colliders.iter() {
        // Colliders on moving bodies only: fixed ones change handles between runs
        // (mesh shapes rebuilt) without moving anything.
        let Some(pb) = c.parent().filter(|b| world.bodies.get(*b).is_some_and(|b| !b.is_fixed())) else { continue };
        pb.into_raw_parts().hash(&mut hc);
        let p = c.position();
        if list {
            eprintln!("SVC {id} {} {:?} {:?} {:?} {:?} {}", s.ticks, pb.into_raw_parts(), s.prim_id.get(&h), p.translation, p.rotation, c.is_enabled());
        }
        [p.translation.x, p.translation.y, p.translation.z].map(f32::to_bits).hash(&mut hc);
        [p.rotation.x, p.rotation.y, p.rotation.z, p.rotation.w].map(f32::to_bits).hash(&mut hc);
        c.is_enabled().hash(&mut hc);
        n += 1;
    }
    let mut hb = rustc_hash::FxHasher::default();
    for (h, b) in world.bodies.iter() {
        h.into_raw_parts().hash(&mut hb);
        let p = b.position();
        [p.translation.x, p.translation.y, p.translation.z].map(f32::to_bits).hash(&mut hb);
        [b.linvel().x, b.linvel().y, b.linvel().z, b.angvel().x, b.angvel().y, b.angvel().z].map(f32::to_bits).hash(&mut hb);
        [b.user_force().x, b.user_force().y, b.user_force().z, b.user_torque().x, b.user_torque().y, b.user_torque().z].map(f32::to_bits).hash(&mut hb);
    }
    eprintln!("SVI {id} {} {n} {:016x} {:016x}", s.ticks, hc.finish(), hb.finish());
}

/// rapier's contact recycling (RUSTUDIO_SOLVER_RECYCLE=1). Off by default: Studio
/// refreshes every pair's manifold every tick (STEP §1.2a), and a recycled
/// manifold keeps its local points — a rolling ball's point on the ball turns away
/// with it (lab C3: spin 0.5…4 rad/s drift from tick 3 with recycling, match Studio
/// to float noise without).
fn recycle() -> bool {
    static R: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *R.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_RECYCLE").is_ok_and(|v| v == "1"))
}

/// rapier's hooks plus `external_contacts`: a pair of part shapes narrow.rs knows (blocks,
/// balls, wedges, corner wedges) gets no parry manifolds. Characters (rotation-locked
/// bodies) keep theirs: the Humanoid still reads parry's points (lane CHAR-2;
/// `StudioSolver::external_points` is the accessor for when it moves).
/// RUSTUDIO_SOLVER_PARRY_MANIFOLDS=1 keeps parry's manifolds for every pair.
struct ExternalHooks<'a> {
    inner: &'a dyn rp::PhysicsHooks,
    /// `StudioSolver::shape_class` (collider index → (generation, class)).
    class: &'a [(u32, u8)],
}

fn parry_manifolds() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_PARRY_MANIFOLDS").is_ok_and(|v| v == "1"))
}

impl rp::PhysicsHooks for ExternalHooks<'_> {
    fn filter_contact_pair(&self, c: &rp::PairFilterContext) -> Option<rp::SolverFlags> {
        self.inner.filter_contact_pair(c)
    }

    fn filter_intersection_pair(&self, c: &rp::PairFilterContext) -> bool {
        self.inner.filter_intersection_pair(c)
    }

    fn modify_solver_contacts(&self, c: &mut rp::ContactModificationContext) {
        self.inner.modify_solver_contacts(c)
    }

    fn external_contacts(&self, c: &rp::PairFilterContext) -> bool {
        if parry_manifolds() {
            return false;
        }
        let (Some(a), Some(b)) = (c.colliders.get(c.collider1), c.colliders.get(c.collider2)) else { return false };
        let locked = |h: Option<rp::RigidBodyHandle>| h.and_then(|h| c.bodies.get(h)).is_some_and(|b| b.locked_axes().contains(rp::LockedAxes::ROTATION_LOCKED));
        if locked(c.rigid_body1) || locked(c.rigid_body2) {
            return false;
        }
        let class = |h: rp::ColliderHandle, c: &rp::Collider| {
            let (i, g) = h.into_raw_parts();
            match self.class.get(i as usize) {
                Some(&(cg, k)) if cg == g => k,
                _ => super::narrow::handled_class(c.shape()),
            }
        };
        class(c.collider1, a) != 0 && class(c.collider2, b) != 0
    }
}

pub const STAGES: [&str; 5] = ["sv_collide", "sv_gather", "sv_islands", "sv_solve", "sv_integrate"];

impl StudioSolver {
    /// One 240 Hz tick: collision detection by rapier, everything else here.
    pub fn step(&mut self, world: &mut rp::PhysicsWorld, gravity: Vec3, hooks: &dyn rp::PhysicsHooks, events: &dyn rp::EventHandler) {
        let h = world.integration_parameters.dt;
        // Harness: RUSTUDIO_SOLVER_SKIP lists calls (1-based) on which the world does
        // not step at all (Studio's lab runs show such 4-tick holes), and
        // RUSTUDIO_SOLVER_PHASE the world tick count before the first step (Studio's
        // phase at a lab's start is whatever the session's tick count was).
        self.calls += 1;
        DIAG_CALL.store(self.calls, std::sync::atomic::Ordering::Relaxed);
        if harness().skip.binary_search(&self.calls).is_ok() {
            return;
        }
        if self.calls == 1 {
            self.ticks += harness().phase;
        }
        // Phase p = world tick count mod maxLevel (world, STEP §1.1 step 5).
        let phase = match self.env_phase {
            Some(p) => p % self.cfg.max_level,
            None => {
                let phase = (self.ticks % self.cfg.max_level as u64) as u8;
                self.ticks += 1;
                phase
            }
        };
        // rapier's island manager holds nothing here (Physics::prime runs only
        // collision detection under this solver): its narrow phase would treat
        // an active body's pairs as moved every step. A pair is recomputed only
        // when one of its colliders moved (BodySet::scatter) or changed.
        debug_assert!(world.islands.active_bodies().next().is_none());
        world.collision_pipeline.contact_recycle_distance = if recycle() { world.integration_parameters.contact_recycle_distance() } else { 0.0 };
        // Pairs narrow.rs makes get no parry manifolds (rapier pairs them, filters them, and
        // stops there): `ExternalHooks`.
        let hooks = ExternalHooks { inner: hooks, class: &self.shape_class };
        input_hash(self, world);
        static COLLISION_STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let collision_stats = *COLLISION_STATS.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_COLLISION_STATS").is_some());
        world.collision_pipeline.measure_stages = collision_stats;
        // A body between its solve ticks (level 2 / 4) is not moved (STEP §1.5), and a
        // pair of two unmoved colliders refreshes to the manifold it already holds:
        // keep it, and ContactStore reuses its rows. RUSTUDIO_SOLVER_REFRESH_ALL=1
        // recomputes every pair every tick.
        static REFRESH_ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        world.narrow_phase.skip_unmoved_pairs = !*REFRESH_ALL.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_REFRESH_ALL").is_some());
        // RUSTUDIO_SOLVER_NO_CYLINDER_AXES=1: every contact-free cylinder pair goes to
        // Parry's manifold query, without the separating-axis test first.
        static CYLINDER_AXES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        world.narrow_phase.cylinder_separation_test = !*CYLINDER_AXES.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_NO_CYLINDER_AXES").is_some());
        let ((), c0) = rustudio_core::bench::measure(|| world.detect_collisions(&hooks, events));
        if collision_stats {
            for (name, ms) in ["sv_broad_wall", "sv_narrow_wall", "sv_sensor_wall"].into_iter().zip(world.collision_pipeline.stage_times_ms()) {
                rustudio_core::bench::add(name, ms, 1);
            }
        }
        let ((), c1) = rustudio_core::bench::measure(|| {
            let _gather = rustudio_core::bench::scope("sv_body_gather");
            self.bodies.gather(&world.bodies, Some(world.collision_pipeline.processed_bodies()));
            self.freeze_throttled();
            drop(_gather);
            // SLEEP §1 step 2: preContactStepSleepStage (wakes, state 2 → 3).
            let _pre = rustudio_core::bench::scope("sv_sleep_pre");
            self.sleep.pre_contact(&mut self.bodies, &super::sleep::World { contacts: &self.contacts, joints: &self.joints, rb: &world.bodies, colliders: &world.colliders, np: &world.narrow_phase }, phase);
            drop(_pre);
            enter_levels(&mut self.bodies, phase, &self.cfg);
            // SLEEP §1 step 4: with nothing due and nobody asleep, postContact is skipped.
            let decide = !super::sleep::nothing_due(&self.bodies, &self.sleep, phase);
            let _contacts = rustudio_core::bench::scope("sv_contact_gather");
            self.contacts.update(&self.bodies, &world.bodies, &world.colliders, &world.narrow_phase, &self.prim_id);
            drop(_contacts);
            self.touch_events(world, events);
            // (JOINTS: the constraint Instances' connectors, physics_joints.rs studio_constraints)
            self.joints.gather_studio(&self.bodies);
            // SLEEP §2: postContactStepSleepStage (contact wakes, decisions, transitions).
            let _post = rustudio_core::bench::scope("sv_sleep_post");
            if decide {
                self.sleep.post_contact(&mut self.bodies, &super::sleep::World { contacts: &self.contacts, joints: &self.joints, rb: &world.bodies, colliders: &world.colliders, np: &world.narrow_phase }, phase, self.cfg.max_level);
                if !self.sleep.changed.is_empty() {
                    self.contacts.read_woken(&self.sleep.changed, &self.bodies, &world.bodies, &world.colliders, &world.narrow_phase, &self.prim_id);
                    self.touch_events(world, events);
                }
            }
            drop(_post);
        });
        let ((), c2) = rustudio_core::bench::measure(|| {
            self.islands.build(&mut self.bodies, &self.contacts.list, &self.joints);
            self.islands.set_due(phase);
            // JOINTS: force items (JOINT I.14) on body A's own solve tick, and the
            // spring arbiter's slot-2 requests (JOINT §5).
            self.joints.apply_forces(&mut self.bodies, &self.islands);
            self.joints.winch_callbacks(&self.bodies, &self.islands, h);
            self.joints.apply_level_requests(&mut self.bodies);
            if self.cfg.max_level > 1 {
                boxes(&mut self.bodies, &self.islands, &world.bodies, &world.colliders);
            }
        });
        let ((), c3) = rustudio_core::bench::measure(|| {
            let nrows = self.solve_islands(h, gravity, phase);
            self.stats.rows = nrows as u32;
            self.apply_queue();
        });
        let ((), c4) = rustudio_core::bench::measure(|| {
            kinematic(&mut self.bodies, h);
            self.bodies.scatter(&mut world.bodies, &mut world.colliders);
            super::sleep::sync_rapier(&self.bodies, &self.sleep.changed, &mut world.bodies);
            // SLEEP §4.3: history for awake bodies not integrated this tick.
            self.sleep.end_tick(&mut self.bodies);
        });
        for (k, c) in [c0, c1, c2, c3, c4].into_iter().enumerate() {
            self.costs[k] += c;
        }
        self.count();
        state_hash(self);
    }

    /// Touched / TouchEnded of the pairs narrow.rs makes alone, through rapier's event
    /// handler (physics.rs's collector), as rapier's own pairs report them.
    /// A throttled 60 Hz frame (Studio steps solver set S only, Studio): every
    /// awake body outside `throttle_exempt` is held still for the tick. Its pairs with
    /// stepping bodies treat it as a moving non-sim side (`side`). An environment
    /// catch-up tick (`env_phase`) holds the exempt bodies still instead.
    fn freeze_throttled(&mut self) {
        let b = &mut self.bodies;
        for s in 0..b.len() {
            b.flags[s] &= !flag::FROZEN;
        }
        let env_only = self.env_phase.is_some();
        if !self.throttled && !env_only {
            return;
        }
        for s in 0..b.len() {
            let f = b.flags[s];
            if f & (flag::DYNAMIC | flag::AWAKE) == (flag::DYNAMIC | flag::AWAKE) && self.throttle_exempt.contains(&b.handle[s]) == env_only {
                b.flags[s] = (f & !flag::AWAKE) | flag::FROZEN;
            }
        }
    }

    fn touch_events(&mut self, world: &rp::PhysicsWorld, events: &dyn rp::EventHandler) {
        static DBG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let dbg = *DBG.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_TOUCH_DEBUG").is_some());
        for (c1, c2, started) in self.contacts.events.drain(..) {
            if dbg {
                eprintln!("TOUCH call {} {:?} {:?} {}", self.calls, c1.into_raw_parts(), c2.into_raw_parts(), started);
            }
            let e = if started { rp::CollisionEvent::Started(c1, c2, rp::CollisionEventFlags::empty()) } else { rp::CollisionEvent::Stopped(c1, c2, rp::CollisionEventFlags::empty()) };
            events.handle_collision_event(&world.bodies, &world.colliders, e, None);
        }
    }

    /// The last tick's contact points of the pair (c1, c2) when narrow.rs made them alone
    /// (rapier holds no manifolds for it): `f(point on c1, normal out of c1, separation)`.
    /// False: the pair is rapier's, read its manifolds. (For physics.rs and the Humanoid.)
    pub fn external_points(&self, c1: rp::ColliderHandle, c2: rp::ColliderHandle, f: impl FnMut(Vec3, Vec3, f32)) -> bool {
        self.contacts.external_points(c1, c2, f)
    }

    /// The body's own rotation (STEP §4.2: body, what Studio copies into the part
    /// CFrame) — the kept f32 matrix (GEOM2 §B) — while rapier still holds
    /// what was written; None: rapier's quaternion is the pose.
    pub fn body_matrix(&self, b: rp::RigidBodyHandle, pos: Vec3, rot: glam::Quat) -> Option<glam::Mat3> {
        let (i, g) = b.into_raw_parts();
        let p = self.bodies.persist.get(i as usize)?;
        if !p.live || p.generation != g {
            return None;
        }
        let sp = p.pose?;
        (sp.written_pos == pos && sp.written_rot == rot).then(|| super::body::m3::to_glam(&sp.r))
    }

    /// Touching pairs narrow.rs made alone (Stats.ContactsCount adds them to rapier's).
    pub fn external_touching(&self) -> usize {
        self.contacts.external_touching()
    }

    /// A level request from outside the solve (Studio(body, req, slot)): slot 3 =
    /// the Humanoid's / a mover's (STEP §3.7: Studio …), slot 5 = the
    /// density request. 0 withdraws it. Read by the next arbitration.
    pub fn set_level_request(&mut self, body: rp::RigidBodyHandle, slot: usize, level: u8) {
        let (i, g) = body.into_raw_parts();
        if self.bodies.persist.len() <= i as usize {
            self.bodies.persist.resize(i as usize + 1, Default::default());
            self.bodies.slot_of.resize(i as usize + 1, NONE);
        }
        let p = &mut self.bodies.persist[i as usize];
        if !p.live || p.generation != g {
            *p = super::body::Persist { generation: g, live: true, ..Default::default() };
        }
        p.requests[slot.min(6)] = level;
        // The Humanoid's request (slot 3) below the body's level takes effect at once:
        // its level list moves now, and the next due tick's getAdaptiveBodiesInSolver
        // (`enter_levels`, Studio) sets the level and arms the debounce. Measured
        // (SPEC §0.2, PROGRESS "Timestep arbiter"): a take-off frame's ticks are single
        // ones and a 180° turn is at level 1 from its first tick (T3, T4: with the drop
        // only through the arbiter after the next solve, the take-off solve spans 2h).
        if slot == 3 && level != 0 && level < p.list_level {
            p.list_level = level;
            p.node_level = level;
        }
    }

    /// The next tick's level of a body and whether that tick is its own solve tick:
    /// body after getAdaptiveBodiesInSolver (STEP §1.2 step 6, `enter_levels`), and
    /// the test of the assembly force accumulators Studio
    /// (`(p & (L − 1)) == 0`, JOINT §3, STEP §1.3 step 3). The Humanoid's forces enter
    /// only on such ticks. A body the solver has not seen yet: level 1.
    pub fn next_solve(&self, body: rp::RigidBodyHandle) -> (u8, bool) {
        if self.cfg.max_level == 1 {
            return (1, true);
        }
        let mut t = self.ticks;
        if self.calls == 0 {
            t += harness().phase;
        }
        let phase = (t % self.cfg.max_level as u64) as u8;
        let (i, g) = body.into_raw_parts();
        let level = match self.bodies.persist.get(i as usize) {
            Some(p) if p.live && p.generation == g => {
                let l = p.list_level.max(1);
                if phase & (l - 1) == 0 { l } else { p.level.max(1) }
            }
            _ => 1,
        };
        (level, phase & (level - 1) == 0)
    }

    /// A collider is about to be removed, rebuilt, moved (a fixed one), or given new collision
    /// groups (physics.rs): an asleep body touching it wakes at the next tick (sleep.rs
    /// `support`; SLEEP §7.2's property-write wake through the changed part's contacts).
    pub fn support_changed(&mut self, c: rp::ColliderHandle) {
        self.sleep.support.insert(c);
    }

    /// Terrain changed inside [lo, hi] (studs): asleep bodies whose colliders reach into it
    /// wake at the next tick.
    pub fn region_changed(&mut self, lo: Vec3, hi: Vec3) {
        self.sleep.regions.push((lo, hi));
    }

    /// A property write that wakes `body`'s assembly as Studio's do (sleep.rs `poked`: the
    /// Studio path, depth OptimizedContactWakeDepth) even when it changed neither the
    /// body's velocity nor its pose: a mover appearing or changing (physics.rs `wake_part`).
    /// Studio, Vinegar 2026-11-15 (a test place's bottles asleep on their ropes): an
    /// AlignPosition created at its part's own position and then given a Position 3 studs
    /// up moved the bottle the 3 studs, as did one created 3 studs off; rapier's own wake
    /// was undone here (`resleep`: no velocity or pose change), so grabbed bottles
    /// never left their branch.
    pub fn property_wake(&mut self, body: rp::RigidBodyHandle) {
        let (i, g) = body.into_raw_parts();
        if let Some(p) = self.bodies.persist.get_mut(i as usize).filter(|p| p.live && p.generation == g) {
            p.sleep.poked = true;
        }
    }

    /// A Humanoid's floor reaction lands on `body` this frame (sleep.rs `floor_wake`).
    pub fn floor_touched(&mut self, body: rp::RigidBodyHandle) {
        let (i, g) = body.into_raw_parts();
        if let Some(p) = self.bodies.persist.get_mut(i as usize).filter(|p| p.live && p.generation == g) {
            p.sleep.floor_wake = true;
        }
    }

    /// Numbers the parts in this order (physics.rs, at load: the DOM's order).
    pub fn number_parts(&mut self, parts: impl Iterator<Item = Ref>) {
        for r in parts {
            let n = self.part_serial.len() as u64;
            self.part_serial.entry(r).or_insert(n);
        }
    }

    /// A body's Studio mass record (one-part bodies, STEP §5 [L2]); None: rapier's sum.
    pub fn note_mass(&mut self, b: rp::RigidBodyHandle, m: Option<super::mass::PartMass>) {
        self.bodies.mark_for_refresh(b);
        match m {
            Some(m) => {
                self.bodies.studio_mass.insert(b, m);
            }
            None => {
                self.bodies.studio_mass.remove(&b);
            }
        }
    }

    /// What `note_mass` / `note_box` / `note_char_len` hold for a body (round HANG: a
    /// client body's stand-in in the server's world takes its records).
    pub fn body_notes(&self, b: rp::RigidBodyHandle) -> (Option<super::mass::PartMass>, Option<(Vec3, Vec3)>, Option<f32>) {
        (self.bodies.studio_mass.get(&b).copied(), self.bodies.size_box.get(&b).copied(), self.bodies.char_len.get(&b).copied())
    }

    /// The union of an assembly's part Size boxes in its body frame (slot 0, STEP §3.4 [L3]).
    pub fn note_box(&mut self, b: rp::RigidBodyHandle, lo: Vec3, hi: Vec3) {
        self.bodies.size_box.insert(b, (lo, hi));
    }

    /// body = r of Studio (SLEEP §2.1 [S2]).
    pub fn note_char_len(&mut self, b: rp::RigidBodyHandle, r: f32) {
        self.bodies.char_len.insert(b, r);
    }

    /// A collider made for part `r` (a part first seen now takes the next serial).
    pub fn note_collider(&mut self, c: rp::ColliderHandle, r: Ref, shape: &dyn rp::Shape) {
        let n = self.part_serial.len() as u64;
        let id = *self.part_serial.entry(r).or_insert(n);
        self.prim_id.insert(c, id);
        // The class the `external_contacts` hook tests, once per collider (physics.rs
        // makes a new collider for every shape change).
        let (i, g) = c.into_raw_parts();
        if self.shape_class.len() <= i as usize {
            self.shape_class.resize(i as usize + 1, (u32::MAX, 0));
        }
        self.shape_class[i as usize] = (g, super::narrow::handled_class(shape));
    }

    /// A Humanoid's collider (physics_humanoid.rs `add_humanoid`): its pairs keep parry's
    /// manifolds (the `external_contacts` hook's "characters excepted": the Humanoid reads
    /// parry's points). A character body is not rotation-locked, so the hook's lock test
    /// alone missed it and the wall lanes lost their contacts (movement-wall-r15 85.8 →
    /// 76.8, -r6 79.4 → 75.7).
    pub fn note_character_collider(&mut self, c: rp::ColliderHandle) {
        let (i, g) = c.into_raw_parts();
        if self.shape_class.len() <= i as usize {
            self.shape_class.resize(i as usize + 1, (u32::MAX, 0));
        }
        self.shape_class[i as usize] = (g, 0);
    }

    /// The stage costs since the last call, as (bench section, cost).
    pub fn take_costs(&mut self) -> [(&'static str, f64); 5] {
        let c = std::mem::take(&mut self.costs);
        [(STAGES[0], c[0]), (STAGES[1], c[1]), (STAGES[2], c[2]), (STAGES[3], c[3]), (STAGES[4], c[4])]
    }

    /// The due islands: in parallel on rayon's workers (Studio's batches are job
    /// tasks, DUAL §6.2), each worker with its own working set, reading the shared
    /// state; then their results go back serially. Islands share no body and no
    /// connector, so the outcome does not depend on the split. Returns the rows.
    fn solve_islands(&mut self, h: f32, gravity: Vec3, phase: u8) -> usize {
        if self.path == SolverKind::StudioNextGen {
            let mut n = 0;
            for i in 0..self.islands.len() {
                if self.islands.due[i] {
                    n += self.solve_island_primal(i, h, gravity);
                }
            }
            return n;
        }
        self.due.clear();
        self.due.extend((0..self.islands.len() as u32).filter(|&i| self.islands.due[i as usize]));
        if self.due.is_empty() {
            return 0;
        }
        static RATE_STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *RATE_STATS.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_RATE_STATS").is_some()) {
            let mut solved = [0; 4];
            for &i in &self.due {
                let i = i as usize;
                let bucket = self.islands.level[i].trailing_zeros() as usize;
                solved[bucket.min(3)] += self.islands.bodies(i).len() as u64;
            }
            for (name, count) in ["sv_bodies_240hz", "sv_bodies_120hz", "sv_bodies_60hz", "sv_bodies_30hz"].into_iter().zip(solved) {
                rustudio_core::bench::add(name, 0.0, count);
            }
        }
        use rapier3d::rayon::prelude::*;
        // A resting island with rows costs more than a loose body. Balance
        // contiguous ranges by the bodies and constraints they will solve.
        // Keep the result merge in original island order.
        let weight = |i: u32| {
            let i = i as usize;
            self.islands.bodies(i).len()
                + self.islands.manifolds(i).iter()
                    .map(|&m| 2 * self.contacts.list.count[m as usize] as usize)
                    .sum::<usize>()
                + 6 * self.islands.joint_ids(i).len()
        };
        let total: usize = self.due.iter().copied().map(weight).sum();
        let threads = rapier3d::rayon::current_num_threads()
            .min(total.div_ceil(64))
            .min(self.due.len())
            .max(1);
        if self.workers.len() < threads {
            self.workers.resize_with(threads, Default::default);
        }
        self.cuts.clear();
        self.cuts.push(0);
        let mut acc = 0;
        for (k, &i) in self.due.iter().enumerate() {
            acc += weight(i);
            if acc * threads >= total * self.cuts.len() && self.cuts.len() < threads {
                self.cuts.push(k + 1);
            }
        }
        while self.cuts.len() <= threads {
            self.cuts.push(self.due.len());
        }
        let sh = Shared { bodies: &self.bodies, contacts: &self.contacts, islands: &self.islands, joints: &self.joints, anchored_vel: &self.anchored_vel, cfg: &self.cfg };
        let (due, cuts) = (&self.due, &self.cuts);
        let work = |(w, k): (&mut Worker, usize)| {
            w.reset(sh.bodies.len());
            for &i in &due[cuts[k]..cuts[k + 1]] {
                w.rows += solve_island(w, &sh, i as usize, h, gravity, phase);
            }
        };
        if threads == 1 {
            work((&mut self.workers[0], 0));
        } else {
            self.workers[..threads].par_iter_mut().zip(0..threads).for_each(work);
            rustudio_core::bench::add("sv_parallel_island_batches", 0.0, threads as u64);
        }
        // Results back (serial): bodies, kept impulses, queued level changes.
        let mut rows = 0;
        for w in &mut self.workers[..threads] {
            rows += w.rows;
            for o in w.out.drain(..) {
                let s = o.slot as usize;
                if o.dirty {
                    self.bodies.linvel[s] = o.v;
                    self.bodies.angvel[s] = o.w;
                    self.bodies.com[s] = o.com;
                    self.bodies.rot[s] = o.rot;
                    self.bodies.mat[s] = o.mat;
                    self.bodies.pos[s] = o.pos;
                    self.bodies.flags[s] |= flag::DIRTY | flag::STUDIO_POSE;
                }
                let idx = self.bodies.handle[s].into_raw_parts().0 as usize;
                self.bodies.persist[idx] = o.persist;
            }
            for (c, kept) in w.kept.drain(..) {
                self.contacts.conn_mut(c as usize).kept = kept;
            }
            self.joints.apply_results(&mut w.joint_state, &mut w.joint_kept);
            self.queue.extend(w.queue.drain(..));
        }
        rows
    }

    /// Studio (STEP §3.6): queued level changes, newest first.
    fn apply_queue(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        while let Some(q) = self.queue.pop() {
            let Some(p) = self.bodies.persist.get_mut(q.idx as usize) else { continue };
            if !p.live || p.generation != q.gen {
                continue;
            }
            p.pending = false;
            // A rise is refused while the debounce counter runs.
            if p.debounce >= 1 && q.level > p.level {
                continue;
            }
            // [C4] Type 3 (the arbiter, Studio(…, 1)) writes only the node level
            // byte (Studio: graph[node]); type 1 (the island's level,
            // Studio then Studio(…, 0)) moves the list and writes the byte.
            p.node_level = q.level;
            if q.ty == 1 {
                p.list_level = q.level;
            }
        }
        // Studio(p + 1, set) (only when the queue held something): islands rebuilt
        // from the node levels (Studio with arg 1 records every island in set)
        // and every body of each moved to its island's level list (lambda Studio:
        // Studio). follows on the list's next due tick
        // (Studio; a drop arms the debounce).
        for i in 0..self.islands.len() {
            let mut lvl = u8::MAX;
            for &s in self.islands.bodies(i) {
                let idx = self.bodies.handle[s as usize].into_raw_parts().0 as usize;
                lvl = lvl.min(self.bodies.persist[idx].node_level.max(1));
            }
            for &s in self.islands.bodies(i) {
                let idx = self.bodies.handle[s as usize].into_raw_parts().0 as usize;
                self.bodies.persist[idx].list_level = lvl;
            }
        }
    }

    fn solve_island_primal(&mut self, i: usize, h: f32, gravity: Vec3) -> usize {
        let island = &self.islands.order[self.islands.start[i] as usize..self.islands.start[i + 1] as usize];
        self.local.set(&self.bodies, island);
        self.rows.clear();
        external(island, &mut self.bodies, h, gravity);
        rows::build_contact_rows_primal(i, &self.islands, &mut self.contacts, &self.bodies, &self.local, &self.cfg, h, &mut self.rows);
        rows::build_joint_rows_primal(i, &self.islands, &self.joints, &self.bodies, &self.local, &self.cfg, h, &mut self.rows);
        self.sys.assemble(island, &self.bodies, &self.rows, h);
        self.sys.factor();
        iterate(island, &mut self.bodies, &self.rows, &mut self.sys, h);
        for &s in island {
            let s = s as usize;
            let w = self.bodies.angvel[s];
            let com = self.bodies.com[s] + self.bodies.linvel[s] * h;
            let r = Quat::from_scaled_axis(w * h) * self.bodies.rot[s];
            self.bodies.com[s] = com;
            self.bodies.rot[s] = r;
            self.bodies.pos[s] = com - r * self.bodies.local_com[s];
            self.bodies.flags[s] |= flag::DIRTY;
        }
        self.local.unset(island);
        self.rows.len()
    }

    fn count(&mut self) {
        let b = &self.bodies;
        let mut st = StepStats { bodies: b.len() as u32, rows: self.stats.rows, ..Default::default() };
        for s in 0..b.len() {
            if b.flags[s] & flag::KINEMATIC != 0 {
                st.kinematic += 1;
            } else if b.flags[s] & flag::AWAKE != 0 {
                st.awake += 1;
            }
        }
        st.contact_pairs = self.contacts.list.pairs;
        st.manifolds = self.contacts.list.manifold_count() as u32;
        st.points = self.contacts.list.len() as u32;
        st.joints = self.joints.len() as u32;
        st.islands = self.islands.len() as u32;
        st.largest_island = (0..self.islands.len()).map(|i| self.islands.start[i + 1] - self.islands.start[i]).max().unwrap_or(0);
        if !self.throttled && self.env_phase.is_none() {
            self.full_stats = st;
        }
        if rustudio_core::profiler::active() {
            use rustudio_core::profiler::{counter, Cat};
            counter("physics bodies", Cat::Physics, st.bodies as f64);
            counter("physics awake bodies", Cat::Physics, st.awake as f64);
            counter("physics contact pairs", Cat::Physics, st.contact_pairs as f64);
            counter("physics contact points", Cat::Physics, st.points as f64);
            counter("physics joints", Cat::Physics, st.joints as f64);
            counter("physics islands", Cat::Physics, st.islands as f64);
            counter("physics largest island", Cat::Physics, st.largest_island as f64);
            counter("physics solver rows", Cat::Physics, st.rows as f64);
        }
        self.stats = st;
    }
}

/// What the islands' workers read.
struct Shared<'a> {
    bodies: &'a BodySet,
    contacts: &'a ContactStore,
    islands: &'a Islands,
    joints: &'a JointList,
    anchored_vel: &'a FxHashMap<rp::ColliderHandle, (Vec3, Vec3, Vec3)>,
    cfg: &'a Config,
}

/// A solved body's new state and its persistent record (applied serially).
#[derive(Clone, Copy)]
struct BodyOut {
    slot: u32,
    dirty: bool,
    v: Vec3,
    w: Vec3,
    com: Vec3,
    rot: Quat,
    /// Studio's R after the solve (STEP §4.2 [L2]).
    mat: Mat3,
    pos: Vec3,
    persist: super::body::Persist,
}

/// One worker's working set: the batch (DUAL §0.2 X, kept between ticks), the
/// island-local index maps, and its results.
#[derive(Default)]
pub struct Worker {
    batch: DualBatch,
    local: LocalIndex,
    nonsim: Vec<u32>,
    out: Vec<BodyOut>,
    kept: Vec<(u32, [contact::Kept; 3])>,
    /// Joint connectors' slot-10 geometry and slot-7 kept records (lane JOINTS).
    joint_state: Vec<(u32, super::joints::Kind)>,
    joint_kept: Vec<(u32, [super::joints::Kept; super::joints::MAX_ROWS])>,
    /// Geometric-stiffness norms per sim body of the island (body, JOINTS [J2]).
    gs: Vec<Vec3>,
    queue: Vec<LevelChange>,
    rows: usize,
}

impl Worker {
    fn reset(&mut self, n: usize) {
        if self.nonsim.len() < n {
            self.nonsim.resize(n, NONE);
        }
        if self.local.local.len() < n {
            self.local.local.resize(n, NONE);
        }
        self.rows = 0;
    }
}

/// RUSTUDIO_SOLVER_POSE_TRACE=1 (diagnostic, lane STACK): each integration's Δθ / Δω / R and
/// the solve's position lane after warm start, sweeps and the LDL correction (dual.rs).
fn spin_debug() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_SPIN_DEBUG").is_some())
}

pub fn pose_trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_POSE_TRACE").is_some())
}

/// RUSTUDIO_SOLVER_ROW_TRACE=1 (diagnostic): every committed row of the batch (dual.rs).
pub fn row_trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_ROW_TRACE").is_some())
}

/// Diagnostics (lane STACK): the harness call number of the running step.
pub static DIAG_CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// RUSTUDIO_SOLVER_DIAG=T:what[,..] (diagnostic) changes the batch config of call T: nowarm,
/// sweeps=N, level=L (the config only; dt stays).
fn diag_cfg(cfg: &mut LevelCfg, h: f32, gravity: Vec3) {
    static D: std::sync::OnceLock<Option<(u64, String)>> = std::sync::OnceLock::new();
    let Some((t, what)) = D.get_or_init(|| {
        let v = std::env::var("RUSTUDIO_SOLVER_DIAG").ok()?;
        let (t, w) = v.split_once(':')?;
        Some((t.parse().ok()?, w.to_string()))
    }) else {
        return;
    };
    if *t != DIAG_CALL.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    for w in what.split(',') {
        if w == "nowarm" {
            cfg.warm = false;
        } else if let Some(n) = w.strip_prefix("sweeps=") {
            cfg.sweeps = n.parse().unwrap_or(cfg.sweeps);
            cfg.sweeps_pre = cfg.sweeps - 2;
        } else if let Some(l) = w.strip_prefix("level=") {
            let dt = cfg.dt;
            *cfg = LevelCfg::new(l.parse().unwrap_or(1), h, -gravity.y);
            cfg.dt = dt;
        }
    }
}

/// One due island ("Solve Batch" Studio for its islands); returns its row count.
fn solve_island(w: &mut Worker, sh: &Shared, i: usize, h: f32, gravity: Vec3, phase: u8) -> usize {
    let isl = sh.islands;
    let start = isl.start[i] as usize;
    let end = isl.start[i + 1] as usize;
    let level = isl.level[i];
    // cfg = Workspace.Gravity (STEP §0.2 [STEP2]).
    let mut cfg = LevelCfg::new(level, h, -gravity.y);
    diag_cfg(&mut cfg, h, gravity);
    let damping = (-(cfg.ang_damp) * cfg.dt).exp();
    let (mf, ml) = (isl.contact_start[i] as usize, isl.contact_start[i + 1] as usize);
    let list = &sh.contacts.list;
    let constrained = mf != ml || !isl.joint_ids(i).is_empty();
    let mut rows = 0;
    // An island without connectors has zero solve deltas. Integrate it
    // from the same free velocity without filling a batch or local indices.
    if constrained {
        w.batch.clear();
        // Studio: sim bodies' records in list order.
        for k in start..end {
            let s = isl.order[k] as usize;
            let (rec, mass) = free_velocity(sh.bodies, s, &cfg, gravity, damping);
            let l = w.batch.add_body(rec, mass, true);
            w.local.local[s] = l;
        }
        for mk in mf..ml {
            let m = isl.contacts[mk] as usize;
            let a = side(w, sh, m, true);
            let b = side(w, sh, m, false);
            let (first, count) = (list.first[m] as usize, list.count[m] as usize);
            let (ra, rb) = (w.batch.body(a), w.batch.body(b));
            for c in first..first + count {
                let kept = sh.contacts.conn(c).kept.map(|k| (k.lv, k.lp, k.s_v, k.s_p));
                let r = w.batch.begin(&kept, &cfg);
                // The connector's own A / B (narrow.rs: a face point's A is the incident body).
                let (ca, cb, pair) = if list.swap[c] { (&rb, &ra, [b, a]) } else { (&ra, &rb, [a, b]) };
                rows::contact_rows_dual(r, list.p[c], list.n[c], list.sep[c], list.mu[c], list.e[c], ca, cb, &cfg);
                // Contacts use three ordinary rows in group 4 and keep the connector key.
                w.batch.commit(dual::ctype::CONTACT, list.id[c], pair, 3, 0, [0.0, 0.0], ConnSrc::Contact(c as u32));
                rows += 3;
            }
        }
        rows::build_joint_rows_dual(i, isl, sh.joints, sh.bodies, &w.local, &mut w.nonsim, &cfg, &mut w.batch, &mut w.joint_state, &mut w.gs);
        // "Init Batch Solve" and "PGS Solve" / "Finalize" (DUAL §2–§4).
        if w.batch.conn_count() > 0 {
            w.batch.prepare(&cfg);
            w.batch.solve(&cfg);
            // When enabled, seed slot 7 for each connector in solver order.
            if cfg.store {
                for (src, pgs, omega, ldl) in w.batch.results() {
                    match src {
                        ConnSrc::Contact(c) => {
                            let lam = [[pgs[0][6], pgs[0][7]], [pgs[1][6], pgs[1][7]], [pgs[2][6], pgs[2][7]]];
                            let s_in = [omega[0], omega[1], omega[2]];
                            let mut conn = *sh.contacts.conn(c as usize);
                            contact::write_back_contact(&mut conn, &lam, &s_in, cfg.dt);
                            w.kept.push((c, conn.kept));
                        }
                        ConnSrc::Joint(j) => w.joint_kept.push((j, sh.joints.write_back_dual(j, pgs, omega, ldl, cfg.level, cfg.dt))),
                    }
                }
            }
        }
    }
    // §5.4 step 6: integration Studio per sim body, then the
    // arbiter Studio (STEP §3.2). An island holding an asleep body (state 4) is
    // not arbitrated (STEP §3.2, islandFlag Studio).
    let any_asleep = (start..end).any(|k| {
        let s = isl.order[k] as usize;
        sh.bodies.persist[sh.bodies.handle[s].into_raw_parts().0 as usize].sleep.state == super::sleep::ASLEEP_IN
    });
    for k in start..end {
        let s = isl.order[k] as usize;
        let l = k - start;
        let (rec, u, cond) = if constrained {
            (w.batch.rec[l], w.batch.u[l], w.batch.cond[l])
        } else {
            (free_velocity(sh.bodies, s, &cfg, gravity, damping).0, [0.0; 12], f32::NAN)
        };
        let idx = sh.bodies.handle[s].into_raw_parts();
        let mut p = sh.bodies.persist[idx.0 as usize];
        if p.sleep.state == super::sleep::ASLEEP_IN {
            // STEP §4.2a step 2: not integrated; the would-be velocity
            // change feeds /.
            let dv = (rec.vf + Vec3::new(u[0], u[4], u[8])) - rec.v0;
            let dw = (rec.wf + Vec3::new(u[2], u[6], u[10])) - rec.w0;
            p.sleep.asleep_solve(dv, dw, phase, sh.cfg.max_level);
            let b = sh.bodies;
            w.out.push(BodyOut { slot: s as u32, dirty: false, v: b.linvel[s], w: b.angvel[s], com: b.com[s], rot: b.rot[s], mat: b.mat[s], pos: b.pos[s], persist: p });
            continue;
        }
        let mut o = integrate(sh.bodies, s, &rec, &u, &cfg, sh.cfg, &mut p);
        if spin_debug() && (o.w - rec.w0).length() > 300.0 {
            let b = sh.bodies;
            let connectors = if constrained { w.batch.conn_count() } else { 0 };
            eprintln!(
                "SPIN body {:?} L{} dt {} v0 {:?} v1 {:?} w0 {:?} wf {:?} w1 {:?} du {:?} im {} ii {:?} ib {:?} com {:?} nrows {}",
                b.handle[s].into_raw_parts(), level, cfg.dt, rec.v0, o.v, rec.w0, rec.wf, o.w, u, b.inv_mass[s], b.inv_inertia[s], b.inertia_body[s], o.com, connectors
            );
            for mk in mf..ml {
                let m = isl.contacts[mk] as usize;
                if list.a[m] as usize != s && list.b[m] as usize != s {
                    continue;
                }
                for c in list.first[m] as usize..(list.first[m] + list.count[m] as u32) as usize {
                    eprintln!("SPIN   conn a {} b {} swap {} p {:?} n {:?} sep {} mu {} e {} (body com {:?})", list.a[m], list.b[m], list.swap[c], list.p[c], list.n[c], list.sep[c], list.mu[c], list.e[c], rec.x);
                }
            }
        }
        // Gyroscopic impulse: cleared by this solve's free velocity, refilled by its row collection
        p.gs = if constrained { w.gs[l] } else { Vec3::ZERO };
        p.f_solver = Vec3::ZERO;
        p.t_solver = Vec3::ZERO;
        // SLEEP §3, §4: activity and eligibility after the write-back (tickByte = phase).
        p.sleep.after_solve(rec.v0, rec.w0, o.v, o.w, o.com, o.rot, phase, p.level, sh.cfg.max_level, super::sleep::r_of(sh.bodies, s));
        if !cond.is_nan() {
            // STEP §3.3a: the LDL component's c to body (last component wins).
            p.cond = cond;
        }
        if any_asleep {
            o.persist = p;
            w.out.push(o);
            continue;
        }
        // STEP §2.3: a body whose level differs from the island's queues type 1.
        if p.level != level && !p.pending {
            p.pending = true;
            w.queue.push(LevelChange { idx: idx.0, gen: idx.1, ty: 1, level });
        }
        let l2 = if sh.cfg.max_level > 1 {
            let slot0 = box_level(sh.bodies, s, o.v, o.w, sh.cfg);
            arbiter(&mut p, slot0, phase, sh.cfg)
        } else {
            None
        };
        if arb_debug() {
            eprintln!("ARB body {} island L{} level {} list {} node {} req {:?} prev {:?} pcf {:.3} cfl {:.2e} debounce {} -> {:?} com {:.3},{:.3},{:.3} ph {}", idx.0, level, p.level, p.list_level, p.node_level, p.requests, p.prev_requests, p.pcf, p.cfl, p.debounce, l2, o.com.x, o.com.y, o.com.z, phase);
        }
        if let Some(l2) = l2 {
            w.queue.push(LevelChange { idx: idx.0, gen: idx.1, ty: 3, level: l2 });
        }
        o.persist = p;
        w.out.push(o);
    }
    if constrained {
        for k in start..end {
            w.local.local[isl.order[k] as usize] = NONE;
        }
        for mk in mf..ml {
            let m = isl.contacts[mk] as usize;
            for s in [list.a[m], list.b[m]] {
                if s != NONE {
                    w.nonsim[s as usize] = NONE;
                }
            }
        }
    }
    rows
}

/// The island-local index of one side of manifold m: a sim body of the island,
/// a non-sim record (added on first use), or NONE (static, zero record).
fn side(w: &mut Worker, sh: &Shared, m: usize, first: bool) -> u32 {
    let l = &sh.contacts.list;
    let (s, fixed) = if first { (l.a[m], l.fixed_a[m]) } else { (l.b[m], l.fixed_b[m]) };
    if s != NONE {
        let li = w.local.of(s);
        if li != NONE {
            return li;
        }
        if w.nonsim[s as usize] != NONE {
            return w.nonsim[s as usize];
        }
        // Kinematic / not solved: v = its velocity, infinite mass (STEP §7a step 5).
        let b = sh.bodies;
        let su = s as usize;
        let rec = BodyRec { vf: b.linvel[su], wf: b.angvel[su], r: b.mat[su], x: b.com[su], v0: b.linvel[su], w0: b.angvel[su] };
        let li = w.batch.add_body(rec, MassRec::default(), false);
        w.nonsim[su] = li;
        return li;
    }
    // An anchored part with a script velocity (a conveyor, STEP §7a step 5); every
    // other static side is the one shared zero record (DUAL §1 step 3).
    if let Some((lin, ang, centre)) = fixed.and_then(|c| sh.anchored_vel.get(&c)).copied() {
        let rec = BodyRec { vf: lin, wf: ang, r: Mat3::IDENTITY, x: centre, v0: lin, w0: ang };
        return w.batch.add_body(rec, MassRec::default(), false);
    }
    NONE
}


/// getAdaptiveBodiesInSolver (STEP §1.2 step 6 → Studio): every body whose
/// level list is due this phase takes that level; a drop starts the debounce
/// counter. The rollback Studio of a drop is not applied: Studio's recordings
/// (drop_default: L 4 → 1 after the bounce at tick 65, tick 66 continues from the
/// level-4 pose) show none.
fn enter_levels(bodies: &mut BodySet, phase: u8, cfg: &Config) {
    for s in 0..bodies.len() {
        if bodies.flags[s] & flag::DYNAMIC == 0 || bodies.flags[s] & flag::FROZEN != 0 {
            continue;
        }
        let idx = bodies.handle[s].into_raw_parts().0 as usize;
        let p = &mut bodies.persist[idx];
        let l = p.list_level.clamp(1, cfg.max_level);
        p.node_level = p.node_level.min(cfg.max_level);
        if phase & (l - 1) != 0 {
            continue;
        }
        if l < p.level {
            p.debounce = cfg.debounce_count;
        }
        p.level = l;
    }
}

/// Studio(b, out, p = cfg = 0.4) [J2]: Iw = R·I·Rᵀ; if every
/// gs_k ≤ Iw_kk·4p the body's own Iw⁻¹ () is used, else Iw'_kk = Iw_kk +
/// clamp(gs_k − Iw_kk·4p, 0, Iw_kk·1000) (.rdata Studio = 4, Studio = 1000) and
/// Iw'⁻¹ its inverse (Studio; a singular Iw' takes the diagonal's reciprocals).
/// Returns (Iw'⁻¹, Some(Iw) when augmented).
fn gs_inertia(b: &BodySet, s: usize, gs: Vec3, p: f32) -> (Mat3, Option<Mat3>) {
    let inv = b.inv_inertia_world[s];
    let ii = b.inv_inertia[s];
    if gs == Vec3::ZERO || ii.x <= 0.0 || ii.y <= 0.0 || ii.z <= 0.0 {
        return (inv, None);
    }
    let iw = match b.inertia_body[s] {
        // Studio: Iw = R·I·Rᵀ from body (STEP §5 [L2])
        Some(i) => b.mat[s] * i * b.mat[s].transpose(),
        None => {
            let frame = b.mat[s] * Mat3::from_quat(b.inertia_frame[s]);
            frame * Mat3::from_diagonal(Vec3::new(1.0 / ii.x, 1.0 / ii.y, 1.0 / ii.z)) * frame.transpose()
        }
    };
    let t = p * 4.0;
    let d = Vec3::new(iw.x_axis.x, iw.y_axis.y, iw.z_axis.z);
    if gs.x <= d.x * t && gs.y <= d.y * t && gs.z <= d.z * t {
        return (inv, None);
    }
    let add = |g: f32, dd: f32| (g - dd * t).max(0.0).min(dd * 1000.0);
    let mut iw2 = iw;
    iw2.x_axis.x += add(gs.x, d.x);
    iw2.y_axis.y += add(gs.y, d.y);
    iw2.z_axis.z += add(gs.z, d.z);
    let det = iw2.determinant();
    let inv2 = if det != 0.0 && det.is_finite() {
        iw2.inverse()
    } else {
        Mat3::from_diagonal(Vec3::new(1.0 / iw2.x_axis.x, 1.0 / iw2.y_axis.y, 1.0 / iw2.z_axis.z))
    };
    (inv2, Some(iw))
}

/// Studio (STEP §4.1a, DUAL §5.1): a sim body's record and inverse-mass record.
fn free_velocity(b: &BodySet, s: usize, cfg: &LevelCfg, gravity: Vec3, damp: f32) -> (BodyRec, MassRec) {
    let dt = cfg.dt;
    let im = b.inv_mass[s];
    // 4: q = powf(im, cfg); k = (im > 0 ? q/im : 0)·cfg; Iw⁻¹.
    let q = b.inv_mass_position[s];
    let k = if im > 0.0 { q / im } else { 0.0 } * cfg.ang_factor;
    // Studio(b, out, cfg = 0.4) [J2]: the world inertia with the joints'
    // geometric stiffness (body, last solve's row collection) on its diagonal.
    let idx = b.handle[s].into_raw_parts().0 as usize;
    let (iw, w_scale) = gs_inertia(b, s, b.persist.get(idx).map_or(Vec3::ZERO, |p| p.gs), 0.4);
    let mass = MassRec { im, im_p: q, iw, iw_p: iw * k };
    let r = b.mat[s];
    let (v0, w0) = (b.linvel[s], b.angvel[s]);
    // Gravity is the force (0, −m·g, 0) seeded into body after every solve
    // (STEP §4.0, Studio); rustudio's GravityScale (the Humanoid's) scales it.
    // The Humanoid / movers' forces (rapier user force) stand for (STEP §1.4
    // "Apply forces"); no impulse accumulator () is fed yet.
    let m = if im > 0.0 { 1.0 / im } else { 0.0 };
    let seed = gravity * m * b.gravity_scale[s];
    // 6: F = (F_asm + F_solver)·dt + J;  τ likewise (f32, per axis, in this order).
    // /: the Force items' accumulators (JOINT I.14), kept until this solve.
    let (fs, ts) = b.persist.get(idx).map_or((Vec3::ZERO, Vec3::ZERO), |p| (p.f_solver, p.t_solver));
    let f = (seed + (b.force[s] + fs)) * dt;
    let tau = (Vec3::ZERO + (b.torque[s] + ts)) * dt;
    let vf = v0 + f * im;
    // damp = expf(−cfg·dt); ωf = Iw⁻¹·τ + Rdamp·ω0. Rdamp = R·damp·Rᵀ with a
    // scalar damp is damp·I (DUAL Open 5 / STEP §4.1a: the exact matrix unconfirmed).
    // ωf = Iw'⁻¹·τ + (damp·Iw'⁻¹)·Iw·ω0 (Studio = s·A, Studio = A·B; DUAL Open 5
    // [J2]); Iw'⁻¹·Iw = I when no GS term applies (then damp·ω0, as before).
    let wf = match w_scale {
        Some(iw_w) => iw * tau + ((iw * damp) * iw_w) * w0,
        None => iw * tau + w0 * damp,
    };
    (BodyRec { vf, wf, r, x: b.com[s], v0, w0 }, mass)
}

/// Studio (STEP §4.2b [L3]): Euler angles XYZ of a row-major R (what slot 6
/// takes for "θ"; it is not an axis-angle). y = asin(R02), x = atan2(−R12, R22),
/// z = atan2(−R01, R00); at |R02| ≥ 0.99999958 (.rdata Studio):
/// x = ±atan2(R10, R11), y = ±π/2 (0x3fc90fdb), z = 0. The asin clamp at ±1
/// (.rdata Studio) gives ±π/2. f32.
#[inline]
fn euler_xyz(r: Mat3) -> Vec3 {
    // glam is column-major: R[i][j] = r.col(j)[i].
    let e = |i: usize, j: usize| r.col(j)[i];
    let r02 = e(0, 2);
    if 0.999_999_6_f32 <= r02 {
        return Vec3::new(e(1, 0).atan2(e(1, 1)), std::f32::consts::FRAC_PI_2, 0.0);
    }
    if -0.999_999_6_f32 < r02 {
        let x = (-e(1, 2)).atan2(e(2, 2));
        let y = if -1.0 < r02 && r02 < 1.0 { r02.asin() } else if r02 <= -1.0 { -std::f32::consts::FRAC_PI_2 } else { std::f32::consts::FRAC_PI_2 };
        let z = (-e(0, 1)).atan2(e(0, 0));
        return Vec3::new(x, y, z);
    }
    Vec3::new(-e(1, 0).atan2(e(1, 1)), -std::f32::consts::FRAC_PI_2, 0.0)
}

/// The pose update Studio (STEP §4.2, §4.2a) with the
/// write-back's bookkeeping: debounce decrement (Studio, §3.6) and the slot-6
/// factors (Studio, §4.2b).
fn integrate(bodies: &BodySet, s: usize, rec: &BodyRec, u: &[f32; 12], cfg: &LevelCfg, sc: &Config, p: &mut super::body::Persist) -> BodyOut {
    let dt = cfg.dt;
    // Unpack: dv = [0],[4],[8]; dx = [1],[5],[9]; dω = [2],[6],[10]; dθ = [3],[7],[11].
    let dv = Vec3::new(u[0], u[4], u[8]);
    let dx = Vec3::new(u[1], u[5], u[9]);
    let dw = Vec3::new(u[2], u[6], u[10]);
    let dth = Vec3::new(u[3], u[7], u[11]);
    // 1 v1 = vf + Δv; 2 x += Δx; 3 Q = rot(Δθ), R = Q·R, ω' = Q·ωf; 4 ω1 = ω' + Δω;
    // 5 (R, ω1) = Studio(R, ω1, I, dt, cfg) (GEOM2 §B [P]: the gyro byte is 1 on the
    // legacy Finalize path, the call at Studio; f64, one Newton–Schulz step, pose.rs);
    // 6 x += dt·v1. Steps 1–4 f32 in Studio's order (asm): body.rs `m3`
    //. R is the body's own state between ticks.
    use super::body::m3;
    let r0 = m3::from_glam(&rec.r);
    let v1 = dv + rec.vf;
    let mut x = dx + rec.x;
    let q = m3::rot(dth);
    let r = m3::mul(&q, &r0);
    let w1 = dw + m3::mul_vec(&q, rec.wf);
    let rq = r;
    let ib = body_inertia(bodies, s);
    let (r, w1) = gyro_guarded(&r, w1, &ib, dt);
    if pose_trace() {
        eprintln!("pose call {} slot {s} dth {:?} dw {:?} wf {:?} Q·R {:?} R' {:?}", DIAG_CALL.load(std::sync::atomic::Ordering::Relaxed), [dth.x, dth.y, dth.z], [dw.x, dw.y, dw.z], [rec.wf.x, rec.wf.y, rec.wf.z], rq, r);
    }
    x = Vec3::new(dt * v1.x + x.x, v1.y * dt + x.y, v1.z * dt + x.z);
    let rm = m3::to_glam(&r);
    let changed = x != rec.x || v1 != bodies.linvel[s] || w1 != bodies.angvel[s] || r != r0;
    let rot_q = if r != r0 { Quat::from_mat3(&rm).normalize() } else { bodies.rot[s] };
    // Part pose (write-back Studio): R_part = R, p = R·(−c) + x,
    // c = the CoM in the part frame (Studio, negated by sign-flip).
    let pos = m3::mul_vec(&r, -bodies.local_com[s]) + x;
    let out = BodyOut {
        slot: s as u32,
        dirty: changed,
        v: v1,
        w: w1,
        com: x,
        rot: rot_q,
        mat: rm,
        pos,
        persist: *p,
    };
    // Studio: b = max(b − b, 0).
    p.debounce = (p.debounce - p.level as i32).max(0);
    // Studio: prev = the state at the solve's start, new = after it.
    let th = euler_xyz(rm * rec.r.transpose());
    let d = x - rec.x;
    let (ld, la) = (d.length(), th.length());
    let el = d - v1 * dt;
    let ea = th - w1 * dt;
    let c = sc.pos_corr_scale;
    p.pcf = 0.5 * (el.length() / (c + ld) + ea.length() / (c + la));
    // b, the characteristic length r: written by Studio (SLEEP §2.1 [S2]), the
    // same body field sleep.rs reads.
    let rlen = super::sleep::r_of(bodies, s);
    p.cfl = ((la / dt) * rlen + ld / dt) * dt / sc.cfl_length_scale;
    out
}

/// The rotation update Studio (pose.rs `gyro_step`), with rustudio's guard (not Studio's;
/// `RUSTUDIO_SOLVER_GYRO_GUARD=off` turns it off). The update carries the world angular momentum
/// over exactly but not the kinetic energy: for a long thin body (inertia ratio in the hundreds)
/// turning more than ~1 rad in one step (a 1/60 s step at 60+ rad/s) its few Newton iterations
/// leave R' off and ω' = Iw'⁻¹·L gains up to ~100× in a step, the spin moving into the thin axis.
/// Studio's own step does the same (measured: a lone 25.5×0.78 cylinder given ω (150, 60, 30) in
/// zero gravity reaches 5 700 rad/s in Studio, scenes rod_free_*); in Place B's loose stick pile such
/// rods reached 10⁴–10⁶ rad/s and threw what they touched 1.6 million studs (STATUS round RUNAWAY).
/// A free rotation keeps its energy, so when the step's ½ωᵀIω rises by more than 1 % (+1e-6) the
/// same update is redone in 2, 4, … 64 equal sub-steps (small turns, where it is exact enough)
/// until it does not; failing that, ω' is scaled back to the input energy. Every recorded lab stays
/// bit-identical (none trips it).
fn gyro_guarded(r0: &super::pose::M3, w0: Vec3, ib: &super::pose::M3, dt: f32) -> (super::pose::M3, Vec3) {
    use super::pose::{gyro_step, tol_of};
    let (r1, w1) = gyro_step(r0, w0, ib, dt, tol_of(dt));
    // RUSTUDIO_SOLVER_GYRO_GUARD=off|0 or the rustudio option (crates/rustudio-core/src/rustudio_settings.rs).
    if !rustudio_core::rustudio_settings::spin_guard() {
        return (r1, w1);
    }
    // ½ ωᵀ R I Rᵀ ω = ½ ω_bᵀ I ω_b with ω_b = Rᵀω (row-major R).
    let ke = |r: &super::pose::M3, w: Vec3| {
        let wb = Vec3::new(r[0] * w.x + r[3] * w.y + r[6] * w.z, r[1] * w.x + r[4] * w.y + r[7] * w.z, r[2] * w.x + r[5] * w.y + r[8] * w.z);
        let iw = Vec3::new(ib[0] * wb.x + ib[1] * wb.y + ib[2] * wb.z, ib[3] * wb.x + ib[4] * wb.y + ib[5] * wb.z, ib[6] * wb.x + ib[7] * wb.y + ib[8] * wb.z);
        0.5 * wb.dot(iw)
    };
    let e0 = ke(r0, w0);
    let ok = |r: &super::pose::M3, w: Vec3| {
        let e = ke(r, w);
        e.is_finite() && w.is_finite() && r.iter().all(|x| x.is_finite()) && e <= e0 * 1.01 + 1e-6
    };
    if !(e0 >= 0.0 && e0.is_finite()) || ok(&r1, w1) {
        return (r1, w1);
    }
    let mut n = 2;
    while n <= 64 {
        let h = dt / n as f32;
        let (mut r, mut w) = (*r0, w0);
        for _ in 0..n {
            (r, w) = gyro_step(&r, w, ib, h, tol_of(h));
        }
        if ok(&r, w) {
            return (r, w);
        }
        n *= 2;
    }
    let e1 = ke(&r1, w1);
    if r1.iter().all(|x| x.is_finite()) && w1.is_finite() && e1 > 0.0 {
        (r1, w1 * (e0 / e1).sqrt())
    } else {
        (*r0, w0)
    }
}

/// body, the body-frame inertia Studio reads (row-major): Studio's mass record when
/// the body has one, else rapier's principal inertia turned into the body frame. A body with a
/// locked or zero axis (rustudio's upright characters) takes I (its ω is not integrated by them).
fn body_inertia(b: &BodySet, s: usize) -> super::pose::M3 {
    let m = match b.inertia_body[s] {
        Some(i) => i,
        None => {
            let ii = b.inv_inertia[s];
            if !(ii.x > 0.0 && ii.y > 0.0 && ii.z > 0.0) {
                Mat3::IDENTITY
            } else {
                let f = Mat3::from_quat(b.inertia_frame[s]);
                f * Mat3::from_diagonal(Vec3::new(1.0 / ii.x, 1.0 / ii.y, 1.0 / ii.z)) * f.transpose()
            }
        }
    };
    super::body::m3::from_glam(&m)
}

/// The per-body arbiter Studio (STEP §3.3) and the proposal Studio (§3.5);
/// returns a level to queue (type 3).
fn arbiter(p: &mut super::body::Persist, slot0: u8, phase: u8, sc: &Config) -> Option<u8> {
    let cap = if sc.enable_60hz { 4 } else { 2 };
    // 0: previous requests (Studio; SimAdaptiveDisableDebounce live off).
    p.prev_requests = p.requests;
    // 1: slot 6, position correction.
    p.requests[6] = if p.pcf > sc.pcf_threshold && p.cfl > 1e-6 { 1 } else { cap };
    // 2: slot 0, velocity / box criterion Studio (RemoveVelocityCriteriaHistory
    // Dependence live on), `box_level`.
    p.requests[0] = slot0;
    // 3: slot 4, condition number: c < t → 4, < 2t → 2, else 1; capped.
    let c = p.cond;
    p.cond = 1.0;
    if arb_debug() {
        eprintln!("ARB cond {c:.3}");
    }
    let t = sc.cond_threshold;
    let r4: u8 = if c < t { 4 } else if c < 2.0 * t { 2 } else { 1 };
    p.requests[4] = r4.min(cap);
    // Slot 3 (Humanoid / movers) and slot 5 (density, gates P & 0x40 and
    // P unknown: STEP Open 3) are written by their owners; none here.
    // 4: debounce: a rise is undone while the counter runs.
    let choose = |r: &[u8; 7]| r.iter().copied().filter(|&x| x != 0).min().unwrap_or(sc.max_level);
    let prev_min = p.prev_requests.iter().copied().filter(|&x| x != 0).min().unwrap_or(sc.max_level).min(sc.max_level);
    if prev_min < choose(&p.requests) && p.debounce > 0 {
        p.requests = p.prev_requests;
    }
    // Studio: L* = chooseLevel; a rise only on a tick aligned to it, a drop at once.
    let ls = choose(&p.requests);
    let l = p.level;
    if ls == l || p.pending {
        return None;
    }
    let to = if l < ls {
        if (l as u32 + phase as u32) % ls as u32 == 0 { ls } else { 0 }
    } else {
        ls
    };
    if to == 0 {
        return None;
    }
    p.pending = true;
    Some(to)
}

/// RUSTUDIO_SOLVER_ARB_DEBUG=1: every arbitration printed (ARB lines).
fn arb_debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_ARB_DEBUG").is_some())
}

/// Margin of the slot-0 box around the body's bounding box (RUSTUDIO_SOLVER_BOX_MARGIN).
fn box_margin() -> f32 {
    static M: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *M.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_BOX_MARGIN").ok().and_then(|x| x.parse().ok()).unwrap_or(0.3))
}

/// The slot-0 box of every body of a due island (STEP §3.4 [L3]: body..).
/// Measured (LABS-NOTES §2.1–§2.2, lab_arb_*): the union of the assembly's part Size
/// boxes, axis-aligned in the body's own frame, Massless parts included, grown by
/// 0.3 studs a side; fixed per assembly (not refreshed from the pose). Stored as the
/// centre relative to the centre of mass (body frame) and the half extents. A body
/// without a recorded box (none should be) falls back to its colliders' local boxes.
fn boxes(bodies: &mut BodySet, islands: &Islands, rb: &rp::RigidBodySet, colliders: &rp::ColliderSet) {
    let m = Vec3::splat(box_margin());
    for i in 0..islands.len() {
        if !islands.due[i] {
            continue;
        }
        for &s in islands.bodies(i) {
            let s = s as usize;
            let h = bodies.handle[s];
            let (lo, hi) = match bodies.size_box.get(&h) {
                Some(&b) => b,
                None => {
                    let Some(b) = rb.get(h) else { continue };
                    let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
                    for &c in b.colliders() {
                        if let Some(c) = colliders.get(c) {
                            let pw = c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY);
                            let a = c.shape().compute_aabb(&pw);
                            lo = lo.min(super::body::v(a.mins));
                            hi = hi.max(super::body::v(a.maxs));
                        }
                    }
                    (lo, hi)
                }
            };
            if lo.x > hi.x {
                bodies.box_c[s] = Vec3::ZERO;
                bodies.box_e[s] = Vec3::ZERO;
                continue;
            }
            bodies.box_c[s] = (lo + hi) * 0.5 - bodies.local_com[s];
            bodies.box_e[s] = (hi - lo) * 0.5 + m;
        }
    }
}

/// Studio (STEP §3.4): the level the box test asks for, with the
/// velocity after this solve (b, ω at).
fn box_level(bodies: &BodySet, s: usize, v: Vec3, w: Vec3, sc: &Config) -> u8 {
    let (c, e) = (bodies.box_c[s], bodies.box_e[s]);
    let (min, max) = (c - e, c + e);
    if max.x < min.x || max.y < min.y || max.z < min.z {
        return 1;
    }
    if !min.is_finite() || !max.is_finite() {
        return 1;
    }
    let d = max - min;
    let vol = d.x * d.y * d.z;
    if vol <= 0.0 || !vol.is_finite() {
        return 1;
    }
    let overlap = |n: f32| {
        let dt = sc_h(sc) * n;
        let r = super::body::m3::to_glam(&super::body::m3::rot(w * dt));
        let c2 = r * c + v * dt;
        let ra = Mat3::from_cols(r.x_axis.abs(), r.y_axis.abs(), r.z_axis.abs());
        let e2 = ra * e;
        let lo = (c - e).max(c2 - e2);
        let hi = (c + e).min(c2 + e2);
        let x = (hi - lo).max(Vec3::ZERO);
        x.x * x.y * x.z / vol
    };
    // TimestepArbiterBoundingBoxIntersectionThresholdThou1 / 2 = 500 / 250 (STEP §9).
    if sc.enable_60hz && overlap(4.0) > 0.5 {
        return 4;
    }
    if overlap(2.0) > 0.25 { 2 } else { 1 }
}

#[inline]
fn sc_h(sc: &Config) -> f32 {
    sc.h
}

/// Kinematic bodies go to their target pose every tick.
fn kinematic(bodies: &mut BodySet, h: f32) {
    for s in 0..bodies.len() {
        if bodies.flags[s] & flag::KINEMATIC != 0 && (bodies.next_pos[s] != bodies.pos[s] || bodies.next_rot[s] != bodies.rot[s]) {
            let inv_h = 1.0 / h;
            bodies.linvel[s] = (bodies.next_pos[s] - bodies.pos[s]) * inv_h;
            let dq = bodies.next_rot[s] * bodies.rot[s].inverse();
            let (axis, ang) = dq.to_axis_angle();
            bodies.angvel[s] = if ang.is_finite() { axis * ang * inv_h } else { Vec3::ZERO };
            bodies.pos[s] = bodies.next_pos[s];
            bodies.rot[s] = bodies.next_rot[s];
            bodies.flags[s] |= flag::DIRTY;
        }
    }
}

/// Next-gen path stand-in for the external forces (ITER owns it).
fn external(island: &[u32], bodies: &mut BodySet, h: f32, gravity: Vec3) {
    for &s in island {
        let s = s as usize;
        if bodies.flags[s] & flag::DYNAMIC == 0 {
            continue;
        }
        let a = gravity * bodies.gravity_scale[s] + bodies.force[s] * bodies.inv_mass[s];
        bodies.linvel[s] += a * h;
        bodies.angvel[s] += bodies.inv_inertia_world[s] * bodies.torque[s] * h;
    }
}

/// ITER (next-gen path): the island's primal solve, from the assembled system
/// to new velocities (objective, iterations, line search, convergence above
/// Studio).
fn iterate(island: &[u32], bodies: &mut BodySet, rows: &RowBuffer, sys: &mut LinSys, h: f32) {
    if rows.len() > 0 {
        crate::todo_spec!("ITER §2 augmented Lagrangian + Newton (2.1–2.6), §3 row bounds, §4 result to bodies and connectors");
        let _ = (island, bodies, sys, h);
    }
}


#[cfg(test)]
mod gyro_guard_tests {
    use super::*;

    /// Place B's runaway rods (STATUS round RUNAWAY): a 25.5 × 0.78 cylinder (mass 8.63) given
    /// ω = (110, 20, 0) in one 1/60 s step. Studio's update alone takes |ω| to ~900 rad/s; with the
    /// guard the step keeps the rotational energy (and |ω|) within 1 %.
    #[test]
    fn thin_rod_step_keeps_its_energy() {
        let ib: super::super::pose::M3 = [0.6639, 0.0, 0.0, 0.0, 468.94, 0.0, 0.0, 0.0, 468.94];
        let r0: super::super::pose::M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let w0 = Vec3::new(110.0, 20.0, 0.0);
        let dt = 1.0 / 60.0;
        let (_, raw) = super::super::pose::gyro_step(&r0, w0, &ib, dt, super::super::pose::tol_of(dt));
        assert!(raw.length() > 4.0 * w0.length(), "Studio's update alone blows up: {raw}");
        let ke = |r: &super::super::pose::M3, w: Vec3| {
            let wb = Vec3::new(r[0] * w.x + r[3] * w.y + r[6] * w.z, r[1] * w.x + r[4] * w.y + r[7] * w.z, r[2] * w.x + r[5] * w.y + r[8] * w.z);
            0.5 * (ib[0] * wb.x * wb.x + ib[4] * wb.y * wb.y + ib[8] * wb.z * wb.z)
        };
        let (mut r, mut w) = (r0, w0);
        for _ in 0..240 {
            (r, w) = gyro_guarded(&r, w, &ib, dt);
        }
        assert!(ke(&r, w) <= ke(&r0, w0) * 1.02, "energy {} -> {}", ke(&r0, w0), ke(&r, w));
        assert!(w.length() < 1.5 * w0.length(), "|ω| {} -> {}", w0.length(), w.length());
    }

    /// A small turn per step: the guard leaves Studio's update untouched (bit for bit).
    #[test]
    fn small_turns_are_untouched() {
        let ib: super::super::pose::M3 = [0.6639, 0.0, 0.0, 0.0, 468.94, 0.0, 0.0, 0.0, 468.94];
        let r0: super::super::pose::M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let w0 = Vec3::new(3.0, 2.0, 1.0);
        let dt = 1.0 / 240.0;
        let a = super::super::pose::gyro_step(&r0, w0, &ib, dt, super::super::pose::tol_of(dt));
        let b = gyro_guarded(&r0, w0, &ib, dt);
        assert_eq!(a.0, b.0);
        assert_eq!(a.1, b.1);
    }
}

/// The collision pipeline changes behind the default solver (vendored rapier):
/// touch-quiet sensor pairs and offset-only collider moves.
#[cfg(test)]
mod collision_change_tests {
    use rapier3d::prelude as rp;

    fn world() -> (rp::PhysicsWorld, rp::ColliderHandle) {
        let mut w = rp::PhysicsWorld::new();
        let ground = w.insert_collider(rp::ColliderBuilder::cuboid(10.0, 0.5, 10.0), None);
        (w, ground)
    }

    fn detect(w: &mut rp::PhysicsWorld) {
        w.detect_collisions(&(), &());
    }

    /// A sensor pair is made and tested unless both colliders are touch-quiet; a quiet
    /// collider gaining interest again re-pairs. Contact pairs ignore the flag.
    #[test]
    fn quiet_sensor_pairs_are_skipped() {
        let (mut w, ground) = world();
        let b = w.insert_body(rp::RigidBodyBuilder::dynamic().translation(rp::Vector::new(0.0, 0.6, 0.0)).build());
        let sensor = w.insert_collider(rp::ColliderBuilder::ball(0.5).sensor(true), Some(b));
        let solid = w.insert_collider(rp::ColliderBuilder::cuboid(0.4, 0.4, 0.4), Some(b));
        detect(&mut w);
        assert_eq!(w.narrow_phase.intersection_pair(sensor, ground), Some(true));
        for c in [sensor, ground, solid] {
            w.colliders.get_mut(c).unwrap().set_touch_quiet(true);
        }
        detect(&mut w);
        assert_ne!(w.narrow_phase.intersection_pair(sensor, ground), Some(true), "both quiet: no overlap test");
        assert!(w.narrow_phase.contact_pair(solid, ground).is_some_and(|p| p.has_any_active_contact()), "contacts unaffected");
        w.colliders.get_mut(ground).unwrap().set_touch_quiet(false);
        detect(&mut w);
        assert_eq!(w.narrow_phase.intersection_pair(sensor, ground), Some(true), "interest back: pair again");
        let before = w.colliders.inserted_count();
        w.insert_collider(rp::ColliderBuilder::ball(0.1), None);
        assert_eq!(w.colliders.inserted_count(), before + 1);
    }

    #[test]
    fn rounded_cylinder_retains_four_or_fewer_contact_anchors() {
        let (mut world, ground) = world();
        let body = world.insert_body(rp::RigidBodyBuilder::dynamic()
            .translation(rp::Vector::new(0.0, 1.28, 0.0)));
        let cylinder = world.insert_collider(rp::ColliderBuilder::round_cylinder(0.75, 0.45, 0.05), Some(body));
        let anchors = |world: &rp::PhysicsWorld| {
            let pair = world.narrow_phase.contact_pair(cylinder, ground).unwrap();
            assert!(pair.has_any_active_contact());
            let points = &pair.manifolds()[0].points;
            assert!((1..=4).contains(&points.len()));
            points.iter().map(|p| (p.local_p1, p.local_p2)).collect::<Vec<_>>()
        };
        detect(&mut world);
        let saved = anchors(&world);
        world.bodies.get_mut(body).unwrap().set_translation(rp::Vector::new(0.0, 1.2801, 0.0), false);
        detect(&mut world);
        assert_eq!(saved, anchors(&world));
    }

    #[test]
    fn incremental_gather_tracks_disabled_and_replaced_bodies() {
        let mut world = rp::PhysicsWorld::new();
        let mut bodies = super::BodySet::default();
        let a = world.insert_body(rp::RigidBodyBuilder::dynamic());
        let b = world.insert_body(rp::RigidBodyBuilder::dynamic());
        let gather = |world: &mut rp::PhysicsWorld, bodies: &mut super::BodySet| {
            detect(world);
            bodies.gather(&world.bodies, Some(world.collision_pipeline.processed_bodies()));
        };
        gather(&mut world, &mut bodies);
        assert_eq!(bodies.handle, [a, b]);

        world.bodies.get_mut(a).unwrap().set_enabled(false);
        gather(&mut world, &mut bodies);
        assert_eq!(bodies.handle, [b]);

        world.bodies.get_mut(a).unwrap().set_enabled(true);
        gather(&mut world, &mut bodies);
        assert_eq!(bodies.handle, [a, b]);

        world.remove_body(a);
        let fixed = world.insert_body(rp::RigidBodyBuilder::fixed());
        gather(&mut world, &mut bodies);
        assert_eq!(bodies.handle, [b]);
        assert_eq!(bodies.slot(a), super::NONE);
        assert_eq!(bodies.slot(fixed), super::NONE);
    }

    #[test]
    fn sparse_broad_phase_tracks_a_moved_anchor() {
        let (mut world, ground) = world();
        for i in 0..64 {
            let pos = rp::Vector::new(100.0 + 3.0 * i as f32, 0.0, 0.0);
            world.insert_collider(rp::ColliderBuilder::ball(1.0).translation(pos), None);
        }
        let pos = rp::Vector::new(0.0, 4.0, 0.0);
        let body = world.insert_body(rp::RigidBodyBuilder::dynamic().translation(pos).build());
        let ball = world.insert_collider(rp::ColliderBuilder::ball(0.5), Some(body));
        detect(&mut world);
        assert!(world.narrow_phase.contact_pair(ball, ground).is_none());

        world.colliders.get_mut(ground).unwrap()
            .set_translation(rp::Vector::new(0.0, 3.5, 0.0));
        detect(&mut world);
        assert!(world.narrow_phase.contact_pair(ball, ground)
            .is_some_and(|pair| pair.has_any_active_contact()));

        world.colliders.get_mut(ground).unwrap().set_translation(rp::Vector::ZERO);
        detect(&mut world);
        assert!(world.narrow_phase.contact_pair(ball, ground).is_none());

        let irrelevant = world.insert_collider(rp::ColliderBuilder::ball(0.1), None);
        detect(&mut world);
        world.remove_collider(irrelevant);
        world.colliders.get_mut(ground).unwrap()
            .set_translation(rp::Vector::new(0.0, 3.5, 0.0));
        detect(&mut world);
        assert!(world.narrow_phase.contact_pair(ball, ground)
            .is_some_and(|pair| pair.has_any_active_contact()));
    }

    /// Moving a collider within its body (set_position_wrt_parent) moves it in the
    /// world and in the broad / narrow phases as a body move would.
    #[test]
    fn offset_moves_update_pairs() {
        let (mut w, ground) = world();
        let b = w.insert_body(rp::RigidBodyBuilder::dynamic().translation(rp::Vector::new(0.0, 5.0, 0.0)).build());
        let c = w.insert_collider(rp::ColliderBuilder::cuboid(0.4, 0.4, 0.4), Some(b));
        detect(&mut w);
        assert!(!w.narrow_phase.contact_pair(c, ground).is_some_and(|p| p.has_any_active_contact()));
        // Down by 4.2: the box's bottom 0.4 into the ground's top at 0.5.
        w.colliders.get_mut(c).unwrap().set_position_wrt_parent(rp::Pose::from_translation(rp::Vector::new(0.0, -4.2, 0.0)));
        detect(&mut w);
        let y = w.colliders.get(c).unwrap().position().translation.y;
        assert!((y - 0.8).abs() < 1e-5, "world pose follows the offset: {y}");
        assert!(w.narrow_phase.contact_pair(c, ground).is_some_and(|p| p.has_any_active_contact()), "new contact found");
        w.colliders.get_mut(c).unwrap().set_position_wrt_parent(rp::Pose::IDENTITY);
        detect(&mut w);
        assert!(!w.narrow_phase.contact_pair(c, ground).is_some_and(|p| p.has_any_active_contact()), "contact gone");
        // A massive collider's offset still moves the body's centre of mass; a massless
        // one's leaves the mass properties alone.
        // (rapier's collision pipeline recomputes a body's mass properties on the step
        // after a collider change marked it, before as after PARENT_OFFSET.)
        w.colliders.get_mut(c).unwrap().set_position_wrt_parent(rp::Pose::from_translation(rp::Vector::new(1.0, 0.0, 0.0)));
        detect(&mut w);
        detect(&mut w);
        let com = w.bodies.get(b).unwrap().center_of_mass();
        assert!((com.x - 1.0).abs() < 1e-5, "centre of mass follows: {com:?}");
        let ghost = w.insert_collider(rp::ColliderBuilder::cuboid(0.4, 0.4, 0.4).density(0.0), Some(b));
        detect(&mut w);
        detect(&mut w);
        let m = w.bodies.get(b).unwrap().mass();
        w.colliders.get_mut(ghost).unwrap().set_position_wrt_parent(rp::Pose::from_translation(rp::Vector::new(-3.0, 0.0, 0.0)));
        detect(&mut w);
        detect(&mut w);
        let bd = w.bodies.get(b).unwrap();
        assert_eq!((bd.mass(), bd.center_of_mass().x), (m, com.x));
    }
}
