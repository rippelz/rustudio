//! Joints and constraints of the legacy solver (`RUSTUDIO_SOLVER=studio`): the
//! Roblox constraint Instances as Studio's `Solver::Constraint` connectors (JOINT §1.1),
//! their slot-10 rows (JOINT D1–D3, builders in `dual_rows.rs`), the kept state per
//! row and its write-back (JOINT §1.5 / D1.3, Studio), the spring arbiter's level
//! request (JOINT §5) and the `Solver::Force` items (VectorForce / Torque / LineForce,
//! JOINT I.14). Which connectors each Instance inserts and which property lands in
//! which field: `instance.rs` (JOINT I).
//!
//! Rigid joints (Weld, Motor6D, …) are not here: their parts are one body (JOINT §2;
//! physics.rs assemblies). The connectors are allocated once per Instance and live as
//! long as it does, so their kept impulses survive every remove / re-add (JOINT I.0).
//!
//! What the batch (dual.rs, lane CORE) gets per active connector, in `JointList`'s flat
//! per-tick arrays: its two body slots (`a`, `b`; NONE = not in the solve: anchored,
//! the world, or a no-reaction B), `n_rows`, the LDL row count / mask (conn /
//!), the compliance floors (conn /) and the DUAL type (conn.vtbl[1]).
//! It calls `build_rows` (D1.1) with the body records and poses, and `write_back`
//! (slot 7, D1.3) with the solved impulses.

pub mod dual_rows;
pub mod instance;

use glam::{Mat3, Quat, Vec3};
use rapier3d::prelude as rp;
use rbx_dom_weak::types::Ref;
use rustc_hash::FxHashMap;

use super::body::{self, BodySet, NONE};
use super::dual::{ctype, BodyRec, ConnSrc, DualBatch, LevelCfg, MassRec};
use super::island::Islands;
use super::rows::LocalIndex;

pub use instance::{Desc, End, Inst};

/// Most rows a connector class here declares (BallInSocket, Align2Axes, Orientation,
/// AchievePosition, BodyAngularVelocity: 3; Levitation's 6 is ControllerManager-only).
pub const MAX_ROWS: usize = 3;

// ---------------------------------------------------------------------------------
// Records the builders read

/// The part of the batch's body record (DUAL §0.4, 0x78 bytes) slot 10 reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rec {
    /// rec[0..5]: ṽ, the solve's start iterate (velocity prediction, DUAL §5.1).
    pub v: Vec3,
    pub w: Vec3,
    /// rec: world centre of mass.
    pub com: Vec3,
    /// rec /: v, ω at the step start.
    pub v_ref: Vec3,
    pub w_ref: Vec3,
}

/// The pose a connector reads (D1.1 step 2): body frame in the world; identity for
/// the world / an anchored end (whose attachment frames are stored in world space).
#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub r: Mat3,
    pub p: Vec3,
}

impl Pose {
    pub const IDENTITY: Pose = Pose { r: Mat3::IDENTITY, p: Vec3::ZERO };

    pub fn new(rot: Quat, p: Vec3) -> Self {
        Pose { r: Mat3::from_quat(rot), p }
    }
    #[inline]
    pub fn rot(&self, a: Vec3) -> Vec3 {
        self.r * a
    }
    #[inline]
    pub fn point(&self, a: Vec3) -> Vec3 {
        self.r * a + self.p
    }
}

/// Solver-config fields the joint rows and the write-back read (JOINT D4, values from
/// ITER §5: ctor Studio, re-derived per batch by Studio with L = dt·240).
#[derive(Debug, Clone)]
pub struct JointCfg {
    pub level: u8,
    /// cfg: no block rows (always 0 from the world step, DUAL §2.1).
    pub no_block: bool,
    /// cfg restitution threshold (studs/s).
    pub restitution_threshold: f32,
    /// cfg slop (studs).
    pub slop: f32,
    /// cfg / hinge friction coefficients (v / p lane).
    pub fric_v: f32,
    pub fric_p: f32,
    /// cfg = 3L (deg) max angular correction; cfg fraction.
    pub max_ang: f32,
    pub ang_frac: f32,
    /// cfg = 3L (deg) / cfg = 0.7: Orientation's linear regime / fraction.
    pub orient_max: f32,
    pub orient_frac: f32,
    /// cfg = 0.3L (studs) max linear correction; cfg fraction.
    pub max_lin: f32,
    pub lin_frac: f32,
    /// cfg = 15L (deg) cone max correction; cfg = 1 (deg) slop.
    pub cone_max: f32,
    pub cone_slop: f32,
    /// cfg (u8) = 1: BallInSocket lever arms at the midpoint.
    pub midpoint_lever: bool,
    /// cfg = 0.7 InsideOfSphere position-error fraction.
    pub sphere_frac: f32,
    /// cfg = 0.0087266 (0.5°) servo unwrap threshold.
    pub servo_unwrap: f32,
    /// cfg / / /: adaptive ω / scale parameter blocks (D4).
    pub sor_bilateral: [f32; 6],
    pub sor_unilateral: [f32; 6],
    pub scale_v: [f32; 6],
    pub scale_p: [f32; 6],
    /// cfg warm start on.
    pub warm_start: bool,
    /// cfg / adaptive scale on (DUAL §7: ctor 1, 1).
    pub adapt_scale_v: bool,
    pub adapt_scale_p: bool,
    /// cfg = solver: adaptive timestepping (STEP: on) → substep integrator.
    pub adaptive: bool,
    /// cfg /: compliance (DUAL §3.2 / §4.1).
    pub compliance_v: f32,
    pub compliance_p: f32,
}

impl JointCfg {
    pub fn for_level(level: u8) -> Self {
        let l = level.max(1) as f32;
        JointCfg {
            level,
            no_block: false,
            restitution_threshold: 1.0,
            slop: 0.05,
            fric_v: 0.02,
            fric_p: 0.01,
            max_ang: 3.0 * l,
            ang_frac: 1.0,
            orient_max: 3.0 * l,
            orient_frac: 0.7,
            max_lin: 0.3 * l,
            lin_frac: 1.0,
            cone_max: 15.0 * l,
            cone_slop: 1.0,
            midpoint_lever: true,
            sphere_frac: 0.7,
            servo_unwrap: 0.0087266,
            sor_bilateral: [1.0, 0.01, 1.5, 0.75, 1e-4, 0.2],
            sor_unilateral: [0.05, 0.01, 1.9, 1.0, 1e-4, 0.2],
            scale_v: [2.0, 0.5, 0.99, 0.93, 2e-4, 0.01],
            scale_p: [1.5, 0.5, 0.99, 0.7, 2e-4, 0.01],
            warm_start: true,
            adapt_scale_v: true,
            adapt_scale_p: true,
            adaptive: true,
            compliance_v: 1e-5,
            compliance_p: 1e-3,
        }
    }
}

/// The builders' context: both bodies' records and poses, the cfg, dt = L·h.
pub struct Ctx<'a> {
    pub ra: &'a Rec,
    pub rb: &'a Rec,
    pub pa: &'a Pose,
    pub pb: &'a Pose,
    pub cfg: &'a JointCfg,
    pub h: f32,
    /// The geometric-stiffness output p9 (JOINT D1.4 [J2]): {own[A], own[B], the pair
    /// group's accumulator of B, of A}; `None` = no p9 (each builder skips its GS block).
    pub gs: Option<&'a std::cell::Cell<[Mat3; 4]>>,
    /// The connector's kept λ0 per row (K[r] f64), read by the GS blocks.
    pub lam0: [f64; MAX_ROWS],
}

impl Ctx<'_> {
    /// p9[k] += m or −= m.
    pub fn gs_add(&self, k: usize, m: Mat3, add: bool) {
        if let Some(c) = self.gs {
            let mut g = c.get();
            g[k] = if add { g[k] + m } else { g[k] - m };
            c.set(g);
        }
    }
}

// ---------------------------------------------------------------------------------
// The scratch row (DUAL §2.3 / JOINT D1.2, 0xb0 bytes)

/// The scratch row is CORE's `rows::DualRow` (DUAL §2.3 / JOINT D1.2, 0xb0 bytes):
/// λ (, f64 pair), rhs = (b, e) (), lo / hi per lane ( /), ω (),
/// `jv` (..: A.lin, A.ang, B.lin, B.ang) and `jp` (the second copy,..).
pub use super::rows::DualRow;

/// What the slot-10 builders do to a row.
pub trait RowExt {
    fn prepare(&mut self, k: &Kept, h: f32, cfg: &JointCfg);
    fn set_j(&mut self, al: Vec3, aa: Vec3, bl: Vec3, ba: Vec3);
    fn inactive(&mut self);
    fn jdotv(&self, cx: &Ctx) -> f32;
    fn swap_halves(&mut self);
}

impl RowExt for DualRow {
    /// D1.1 step 1 (Studio @Studio): bounds ±∞; warm start h·K.λ and ω = K.c
    /// when cfg, else 0 and (1, 1). b, e and J start at 0 (as `DualBatch::begin`).
    fn prepare(&mut self, k: &Kept, h: f32, cfg: &JointCfg) {
        *self = DualRow::default();
        self.lo = [f32::NEG_INFINITY; 2];
        self.hi = [f32::INFINITY; 2];
        if cfg.warm_start {
            self.lambda = [h as f64 * k.lam[0], h as f64 * k.lam[1]];
            self.omega = k.c;
        } else {
            self.omega = [1.0; 2];
        }
    }

    /// Both Jacobian copies (every joint class writes them identically, D1.2).
    #[inline]
    fn set_j(&mut self, al: Vec3, aa: Vec3, bl: Vec3, ba: Vec3) {
        self.jv = [al, aa, bl, ba];
        self.jp = self.jv;
    }

    /// "inactive row" (JOINT D3 notation): J = (−1,0,0 | 0) / (1,0,0 | 0), b = e = 0,
    /// every bound 0, warm start 0.
    fn inactive(&mut self) {
        self.set_j(Vec3::new(-1.0, -0.0, -0.0), Vec3::ZERO, Vec3::X, Vec3::ZERO);
        self.rhs = [0.0, 0.0];
        self.lo = [0.0, 0.0];
        self.hi = [0.0, 0.0];
        self.lambda = [0.0, 0.0];
    }

    /// J·v with the current velocities (rec[0..5]).
    #[inline]
    fn jdotv(&self, cx: &Ctx) -> f32 {
        self.jv[0].dot(cx.ra.v) + self.jv[1].dot(cx.ra.w) + self.jv[2].dot(cx.rb.v) + self.jv[3].dot(cx.rb.w)
    }

    /// D1.1 step 4: swap the A / B halves of both Jacobian copies.
    fn swap_halves(&mut self) {
        self.jv.swap(0, 2);
        self.jv.swap(1, 3);
        self.jp.swap(0, 2);
        self.jp.swap(1, 3);
    }
}

// ---------------------------------------------------------------------------------
// Kept state (JOINT §1.5 / D2), one 0x40-byte record per row (conn)

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kept {
    /// / f64: λ per unit time (v / p lane).
    pub lam: [f64; 2],
    /// / trust a; / previous λ; / Δλ; / Δ²λ.
    pub a: [f32; 2],
    pub p: [f32; 2],
    pub d: [f32; 2],
    pub dd: [f32; 2],
    /// / adapted relaxation ω (ctor 1.9 = 0x3ff33333);
    /// / adapted scale (ctor 1.0).
    pub c: [f32; 2],
    pub s: [f32; 2],
}

impl Default for Kept {
    fn default() -> Self {
        Kept { lam: [0.0; 2], a: [0.0; 2], p: [0.0; 2], d: [0.0; 2], dd: [0.0; 2], c: [1.9; 2], s: [1.0; 2] }
    }
}

/// Studio (relaxation adaptation), transcribed from ../dumps/dc/Studio.c.
pub fn adapt_omega(c: f32, rel: f32, p: &[f32; 6]) -> f32 {
    let mut t = p[2];
    if p[1] < rel {
        let x = if rel <= p[0] { rel } else { p[0] };
        let x = if p[1] <= x { x } else { p[1] };
        let u = (x - p[1]) / (p[0] - p[1]);
        t = (1.0 - u) * p[2] + u * p[3];
    }
    let w = if t <= c { p[5] } else { p[4] };
    (1.0 - w) * c + t * w
}

/// Studio (scale adaptation), transcribed from ../dumps/dc/Studio.c.
pub fn adapt_scale(rel: f32, s: f32, p: &[f32; 6]) -> f32 {
    let x = if rel <= p[0] { rel } else { p[0] };
    let x = if x < p[1] { p[1] } else { x };
    let u = (x - p[1]) / (p[0] - p[1]);
    let t = (1.0 - u) * p[2] + u * p[3];
    if t < s { (1.0 - p[5]) * s + p[5] * t } else { (1.0 - p[4]) * s + p[4] * t }
}

impl Kept {
    /// Studio(K, x, c, h, unilateral, adapt, cfg), as Studio computes it:
    /// x = the row's solved impulse pair (f64), c = the ω it was solved with.
    pub fn store(&mut self, x: [f64; 2], c: [f32; 2], h: f32, unilateral: bool, adapt: bool, cfg: &JointCfg) {
        let inv = 1.0f64 / h as f64;
        let mut l0 = inv * x[0];
        let l1 = inv * x[1];
        if !adapt {
            self.lam = [l0, l1];
            return;
        }
        const E: f32 = 1e-5; // .rdata Studio
        let f0 = l0 as f32;
        let f1 = l1 as f32;
        let (s0, s1, c0, c1);
        let mut l1s = l1;
        if !unilateral {
            let pred0 = self.d[0] + self.p[0] + self.dd[0];
            let pred1 = self.d[1] + self.p[1] + self.dd[1];
            let rel0 = ((f0 - pred0).abs() * self.a[0]) / (pred0.abs().max(f0.abs()) + E);
            let rel1 = ((f1 - pred1).abs() * self.a[1]) / (pred1.abs().max(f1.abs()) + E);
            s0 = if cfg.adapt_scale_v { adapt_scale(rel0, self.s[0], &cfg.scale_v) } else { 1.0 };
            s1 = if cfg.adapt_scale_p { adapt_scale(rel1, self.s[1], &cfg.scale_p) } else { 1.0 };
            self.s = [s0, s1];
            c0 = adapt_omega(c[0], rel0, &cfg.sor_bilateral);
            c1 = adapt_omega(c[1], rel1, &cfg.sor_bilateral);
            l0 *= s0 as f64;
            l1s = s1 as f64 * l1;
        } else {
            let rel0 = ((f0 - self.p[0]).abs() * self.a[0]) / (self.p[0].abs().max(f0.abs()) + E);
            let rel1 = ((f1 - self.p[1]).abs() * self.a[1]) / (self.p[1].abs().max(f1.abs()) + E);
            c0 = adapt_omega(c[0], rel0, &cfg.sor_unilateral);
            c1 = adapt_omega(c[1], rel1, &cfg.sor_unilateral);
        }
        self.lam = [l0, l1s];
        self.c = [c0, c1];
        // history (f32 of the stored λ), then a += (1 − a)·0.1 (.rdata Studio)
        for k in 0..2 {
            let lf = self.lam[k] as f32;
            let d = lf - self.p[k];
            self.p[k] = lf;
            let old = self.d[k];
            self.d[k] = d;
            self.dd[k] = d - old;
            self.a[k] += (1.0 - self.a[k]) * 0.1;
        }
    }
}

// ---------------------------------------------------------------------------------
// Connectors

/// The connector classes (JOINT §4.0) with their own parameters.
#[derive(Clone)]
pub enum Kind {
    BallInSocket { p_a: Vec3, p_b: Vec3, use_com_a: bool },
    Align2Axes(dual_rows::Align2Axes),
    Motor(dual_rows::Motor),
    Servo(dual_rows::Servo),
    Cone(dual_rows::Cone),
    Spring(dual_rows::Spring),
    Torsion(dual_rows::Torsion),
    Sphere(dual_rows::Sphere),
    Rod(dual_rows::Rod),
    BallOnAxis(dual_rows::BallOnAxis),
    Perp2Axes(dual_rows::Perp2Axes),
    Twist(dual_rows::Twist),
    PosCones(dual_rows::PosCones),
    Achieve(dual_rows::Achieve),
    Orient(dual_rows::Orient),
    BodyAngVel(dual_rows::BodyAngVel),
    Plane { p_a: Vec3, p_b: Vec3, normal_a: Vec3 },
    LinServo(dual_rows::LinServo),
    SingleLinVel(dual_rows::SingleLinVel),
    /// D3.22 DoubleAxisLinearVelocity (LinearVelocity Plane mode, [J3]).
    DoubleLinVel(dual_rows::DoubleLinVel),
    TripleLinVel(dual_rows::TripleLinVel),
    Winch(dual_rows::Winch),
    /// D3.20 ConstraintLinearVelocity (BodyVelocity): max force, target.
    BodyLinVel { max_force: Vec3, target: Vec3 },
    /// D3.18 LegacyGyroAngularVelocity (BodyAngularVelocity, [J3]): target, hi,
    /// lo, use current ω.
    LegacyGyro { target: Vec3, hi: Vec3, lo: Vec3, use_current: bool },
    /// D3.19 LinearSpring (BodyPosition, [J3]).
    LinearSpring(dual_rows::LinearSpring),
}

impl Kind {
    /// (nRows conn, mask conn) per class (JOINT §4.0 table). The servo modes of
    /// Orientation / Align2Axes / Perp2Axes have no block rows: their mode setters
    /// Studio write conn = 0 in servo mode, 7 / 3 / 1
    /// rigid ([J2], JOINT I.11).
    pub fn shape(&self) -> (u8, u8) {
        match self {
            Kind::BallInSocket { .. } => (3, 7),
            Kind::Align2Axes(c) => (3, if c.servo { 0 } else { 3 }),
            Kind::Motor(_) | Kind::Servo(_) | Kind::Cone(_) | Kind::Spring(_) | Kind::Torsion(_) | Kind::Sphere(_) => (1, 0),
            Kind::Rod(_) => (1, 1),
            Kind::BallOnAxis(_) => (2, 3),
            Kind::Perp2Axes(c) => (1, if c.servo { 0 } else { 1 }),
            Kind::Twist(_) => (1, 0),
            Kind::PosCones(_) => (2, 0),
            Kind::Achieve(_) => (3, 0),
            Kind::Orient(c) => (3, if c.servo { 0 } else { 7 }),
            Kind::BodyAngVel(_) => (3, 0),
            Kind::Plane { .. } => (1, 0),
            Kind::LinServo(_) => (1, 0),
            Kind::SingleLinVel(_) => (1, 0),
            Kind::DoubleLinVel(_) => (2, 0),
            Kind::TripleLinVel(_) => (3, 0),
            Kind::Winch(_) => (1, 0),
            Kind::BodyLinVel { .. } => (3, 0),
            Kind::LegacyGyro { .. } | Kind::LinearSpring(_) => (3, 0),
        }
    }

    /// The connector type, vtbl[1] (DUAL §2.1): Triple Studio = 2 iff & 3 == 3,
    /// Double Studio = 1 iff & 3 == 3 (force-limited, magnitude mode); else 0.
    pub fn ctype(&self) -> u8 {
        match self {
            Kind::TripleLinVel(c) if c.bits & 3 == 3 => 2,
            Kind::DoubleLinVel(c) if c.bits & 3 == 3 => 1,
            _ => 0,
        }
    }
}

pub struct Conn {
    pub kind: Kind,
    /// conn nRows, conn mask, conn = popcount(mask & ((1<<n)−1)).
    pub n_rows: u8,
    pub mask: u8,
    pub n_ldl: u8,
    pub kept: [Kept; MAX_ROWS],
    /// conn / compliance floors (v / p): 0 from the base ctor Studio
    /// (`param_1[0xb] = 0`, the qword at; JOINT §1.1 [J2]), so max(cfg, floor) = cfg's.
    pub floor: [f32; 2],
    /// conn bit 1: B is not in the solve (ReactionForce/TorqueEnabled false, I.0).
    pub no_reaction: bool,
    /// conn: the connector's max level (4; SingleAxisSpring: its arbiter's, §5).
    pub max_level: u8,
    /// Sort key of the batch (conn): allocation order.
    pub key: u64,
}

impl Conn {
    pub fn new(kind: Kind, key: u64) -> Self {
        let (n, mask) = kind.shape();
        let n_ldl = (mask & ((1u8 << n) - 1)).count_ones() as u8;
        Conn { kind, n_rows: n, mask, n_ldl, kept: [Kept::default(); MAX_ROWS], floor: [0.0; 2], no_reaction: false, max_level: 4, key }
    }

    /// conn / again after a mode change.
    pub fn refresh_mask(&mut self) {
        let (n, mask) = self.kind.shape();
        self.mask = mask;
        self.n_ldl = (mask & ((1u8 << n) - 1)).count_ones() as u8;
    }

    /// conn.vtbl[1]: the DUAL group (0 = ordinary joints, DUAL §2.1).
    pub fn dual_type(&self) -> u8 {
        0
    }

    /// Whether row r goes to the LDL set (DUAL §2.1: cfg+8 == 0 and bit r of conn).
    #[inline]
    pub fn is_ldl_row(&self, r: usize, cfg: &JointCfg) -> bool {
        !cfg.no_block && self.mask & (1 << r) != 0
    }

    /// D1.1 step 1 for this connector's rows (the batch's `begin` does the same).
    pub fn prepare_rows(&self, rows: &mut [DualRow], h: f32, cfg: &JointCfg) {
        for r in 0..self.n_rows as usize {
            rows[r].prepare(&self.kept[r], h, cfg);
        }
    }

    /// D1.1 step 3: the class's slot 10 on rows pre-set by step 1.
    pub fn build(&mut self, rows: &mut [DualRow], cx: &Ctx) {
        let n = self.n_rows as usize;
        build_kind(&mut self.kind, &mut rows[..n], cx);
    }

    /// Slot 7 write-back (see `write_back_rows`).
    pub fn write_back(&mut self, ldl: &[[f64; 2]], pgs: &[[f64; 2]], omega: &[[f32; 2]], h: f32, cfg: &JointCfg) {
        write_back_rows(self.n_rows, self.mask, &mut self.kept, ldl, pgs, omega, h, cfg);
    }
}

/// Slot 10 of the connector's class on its rows (JOINT D3).
pub fn build_kind(kind: &mut Kind, rows: &mut [DualRow], cx: &Ctx) {
    use dual_rows as d;
    match kind {
        Kind::BallInSocket { p_a, p_b, use_com_a } => d::ball_in_socket(rows, cx, *p_a, *p_b, *use_com_a),
        Kind::Align2Axes(c) => d::align2axes(rows, cx, c),
        Kind::Motor(c) => d::motor(rows, cx, c),
        Kind::Servo(c) => d::servo(rows, cx, c),
        Kind::Cone(c) => d::cone(rows, cx, c),
        Kind::Spring(c) => d::spring(rows, cx, c),
        Kind::Torsion(c) => d::torsion(rows, cx, c),
        Kind::Sphere(c) => d::inside_sphere(rows, cx, c),
        Kind::Rod(c) => d::fixed_distance(rows, cx, c),
        Kind::BallOnAxis(c) => d::ball_on_axis(rows, cx, c),
        Kind::Perp2Axes(c) => d::perp2axes(rows, cx, c),
        Kind::Twist(c) => d::twist(rows, cx, c),
        Kind::PosCones(c) => d::positional_cones(rows, cx, c),
        Kind::Achieve(c) => d::achieve_position(rows, cx, c),
        Kind::Orient(c) => d::orientation(rows, cx, c),
        Kind::BodyAngVel(c) => d::body_angular_velocity(rows, cx, c),
        Kind::Plane { p_a, p_b, normal_a } => d::plane(rows, cx, *p_a, *p_b, *normal_a),
        Kind::LinServo(c) => d::linear_servo(rows, cx, c),
        Kind::SingleLinVel(c) => d::single_axis_linear_velocity(rows, cx, c),
        Kind::DoubleLinVel(c) => d::multi_axis_linear_velocity(rows, cx, c.p_a, c.p_b, &c.axes, c.frame, &c.max_force, &c.target, c.bits & 1 != 0, false),
        Kind::TripleLinVel(c) => d::triple_axis_linear_velocity(rows, cx, c),
        Kind::Winch(c) => d::winch(rows, cx, c),
        Kind::BodyLinVel { max_force, target } => d::constraint_linear_velocity(rows, cx, *max_force, *target),
        Kind::LegacyGyro { target, hi, lo, use_current } => d::legacy_gyro(rows, cx, *target, *hi, *lo, *use_current),
        Kind::LinearSpring(c) => d::body_position(rows, cx, c),
    }
}

/// Slot 7 write-back Studio (JOINT D1.3 / §1.5): block rows from the LDL output
/// (`ldl`, one pair per LDL row in row order) with ω = (1, 1) and no adaptation; the
/// others from the PGS output (`pgs`, `omega`: the ω each was solved with).
#[allow(clippy::too_many_arguments)]
pub fn write_back_rows(n_rows: u8, mask: u8, kept: &mut [Kept; MAX_ROWS], ldl: &[[f64; 2]], pgs: &[[f64; 2]], omega: &[[f32; 2]], h: f32, cfg: &JointCfg) {
    let (mut i, mut j) = (0, 0);
    for r in 0..n_rows as usize {
        if !cfg.no_block && mask & (1 << r) != 0 {
            kept[r].store(ldl[j], [1.0, 1.0], h, false, false, cfg);
            j += 1;
        } else {
            kept[r].store(pgs[i], omega[i], h, false, true, cfg);
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------------
// The spring arbiter (JOINT §5): Studio with the live constants

/// c = 2π·TimestepArbiterOmegaThou/1000 (10000 → 62.83 rad/s), h = 1/240.
pub fn spring_level(k: f32, d: f32, m: f32) -> u8 {
    let c = 2.0 * 3.1415927f32 * 10.0;
    let h = 1.0f32 / 240.0;
    // 1. x = logf(d/(c·m)); y = logf(k/(c·m·c))
    let x = (d / (c * m)).ln();
    let y = (k / (c * m * c)).ln();
    // 2.
    let mut t = 2.919f32;
    if x > -3.195 && y > -0.4226 {
        t = (-3.97245 - 0.155662 * y - 0.114607 * x).exp();
    }
    // 3.
    let dtmax = t / c;
    let l = if dtmax < 4.0 * h {
        if dtmax >= 2.0 * h { 2 } else { 1 }
    } else {
        4
    };
    // 4. conn = min(L, 2)
    l.min(2)
}

// ---------------------------------------------------------------------------------
// Solver::Force items (JOINT I.14): VectorForce, LineForce (ForceLinear) and Torque
// (ForceAngular), applied by Studio before the solve

#[derive(Debug, Clone, Copy)]
pub enum Force {
    /// ForceLinear slot 2 Studio: point in A, point in B, force
    /// (in A's / B's body frame or world, by `frame`), Magnitude, MaxForce,
    /// bits (0 at COM, 1 LineForce mode, 2 inverse square), bit 0 reaction.
    Linear { p_a: Vec3, p_b: Vec3, force: Vec3, magnitude: f32, max_force: f32, at_com: bool, line: bool, inverse_square: bool, frame: u8, reaction: bool },
    /// ForceAngular slot 2 Studio: τ_A = frame·torque, τ_B = −τ_A.
    Angular { torque: Vec3, frame: u8, reaction: bool },
}

impl Force {
    /// (F_A, τ_A, F_B, τ_B) for this tick.
    pub fn eval(&self, ra: &Rec, pa: &Pose, rb: &Rec, pb: &Pose) -> (Vec3, Vec3, Vec3, Vec3) {
        let frame = |f: u8, x: Vec3| match f {
            0 => pa.rot(x),
            1 => pb.rot(x),
            _ => x,
        };
        match *self {
            Force::Linear { p_a, p_b, force, magnitude, max_force, at_com, line, inverse_square, frame: fr, .. } => {
                let pa_w = if at_com { ra.com } else { pa.point(p_a) };
                let (f, q) = if !line {
                    (frame(fr, force), pa_w)
                } else {
                    let pb_w = pb.point(p_b);
                    let d = pb_w - pa_w;
                    let l = d.length();
                    let f = if l <= 0.001 {
                        Vec3::ZERO
                    } else if !inverse_square {
                        d * (magnitude / l)
                    } else {
                        let m = magnitude / (l * l);
                        d * (m.max(-max_force).min(max_force) / l)
                    };
                    (f, pb_w)
                };
                (f, (pa_w - ra.com).cross(f), -f, -((q - rb.com).cross(f)))
            }
            Force::Angular { torque, frame: fr, .. } => {
                let t = frame(fr, torque);
                (Vec3::ZERO, t, Vec3::ZERO, -t)
            }
        }
    }

    pub fn reaction(&self) -> bool {
        match *self {
            Force::Linear { reaction, .. } | Force::Angular { reaction, .. } => reaction,
        }
    }
}

// ---------------------------------------------------------------------------------
// The list the solver walks

/// Persistent Instances plus this tick's flat list of active connectors.
#[derive(Default)]
pub struct JointList {
    /// The constraint Instances (slab; `of` maps the Roblox Ref to its slot).
    pub insts: Vec<Option<Inst>>,
    pub of: FxHashMap<Ref, u32>,
    free: Vec<u32>,
    next_key: u64,
    // -- per tick, per active connector --
    /// Body slots in the solve (NONE: world / anchored / not in the solve).
    pub a: Vec<u32>,
    pub b: Vec<u32>,
    /// Slots whose pose / record the rows read (`b` may be NONE while `pose_b` is not:
    /// a no-reaction B, I.0).
    pub pose_a: Vec<u32>,
    pub pose_b: Vec<u32>,
    /// (Instance slot, connector index in it).
    pub conn: Vec<(u32, u8)>,
    /// Sort key (conn).
    pub key: Vec<u64>,
    // -- per tick, forces and level requests --
    pub force_inst: Vec<u32>,
    /// Slot-2 level requests of the spring arbiter (§5): (body slot, level).
    pub level_req: Vec<(u32, u8)>,
    force_out: Vec<(u32, Vec3, Vec3)>,
    had_req: bool,
}

impl JointList {
    /// Active connectors this tick.
    pub fn len(&self) -> usize {
        self.a.len()
    }

    fn clear_tick(&mut self) {
        self.a.clear();
        self.b.clear();
        self.pose_a.clear();
        self.pose_b.clear();
        self.conn.clear();
        self.key.clear();
        self.force_inst.clear();
        self.level_req.clear();
    }

    /// The census path (rapier): counts rapier's impulse joints (no rows).
    pub fn gather(&mut self, joints: &rp::ImpulseJointSet, bodies: &BodySet, _instances: &FxHashMap<rp::ImpulseJointHandle, Ref>) {
        self.clear_tick();
        for (_, j) in joints.iter() {
            if !j.data.is_enabled() {
                continue;
            }
            self.a.push(bodies.slot(j.body1()));
            self.b.push(bodies.slot(j.body2()));
            self.pose_a.push(NONE);
            self.pose_b.push(NONE);
            self.conn.push((NONE, 0));
            self.key.push(0);
        }
    }

    /// Instances seen by `begin_sync` and not `upsert`ed again are dropped by `end_sync`.
    pub fn begin_sync(&mut self) {
        for i in self.insts.iter_mut().flatten() {
            i.seen = false;
        }
    }

    /// Keeps the Instance `r` as it is this sync (its description did not change).
    pub fn keep(&mut self, r: Ref) {
        if let Some(&i) = self.of.get(&r) {
            if let Some(x) = self.insts[i as usize].as_mut() {
                x.seen = true;
            }
        }
    }

    /// Creates or updates the Instance `r` from its description (JOINT I).
    pub fn upsert(&mut self, r: Ref, desc: &Desc, wake_for_edit: bool) -> [Option<rp::RigidBodyHandle>; 2] {
        let idx = match self.of.get(&r) {
            Some(&i) if self.insts[i as usize].as_ref().is_some_and(|x| x.class_id() == desc.class_id()) => i,
            Some(&i) => {
                // the Ref now names another class: a new Instance
                self.insts[i as usize] = None;
                self.free.push(i);
                self.of.remove(&r);
                self.alloc(r, desc)
            }
            None => self.alloc(r, desc),
        };
        let next_key = &mut self.next_key;
        let inst = self.insts[idx as usize].as_mut().unwrap();
        // An attachment edit can displace a passive joint whose island is
        // asleep. Wake its bodies before gathering rows for the new frames.
        let frame_changed = |old: instance::End, new: instance::End| {
            old.body != new.body || old.pos != new.pos || old.rot != new.rot
        };
        let wake = wake_for_edit && desc.in_world
            && (!inst.in_world || frame_changed(inst.a, desc.a) || frame_changed(inst.b, desc.b));
        inst.apply(desc, next_key);
        inst.seen = true;
        if wake { [desc.a.body, desc.b.body] } else { [None; 2] }
    }

    fn alloc(&mut self, r: Ref, desc: &Desc) -> u32 {
        let inst = Inst::new(desc, &mut self.next_key);
        let i = match self.free.pop() {
            Some(i) => {
                self.insts[i as usize] = Some(inst);
                i
            }
            None => {
                self.insts.push(Some(inst));
                (self.insts.len() - 1) as u32
            }
        };
        self.of.insert(r, i);
        i
    }

    pub fn end_sync(&mut self) {
        let gone: Vec<Ref> = self.of.iter().filter(|(_, &i)| !self.insts[i as usize].as_ref().is_some_and(|x| x.seen)).map(|(r, _)| *r).collect();
        for r in gone {
            if let Some(i) = self.of.remove(&r) {
                self.insts[i as usize] = None;
                self.free.push(i);
            }
        }
    }

    /// This tick's active connectors, in key order (DUAL §1 step 5 sorts by conn),
    /// the force items and the spring arbiter's requests.
    pub fn gather_studio(&mut self, bodies: &BodySet) {
        self.clear_tick();
        let slot = |h: Option<rp::RigidBodyHandle>| h.map_or(NONE, |h| bodies.slot(h));
        for (ii, inst) in self.insts.iter().enumerate() {
            let Some(inst) = inst else { continue };
            if !inst.in_world {
                continue;
            }
            let (sa, sb) = (slot(inst.a.body), slot(inst.b.body));
            // an end whose body is not gathered this tick (disabled) takes the Instance out
            if (inst.a.body.is_some() && sa == NONE) || (inst.b.body.is_some() && sb == NONE) {
                continue;
            }
            if inst.force.is_some() {
                self.force_inst.push(ii as u32);
            }
            for (ci, c) in inst.conns.iter().enumerate() {
                if inst.active & (1 << ci) == 0 {
                    continue;
                }
                self.a.push(sa);
                self.b.push(if c.no_reaction { NONE } else { sb });
                self.pose_a.push(sa);
                self.pose_b.push(sb);
                self.conn.push((ii as u32, ci as u8));
                self.key.push(c.key);
                if let Kind::Spring(_) = c.kind {
                    for s in [sa, sb] {
                        if s != NONE {
                            self.level_req.push((s, c.max_level));
                        }
                    }
                }
            }
        }
        // stable sort of the flat list by key (allocation order)
        let n = self.a.len();
        if (1..n).any(|i| self.key[i - 1] > self.key[i]) {
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by_key(|&i| self.key[i]);
            let perm = |v: &mut Vec<u32>| *v = idx.iter().map(|&i| v[i]).collect();
            perm(&mut self.a);
            perm(&mut self.b);
            perm(&mut self.pose_a);
            perm(&mut self.pose_b);
            self.conn = idx.iter().map(|&i| self.conn[i]).collect();
            self.key = idx.iter().map(|&i| self.key[i]).collect();
        }
    }

    pub fn conn_mut(&mut self, c: usize) -> &mut Conn {
        let (i, k) = self.conn[c];
        &mut self.insts[i as usize].as_mut().unwrap().conns[k as usize]
    }

    /// SLEEP §2.1 [L2]: edge vtbl[0x78] of connector `c`'s Instance.
    pub fn conn_actuated(&self, c: usize) -> bool {
        static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *ALL.get_or_init(|| std::env::var("RUSTUDIO_SOLVER_ACTUATED").is_ok_and(|v| v == "all")) {
            return self.insts[self.conn[c].0 as usize].as_ref().unwrap().is_constraint_edge();
        }
        // SLEEP Open 4: which classes answer true is not settled; the motor / servo connectors.
        matches!(self.conn_ref(c).kind, Kind::Motor(_) | Kind::Servo(_) | Kind::LinServo(_))
    }

    pub fn conn_ref(&self, c: usize) -> &Conn {
        let (i, k) = self.conn[c];
        &self.insts[i as usize].as_ref().unwrap().conns[k as usize]
    }

    /// D1.1 for flat connector `c`: rows pre-set, slot 10, `rows[..n_rows]` filled.
    /// `rec(slot)` gives a body's record and pose (only called for slots ≠ NONE).
    pub fn build_rows(&mut self, c: usize, rec: &dyn Fn(u32) -> (Rec, Pose), cfg: &JointCfg, h: f32, rows: &mut [DualRow]) {
        let get = |s: u32| if s == NONE { (Rec::default(), Pose::IDENTITY) } else { rec(s) };
        let (ra, pa) = get(self.pose_a[c]);
        let (mut rb, pb) = get(self.pose_b[c]);
        let conn = self.conn_mut(c);
        if conn.no_reaction {
            // D1.1 step 2 (conn bit 1): B's record with v = 0, not in the solve
            rb.v = Vec3::ZERO;
            rb.w = Vec3::ZERO;
        }
        let cx = Ctx { ra: &ra, rb: &rb, pa: &pa, pb: &pb, cfg, h, gs: None, lam0: [0.0; MAX_ROWS] };
        conn.prepare_rows(rows, h, cfg);
        conn.build(rows, &cx);
    }

    /// Studio (JOINT I.14 step 2) for every force item whose body A is `due`
    /// this tick (`(phase & (A.level − 1)) == 0`): F, τ into the Solver::Force
    /// accumulator (rustudio's `BodySet::force/torque`, which the free velocity reads
    /// with dt = L·h), B's reaction on A's tick.
    pub fn apply_forces(&mut self, bodies: &mut BodySet, islands: &Islands) {
        if self.force_inst.is_empty() {
            return;
        }
        // due = the body's island is solved this tick (its level's phase)
        let island = &bodies.island;
        let due = |s: u32| {
            let k = island[s as usize];
            k != NONE && islands.due[k as usize]
        };
        let rec = |s: u32| -> (Rec, Pose) {
            if s == NONE {
                return (Rec::default(), Pose::IDENTITY);
            }
            let su = s as usize;
            (Rec { com: bodies.com[su], ..Default::default() }, Pose { r: bodies.mat[su], p: bodies.pos[su] })
        };
        self.force_out.clear();
        for &ii in &self.force_inst {
            let inst = self.insts[ii as usize].as_ref().unwrap();
            let Some(f) = inst.force else { continue };
            let slot = |h: Option<rp::RigidBodyHandle>| h.map_or(NONE, |h| bodies.slot(h));
            let (sa, sb) = (slot(inst.a.body), slot(inst.b.body));
            // body = the root assembly's solver body; none → nothing
            if sa == NONE || bodies.flags[sa as usize] & body::flag::DYNAMIC == 0 || !due(sa) {
                continue;
            }
            let (ra, pa) = rec(sa);
            let (rb, pb) = rec(sb);
            let (fa, ta, fb, tb) = f.eval(&ra, &pa, &rb, &pb);
            self.force_out.push((sa, fa, ta));
            if f.reaction() && sb != NONE {
                self.force_out.push((sb, fb, tb));
            }
        }
        // /: kept until the body's own solve (I.14 step 2: B's reaction on A's
        // tick and cadence, whatever B's level)
        for &(s, f, t) in &self.force_out {
            let s = s as usize;
            if bodies.flags[s] & body::flag::DYNAMIC == 0 {
                continue;
            }
            let idx = bodies.handle[s].into_raw_parts().0 as usize;
            if let Some(p) = bodies.persist.get_mut(idx) {
                p.f_solver += f;
                p.t_solver += t;
            }
        }
    }

    /// The rope winches' callback Studio (JOINT I.7 [J2]) for every active Winch
    /// connector whose island is solved this tick, with dt = h (lab J3).
    pub fn winch_callbacks(&mut self, bodies: &BodySet, islands: &Islands, h: f32) {
        for k in 0..self.conn.len() {
            let (ii, ci) = self.conn[k];
            let due = [self.a[k], self.pose_b[k]].iter().any(|&s| s != NONE && {
                let isl = bodies.island[s as usize];
                isl != NONE && islands.due[isl as usize]
            });
            if !due {
                continue;
            }
            if let Some(inst) = self.insts[ii as usize].as_mut() {
                match &mut inst.conns[ci as usize].kind {
                    Kind::Winch(w) => dual_rows::winch_step(w, h),
                    // [J3] BodyGyro's slot 78, on the body's own solve tick: the
                    // part's pose / ω at the step start and the diagonal of the body's inertia
                    Kind::BodyAngVel(c) if c.gyro.is_some() && self.a[k] != NONE => {
                        let s = self.a[k] as usize;
                        let r = bodies.mat[s] * c.gyro.as_ref().unwrap().part_rot;
                        let i = match bodies.inertia_body[s] {
                            Some(m) => Vec3::new(m.x_axis.x, m.y_axis.y, m.z_axis.z),
                            None => Vec3::ONE / bodies.inv_inertia[s],
                        };
                        dual_rows::gyro_step(c, r, bodies.angvel[s], i, dual_rows::MOVER_DT);
                    }
                    _ => {}
                }
            }
        }
    }

    /// The spring arbiter's requests (JOINT §5 step 5) into slot 2 of both bodies;
    /// a body with no spring any more gets 0 (slot 9, Studio on removal).
    pub fn apply_level_requests(&mut self, bodies: &mut BodySet) {
        if self.level_req.is_empty() && !self.had_req {
            return;
        }
        self.had_req = !self.level_req.is_empty();
        for s in 0..bodies.len() {
            let idx = bodies.handle[s].into_raw_parts().0 as usize;
            bodies.persist[idx].requests[2] = 0;
        }
        for &(s, l) in &self.level_req {
            let idx = bodies.handle[s as usize].into_raw_parts().0 as usize;
            let r = &mut bodies.persist[idx].requests[2];
            *r = if *r == 0 { l } else { (*r).min(l) };
        }
    }
}

impl JointList {
    /// The rows of island `i`'s connectors into the legacy batch (D1.1 for each; CORE-NOTES
    /// "Interface changes"): `batch.begin` pre-sets the rows from the kept records (step 1),
    /// slot 10 fills them (step 3), `batch.commit` hands over key, pair, mask, floors.
    /// A side that is a body outside the island (kinematic, not solved) enters as a
    /// non-sim record with its velocity and infinite mass (STEP §7a); NONE = static.
    /// Runs on an island worker with the list shared read-only: the geometry the
    /// builders keep (Align2Axes' world perpendiculars, the Rod's last direction, …)
    /// goes to `state` and is applied serially (`apply_results`).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn build_dual(&self, i: usize, islands: &Islands, bodies: &BodySet, local: &LocalIndex, nonsim: &mut [u32], cfg: &LevelCfg, batch: &mut DualBatch, state: &mut Vec<(u32, Kind)>, gs_out: &mut Vec<Vec3>) {
        // GS output per sim body of the batch (body, 0 for bodies no joint touches)
        gs_out.clear();
        gs_out.resize(batch.n_sim, Vec3::ZERO);
        let ids = islands.joint_ids(i);
        if ids.is_empty() {
            return;
        }
        // p9 targets (DUAL §2.2 [J2], Studio): own[b] per sim body (the 0x24-stride
        // array), and per body-pair group two accumulators (lower / higher index).
        let n_sim = batch.n_sim as u32;
        let mut own: Vec<Mat3> = vec![Mat3::ZERO; batch.n_sim];
        let mut groups: Vec<((u32, u32), [Mat3; 2])> = Vec::new();
        let gs_cell = std::cell::Cell::new([Mat3::ZERO; 4]);
        let jcfg = JointCfg::for_level(cfg.level);
        let mut rows_buf = [DualRow::default(); MAX_ROWS];
        for &j in ids {
            let j = j as usize;
            let mut side = |s: u32, batch: &mut DualBatch| -> u32 {
                if s == NONE {
                    return NONE;
                }
                let l = local.of(s);
                if l != NONE {
                    return l;
                }
                let su = s as usize;
                if nonsim[su] == NONE {
                    let rec = BodyRec { vf: bodies.linvel[su], wf: bodies.angvel[su], r: bodies.mat[su], x: bodies.com[su], v0: bodies.linvel[su], w0: bodies.angvel[su] };
                    nonsim[su] = batch.add_body(rec, MassRec::default(), false);
                }
                nonsim[su]
            };
            let la = side(self.pose_a[j], batch);
            let lb_pose = side(self.pose_b[j], batch);
            let lb = if self.b[j] == NONE { NONE } else { lb_pose };
            let rec = |l: u32, s: u32| -> (Rec, Pose) {
                if s == NONE {
                    return (Rec::default(), Pose::IDENTITY);
                }
                let r = batch.body(l);
                let su = s as usize;
                (Rec { v: r.vf, w: r.wf, com: r.x, v_ref: r.v0, w_ref: r.w0 }, Pose { r: bodies.mat[su], p: bodies.pos[su] })
            };
            let (ra, pa) = rec(la, self.pose_a[j]);
            let (mut rb, pb) = rec(lb_pose, self.pose_b[j]);
            let conn = self.conn_ref(j);
            if conn.no_reaction {
                // D1.1 step 2 (conn bit 1): B's record with v = 0, not in the solve
                rb.v = Vec3::ZERO;
                rb.w = Vec3::ZERO;
            }
            let n = conn.n_rows as usize;
            let kept: [(f64, f64, f32, f32); MAX_ROWS] = std::array::from_fn(|r| {
                let k = &conn.kept[r];
                (k.lam[0], k.lam[1], k.c[0], k.c[1])
            });
            let rows = batch.begin(&kept[..n], cfg);
            rows_buf[..n].copy_from_slice(rows);
            gs_cell.set([Mat3::ZERO; 4]);
            let lam0: [f64; MAX_ROWS] = std::array::from_fn(|r| conn.kept[r].lam[0]);
            let cx = Ctx { ra: &ra, rb: &rb, pa: &pa, pb: &pb, cfg: &jcfg, h: cfg.dt, gs: Some(&gs_cell), lam0 };
            let mut kind = conn.kind.clone();
            build_kind(&mut kind, &mut rows_buf[..n], &cx);
            // p9 = {own[A], own[B], group acc of B, of A}; own[x] is null for a non-sim side
            // (index ≥ nSim): its block is dropped.
            let g = gs_cell.get();
            if g.iter().any(|m| *m != Mat3::ZERO) {
                for (k, x) in [(0usize, la), (1, lb)] {
                    if x != NONE && x < n_sim {
                        own[x as usize] += g[k];
                    }
                }
                let (ka, kb) = (if la == NONE { u32::MAX } else { la }, if lb == NONE { u32::MAX } else { lb });
                let key = (ka.min(kb), ka.max(kb));
                let gi = match groups.iter().position(|(k, _)| *k == key) {
                    Some(x) => x,
                    None => {
                        groups.push((key, [Mat3::ZERO; 2]));
                        groups.len() - 1
                    }
                };
                // [2] → B's accumulator, [3] → A's (Studio..Studio with Studio..)
                let (ia, ib) = if ka < kb { (0, 1) } else { (1, 0) };
                groups[gi].1[ib] += g[2];
                groups[gi].1[ia] += g[3];
            }
            rows.copy_from_slice(&rows_buf[..n]);
            state.push((j as u32, kind));
            let mask = if jcfg.no_block { 0 } else { conn.mask };
            batch.commit(conn.kind.ctype(), conn.key, [la, lb], n as u8, mask, conn.floor, ConnSrc::Joint(j as u32));
        }
        // Studio per group (lower, then higher index), then per sim body its own
        // accumulator and Studio (sqrt of each positive sum): body += Σ col².
        let col2 = |m: &Mat3| Vec3::new(m.x_axis.length_squared(), m.y_axis.length_squared(), m.z_axis.length_squared());
        for ((lo, hi), acc) in &groups {
            for (x, m) in [(*lo, &acc[0]), (*hi, &acc[1])] {
                if x < n_sim {
                    gs_out[x as usize] += col2(m);
                }
            }
        }
        for (x, m) in own.iter().enumerate() {
            let s = gs_out[x] + col2(m);
            gs_out[x] = Vec3::new(if s.x > 0.0 { s.x.sqrt() } else { s.x }, if s.y > 0.0 { s.y.sqrt() } else { s.y }, if s.z > 0.0 { s.z.sqrt() } else { s.z });
        }
        // the non-sim map back to empty for the next island (the contacts' sides were
        // built already; their records stay in the batch)
        for &j in ids {
            for s in [self.pose_a[j as usize], self.pose_b[j as usize]] {
                if s != NONE {
                    nonsim[s as usize] = NONE;
                }
            }
        }
    }

    /// The workers' results, serially: each connector's builder geometry and kept records.
    pub fn apply_results(&mut self, state: &mut Vec<(u32, Kind)>, kept: &mut Vec<(u32, [Kept; MAX_ROWS])>) {
        for (j, k) in state.drain(..) {
            self.conn_mut(j as usize).kind = k;
        }
        for (j, k) in kept.drain(..) {
            self.conn_mut(j as usize).kept = k;
        }
    }

    /// Slot 7 for flat connector `j` (JOINT D1.3), from the batch's results: the PGS
    /// rows' state (λ at [6], [7]) and ω, the LDL rows' λ pairs. Returns the new kept
    /// records (applied serially by `apply_results`).
    pub fn write_back_dual(&self, j: u32, pgs: &[[f64; 8]], omega: &[[f32; 2]], ldl: &[[f64; 2]], level: u8, dt: f32) -> [Kept; MAX_ROWS] {
        let jcfg = JointCfg::for_level(level);
        let mut lam = [[0.0f64; 2]; MAX_ROWS];
        for (k, r) in pgs.iter().enumerate().take(MAX_ROWS) {
            lam[k] = [r[6], r[7]];
        }
        let conn = self.conn_ref(j as usize);
        let mut kept = conn.kept;
        write_back_rows(conn.n_rows, conn.mask, &mut kept, ldl, &lam[..pgs.len().min(MAX_ROWS)], omega, dt, &jcfg);
        kept
    }
}

/// A body's record and pose from the BodySet with explicit current / step-start
/// velocities (the batch passes ṽ as `v`, `w`).
pub fn rec_of(bodies: &BodySet, s: u32, v: Vec3, w: Vec3, v_ref: Vec3, w_ref: Vec3) -> (Rec, Pose) {
    let s = s as usize;
    (Rec { v, w, com: bodies.com[s], v_ref, w_ref }, Pose { r: bodies.mat[s], p: bodies.pos[s] })
}

#[cfg(test)]
mod tests;
