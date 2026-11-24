//! Contacts: rapier / parry manifolds -> Studio's contact connectors, one per
//! manifold point, kept between ticks with their kept impulses (CONTACT.md §1, §2,
//! §3, §7; DUALROWS.md §4).
//!
//! Keys: a manifold is (collider 1, collider 2, subshape 1, subshape 2) — Studio's
//! connectors belong to a primitive pair (a Contact edge), a mesh's pieces are
//! told apart by the subshapes — and a connector inside it is matched by its
//! feature pair (fid1, fid2), parry's feature ids standing in for FeaturePoly's
//! (kind, idA, idB) (CONTACT §0 / §2.3).
//!
//! Lifetime per tick (CONTACT §2.2 FeaturePoly Studio, §2.4 slot 9 Studio):
//!  1 every connector inactive (n = 0);
//!  2 every manifold point with sep < −margin (margin 0.0: STEP §1.2a, Studio)
//!    takes the inactive connector with its feature pair;
//!  3 an unmatched point: with 8 connectors already, the first inactive one or
//!    connector 7, kept impulses NOT reset; else a new connector (ctor Studio:
//!    kept impulses zero, s = 1.0);
//!  4 slot 9: a connector still inactive is removed from the solver and deleted.
//! So a connector lives while its feature pair penetrates, and its kept impulses
//! go with it.

use glam::Vec3;
use rapier3d::prelude as rp;
use rustc_hash::FxHashMap;

use super::body::{self, BodySet, NONE};
use super::narrow;
use glam::Quat;

/// Feature kind of parry's points (pairs narrow.rs leaves to parry).
pub const KIND_PARRY: u8 = 0xff;

/// One point of this tick's manifold, before matching.
#[derive(Debug, Clone, Copy, Default)]
struct TempPt {
    kind: u8,
    id_a: i32,
    id_b: i32,
    a1: bool,
    p: Vec3,
    n: Vec3,
    sep: f32,
}

/// RUSTUDIO_SOLVER_VISIT: slot (0, default: body slot order), born (1: oldest contact
/// first), newborn (2: newest first). No recording separates them yet (stack3 / stepD give
/// the same numbers); the sort costs ~0.2 M instructions per world step on og.rbxl.
fn visit_mode() -> u8 {
    static F: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *F.get_or_init(|| match std::env::var("RUSTUDIO_SOLVER_VISIT").as_deref() {
        Ok("born") => 1,
        Ok("newborn") => 2,
        _ => 0,
    })
}

/// RUSTUDIO_SOLVER_FACE_ORDER: quadrant (1, default: the fitted quadrant tables),
/// bowtie (2: Reduce's start, then [0, 2, 3, 1]), literal (0: slot 9's order).
fn face_order() -> u8 {
    static F: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *F.get_or_init(|| match std::env::var("RUSTUDIO_SOLVER_FACE_ORDER").as_deref() {
        Ok("literal") => 0,
        Ok("bowtie") => 2,
        Ok("perm") => 3,
        _ => 1,
    })
}

/// A new face manifold between two dynamic blocks whose four depths spread more than
/// this takes its keys from Reduce's output (`reduce_keys`); below it, the fitted side
/// rule (`creation_order`). Fitted (lane STACK): the stepD re-landings (spread ~1.2e-4)
/// need the first, the pair labs' first landings (~9e-6) the second; any τ in
/// [2e-5, 1e-4] gives the same tables. RUSTUDIO_SOLVER_REDUCE_ORDER=off: never.
const REDUCE_SPREAD: f32 = 5e-5;

fn reduce_keys(both_dynamic: bool, pts: &[TempPt]) -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !both_dynamic || *OFF.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_REDUCE_ORDER").is_ok_and(|v| v == "off")) {
        return false;
    }
    let (lo, hi) = pts.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |a, p| (a.0.min(p.sep), a.1.max(p.sep)));
    hi - lo > REDUCE_SPREAD
}

/// RUSTUDIO_SOLVER_DELETE=compact: delete inactive connectors keeping the list order
/// (the port before lane PASS-2; diagnostic). Default: slot 9's swap-with-last.
fn delete_compact() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_DELETE").is_ok_and(|v| v == "compact"))
}

/// RUSTUDIO_SOLVER_PASS_SORT=on: register contacts in the serial pass's (id A, id B) order
/// (PASS §A.3 step 8) instead of visit order. **Off by default** (LOOSE-NOTES round SLEEP-2):
/// the id it sorts by (prim) is not identified and the port's part serial stands in;
/// with it on, r15_wall first differs at 33 (matches every tick with it off) and stack10's
/// velocity error grows 2.37 → 18.8. Recordings decide over the reading.
fn pass_sort() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_PASS_SORT").is_ok_and(|v| v == "on"))
}

fn fitted_face_order() -> bool {
    face_order() != 0
}

/// RUSTUDIO_SOLVER_CONTACT_TRACE=1: every placed point on stderr.
fn trace() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var_os("RUSTUDIO_SOLVER_CONTACT_TRACE").is_some())
}

/// RUSTUDIO_SOLVER_KEY_FORWARD=1: register a manifold's new connectors first to last
/// (diagnostic; slot 9 Studio walks them last to first).
fn key_forward() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_KEY_FORWARD").is_ok_and(|v| v == "1"))
}

/// Connectors per pair: FeaturePoly keeps up to 8 (CONTACT §2.1, §2.2 step 9).
pub const MAX_POINTS: usize = 8;

/// The kept-impulse block of one row, 0x40 bytes at conn + 0x40·k
/// (CONTACT §1.2; init Studio, reset Studio).
#[derive(Debug, Clone, Copy)]
pub struct Kept {
    /// λ_v, λ_p: the solve's impulse / h_b (force units, f64).
    pub lv: f64,
    pub lp: f64,
    /// w_v age weight, previous λ_v, Δλ_v, ΔΔλ_v.
    pub w_v: f32,
    pub prev_v: f32,
    pub d_v: f32,
    pub dd_v: f32,
    /// .. the same for the position pass.
    pub w_p: f32,
    pub prev_p: f32,
    pub d_p: f32,
    pub dd_p: f32,
    /// s_v, s_p: the adaptive per-row SOR factors (ctor 1.0);
    /// m_v, m_p generic multipliers (1.0, unused by contacts).
    pub s_v: f32,
    pub m_v: f32,
    pub s_p: f32,
    pub m_p: f32,
}

impl Default for Kept {
    fn default() -> Self {
        // Studio: zero, then / = 1.0 (its last six stores), / = 1.0.
        Kept { lv: 0.0, lp: 0.0, w_v: 0.0, prev_v: 0.0, d_v: 0.0, dd_v: 0.0, w_p: 0.0, prev_p: 0.0, d_p: 0.0, dd_p: 0.0, s_v: 1.0, m_v: 1.0, s_p: 1.0, m_p: 1.0 }
    }
}

/// One contact connector (CONTACT §1.1): its feature pair, solver key and the
/// three rows' kept impulses (row 0 normal, rows 1, 2 friction).
#[derive(Debug, Clone, Copy)]
pub struct Conn {
    /// Feature kind (conn: narrow.rs KIND_*; KIND_PARRY for parry's points) and
    /// ids on A / B ( /; parry: fid1 / fid2).
    pub kind: u8,
    pub fid1: i32,
    pub fid2: i32,
    /// The connector's A is the pair's collider 1 (else the bodies are swapped).
    pub a1: bool,
    /// conn: the key the solver sorts connectors by (DUAL §1 step 5). Taken
    /// from a counter at creation, so the order is creation order.
    pub id: u64,
    /// n ≠ 0 this tick.
    pub active: bool,
    /// conn μ, conn e (CONTACT §3), refreshed at creation / reuse.
    pub mu: f32,
    pub e: f32,
    pub kept: [Kept; 3],
}

impl Default for Conn {
    fn default() -> Self {
        Conn { kind: 0, fid1: 0, fid2: 0, a1: true, id: 0, active: false, mu: 0.0, e: 0.0, kept: [Kept::default(); 3] }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ManifoldState {
    pub key: ManifoldKey,
    /// Tick the manifold was last seen.
    pub stamp: u64,
    /// The rapier bodies (None = fixed collider): a pair whose bodies both
    /// sleep is not visited, and keeps its state.
    pub body1: Option<rp::RigidBodyHandle>,
    pub body2: Option<rp::RigidBodyHandle>,
    /// Connectors in the contact's list (contact count).
    pub n: u8,
    pub conns: [Conn; MAX_POINTS],
    /// Studio's per-contact narrow-phase state (narrow.rs).
    pub geom: narrow::PairState,
    /// Creation counter of the contact (visit order).
    pub born: u64,
    /// narrow.rs made this tick's points and rapier computed no manifolds for the pair
    /// (the `external_contacts` hook): its Touched events and contact reads are ours.
    pub external: bool,
    /// Some connector is active (the pair touches: Touched / TouchEnded).
    pub touching: bool,
    /// The pair's colliders (events).
    pub col1: Option<rp::ColliderHandle>,
    pub col2: Option<rp::ColliderHandle>,
    /// This tick's manifold index in `list` (valid when `list_tick` is the store's tick).
    pub list_m: u32,
    pub list_tick: u64,
    cached_parry: Option<ParryStamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ManifoldKey {
    pub c1: u32,
    pub c2: u32,
    pub sub1: u32,
    pub sub2: u32,
}

/// This tick's active connectors, struct-of-arrays: per manifold (bodies,
/// persistent slot) and per connector (point, normal, separation, material, key).
/// Manifold m owns connectors `first[m] .. first[m] + count[m]`.
#[derive(Default)]
pub struct ContactList {
    // -- per manifold --
    /// Body slots (body.rs), NONE for a fixed collider.
    pub a: Vec<u32>,
    pub b: Vec<u32>,
    /// The non-sim side's velocity source when it has no body slot: the
    /// anchored collider (for a conveyor's velocity, STEP §7a), else None.
    pub fixed_a: Vec<Option<rp::ColliderHandle>>,
    pub fixed_b: Vec<Option<rp::ColliderHandle>>,
    /// The manifold's persistent state (ContactStore::slots).
    pub slot: Vec<u32>,
    pub first: Vec<u32>,
    pub count: Vec<u8>,
    /// A connector was created or placed this tick (CONTACT §2.4: added to the
    /// solver; the deferred contact wake of SLEEP §7.2).
    pub added: Vec<bool>,
    // -- per connector --
    /// conn p: the point on A (world), conn n (world, from A into B),
    /// conn separation (negative = penetration) (CONTACT §0).
    pub p: Vec<Vec3>,
    pub n: Vec<Vec3>,
    pub sep: Vec<f32>,
    pub mu: Vec<f32>,
    pub e: Vec<f32>,
    pub id: Vec<u64>,
    /// Index of the connector in its manifold's `conns`.
    pub k: Vec<u8>,
    /// The connector's A is the manifold's b (p on b, n from b into a).
    pub swap: Vec<bool>,
    /// The manifold a connector belongs to.
    pub manifold: Vec<u32>,
    pub pairs: u32,
    pub overflow: u32,
}

impl ContactList {
    fn clear(&mut self) {
        self.a.clear();
        self.b.clear();
        self.fixed_a.clear();
        self.fixed_b.clear();
        self.slot.clear();
        self.first.clear();
        self.count.clear();
        self.added.clear();
        self.p.clear();
        self.n.clear();
        self.sep.clear();
        self.mu.clear();
        self.e.clear();
        self.id.clear();
        self.k.clear();
        self.swap.clear();
        self.manifold.clear();
        self.pairs = 0;
        self.overflow = 0;
    }

    /// Active connectors this tick.
    pub fn len(&self) -> usize {
        self.p.len()
    }

    pub fn manifold_count(&self) -> usize {
        self.a.len()
    }
}

/// A contact whose slot 9 registers new connectors this tick: its serial-pass sort key
/// (prim A id, prim B id), its manifold in `list`, and the connectors in registration order.
struct Pending {
    key: (u64, u64),
    m: u32,
    slot: u32,
    n: u8,
    reg: [u8; MAX_POINTS],
}

/// Independent narrow-phase result. Connector IDs and touch events are still
/// committed in the original pair visit order.
struct Geometry {
    first_a: bool,
    geom: narrow::PairState,
    temps: narrow::Temps,
}

// Most convex pairs have one manifold. Project its local points on the
// same workers as native narrow-phase geometry, before the ordered commit.
struct ProjectedManifold {
    frame2: rp::Pose,
    points: [TempPt; MAX_POINTS],
    count: usize,
}

enum PreparedGeometry {
    Retained(u32),
    Native(Geometry),
    Parry(ProjectedManifold),
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ParryStamp {
    epoch: u64,
    handles: [rp::ColliderHandle; 2],
    poses: [rp::Pose; 2],
}

impl ParryStamp {
    fn new(pair: &rp::ContactPair, co1: &rp::Collider, co2: &rp::Collider) -> Self {
        Self {
            epoch: pair.contact_epoch(),
            handles: [pair.collider1, pair.collider2],
            poses: [*co1.position(), *co2.position()],
        }
    }
}

#[derive(Default)]
pub struct ContactStore {
    pub map: FxHashMap<ManifoldKey, u32>,
    pub slots: Vec<ManifoldState>,
    free: Vec<u32>,
    pub list: ContactList,
    previous: ContactList,
    reuse_rows: bool,
    /// Manifolds this tick took unchanged from last tick's rows (bench counter).
    retained: u64,
    tick: u64,
    /// Connector key counter (conn).
    next_id: u64,
    /// Scratch: this tick's penetrating points of one manifold.
    temp: Vec<TempPt>,
    geometry: Vec<PreparedGeometry>,
    /// Contact creation counter (`ManifoldState::born`).
    born: u64,
    /// This tick's contacts with connectors to register (slot 9 runs in `flush_keys`).
    pending: Vec<Pending>,
    /// Collider pairs (lower index first) a NoCollisionConstraint keeps apart
    /// (physics_joints.rs `apply_joints`): no contact at all. rapier's CollisionPipeline
    /// (the narrow phase this solver runs) passes an empty joint set to the narrow phase,
    /// so a rapier joint's contacts_enabled(false) filters nothing here.
    pub no_collide: rustc_hash::FxHashSet<(u32, u32)>,
    /// This tick's touch transitions of the pairs narrow.rs makes alone (collider 1, 2,
    /// started), handed to rapier's event handler by step.rs.
    pub events: Vec<(rp::ColliderHandle, rp::ColliderHandle, bool)>,
    /// `class_of`: collider index → (generation, narrow.rs class).
    classes: Vec<(u32, u8)>,
}

fn cidx(c: rp::ColliderHandle) -> u32 {
    c.into_raw_parts().0
}

fn primitive_id(prim_id: &FxHashMap<rp::ColliderHandle, u64>, c: rp::ColliderHandle) -> u64 {
    prim_id.get(&c).copied().unwrap_or((1 << 40) + cidx(c) as u64)
}

/// A collider's friction and elasticity weights, kept in its user data by
/// physics.rs (physics_humanoid.rs `weights_data`; 0 = the default 1, 1).
fn weights_of(c: &rp::Collider) -> (f32, f32) {
    let d = c.user_data;
    if d == 0 { (1.0, 1.0) } else { (f32::from_bits((d >> 32) as u32), f32::from_bits(d as u32)) }
}

/// μ and e of a pair (CONTACT §3, mix Studio): each the weighted mean,
/// friction with FrictionWeight, elasticity with ElasticityWeight, 0 when the
/// weights sum to ≤ 1e-6 (.rdata Studio). f32.
pub fn mix(c1: &rp::Collider, c2: &rp::Collider) -> (f32, f32) {
    let ((fw1, ew1), (fw2, ew2)) = (weights_of(c1), weights_of(c2));
    let wf = fw1 + fw2;
    let mu = if wf > 1.0e-6 { (fw1 * c1.friction() + fw2 * c2.friction()) / wf } else { 0.0 };
    let we = ew1 + ew2;
    let e = if we > 1.0e-6 { (ew1 * c1.restitution() + ew2 * c2.restitution()) / we } else { 0.0 };
    (mu, e)
}

impl ContactStore {
    /// Reads every pair touching an awake body into `list`, running the connector
    /// lifetime above; sweeps the pairs that went away.
    pub fn update(&mut self, bodies: &BodySet, rb: &rp::RigidBodySet, colliders: &rp::ColliderSet, np: &rp::NarrowPhase, prim_id: &FxHashMap<rp::ColliderHandle, u64>) {
        self.tick += 1;
        let tick = self.tick;
        // Each task returns a full manifold. Keep small working sets inline
        // so their geometry costs less than dispatching and joining the jobs.
        let parallel = self.list.pairs >= 512 && rapier3d::rayon::current_num_threads() > 1;
        let visit_mode = visit_mode();
        let mut visit = Vec::new();
        self.reuse_rows = true;
        if self.reuse_rows {
            std::mem::swap(&mut self.list, &mut self.previous);
        }
        self.list.clear();
        let visit_scope = rustudio_core::bench::scope("cg_visit");
        for s in 0..bodies.len() {
            if bodies.flags[s] & body::flag::AWAKE == 0 || bodies.flags[s] & body::flag::DYNAMIC == 0 {
                continue;
            }
            let Some(b) = rb.get(bodies.handle[s]) else { continue };
            for &ch in b.colliders() {
                // (A sensor's pairs are intersection pairs: it has no contact pair.)
                if colliders.get(ch).is_none_or(|c| c.is_sensor()) {
                    continue;
                }
                for pair in np.contact_pairs_with(ch) {
                    // Each pair once: from its lower awake dynamic slot.
                    let other = if pair.collider1 == ch { pair.collider2 } else { pair.collider1 };
                    let os = colliders.get(other).and_then(|c| c.parent()).map(|h| bodies.slot(h)).unwrap_or(NONE);
                    if os != NONE && (os as usize) < s && bodies.flags[os as usize] & body::flag::AWAKE != 0 && bodies.flags[os as usize] & body::flag::DYNAMIC != 0 {
                        continue;
                    }
                    if !parallel && visit_mode == 0 {
                        let retained = self.retained_inline(pair, colliders);
                        self.read_pair(pair, bodies, colliders, tick, prim_id, retained);
                    } else {
                        let key = ManifoldKey { c1: cidx(pair.collider1), c2: cidx(pair.collider2), sub1: 0, sub2: 0 };
                        let born = if visit_mode == 0 {
                            0
                        } else {
                            self.map.get(&key).map_or(u64::MAX, |&i| self.slots[i as usize].born)
                        };
                        visit.push((born, pair));
                    }
                }
            }
        }
        drop(visit_scope);
        let commit_scope = rustudio_core::bench::scope("cg_commit");
        // The order stepContacts visits the contacts (C, ORDER §1: it orders the
        // keys of connectors registered in the same tick) is open (ORDER Open 3):
        // RUSTUDIO_SOLVER_VISIT=born takes the contacts oldest first.
        match visit_mode {
            1 => visit.sort_by_key(|x| x.0),
            2 => visit.sort_by_key(|x| std::cmp::Reverse(x.0)),
            _ => {}
        }
        if parallel {
            use rapier3d::rayon::prelude::*;
            let scope = rustudio_core::bench::scope("sv_contact_geometry");
            let mut geometry = std::mem::take(&mut self.geometry);
            visit.par_iter().with_min_len(24).map(|&(_, pair)| {
                self.prepare_geometry(pair, bodies, colliders, prim_id)
            }).collect_into_vec(&mut geometry);
            drop(scope);
            for ((_, pair), geom) in visit.into_iter().zip(geometry.drain(..)) {
                self.read_pair(pair, bodies, colliders, tick, prim_id, Some(geom));
            }
            self.geometry = geometry;
        } else {
            for (_, pair) in visit {
                let retained = self.retained_inline(pair, colliders);
                self.read_pair(pair, bodies, colliders, tick, prim_id, retained);
            }
        }
        drop(commit_scope);
        let _sweep_scope = rustudio_core::bench::scope("cg_sweep");
        // Contact slot 10 Studio: a pair the broad phase no longer pairs loses
        // all its connectors. A manifold not seen this tick is gone, unless neither
        // body was visited (both asleep / fixed): then it keeps its state.
        for i in 0..self.slots.len() {
            let m = &self.slots[i];
            if m.stamp == 0 || m.stamp == tick {
                continue;
            }
            let visited = |h: Option<rp::RigidBodyHandle>| {
                h.map(|h| bodies.slot(h)).is_some_and(|s| s != NONE && bodies.flags[s as usize] & body::flag::AWAKE != 0)
            };
            if visited(m.body1) || visited(m.body2) || (m.body1.is_none() && m.body2.is_none()) || stale(m, bodies) {
                if m.touching && m.external {
                    if let (Some(c1), Some(c2)) = (m.col1, m.col2) {
                        self.events.push((c1, c2, false));
                    }
                }
                let key = m.key;
                self.map.remove(&key);
                self.slots[i].stamp = 0;
                self.free.push(i as u32);
            }
        }
        self.flush_keys();
        if self.retained > 0 {
            rustudio_core::bench::add("sv_contact_retained", 0.0, std::mem::take(&mut self.retained));
        }
    }

    fn prepare_geometry(&self, pair: &rp::ContactPair, bodies: &BodySet, colliders: &rp::ColliderSet, prim_id: &FxHashMap<rp::ColliderHandle, u64>) -> PreparedGeometry {
        let manifolds = pair.manifolds();
        if manifolds.is_empty() {
            return self.geometry_for(pair.collider1, pair.collider2, bodies, colliders, prim_id)
                .map_or(PreparedGeometry::Deferred, PreparedGeometry::Native);
        }
        let [m] = manifolds else { return PreparedGeometry::Deferred };
        if m.points.is_empty() || m.points.len() > MAX_POINTS
            || !m.data.solver_flags.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES)
        {
            return PreparedGeometry::Deferred;
        }
        let (Some(co1), Some(co2)) = (colliders.get(pair.collider1), colliders.get(pair.collider2)) else {
            return PreparedGeometry::Deferred;
        };
        if let Some(slot) = self.retained_slot(pair, m, co1, co2) {
            return PreparedGeometry::Retained(slot);
        }
        let (wp1, wp2) = manifold_frames(m, co1, co2);
        let ball = co1.shape().as_ball().is_some() || co2.shape().as_ball().is_some();
        let mut points = [TempPt::default(); MAX_POINTS];
        let count = project_manifold(m, wp1, wp2, ball, &mut points);
        PreparedGeometry::Parry(ProjectedManifold { frame2: wp2, points, count })
    }

    /// The slot of a single-manifold Parry pair whose manifold the narrow phase kept
    /// (same epoch, same poses) since last tick's rows were made from it.
    fn retained_slot(&self, pair: &rp::ContactPair, m: &rp::ContactManifold, co1: &rp::Collider, co2: &rp::Collider) -> Option<u32> {
        if !self.reuse_rows || pair.contact_epoch() == 0 {
            return None;
        }
        let key = ManifoldKey { c1: cidx(pair.collider1), c2: cidx(pair.collider2), sub1: m.subshape1, sub2: m.subshape2 };
        let slot = *self.map.get(&key)?;
        let state = &self.slots[slot as usize];
        (state.stamp + 1 == self.tick && state.list_tick + 1 == self.tick
            && state.cached_parry == Some(ParryStamp::new(pair, co1, co2)))
            .then_some(slot)
    }

    /// `retained_slot` for the inline path, with `prepare_geometry`'s conditions.
    fn retained_inline(&self, pair: &rp::ContactPair, colliders: &rp::ColliderSet) -> Option<PreparedGeometry> {
        let [m] = pair.manifolds() else { return None };
        if m.points.is_empty() || m.points.len() > MAX_POINTS
            || !m.data.solver_flags.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES)
        {
            return None;
        }
        let (co1, co2) = (colliders.get(pair.collider1)?, colliders.get(pair.collider2)?);
        self.retained_slot(pair, m, co1, co2).map(PreparedGeometry::Retained)
    }

    fn geometry_for(&self, c1: rp::ColliderHandle, c2: rp::ColliderHandle, bodies: &BodySet, colliders: &rp::ColliderSet, prim_id: &FxHashMap<rp::ColliderHandle, u64>) -> Option<Geometry> {
        let (co1, co2) = (colliders.get(c1)?, colliders.get(c2)?);
        let (i1, i2) = (cidx(c1), cidx(c2));
        if co1.is_sensor() || co2.is_sensor() || self.no_collide.contains(&(i1.min(i2), i1.max(i2))) {
            return None;
        }
        let class = |h: rp::ColliderHandle, co: &rp::Collider| {
            let (i, g) = h.into_raw_parts();
            match self.classes.get(i as usize) {
                Some(&(cg, k)) if cg == g => k,
                _ => narrow::handled_class(co.shape()),
            }
        };
        if class(c1, co1) == 0 || class(c2, co2) == 0 {
            return None;
        }
        let (g1, g2) = (narrow::shape_of(co1, bodies)?, narrow::shape_of(co2, bodies)?);
        if !narrow::handles(&g1, &g2) {
            return None;
        }
        let id = |c: rp::ColliderHandle| primitive_id(prim_id, c);
        let first_a = narrow::first_is_a(&g1, &g2, id(c1), id(c2));
        let key = ManifoldKey { c1: i1, c2: i2, sub1: 0, sub2: 0 };
        let mut geom = self.map.get(&key).map_or_else(Default::default, |&i| self.slots[i as usize].geom);
        let mut temps = narrow::Temps::default();
        let (ga, gb) = if first_a { (&g1, &g2) } else { (&g2, &g1) };
        narrow::collide(ga, gb, &mut geom, 0.0, &mut temps);
        Some(Geometry { first_a, geom, temps })
    }

    fn read_pair(&mut self, pair: &rp::ContactPair, bodies: &BodySet, colliders: &rp::ColliderSet, tick: u64, prim_id: &FxHashMap<rp::ColliderHandle, u64>, geometry: Option<PreparedGeometry>) {
        let (Some(co1), Some(co2)) = (colliders.get(pair.collider1), colliders.get(pair.collider2)) else { return };
        if co1.is_sensor() || co2.is_sensor() {
            return;
        }
        let (i1, i2) = (cidx(pair.collider1), cidx(pair.collider2));
        if !self.no_collide.is_empty() && self.no_collide.contains(&(i1.min(i2), i1.max(i2))) {
            return;
        }
        let manifolds = pair.manifolds();
        // rapier's filters (collision groups, hooks, NoCollisionConstraint joints) act on
        // its manifolds; a pair it lets through has a manifold with rigid impulses on.
        // A pair the `external_contacts` hook took from parry keeps the filters' flags.
        let external = pair.external_solver_flags();
        let allow = match external {
            Some(f) => f.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES),
            None => manifolds.iter().any(|m| m.data.solver_flags.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES)),
        };
        let (h1, h2) = (co1.parent(), co2.parent());
        let (s1, s2) = (h1.map(|h| bodies.slot(h)).unwrap_or(NONE), h2.map(|h| bodies.slot(h)).unwrap_or(NONE));
        let (native, projected) = match geometry {
            Some(PreparedGeometry::Retained(slot)) if self.slots[slot as usize].stamp != tick => {
                self.retain_parry_rows(slot, s1, s2, h1, h2, pair);
                return;
            }
            Some(PreparedGeometry::Retained(_)) => return,
            Some(PreparedGeometry::Native(g)) => (Some(g), None),
            Some(PreparedGeometry::Parry(g)) => (None, Some(g)),
            Some(PreparedGeometry::Deferred) => (None, None),
            None => {
                // Cache the shape class even for pairs that fall back to Parry.
                let native = if self.class_of(pair.collider1, co1) == 0 || self.class_of(pair.collider2, co2) == 0 {
                    None
                } else {
                    self.geometry_for(pair.collider1, pair.collider2, bodies, colliders, prim_id)
                };
                (native, None)
            }
        };
        if let Some(Geometry { first_a, geom, temps }) = native {
            let key = ManifoldKey { c1: i1, c2: i2, sub1: 0, sub2: 0 };
            if self.map.get(&key).is_some_and(|&i| self.slots[i as usize].stamp == tick) {
                return; // read this tick already (`read_woken`)
            }
            let slot = self.slot_for(key, h1, h2, tick);
            self.slots[slot as usize].geom = geom;
            self.temp.clear();
            if allow {
                for t in temps.t.iter().filter(|t| t.n != Vec3::ZERO) {
                    // Row point p (ctor arg 1) → the p that rows.rs moves by n·sep/2.
                    let p = if t.row_at_p { t.p - t.n * (t.sep * 0.5) } else { t.p };
                    self.temp.push(TempPt { kind: t.kind, id_a: t.id_a, id_b: t.id_b, a1: t.a_first == first_a, p, n: t.n, sep: t.sep });
                }
            }
            self.list.pairs += 1;
            let dynamic = |s: u32| s != NONE && bodies.flags[s as usize] & body::flag::DYNAMIC != 0;
            let both = dynamic(s1) && dynamic(s2);
            let was = self.slots[slot as usize].touching && self.slots[slot as usize].external;
            let pkey = || {
                let (ia, ib) = (primitive_id(prim_id, pair.collider1), primitive_id(prim_id, pair.collider2));
                if first_a { (ia, ib) } else { (ib, ia) }
            };
            self.place(slot, s1, s2, h1, h2, pair, co1, co2, false, both, body::q(co2.position().rotation), lower_centre(co1, co2), pkey);
            let st = &mut self.slots[slot as usize];
            st.external = external.is_some();
            st.col1 = Some(pair.collider1);
            st.col2 = Some(pair.collider2);
            let now = st.touching && st.external;
            if now != was {
                self.events.push((pair.collider1, pair.collider2, now));
            }
            return;
        }
        if manifolds.is_empty() {
            return;
        }
        self.list.pairs += 1;
        for m in manifolds {
            if !m.data.solver_flags.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES) {
                continue;
            }
            // No points and no state from earlier ticks: nothing to place (the check
            // below, without the poses).
            if m.points.is_empty() && !self.map.contains_key(&ManifoldKey { c1: cidx(pair.collider1), c2: cidx(pair.collider2), sub1: m.subshape1, sub2: m.subshape2 }) {
                continue;
            }
            self.temp.clear();
            let wp2 = if let Some(projected) = &projected {
                self.temp.extend_from_slice(&projected.points[..projected.count]);
                projected.frame2
            } else {
                let (wp1, wp2) = manifold_frames(m, co1, co2);
                let ball = co1.shape().as_ball().is_some() || co2.shape().as_ball().is_some();
                self.temp.resize(m.points.len(), TempPt::default());
                let count = project_manifold(m, wp1, wp2, ball, &mut self.temp);
                self.temp.truncate(count);
                wp2
            };
            let key = ManifoldKey { c1: cidx(pair.collider1), c2: cidx(pair.collider2), sub1: m.subshape1, sub2: m.subshape2 };
            if !self.map.contains_key(&key) && self.temp.is_empty() {
                continue;
            }
            if self.map.get(&key).is_some_and(|&i| self.slots[i as usize].stamp == tick) {
                continue; // read this tick already (`read_woken`)
            }
            let slot = self.slot_for(key, h1, h2, tick);
            // Sort key: the pair's collider order (Bullet contacts' primitives not read).
            let pkey = || (primitive_id(prim_id, pair.collider1), primitive_id(prim_id, pair.collider2));
            self.place(slot, s1, s2, h1, h2, pair, co1, co2, true, false, body::q(wp2.rotation), (body::v(wp2.translation), body::q(wp2.rotation)), pkey);
            self.slots[slot as usize].cached_parry = (self.reuse_rows && manifolds.len() == 1)
                .then(|| ParryStamp::new(pair, co1, co2));
        }
    }

    fn retain_parry_rows(&mut self, slot: u32, s1: u32, s2: u32, h1: Option<rp::RigidBodyHandle>, h2: Option<rp::RigidBodyHandle>, pair: &rp::ContactPair) {
        self.retained += 1;
        let state = &mut self.slots[slot as usize];
        let prev = state.list_m as usize;
        let count = self.previous.count[prev];
        let start = self.previous.first[prev] as usize;
        let range = start..start + count as usize;
        let first = self.list.len() as u32;
        let index = self.list.manifold_count() as u32;

        self.list.p.extend_from_slice(&self.previous.p[range.clone()]);
        self.list.n.extend_from_slice(&self.previous.n[range.clone()]);
        self.list.sep.extend_from_slice(&self.previous.sep[range.clone()]);
        self.list.mu.extend_from_slice(&self.previous.mu[range.clone()]);
        self.list.e.extend_from_slice(&self.previous.e[range.clone()]);
        self.list.id.extend_from_slice(&self.previous.id[range.clone()]);
        self.list.k.extend_from_slice(&self.previous.k[range.clone()]);
        self.list.swap.extend_from_slice(&self.previous.swap[range]);
        self.list.manifold.resize(self.list.len(), index);
        self.list.a.push(s1);
        self.list.b.push(s2);
        self.list.fixed_a.push(h1.is_none().then_some(pair.collider1));
        self.list.fixed_b.push(h2.is_none().then_some(pair.collider2));
        self.list.slot.push(slot);
        self.list.first.push(first);
        self.list.count.push(count);
        self.list.added.push(false);
        self.list.pairs += 1;
        state.list_m = index;
        state.list_tick = self.tick;
        state.stamp = self.tick;
        state.body1 = h1;
        state.body2 = h2;
    }

    /// The pairs of bodies the sleep stage woke after this tick's narrow phase (SLEEP §7.2's
    /// deferred contact wake): an asleep edge keeps its connectors in Studio (SLEEP §8), so the
    /// woken body is solved with them this tick; their geometry is the asleep pose's, read now.
    pub fn read_woken(&mut self, woken: &[u32], bodies: &BodySet, rb: &rp::RigidBodySet, colliders: &rp::ColliderSet, np: &rp::NarrowPhase, prim_id: &FxHashMap<rp::ColliderHandle, u64>) {
        let tick = self.tick;
        for &s in woken {
            let s = s as usize;
            if bodies.flags[s] & body::flag::AWAKE == 0 {
                continue;
            }
            let Some(b) = rb.get(bodies.handle[s]) else { continue };
            for &ch in b.colliders() {
                for pair in np.contact_pairs_with(ch) {
                    self.read_pair(pair, bodies, colliders, tick, prim_id, None);
                }
            }
        }
        self.flush_keys();
    }

    /// narrow.rs's class of a collider's shape (`narrow::handled_class`, 0 = not
    /// narrow.rs's), kept per collider: physics.rs never changes a collider's shape in
    /// place (a new shape is a new collider), and a reused index has a new generation.
    fn class_of(&mut self, c: rp::ColliderHandle, co: &rp::Collider) -> u8 {
        let (i, g) = c.into_raw_parts();
        let i = i as usize;
        if self.classes.len() <= i {
            self.classes.resize(i + 1, (u32::MAX, 0));
        }
        if self.classes[i].0 != g {
            self.classes[i] = (g, narrow::handled_class(co.shape()));
        }
        self.classes[i].1
    }

    /// The persistent state of a manifold key (created on first use).
    fn slot_for(&mut self, key: ManifoldKey, h1: Option<rp::RigidBodyHandle>, h2: Option<rp::RigidBodyHandle>, tick: u64) -> u32 {
        let slot = match self.map.get(&key) {
            Some(&i) => i,
            None => {
                self.born += 1;
                let st = ManifoldState { key, stamp: tick, body1: h1, body2: h2, n: 0, conns: [Conn::default(); MAX_POINTS], geom: Default::default(), born: self.born, external: false, touching: false, col1: None, col2: None, list_m: NONE, list_tick: 0, cached_parry: None };
                let i = if let Some(i) = self.free.pop() {
                    self.slots[i as usize] = st;
                    i
                } else {
                    self.slots.push(st);
                    (self.slots.len() - 1) as u32
                };
                self.map.insert(key, i);
                i
            }
        };
        let st = &mut self.slots[slot as usize];
        st.stamp = tick;
        st.body1 = h1;
        st.body2 = h2;
        slot
    }

    /// The connector lifetime of one contact for this tick's points `self.temp`
    /// (CONTACT §2.2 Studio steps 1, 5–10; §2.4 slot 9 Studio).
    #[allow(clippy::too_many_arguments)]
    fn place(&mut self, slot: u32, s1: u32, s2: u32, h1: Option<rp::RigidBodyHandle>, h2: Option<rp::RigidBodyHandle>, pair: &rp::ContactPair, co1: &rp::Collider, co2: &rp::Collider, parry: bool, both_dynamic: bool, frame2: Quat, lower: (Vec3, Quat), pkey: impl FnOnce() -> (u64, u64)) {
        let st = &mut self.slots[slot as usize];
        let cn = st.n as usize;
        let nt = self.temp.len().min(32);
        if nt == 0 && cn == 0 {
            st.touching = false;
            return;
        }
        if nt == cn && st.conns[..cn].iter().zip(&self.temp).all(|(c, p)| {
            c.id != 0 && c.active && c.kind == p.kind && c.a1 == p.a1
                && c.fid1 == p.id_a && c.fid2 == p.id_b
        }) {
            // Slot order already agrees with the incoming geometry.
            let order = [0, 1, 2, 3, 4, 5, 6, 7];
            self.append_manifold(slot, s1, s2, h1, h2, pair, &order[..nt], false, &[], pkey);
            return;
        }
        // 1: every connector inactive.
        for c in &mut st.conns[..cn] {
            c.active = false;
        }
        // 5–7: match Studio: an inactive connector with the same (kind, A, idA, idB);
        // edge–edge (kind 1) also from the other side, then the connector is swapped.
        let mut slot_of = [u8::MAX; 32];
        for t in 0..nt {
            let tp = self.temp[t];
            let n = st.n as usize;
            let hit = st.conns[..n].iter().position(|c| !c.active && c.kind == tp.kind && ((c.a1 == tp.a1 && c.fid1 == tp.id_a && c.fid2 == tp.id_b) || (tp.kind == 1 && c.a1 != tp.a1 && c.fid1 == tp.id_b && c.fid2 == tp.id_a)));
            if let Some(c) = hit {
                let cc = &mut st.conns[c];
                if cc.a1 != tp.a1 {
                    cc.a1 = tp.a1;
                    std::mem::swap(&mut cc.fid1, &mut cc.fid2);
                }
                cc.active = true;
                slot_of[t] = c as u8;
            }
        }
        // A steady manifold already has the registered connector keys and
        // warm-start impulses. Its contact rows need only the new world points.
        if nt == cn
            && slot_of[..nt].iter().all(|&c| c != u8::MAX)
            && st.conns[..cn].iter().all(|c| c.id != 0)
        {
            self.append_manifold(slot, s1, s2, h1, h2, pair, &slot_of[..nt], false, &[], pkey);
            return;
        }

        // 8–10: place the rest in temp order. Parry pairs keep CORE's
        // fitted creation order (no spec for Bullet's point order).
        let mut ord = [0u8; 32];
        if parry {
            let n0 = self.temp.first().map_or(Vec3::Y, |t| t.n);
            creation_order(&self.temp[..nt], n0, frame2, lower, false, &mut ord[..nt]);
        } else {
            for (i, o) in ord[..nt].iter_mut().enumerate() {
                *o = i as u8;
            }
        }
        let mut added = false;
        for &t in &ord[..nt] {
            let t = t as usize;
            if slot_of[t] != u8::MAX {
                continue;
            }
            let tp = self.temp[t];
            let n = st.n as usize;
            let c = if n == MAX_POINTS {
                // First inactive connector, or connector 7; kept impulses not reset.
                let c = st.conns.iter().position(|c| !c.active).unwrap_or(MAX_POINTS - 1);
                if st.conns[c].active {
                    // Connector 7 was this tick's already: the point is dropped.
                    self.list.overflow += 1;
                    continue;
                }
                c
            } else {
                // A new connector (Studio + ctor Studio): zero kept impulses,
                // key conn = 0 until it is registered (below).
                st.conns[n] = Conn::default();
                st.n += 1;
                n
            };
            let (mu, e) = mix(co1, co2);
            let cc = &mut st.conns[c];
            cc.kind = tp.kind;
            cc.a1 = tp.a1;
            cc.fid1 = tp.id_a;
            cc.fid2 = tp.id_b;
            cc.mu = mu;
            cc.e = e;
            cc.active = true;
            slot_of[t] = c as u8;
            added = true;
        }
        // Slot 9, connectors from last to first: an active one not yet in the solver is
        // registered, its key conn taken once from the global counter (ORDER §1,
        // Studio); an inactive one leaves the solver and is deleted.
        let n = st.n as usize;
        // Fitted (ORDER §4, Open 1): the new face points of a box manifold take their keys
        // in the recorded corner order, not slot 9's list order — Reduce's output is a
        // cyclic polygon order, the recordings select "start corner, its opposite, the
        // other two". `creation_order` ranks the new points by quadrant.
        // Registration order of this manifold's new connectors (keys assigned in `flush_keys`).
        let mut reg = [0u8; MAX_POINTS];
        let mut nreg = 0usize;
        let fresh: usize = (0..n).filter(|&r| st.conns[r].active && st.conns[r].id == 0).count();
        let face = !parry && fresh >= 2 && (0..nt).all(|t| slot_of[t] == u8::MAX || matches!(self.temp[t].kind, 0 | 2));
        if face && fitted_face_order() {
            let mut tl = [0u8; 32];
            let mut pts = [TempPt { kind: 0, id_a: 0, id_b: 0, a1: true, p: Vec3::ZERO, n: Vec3::ZERO, sep: 0.0 }; MAX_POINTS];
            let mut cs = [0usize; MAX_POINTS];
            let mut m = 0;
            for t in 0..nt {
                let c = slot_of[t] as usize;
                if slot_of[t] != u8::MAX && st.conns[c].id == 0 && !cs[..m].contains(&c) {
                    pts[m] = self.temp[t];
                    cs[m] = c;
                    m += 1;
                }
            }
            let n0 = pts[0].n;
            // RUSTUDIO_SOLVER_PERM_TICK=t limits the permutation to manifolds made at world tick t.
            let perm_tick = || std::env::var("RUSTUDIO_SOLVER_PERM_TICK").ok().and_then(|v| v.parse::<u64>().ok());
            if m == 4 && face_order() == 3 && perm_tick().is_none_or(|t| t == self.tick) {
                // Diagnostic: RUSTUDIO_SOLVER_PERM / _PERM_DYN = the temp indices in key order.
                let v = if both_dynamic { "RUSTUDIO_SOLVER_PERM_DYN" } else { "RUSTUDIO_SOLVER_PERM" };
                let p = std::env::var(v).unwrap_or_else(|_| "0123".into());
                for (i, c) in p.bytes().take(4).enumerate() {
                    tl[i] = c.saturating_sub(b'0').min(3);
                }
            } else if m == 4 && reduce_keys(both_dynamic, &pts[..m]) {
                // Keys (sweep order) [s3, s1, s2, s0] of Reduce's output [s0..s3] (Studio:
                // s0 = the deepest, then polygon order), i.e. registration [s0, s2, s1, s3]:
                // the start, its diagonal, then the other diagonal. Not what slot 9's
                // last-to-first walk of [s0..s3] gives (ORDER §1); measured (LOOSE-NOTES STACK).
                tl[..4].copy_from_slice(&[3, 1, 2, 0]);
            } else if m == 4 && face_order() == 2 {
                // Reduce's output is [i0, i0+1, i0+2, i0+3] (polygon order from the deepest):
                // keys in the recorded shape "start, opposite, after it, after the start".
                tl[..4].copy_from_slice(&[0, 2, 3, 1]);
            } else {
                creation_order(&pts[..m], n0, frame2, lower, both_dynamic, &mut tl[..m]);
            }
            for &i in &tl[..m] {
                reg[nreg] = cs[i as usize] as u8;
                nreg += 1;
            }
        }
        let order: &mut dyn Iterator<Item = usize> = if key_forward() { &mut (0..n) } else { &mut (0..n).rev() };
        for r in order {
            if st.conns[r].active && st.conns[r].id == 0 && !reg[..nreg].contains(&(r as u8)) {
                reg[nreg] = r as u8;
                nreg += 1;
            }
        }
        // Slot 9's deletion (Studio → slot 1 Studio): walking last → first, a
        // connector in the solver with n = 0 is deleted by conn[i] = conn[count − 1],
        // count − 1 (PASS §A.6, §E.1). The list order this leaves is the order later ticks'
        // temps re-match connectors in (Studio, first free in list order). A manifold
        // with no point left loses them all (slot 10 Studio, the same walk).
        let mut at = [0u8; MAX_POINTS];
        let mut remap = [u8::MAX; MAX_POINTS];
        for r in 0..n {
            at[r] = r as u8;
            remap[r] = r as u8;
        }
        let mut cnt = n;
        if delete_compact() {
            cnt = 0;
            for r in 0..n {
                if st.conns[r].active || st.conns[r].id == 0 {
                    st.conns[cnt] = st.conns[r];
                    remap[r] = cnt as u8;
                    cnt += 1;
                } else {
                    remap[r] = u8::MAX;
                }
            }
        } else {
            for i in (0..n).rev() {
                let c = st.conns[i];
                if c.active || c.id == 0 {
                    continue;
                }
                remap[at[i] as usize] = u8::MAX;
                let last = cnt - 1;
                if i != last {
                    st.conns[i] = st.conns[last];
                    at[i] = at[last];
                    remap[at[i] as usize] = i as u8;
                }
                cnt -= 1;
            }
        }
        st.n = cnt as u8;
        for r in &mut reg[..nreg] {
            *r = remap[*r as usize];
        }
        for c in &mut slot_of[..nt] {
            if *c != u8::MAX {
                *c = remap[*c as usize];
            }
        }
        self.append_manifold(slot, s1, s2, h1, h2, pair, &slot_of[..nt], added, &reg[..nreg], pkey);
    }

    #[allow(clippy::too_many_arguments)]
    fn append_manifold(&mut self, slot: u32, s1: u32, s2: u32, h1: Option<rp::RigidBodyHandle>, h2: Option<rp::RigidBodyHandle>, pair: &rp::ContactPair, points: &[u8], added: bool, reg: &[u8], pkey: impl FnOnce() -> (u64, u64)) {
        let first = self.list.len() as u32;
        let mi = self.list.manifold_count() as u32;
        let st = &mut self.slots[slot as usize];
        let mut count = 0u8;
        let trace = trace() && reg.is_empty();
        for (&k, tp) in points.iter().zip(&self.temp) {
            if k == u8::MAX {
                continue;
            }
            let c = &st.conns[k as usize];
            self.list.p.push(tp.p);
            self.list.n.push(tp.n);
            self.list.sep.push(tp.sep);
            self.list.mu.push(c.mu);
            self.list.e.push(c.e);
            self.list.id.push(c.id);
            self.list.k.push(k);
            self.list.swap.push(!c.a1);
            if trace {
                eprintln!("t{} contact {:?} k{} kind {} ids {} {} a1 {} key {} p {:?} n {:?} sep {}", self.tick, st.key, k, c.kind, c.fid1, c.fid2, c.a1, c.id, tp.p, tp.n, tp.sep);
            }
            self.list.manifold.push(mi);
            count += 1;
        }
        st.touching = count != 0;
        if count == 0 {
            return;
        }
        st.list_m = mi;
        st.list_tick = self.tick;
        self.list.a.push(s1);
        self.list.b.push(s2);
        self.list.fixed_a.push(if h1.is_none() { Some(pair.collider1) } else { None });
        self.list.fixed_b.push(if h2.is_none() { Some(pair.collider2) } else { None });
        self.list.slot.push(slot);
        self.list.first.push(first);
        self.list.count.push(count);
        self.list.added.push(added);
        if !reg.is_empty() {
            let mut order = [0; MAX_POINTS];
            order[..reg.len()].copy_from_slice(reg);
            self.pending.push(Pending { key: pkey(), m: mi, slot, n: reg.len() as u8, reg: order });
        }
    }

    /// The serial contact pass (Studio steps 8–9, PASS §A.3): the contacts updated
    /// this tick run slot 9 in the order of their records sorted by (prio(out), prim A
    ///, prim B); slot 9 registers a contact's new connectors last to first
    /// (Studio takes conn from the global counter). Every contact
    /// that registers has out 1 (prio 0), so the order is (id A, id B); ids are the port's
    /// part serials (`StudioSolver::note_collider`; prim itself is not identified).
    fn flush_keys(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut p = std::mem::take(&mut self.pending);
        if pass_sort() {
            p.sort_by_key(|e| e.key);
        }
        for e in &p {
            let st = &mut self.slots[e.slot as usize];
            for &r in &e.reg[..e.n as usize] {
                if r != u8::MAX && st.conns[r as usize].id == 0 {
                    self.next_id += 1;
                    st.conns[r as usize].id = self.next_id;
                }
            }
            let m = e.m as usize;
            let first = self.list.first[m] as usize;
            for j in first..first + self.list.count[m] as usize {
                let c = &st.conns[self.list.k[j] as usize];
                self.list.id[j] = c.id;
                if trace() {
                    eprintln!("t{} contact {:?} k{} kind {} ids {} {} a1 {} key {} p {:?} n {:?} sep {}", self.tick, st.key, self.list.k[j], c.kind, c.fid1, c.fid2, c.a1, c.id, self.list.p[j], self.list.n[j], self.list.sep[j]);
                }
            }
        }
        p.clear();
        self.pending = p;
    }

    /// The connector of entry `i` of this tick's list.
    #[inline]
    pub fn conn_mut(&mut self, i: usize) -> &mut Conn {
        let m = self.list.manifold[i] as usize;
        let k = self.list.k[i] as usize;
        &mut self.slots[self.list.slot[m] as usize].conns[k]
    }

    #[inline]
    pub fn conn(&self, i: usize) -> &Conn {
        let m = self.list.manifold[i] as usize;
        let k = self.list.k[i] as usize;
        &self.slots[self.list.slot[m] as usize].conns[k]
    }

    pub fn manifold_count(&self) -> usize {
        self.map.len()
    }

    /// The last tick's active points of the pair (c1, c2) when narrow.rs made them alone
    /// (`ManifoldState::external`): calls `f(p, n, sep)` with the point on the pair's
    /// collider 1, the normal out of collider 1 and the separation (negative =
    /// penetration). Returns false when the pair is not one of those (read parry's
    /// manifolds then).
    pub fn external_points(&self, c1: rp::ColliderHandle, c2: rp::ColliderHandle, mut f: impl FnMut(Vec3, Vec3, f32)) -> bool {
        let key = ManifoldKey { c1: cidx(c1), c2: cidx(c2), sub1: 0, sub2: 0 };
        let Some(&i) = self.map.get(&key) else { return false };
        let st = &self.slots[i as usize];
        if !st.external || st.col1 != Some(c1) {
            return false;
        }
        if !st.touching || st.list_tick != self.tick {
            return true;
        }
        let m = st.list_m as usize;
        let first = self.list.first[m] as usize;
        for j in first..first + self.list.count[m] as usize {
            // A connector's p is on its A, n from A into B; A is collider 1 unless swapped.
            let (p, n, sep) = (self.list.p[j], self.list.n[j], self.list.sep[j]);
            if self.list.swap[j] {
                f(p + n * sep, -n, sep);
            } else {
                f(p, n, sep);
            }
        }
        true
    }

    /// Pairs narrow.rs made alone that touch (Stats.ContactsCount's share).
    pub fn external_touching(&self) -> usize {
        self.slots.iter().filter(|m| m.stamp != 0 && m.external && m.touching).count()
    }
}

/// The order new connectors of one manifold update are created in (their keys).
/// Not in the specs (CORE-NOTES "Connector order"): fitted to Studio's recordings.
/// Points are ranked by quadrant about their centroid, in body B's (collider 2's)
/// frame, in the plane of the two axes most perpendicular to n: q = 2·(u > 0) +
/// (v > 0) with (u, v) = (y, z) for a face along x, (x, z) along y, (x, y) along
/// z; the rank tables (RUSTUDIO_SOLVER_ORDER_X / _Y / _Z, the quadrants in creation
/// order) are "1203" (collide_headon), "0312" (R1, drop_default) and "0312"
/// (no recording yet). Ties keep parry's order.
fn manifold_frames(m: &rp::ContactManifold, co1: &rp::Collider, co2: &rp::Collider) -> (rp::Pose, rp::Pose) {
    (
        m.subshape_pos1().map_or(*co1.position(), |p| *co1.position() * *p),
        m.subshape_pos2().map_or(*co2.position(), |p| *co2.position() * *p),
    )
}

fn project_manifold(m: &rp::ContactManifold, wp1: rp::Pose, wp2: rp::Pose, ball: bool, points: &mut [TempPt]) -> usize {
    // Reproject retained local anchors. A recycled manifold's stored distances
    // can predate the current poses (GEOM §4, CONTACT §0).
    let n = body::v(wp1.rotation * m.local_n1);
    let mut count = 0;
    for p in &m.points {
        let (p1, p2) = (body::v(wp1 * p.local_p1), body::v(wp2 * p.local_p2));
        let dist = (p2 - p1).dot(n);
        if (dist < 0.0 || (ball && dist <= 0.0))
            && !points[..count].iter().any(|t| (t.p - p1).length_squared() < 1e-8)
        {
            points[count] = TempPt { kind: KIND_PARRY, id_a: p.fid1.0 as i32, id_b: p.fid2.0 as i32, a1: true, p: p1, n, sep: dist };
            count += 1;
        }
    }
    count
}

fn creation_order(temp: &[TempPt], n: Vec3, frame: glam::Quat, lower: (Vec3, Quat), both_dynamic: bool, out: &mut [u8]) {
    static RANK: std::sync::OnceLock<[[u8; 4]; 4]> = std::sync::OnceLock::new();
    let rank = RANK.get_or_init(|| {
        let table = |var: &str, def: &str| {
            let s = std::env::var(var).unwrap_or_else(|_| def.into());
            let mut r = [0u8, 1, 2, 3];
            for (i, c) in s.bytes().take(4).enumerate() {
                if (b'0'..=b'3').contains(&c) {
                    r[(c - b'0') as usize] = i as u8;
                }
            }
            r
        };
        [table("RUSTUDIO_SOLVER_ORDER_X", "1203"), table("RUSTUDIO_SOLVER_ORDER_Y", "0312"), table("RUSTUDIO_SOLVER_ORDER_Z", "0312"), table("RUSTUDIO_SOLVER_ORDER_Y_DYN", "3021")]
    });
    for (i, o) in out.iter_mut().enumerate() {
        *o = i as u8;
    }
    if temp.len() < 2 {
        return;
    }
    // Two dynamic blocks: quadrants in the lower block's frame (which collider is "2" varies
    // between runs; a block turned 90° about y then flips the ranks).
    let inv = if both_dynamic { lower.1.inverse() } else { frame.inverse() };
    let c = temp.iter().fold(Vec3::ZERO, |a, t| a + t.p) / temp.len() as f32;
    let a = (inv * n).abs();
    let (axis, iu, iv) = if a.x >= a.y && a.x >= a.z { (0, 1, 2) } else if a.y >= a.z { (1, 0, 2) } else { (2, 0, 1) };
    // Two dynamic blocks on a y face: the start corner follows the side the contact patch
    // lies on relative to the lower block, in its frame (POSE labs, GEOM2 §A.5): x offset
    // < 0, or x = 0 and z > 0 → the static y table (start −−); else the dynamic one (start
    // ++). |offset| < 1e-4 counts as 0. Fitted on 14 pair labs, not read (GEOM2 Open 1).
    let off = lower.1.inverse() * (c - lower.0);
    let z = |x: f32| if x.abs() < 1e-4 { 0.0 } else { x };
    let tab_dyn = if z(off[iu]) < 0.0 || (z(off[iu]) == 0.0 && z(off[iv]) > 0.0) { 1 } else { 3 };
    let q = |p: Vec3| {
        let d = inv * (p - c);
        let tab = if both_dynamic && axis == 1 { tab_dyn } else { axis };
        rank[tab][(2 * (d[iu] > 0.0) as usize) + (d[iv] > 0.0) as usize]
    };
    out.sort_by_key(|&i| q(temp[i as usize].p));
}

/// The centre of the lower of the two colliders (world y): the block the other rests on.
fn lower_centre(co1: &rp::Collider, co2: &rp::Collider) -> (Vec3, Quat) {
    let (a, b) = (co1.position(), co2.position());
    let lo = if a.translation.y <= b.translation.y { a } else { b };
    (body::v(lo.translation), body::q(lo.rotation))
}

/// A body of the manifold no longer exists.
fn stale(m: &ManifoldState, bodies: &BodySet) -> bool {
    let gone = |h: Option<rp::RigidBodyHandle>| {
        h.is_some_and(|h| {
            let (i, g) = h.into_raw_parts();
            bodies.persist.get(i as usize).is_none_or(|p| !p.live || p.generation != g)
        })
    };
    gone(m.body1) || gone(m.body2)
}

// ---------------------------------------------------------------- write-back

/// The adaptation table F = Studio (CONTACT §7): T = [r_hi, r_lo, y_lo,
/// y_hi, rate_up, rate_down].
#[inline]
fn adapt_f(x: f32, r: f32, t: &[f32; 6]) -> f32 {
    let mut y = t[2];
    if r > t[1] {
        let tau = (r.clamp(t[1], t[0]) - t[1]) / (t[0] - t[1]);
        y = (1.0 - tau) * t[2] + tau * t[3];
    }
    let a = if y <= x { t[5] } else { t[4] };
    (1.0 - a) * x + a * y
}

/// cfg.., the contact normal row's table T8c (CONTACT §1.5).
pub const T8C: [f32; 6] = [0.05, 0.01, 1.9, 1.0, 1e-4, 0.2];

/// Slot 7 of a contact connector, Studio ×3 (CONTACT §7,
/// DUAL §5.4 step 5, DUALROWS §4): `lam[r]` = the row's (λ_v, λ_p) after the
/// solve, `s_in[r]` = the row's ω pair the solve used, `h_b` = the batch's L·h.
pub fn write_back_contact(c: &mut Conn, lam: &[[f64; 2]; 3], s_in: &[[f32; 2]; 3], h_b: f32) {
    // 1: impulse → force, (1.0 / (f64)h)·λ (.rdata Studio = 1.0).
    let inv = 1.0 / h_b as f64;
    for r in 0..3 {
        let b = &mut c.kept[r];
        let dv = inv * lam[r][0];
        let dp = inv * lam[r][1];
        if r != 0 {
            // 2: friction rows (adapt off): plain store, nothing else.
            b.lv = dv;
            b.lp = dp;
            continue;
        }
        // 3: the normal row (contactMode = 1, adapt = 1).
        let a = b.prev_p.abs().max((dp as f32).abs());
        let rp = ((dp as f32) - b.prev_p).abs() * b.w_p / (a + 1e-5);
        let a = b.prev_v.abs().max((dv as f32).abs());
        let rv = ((dv as f32) - b.prev_v).abs() * b.w_v / (a + 1e-5);
        b.s_v = adapt_f(s_in[r][0], rv, &T8C);
        b.s_p = adapt_f(s_in[r][1], rp, &T8C);
        b.lv = dv;
        b.lp = dp;
        // 4: history (v), 5: age weights (.rdata Studio = 0.1), 6: history (p).
        let d = (b.lv as f32) - b.prev_v;
        b.prev_v = b.lv as f32;
        let dd = d - b.d_v;
        b.d_v = d;
        b.dd_v = dd;
        b.w_v += (1.0 - b.w_v) * 0.1;
        b.w_p += (1.0 - b.w_p) * 0.1;
        let d = (b.lp as f32) - b.prev_p;
        b.prev_p = b.lp as f32;
        let dd = d - b.d_p;
        b.d_p = d;
        b.dd_p = dd;
    }
}
