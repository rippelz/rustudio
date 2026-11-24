//! Assembly sleep and wake for the legacy solver (SLEEP.md; addresses 0.744's).
//!
//! One SolverRigidBody per root assembly, so the state lives per body (`Sleep`, in
//! `Persist`). States (a, SLEEP §0.1): 3 awake, 4 asleep but kept in the
//! solver (in its island, not integrated), 5 asleep and out of the solver
//! (velocity zeroed and remembered, not in any island: a non-sim node of v = 0 for
//! whatever touches it), 2 wake-pending.
//!
//! The tick (SLEEP §5), driven from step.rs:
//! ```text
//!  pre_contact   property-write wakes (rapier's sleeping flag is the channel), state 2 → 3,
//!                solvedThisTick = false                                           §1 step 2, §7
//!  nothing due and lists 2/4/5 empty → history only (end_tick), no decisions      §1 step 4
//!  post_contact  deferred contact wakes, the per-assembly decision (§2.1), the
//!                state-4 retest (§6), the transitions (§2.2 / §2.3)               §2
//!  solve         after_solve (§3 + §4) per integrated body, asleep_solve (§6 input) per state-4 body
//!  end_tick      history for every awake body not integrated this tick            §4.3
//! ```
//! Rapier's own sleep flag mirrors state 5: `step.rs` puts a body entering 5 to
//! sleep in rapier (zero velocity, physics.rs stops publishing it); a property
//! write wakes it there (physics.rs `set_velocity` / `wake_part` → `wake_up`), and
//! `gather` turns that into SLEEP §7.2's property-write wake.

use glam::{Quat, Vec3};

use super::body::{flag, BodySet, NONE};

/// AssemblyHistoryBufferSize (FInt Studio) = 15 (SLEEP §0.4).
const BUFFER: i32 = 15;
/// AssemblyHistorySkipSize = 8 (§4.1).
const SKIP: i32 = 8;
/// SimSolverImprovedPartialSleepTolerance 5 · 0.01 (.rdata Studio) (§2.1, §4).
const F: f32 = 5.0 * 0.01;
/// SimSolverImprovedPartialSleepAngularWeight 80 · 0.01 (§4.2 step 6).
const ANG_W: f32 = 80.0 * 0.01;
/// SimSleepAngularAccelerationMultiplier = 10 (§4 step 4).
const K_ANG: f32 = 10.0;
/// SimActuatedJointSleepToleranceDivider = 20 (§2.1).
const DIVIDER: f32 = 20.0;
/// SimSolverWakeDisplacementMultiplier 10 · 0.001 (.rdata Studio) (§6).
const WAKE_C: f32 = 10.0 * 0.001;
/// SimSolverWakeDisplacementScalingNoContacts = 50 (§6).
const WAKE_NC: f32 = 50.0;
/// OptimizedContactWakeDepth = 8 (§7.2); Studio(prim) taken as 0.
const WAKE_DEPTH: i32 = 8;
/// body, the characteristic length r, for a body without a record of its own (one not
/// built by physics.rs `add_assembly`: a Humanoid's body). **FITTED: 1** (the value the
/// loose and joint rows were fitted with before SLEEP-2). Every assembly gets Studio's r
/// (Studio, SLEEP §2.1 [S2]) through `BodySet::char_len`.
const R_FALLBACK: f32 = 1.0;

/// RUSTUDIO_SOLVER_SLEEP_R: one r for every body (diagnostic; overrides Studio's).
pub fn r_override() -> Option<f32> {
    static R: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *R.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_SLEEP_R").ok().and_then(|x| x.parse().ok()))
}

/// r of a body: the override, else its assembly's, else the fallback.
pub fn r_of(b: &BodySet, s: usize) -> f32 {
    r_override().or_else(|| b.char_len.get(&b.handle[s]).copied()).unwrap_or(R_FALLBACK)
}

pub const AWAKE: u8 = 3;
pub const ASLEEP_IN: u8 = 4;
pub const ASLEEP_OUT: u8 = 5;
pub const WAKE_PENDING: u8 = 2;

/// Pose history H = body (SLEEP §0.4).
#[derive(Debug, Clone, Copy)]
pub struct Hist {
    pub s: [Vec3; 2],
    pub idx: u8,
    pub qbar: Quat,
    pub skip: i32,
    pub countdown: i32,
    pub var: f32,
    pub ang: f32,
}

impl Default for Hist {
    /// The constructor Studio: s = 0, q̄ = identity, countdown 15.
    fn default() -> Self {
        Hist { s: [Vec3::ZERO; 2], idx: 0, qbar: Quat::IDENTITY, skip: 0, countdown: BUFFER, var: 0.0, ang: 0.0 }
    }
}

impl Hist {
    /// Reset Studio(H, pose): s0 = s1 = CoM, q̄ = normalize(quat(R)), skip 0,
    /// countdown 15, var = ang = 0.
    pub fn reset(&mut self, com: Vec3, rot: Quat) {
        *self = Hist { s: [com; 2], idx: 0, qbar: rot.normalize(), ..Default::default() };
    }

    /// Studio (§4.1): one counted world step; true when the
    /// countdown has run out and the spread is under `tol`.
    pub fn tick(&mut self, com: Vec3, rot: Quat, r: f32, tol: f32) -> bool {
        self.skip += 1;
        if self.skip == SKIP {
            self.countdown = (self.countdown - 1).max(0);
            self.skip = 0;
            self.update(com, rot, r);
        }
        self.countdown == 0 && self.var + self.ang < tol
    }

    /// Studio (§4.2), f32.
    fn update(&mut self, cur: Vec3, rot: Quat, r: f32) {
        let q = rot.normalize();
        let m = (cur + self.s[0] + self.s[1]) * (1.0 / 3.0);
        let d = (self.s[0] - m).length_squared().max((cur - m).length_squared()).max((self.s[1] - m).length_squared());
        self.s[self.idx as usize] = cur;
        self.idx = 1 - self.idx;
        let k = (BUFFER - 1) as f32 * (1.0 / 3.0);
        self.var = (k * k) * d;
        // 6: q̄ = normalize(blend(q, q̄, w)) — Studio read as a normalised lerp toward
        // q̄ by w (SLEEP §4.2 "slerp/nlerp"); with r = 0 step 8 never reads it.
        let qb = if q.dot(self.qbar) < 0.0 { -self.qbar } else { self.qbar };
        self.qbar = (q * (1.0 - ANG_W) + qb * ANG_W).normalize();
        // 7: θ = angle between q and q̄.
        let dq = self.qbar.conjugate() * q;
        let th = 2.0 * Vec3::new(dq.x, dq.y, dq.z).length().atan2(dq.w);
        // 8
        self.ang = ANG_W * ((1.0 - ANG_W) * (th * r) * (th * r) + self.ang);
    }
}

/// The sleep fields of one SolverRigidBody / its root assembly (SLEEP §0.1, §0.3).
#[derive(Debug, Clone, Copy)]
pub struct Sleep {
    /// a; 0 = not yet seen by the sleep stage.
    pub state: u8,
    pub hist: Hist,
    /// /: linear / angular activity over the maxLevel window.
    pub act_v: f32,
    pub act_w: f32,
    /// sleep-eligible.
    pub eligible: bool,
    /// solved this tick.
    pub solved: bool,
    /// ā_v, ā_ω.
    pub abar_v: Vec3,
    pub abar_w: Vec3,
    /// The previous state's velocities ( /): the start of the last solve.
    pub v_prev: Vec3,
    pub w_prev: Vec3,
    ///..: the velocity saved on entering state 5.
    pub saved_v: Vec3,
    pub saved_w: Vec3,
    /// Woken in rapier (a property write) while in state 5: handled by pre_contact.
    pub poked: bool,
    /// Woken in rapier by rapier itself while in state 5: put back to sleep there.
    pub resleep: bool,
    /// A Humanoid stands on it (its floor reaction, Studio, this frame):
    /// pre_contact wakes it with depth 0 (CHAR-NOTES "Filled from recordings", T18).
    pub floor_wake: bool,
    /// The body pose when it entered state 5.
    pub sleep_pos: Vec3,
    pub sleep_rot: Quat,
}

impl Default for Sleep {
    fn default() -> Self {
        Sleep {
            state: 0,
            hist: Hist::default(),
            act_v: 0.0,
            act_w: 0.0,
            eligible: false,
            solved: false,
            abar_v: Vec3::ZERO,
            abar_w: Vec3::ZERO,
            v_prev: Vec3::ZERO,
            w_prev: Vec3::ZERO,
            saved_v: Vec3::ZERO,
            saved_w: Vec3::ZERO,
            poked: false,
            resleep: false,
            floor_wake: false,
            sleep_pos: Vec3::ZERO,
            sleep_rot: Quat::IDENTITY,
        }
    }
}

impl Sleep {
    /// From `BodySet::gather`: whether the body is out of the solver (state 5).
    /// rapier's sleeping flag cleared while in state 5 = a property write woke it.
    /// rapier also wakes bodies on its own (its narrow phase wakes both sides of a pair
    /// that starts touching): only a changed velocity or pose counts as a write; any
    /// other rapier wake is undone (`resleep`).
    pub fn gather(&mut self, rapier_sleeping: bool, v: Vec3, w: Vec3, pos: Vec3, rot: Quat) -> bool {
        if self.state == ASLEEP_OUT && !rapier_sleeping {
            if v != Vec3::ZERO || w != Vec3::ZERO || pos != self.sleep_pos || rot != self.sleep_rot {
                self.poked = true;
            } else {
                self.resleep = true;
            }
        }
        self.state == ASLEEP_OUT
    }

    /// §3 + §4, after an integrated body's write-back (Studio then Studio).
    /// `v0, w0` = the velocity at the solve's start, `v, w` = after it; `pose` the new
    /// one; `tick_byte` = the world phase; `level` = body.
    pub fn after_solve(&mut self, v0: Vec3, w0: Vec3, v: Vec3, w: Vec3, com: Vec3, rot: Quat, tick_byte: u8, level: u8, max_level: u8, r: f32) {
        // §3 step 1: ā = 0.1·Δ + 0.9·ā (per component, in that order).
        self.abar_w = (w - w0) * 0.1 + self.abar_w * 0.9;
        self.abar_v = (v - v0) * 0.1 + self.abar_v * 0.9;
        self.v_prev = v0;
        self.w_prev = w0;
        let (qv, qw) = (self.abar_v.length_squared(), self.abar_w.length_squared());
        if tick_byte % max_level == 0 {
            self.act_v = qv;
            self.act_w = qw;
        } else {
            self.act_v = self.act_v.max(qv);
            self.act_w = self.act_w.max(qw);
        }
        // §4.
        self.solved = true;
        let tol = F * F;
        let h = self.hist.tick(com, rot, r, tol);
        let l2 = (level as f32) * (level as f32);
        self.eligible = h && self.act_v <= l2 * 1e-6 && r * r * self.act_w <= l2 * K_ANG * K_ANG * 1e-6;
    }

    /// Studio (STEP §4.2a step 2): an asleep body of a solved island feeds the
    /// would-be velocity change Δv', Δω' into / (raw, max over the window).
    pub fn asleep_solve(&mut self, dv: Vec3, dw: Vec3, tick_byte: u8, max_level: u8) {
        let (a, b) = (dv.length_squared(), dw.length_squared());
        if tick_byte % max_level == 0 {
            self.act_v = a;
            self.act_w = b;
        } else {
            self.act_v = self.act_v.max(a);
            self.act_w = self.act_w.max(b);
        }
    }

    /// Studio(a, depth) body part (§7.1): eligibility off, ā = the unfiltered
    /// step, the 120-tick clock restarts (samples kept).
    fn wake_reset(&mut self, v: Vec3, w: Vec3) {
        self.eligible = false;
        self.abar_v = v - self.v_prev;
        self.abar_w = w - self.w_prev;
        self.hist.countdown = BUFFER;
        self.hist.skip = 0;
    }

    /// Studio: an awake partner of a woken assembly: not eligible, and its
    /// countdown at least 1 (2 when the skip counter is within 2 of an update).
    fn touch(&mut self) {
        self.eligible = false;
        let m = (self.hist.skip >= SKIP - 2) as i32 + 1;
        self.hist.countdown = self.hist.countdown.max(m);
    }
}

/// A transition the sleep stage asks of the solver for one body slot.
#[derive(Debug, Clone, Copy)]
struct Change {
    slot: u32,
    to: u8,
}

/// The sleep stage's per-tick scratch (contact manager C's work lists, §0.2).
#[derive(Default)]
pub struct SleepStage {
    to4: Vec<u32>,
    to5: Vec<u32>,
    wake: Vec<u32>,
    bfs: Vec<(u32, i32)>,
    /// Slots that entered or left state 5 this tick: step.rs mirrors it in rapier's
    /// sleeping flag (`sync_rapier`).
    pub changed: Vec<u32>,
    /// A state 2 / 4 / 5 body exists (lists C / / non-empty).
    pub any_asleep: bool,
    /// Colliders removed, rebuilt, moved (fixed ones) or regrouped since the last tick
    /// (StudioSolver::support_changed), and terrain regions that changed (`regions`).
    pub support: rustc_hash::FxHashSet<rapier3d::prelude::ColliderHandle>,
    pub regions: Vec<(Vec3, Vec3)>,
}

/// What the stage reads about the world besides the bodies.
pub struct World<'a> {
    pub contacts: &'a super::contact::ContactStore,
    pub joints: &'a super::joints::JointList,
    pub rb: &'a rapier3d::prelude::RigidBodySet,
    pub colliders: &'a rapier3d::prelude::ColliderSet,
    pub np: &'a rapier3d::prelude::NarrowPhase,
}

#[inline]
fn pidx(b: &BodySet, s: usize) -> usize {
    b.handle[s].into_raw_parts().0 as usize
}

#[inline]
fn st(b: &BodySet, s: usize) -> &Sleep {
    &b.persist[pidx(b, s)].sleep
}

#[inline]
fn st_mut(b: &mut BodySet, s: usize) -> &mut Sleep {
    let i = pidx(b, s);
    &mut b.persist[i].sleep
}

/// Dynamic and taking part in the sleep system.
#[inline]
fn dynamic(b: &BodySet, s: u32) -> bool {
    s != NONE && b.flags[s as usize] & flag::DYNAMIC != 0
}

/// The re-entry level Studio(world, L): the largest L' ≤ L with (phase & (L' − 1)) == 0.
fn reentry(level: u8, phase: u8) -> u8 {
    let mut l = level.max(1);
    while l > 1 && phase & (l - 1) != 0 {
        l >>= 1;
    }
    l
}

impl SleepStage {
    /// preContactStepSleepStage Studio (SLEEP §1 step 2) plus the property-write
    /// wakes rapier reports (§7.2 first bullet): new bodies get their history reset;
    /// a poked state-5 body wakes with depth 8 and spreads over its active contacts;
    /// state-2 bodies go awake; solvedThisTick is cleared for awake bodies.
    pub fn pre_contact(&mut self, b: &mut BodySet, w: &World, phase: u8) {
        self.changed.clear();
        self.any_asleep = false;
        for s in 0..b.len() {
            if b.flags[s] & flag::DYNAMIC == 0 {
                continue;
            }
            let (com, rot) = (b.com[s], b.rot[s]);
            let sl = st_mut(b, s);
            if sl.state == 0 {
                // Added to the world: awake, history reset at its pose.
                sl.state = AWAKE;
                sl.hist.reset(com, rot);
            }
        }
        // RUSTUDIO_SOLVER_LAB_WAKE=1 (harness: run_rustudio_play.py, CHAR-NOTES): Studio's
        // character labs step with workspace:StepPhysics(1/240, parts), every unanchored
        // part listed; StepPhysics Studio wakes the listed
        // assemblies in state 4 / 5 (Studio(C, a, 0), §7.2 "StepPhysics"), and the
        // recordings' loose bodies are still settling from the lab's 40 ticks (r15_push4's
        // block creeps down at vy −0.0002 up to the impact): none of them is asleep in 480
        // ticks. rustudio's Play run settles for 1 s first, so its block would be asleep
        // (and re-enter at level 1 with the debounce armed when hit). The harness keeps
        // every body awake: asleep ones woken (depth 0), awake ones' 120-tick clock
        // restarted as a wake does (Studio: countdown 15, not eligible).
        if lab_wake() {
            for s in 0..b.len() as u32 {
                if !dynamic(b, s) {
                    continue;
                }
                if matches!(st(b, s as usize).state, ASLEEP_IN | ASLEEP_OUT) {
                    self.wake_spread(b, w, s, 0, phase, false);
                }
                let sl = st_mut(b, s as usize);
                sl.eligible = false;
                sl.hist.countdown = 15;
            }
        }
        // A Humanoid's floor (CHAR-NOTES "Filled from recordings"): Studio only adds
        // the reaction to the floor body's accumulator (it wakes nothing, and stepHumanoid
        // Studio keeps only the character's own assembly awake: Studio /
        // Studio on *(H), the torso), yet T18's sleeping platforms take the
        // character's weight in Studio (heavy 0.985 m g, light 0.092 m g, both sunk on arrival,
        // the heavy one later back to rest with the weight on it). The port wakes an asleep
        // floor body with depth 0 when a Humanoid's reaction lands on it.
        for s in 0..b.len() as u32 {
            if dynamic(b, s) && std::mem::take(&mut st_mut(b, s as usize).floor_wake) && matches!(st(b, s as usize).state, ASLEEP_IN | ASLEEP_OUT) {
                self.wake_spread(b, w, s, 0, phase, false);
            }
        }
        self.support_wakes(b, w, phase);
        for s in 0..b.len() as u32 {
            if dynamic(b, s) && st(b, s as usize).resleep {
                st_mut(b, s as usize).resleep = false;
                self.changed.push(s);
            }
            if !dynamic(b, s) || !st(b, s as usize).poked {
                continue;
            }
            st_mut(b, s as usize).poked = false;
            // Studio(C, root, depth = OptimizedContactWakeDepth − 0).
            self.wake_spread(b, w, s, WAKE_DEPTH, phase, true);
        }
        for s in 0..b.len() {
            if b.flags[s] & flag::DYNAMIC == 0 {
                continue;
            }
            // Studio: the state-2 list → awake (depth 0).
            if st(b, s).state == WAKE_PENDING {
                wake(b, s, phase, false, &mut self.changed);
            }
            let sl = st_mut(b, s);
            if sl.state == AWAKE {
                sl.solved = false;
            }
            if sl.state != AWAKE {
                self.any_asleep = true;
            }
        }
    }

    /// SLEEP §7.2, first bullet, for the support side: a property write on a part (CFrame,
    /// CanCollide, Size, CollisionGroup, Anchored, its removal) wakes its assembly with depth
    /// OptimizedContactWakeDepth (8) and spreads over its contacts, so an asleep body resting
    /// on or touching the part wakes with depth 7. An anchored or removed part has no body in
    /// the port: its contacts are found here by collider, from the manifolds of the last tick
    /// (a removed collider's are still in the store) and from the narrow phase just run (a
    /// fixed part moved into a sleeper). A terrain edit wakes the asleep bodies whose
    /// colliders reach into the changed voxels. rapier's own wake of a removed collider's
    /// partners is a non-write here (`gather` re-sleeps it), so without this a body was left
    /// hanging where its support had been.
    fn support_wakes(&mut self, b: &mut BodySet, w: &World, phase: u8) {
        if self.support.is_empty() && self.regions.is_empty() {
            return;
        }
        let support = std::mem::take(&mut self.support);
        let regions = std::mem::take(&mut self.regions);
        let mut hit: Vec<rapier3d::prelude::RigidBodyHandle> = Vec::new();
        if !support.is_empty() {
            for m in &w.contacts.slots {
                if m.stamp == 0 {
                    continue;
                }
                if m.col1.is_some_and(|c| support.contains(&c)) {
                    hit.extend(m.body2);
                }
                if m.col2.is_some_and(|c| support.contains(&c)) {
                    hit.extend(m.body1);
                }
            }
            for &c in &support {
                if w.colliders.get(c).is_none() {
                    continue;
                }
                for pair in w.np.contact_pairs_with(c) {
                    if !pair.has_any_active_contact() {
                        continue;
                    }
                    let other = if pair.collider1 == c { pair.collider2 } else { pair.collider1 };
                    hit.extend(w.colliders.get(other).and_then(|o| o.parent()));
                }
            }
        }
        for (lo, hi) in &regions {
            // Smooth terrain's surface lies up to a voxel (4 studs) outside the changed voxels' boxes.
            let (lo, hi) = (*lo - Vec3::splat(4.5), *hi + Vec3::splat(4.5));
            for s in 0..b.len() as u32 {
                if !dynamic(b, s) || st(b, s as usize).state == AWAKE {
                    continue;
                }
                let Some(body) = w.rb.get(b.handle[s as usize]) else { continue };
                let reaches = body.colliders().iter().filter_map(|c| w.colliders.get(*c)).any(|c| {
                    let a = c.compute_aabb();
                    a.mins.x <= hi.x && a.maxs.x >= lo.x && a.mins.y <= hi.y && a.maxs.y >= lo.y && a.mins.z <= hi.z && a.maxs.z >= lo.z
                });
                if reaches {
                    hit.push(b.handle[s as usize]);
                }
            }
        }
        for h in hit {
            let s = b.slot(h);
            if s == NONE || b.handle[s as usize] != h || !dynamic(b, s) || st(b, s as usize).state == AWAKE {
                continue;
            }
            if std::env::var_os("RUSTUDIO_SOLVER_SLEEP_TRACE").is_some() {
                eprintln!("support wake {s}");
            }
            self.wake_spread(b, w, s, WAKE_DEPTH - 1, phase, false);
        }
    }

    /// Studio: wake `s`, then for each active contact of it: an awake
    /// partner is touched, an asleep one within depth is woken with
    /// depth − 1.
    fn wake_spread(&mut self, b: &mut BodySet, w: &World, s0: u32, depth: i32, phase: u8, keep_rapier_v: bool) {
        self.bfs.clear();
        self.bfs.push((s0, depth));
        let mut k = 0;
        while k < self.bfs.len() {
            let (s, d) = self.bfs[k];
            k += 1;
            if st(b, s as usize).state != AWAKE {
                wake(b, s as usize, phase, keep_rapier_v && s == s0, &mut self.changed);
            }
            if d < 0 {
                continue;
            }
            let h = b.handle[s as usize];
            for m in &w.contacts.slots {
                if m.stamp == 0 || !(m.body1 == Some(h) || m.body2 == Some(h)) {
                    continue;
                }
                // "active" = contact bit 1: some connector carried a point last step.
                if !m.conns[..m.n as usize].iter().any(|c| c.active) {
                    continue;
                }
                let other = if m.body1 == Some(h) { m.body2 } else { m.body1 };
                let Some(o) = other.map(|o| b.slot(o)) else { continue };
                if !dynamic(b, o) || o == s {
                    continue;
                }
                match st(b, o as usize).state {
                    AWAKE => st_mut(b, o as usize).touch(),
                    _ if d > 0 && !self.bfs.iter().any(|x| x.0 == o) => {
                        if std::env::var_os("RUSTUDIO_SOLVER_SLEEP_TRACE").is_some() {
                            eprintln!("spread {s} -> {o} via {:?}", m.key);
                        }
                        self.bfs.push((o, d - 1))
                    }
                    _ => {}
                }
            }
        }
    }

    /// postContactStepSleepStage Studio (SLEEP §2) with the deferred contact
    /// wakes of the narrow phase that precedes it (§7.2 second bullet). `tick_mod0` =
    /// tick % maxLevel == 0.
    pub fn post_contact(&mut self, b: &mut BodySet, w: &World, phase: u8, max_level: u8) {
        // §7.2 / CONTACT §2.4: a connector added to the solver this step on a pair with
        // an awake (3 / 4) side wakes an asleep partner, depth −1 (no spread).
        let l = &w.contacts.list;
        self.wake.clear();
        for m in 0..l.manifold_count() {
            if !l.added[m] {
                continue;
            }
            let (a, c) = (l.a[m], l.b[m]);
            for (x, y) in [(a, c), (c, a)] {
                if dynamic(b, x) && dynamic(b, y) && matches!(st(b, x as usize).state, AWAKE | ASLEEP_IN) && st(b, y as usize).state == ASLEEP_OUT {
                    self.wake.push(y);
                }
            }
        }
        for i in 0..self.wake.len() {
            let s = self.wake[i] as usize;
            if st(b, s).state != AWAKE {
                if std::env::var_os("RUSTUDIO_SOLVER_SLEEP_TRACE").is_some() {
                    eprintln!("contact wake");
                }
                wake(b, s, phase, false, &mut self.changed);
            }
        }
        // Step 2, stepAwakeAssemblies Studio: eligible awake assemblies.
        self.to4.clear();
        self.to5.clear();
        self.wake.clear();
        for s in 0..b.len() as u32 {
            if !dynamic(b, s) {
                continue;
            }
            let sl = st(b, s as usize);
            // stepAwakeAssemblies skips throttled assemblies on a throttled frame.
            if sl.state != AWAKE || !sl.eligible || b.flags[s as usize] & flag::FROZEN != 0 {
                continue;
            }
            // Stand-in (no SLEEP.md rule for it): a body a Humanoid / mover holds a slot-3
            // level request on (STEP §3.7) is not put to sleep here.
            if b.persist[pidx(b, s as usize)].requests[3] != 0 {
                continue;
            }
            match neighbour_test(b, w, s) {
                4 => self.to4.push(s),
                5 => self.to5.push(s),
                _ => {}
            }
        }
        // Step 3, Studio (§6): state-4 retest every maxLevel ticks.
        if phase % max_level == 0 {
            for s in 0..b.len() as u32 {
                if !dynamic(b, s) || st(b, s as usize).state != ASLEEP_IN || b.flags[s as usize] & flag::FROZEN != 0 {
                    continue;
                }
                let lv = b.persist[pidx(b, s as usize)].level.max(1);
                // Studio: not due this phase (level mask).
                if phase & (lv - 1) != 0 {
                    continue;
                }
                let sl = st(b, s as usize);
                let r = r_of(b, s as usize);
                let x = sl.act_v.max(r * r * sl.act_w) / (lv as f32 * lv as f32);
                let c2 = WAKE_C * WAKE_C;
                if x > WAKE_NC * c2 || (x > c2 && active_contact(b, w, s)) {
                    self.wake.push(s);
                } else {
                    match neighbour_test(b, w, s) {
                        3 => self.wake.push(s),
                        5 => self.to5.push(s),
                        _ => {}
                    }
                }
            }
        }
        // Step 4, Studio: the work lists in order C (→ 2), C (→ 4), C (→ 5).
        for i in 0..self.wake.len() {
            to_state(b, self.wake[i] as usize, WAKE_PENDING, phase, &mut self.changed);
        }
        for i in 0..self.to4.len() {
            to_state(b, self.to4[i] as usize, ASLEEP_IN, phase, &mut self.changed);
        }
        for i in 0..self.to5.len() {
            let s = self.to5[i] as usize;
            to_state(b, s, ASLEEP_OUT, phase, &mut self.changed);
        }
        // Step 5: the state-2 list → awake when tick % maxLevel == 0 (else at the next
        // preContact).
        if phase % max_level == 0 {
            for i in 0..self.wake.len() {
                let s = self.wake[i] as usize;
                if st(b, s).state == WAKE_PENDING {
                    wake(b, s, phase, false, &mut self.changed);
                }
            }
        }
        self.any_asleep = (0..b.len()).any(|s| b.flags[s] & flag::DYNAMIC != 0 && st(b, s).state != AWAKE);
    }

    /// §4.3: every awake assembly not integrated this World::step counts one history
    /// step (sampleAwakeNonSimulatingAssemblies Studio; it does not
    /// write).
    pub fn end_tick(&mut self, b: &mut BodySet) {
        for s in 0..b.len() {
            // A throttled frame does not run for a frozen body: no history step.
            if b.flags[s] & flag::DYNAMIC == 0 || b.flags[s] & flag::FROZEN != 0 {
                continue;
            }
            let (com, rot, r) = (b.com[s], b.rot[s], r_of(b, s));
            let sl = st_mut(b, s);
            if sl.state == AWAKE && !sl.solved {
                sl.hist.tick(com, rot, r, -0.0);
            }
        }
    }
}

/// Studio: the assembly has a contact with an active connector ( & 2).
fn active_contact(b: &BodySet, w: &World, s: u32) -> bool {
    let h = b.handle[s as usize];
    w.contacts.slots.iter().any(|m| m.stamp != 0 && (m.body1 == Some(h) || m.body2 == Some(h)) && m.conns[..m.n as usize].iter().any(|c| c.active))
}


/// Studio(C, a) → 3 / 4 / 5 (SLEEP §2.1). Edges: the broad-phase contact pairs of
/// the assembly's parts (Contact objects) and its constraint connectors.
fn neighbour_test(b: &BodySet, w: &World, s: u32) -> u8 {
    let d2 = DIVIDER * DIVIDER;
    let quiet_self = {
        let h = &st(b, s as usize).hist;
        h.var + h.ang < F * F / d2
    };
    let mut res = 1u8;
    let mut other = |o: u32, actuated: &dyn Fn() -> bool| -> Option<u8> {
        if dynamic(b, o) && st(b, o as usize).state == AWAKE {
            res = 0;
            // Studio(o, 2·f·f): the neighbour's history spread says it moves.
            let h = &st(b, o as usize).hist;
            if h.var + h.ang > 2.0 * F * F {
                return Some(3);
            }
        }
        // Studio: `!Studio(H_a, f²/D²) && e.vtbl[15](e)`, in that order.
        if !quiet_self && actuated() {
            return Some(3);
        }
        None
    };
    if let Some(rb) = w.rb.get(b.handle[s as usize]) {
        for &ch in rb.colliders() {
            for pair in w.np.contact_pairs_with(ch) {
                let oc = if pair.collider1 == ch { pair.collider2 } else { pair.collider1 };
                let o = w.colliders.get(oc).and_then(|c| c.parent()).map(|h| b.slot(h)).unwrap_or(NONE);
                if o == s {
                    continue;
                }
                if let Some(r) = other(o, &|| false) {
                    return r;
                }
            }
        }
    }
    // Joint edges: one per constraint Instance (its kernel joint KJ) or legacy mover (its
    // edge) in the world; an edge is in the assembly's list only when its other part
    // is in another assembly. The world side of a one-attachment class or a
    // mover is taken as an edge to "nothing" (the BodyGyro lab keeps its block awake).
    let j = w.joints;
    if old_actuated() {
        for c in 0..j.len() {
            let (a, bb) = (j.pose_a[c], j.pose_b[c]);
            if a != s && bb != s {
                continue;
            }
            let o = if a == s { bb } else { a };
            if let Some(r) = other(o, &|| j.conn_actuated(c)) {
                return r;
            }
        }
        return res + 4;
    }
    let pose = |h: rapier3d::prelude::RigidBodyHandle| {
        w.rb.get(h).map(|r| {
            let p = r.position();
            (glam::Mat3::from_quat(super::body::q(p.rotation)), super::body::v(p.translation))
        })
    };
    let sl = |h: Option<rapier3d::prelude::RigidBodyHandle>| h.map(|h| b.slot(h)).unwrap_or(NONE);
    for inst in j.insts.iter().flatten() {
        if !inst.in_world {
            continue;
        }
        let (sa, sb) = (sl(inst.a.body), sl(inst.b.body));
        if (sa != s && sb != s) || sa == sb {
            continue;
        }
        let o = if sa == s { sb } else { sa };
        if let Some(r) = other(o, &|| inst.edge_actuated(&pose)) {
            return r;
        }
    }
    res + 4
}

/// Studio(C, a, new) (SLEEP §2.2) with its solver side Studio (§2.3).
fn to_state(b: &mut BodySet, s: usize, new: u8, phase: u8, changed: &mut Vec<u32>) {
    let old = st(b, s).state;
    if old == new {
        return;
    }
    let (v, w, com, rot, pos) = (b.linvel[s], b.angvel[s], b.com[s], b.rot[s], b.pos[s]);
    trace(b, s, old, new);
    let sl = st_mut(b, s);
    sl.state = new;
    // 4: leaving 4 / 5 for a state outside them: Studio(a, −1) (body impulses
    // Studio: none are kept across ticks here).
    if matches!(old, ASLEEP_IN | ASLEEP_OUT) && !matches!(new, ASLEEP_IN | ASLEEP_OUT) {
        sl.wake_reset(v, w);
    }
    if matches!(new, ASLEEP_IN | ASLEEP_OUT) && old == AWAKE {
        // STEP §6: entering sleep resets the pose history (Studio(b, b)).
        sl.hist.reset(com, rot);
    }
    match new {
        ASLEEP_IN => {
            // §2.3 step 1: (re)inserted, asleep (body = 1): in its island, not integrated.
            if old == ASLEEP_OUT {
                // Studio: v, ω = the values saved on entering 5.
                let (sv, sw) = (sl.saved_v, sl.saved_w);
                b.linvel[s] = sv;
                b.angvel[s] = sw;
                b.flags[s] |= flag::DIRTY;
                restore(b, s, phase);
                changed.push(s as u32);
            }
            b.flags[s] |= flag::AWAKE;
        }
        ASLEEP_OUT => {
            // §2.3 step 2, Studio: saved = (v, ω); v = ω = 0; out of the kernel.
            sl.saved_v = v;
            sl.saved_w = w;
            sl.sleep_pos = pos;
            sl.sleep_rot = rot;
            b.linvel[s] = Vec3::ZERO;
            b.angvel[s] = Vec3::ZERO;
            b.flags[s] &= !flag::AWAKE;
            b.flags[s] |= flag::DIRTY;
            changed.push(s as u32);
        }
        WAKE_PENDING => {
            // Not awake: out of this tick's solve until it reaches state 3.
            b.flags[s] &= !flag::AWAKE;
        }
        _ => {}
    }
}

/// RUSTUDIO_SOLVER_ACTUATED=connectors: the fitted rule before SLEEP-2 (Motor / Servo /
/// LinServo connectors always count as actuated), for comparison.
fn old_actuated() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_ACTUATED").is_ok_and(|v| v == "connectors"))
}

/// RUSTUDIO_SOLVER_LAB_WAKE=1: StepPhysics' per-call wake of the listed parts (pre_contact).
fn lab_wake() -> bool {
    static W: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *W.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_LAB_WAKE").is_ok_and(|v| v == "1"))
}

/// RUSTUDIO_SOLVER_SLEEP_TRACE=1: every transition on stderr.
fn trace(b: &BodySet, s: usize, old: u8, new: u8) {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *T.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_SLEEP_TRACE").is_some()) {
        eprintln!("sleep slot {s} {old} -> {new} v {:?} m {:.3} pos {:?}", b.linvel[s], 1.0 / b.inv_mass[s], b.pos[s]);
    }
}

/// Studio(C, a, depth) (§7.1): to state 3, reinserted at the re-entry level with
/// the velocity saved at state 5 (unless a property write gave it a new one).
fn wake(b: &mut BodySet, s: usize, phase: u8, keep_rapier_v: bool, changed: &mut Vec<u32>) {
    let old = st(b, s).state;
    if old == AWAKE {
        return;
    }
    let (v, w) = (b.linvel[s], b.angvel[s]);
    trace(b, s, old, AWAKE);
    let sl = st_mut(b, s);
    sl.state = AWAKE;
    sl.wake_reset(v, w);
    if old == ASLEEP_OUT {
        changed.push(s as u32);
        // Studio: v, ω = the saved ones. A property write (rapier woken with a
        // velocity of its own) is the setter that ran after the wake (§7.2, stepF).
        let setter = keep_rapier_v && (v != Vec3::ZERO || w != Vec3::ZERO);
        if !setter {
            let (sv, sw) = (sl.saved_v, sl.saved_w);
            b.linvel[s] = sv;
            b.angvel[s] = sw;
            b.flags[s] |= flag::DIRTY;
        }
    }
    restore(b, s, phase);
    b.flags[s] |= flag::AWAKE;
}

/// Studio: back in the solver at the re-entry level L': body (the level list)
/// = Studio(world, body); body itself is left, so the next
/// getAdaptiveBodies sees a drop and arms the debounce (STEP §3.6).
fn restore(b: &mut BodySet, s: usize, phase: u8) {
    let i = pidx(b, s);
    let p = &mut b.persist[i];
    p.list_level = reentry(p.level, phase);
    // A new node of the island graph at that level [C4].
    p.node_level = p.list_level;
    p.pending = false;
}

/// rapier's sleeping flag = state 5 for the bodies that changed (see the module doc).
pub fn sync_rapier(b: &BodySet, changed: &[u32], rb: &mut rapier3d::prelude::RigidBodySet) {
    for &s in changed {
        let s = s as usize;
        let Some(r) = rb.get_mut(b.handle[s]) else { continue };
        if st(b, s).state == ASLEEP_OUT {
            r.sleep();
        } else if r.is_sleeping() {
            r.wake_up(false);
        }
    }
}

/// SLEEP §1 step 4: no body's level list is due this phase and no assembly is in state
/// 2 / 4 / 5 — then postContact does not run.
pub fn nothing_due(b: &BodySet, stage: &SleepStage, phase: u8) -> bool {
    if stage.any_asleep {
        return false;
    }
    !(0..b.len()).any(|s| {
        b.flags[s] & (flag::DYNAMIC | flag::AWAKE) == (flag::DYNAMIC | flag::AWAKE) && {
            let l = b.persist[pidx(b, s)].list_level.max(1);
            phase & (l - 1) == 0
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_needs_120_steps() {
        let mut h = Hist::default();
        h.reset(Vec3::ZERO, Quat::IDENTITY);
        let mut n = 0;
        while !h.tick(Vec3::ZERO, Quat::IDENTITY, 0.0, 0.0025) {
            n += 1;
        }
        assert_eq!(n + 1, 120);
    }

    #[test]
    fn reentry_level() {
        assert_eq!(reentry(4, 0), 4);
        assert_eq!(reentry(4, 2), 2);
        assert_eq!(reentry(4, 1), 1);
        assert_eq!(reentry(2, 3), 1);
    }
}
