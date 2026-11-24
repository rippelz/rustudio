//! The Instance side (JOINT I): which connectors each Constraint class owns and
//! inserts for each setting, and which property lands in which connector field.
//!
//! physics_joints.rs (studio path) reads the Instance's properties and attachments
//! into a `Desc` every frame; `Inst::apply` writes the fields. JOINT I.0: a parameter
//! change is a plain field write effective at the next row build, the connectors are
//! allocated once in the Instance ctor and keep their K records through every remove /
//! re-add; only BallSocket's friction connector is freed and re-allocated (fresh K).
//! Body roles: A = Attachment0's part, B = Attachment1's part (0 = the world).
//!
//! Attachment writers are only spelled out for the Hinge (I.1, Studio). For the
//! other classes the points are the attachments' positions and the axes their X
//! (primary) / Y (secondary) axes; perpendicular pairs are the attachment's Y / Z.
//! Rows built on perpendicular pairs are basis-invariant except for box bounds
//! (JOINT-NOTES "attachment writers").

use glam::{Mat3, Quat, Vec3};
use rapier3d::prelude as rp;

use super::dual_rows::{self as d, rad};
use super::{spring_level, Conn, Force, Kind};

/// clampF(x) = clamp(x, ±3.4028235e38) (I.0).
fn clamp_f(x: f32) -> f32 {
    x.max(-f32::MAX).min(f32::MAX)
}

/// Responsiveness R → (K, D) = (0.25·R², R) (.rdata Studio = 0.25).
fn kd(r: f32) -> (f32, f32) {
    (0.25 * r * r, r)
}

/// One end of a constraint: its body (None = the world / an anchored part, whose
/// frame is then in world space), the attachment frame in that body, and the part's
/// assembly mass (Studio, the spring arbiter's).
#[derive(Debug, Clone, Copy)]
pub struct End {
    pub body: Option<rp::RigidBodyHandle>,
    pub pos: Vec3,
    /// Columns: the attachment's X (primary), Y (secondary), Z axes.
    pub rot: Mat3,
    pub mass: f32,
}

impl End {
    pub const WORLD: End = End { body: None, pos: Vec3::ZERO, rot: Mat3::IDENTITY, mass: 0.0 };
    fn x(&self) -> Vec3 {
        self.rot.x_axis
    }
    fn y(&self) -> Vec3 {
        self.rot.y_axis
    }
    fn z(&self) -> Vec3 {
        self.rot.z_axis
    }
}

/// Roblox properties of each class, as the Instance holds them (degrees as degrees).
#[derive(Debug, Clone)]
pub enum Class {
    Hinge { actuator: u32, ang_resp: f32, ang_speed: f32, ang_vel: f32, lower: f32, upper: f32, motor_max_acc: f32, motor_max_torque: f32, restitution: f32, servo_max_torque: f32, target: f32, limits: bool },
    BallSocket { max_friction: f32, restitution: f32, twist_lower: f32, twist_upper: f32, upper: f32, limits: bool, twist_limits: bool },
    /// Prismatic (0), Cylindrical (1), SlidingBall (2).
    Sliding { variant: u8, actuator: u32, lin_resp: f32, lower: f32, upper: f32, motor_max_acc: f32, motor_max_force: f32, restitution: f32, servo_max_force: f32, speed: f32, target: f32, velocity: f32, limits: bool, ang: Option<CylAngular> },
    Universal { max_angle: f32, restitution: f32, limits: bool },
    Plane,
    Rod { length: f32, limit0: f32, limit1: f32, limits: bool },
    Rope { length: f32, restitution: f32, winch: bool, winch_force: f32, winch_resp: f32, winch_speed: f32, winch_target: f32 },
    Spring { damping: f32, free_length: f32, max_force: f32, max_length: f32, min_length: f32, stiffness: f32, limits: bool },
    Torsion { damping: f32, max_angle: f32, max_torque: f32, restitution: f32, stiffness: f32, limits: bool },
    /// `position` = Position (world) when OneAttachment; `basis` = the ForceRelativeTo
    /// frame's rotation for PerAxis (0 Attachment0, 1 Attachment1, 2 World).
    AlignPosition { one_attachment: bool, position: Vec3, magnitude_mode: bool, relative_to: u8, max_force: f32, max_axes_force: Vec3, max_velocity: f32, responsiveness: f32, at_com: bool, reaction: bool, rigid: bool },
    /// `target` = the world frame (PrimaryAxis / SecondaryAxis) for OneAttachment.
    AlignOrientation { one_attachment: bool, target: Mat3, align_type: u32, primary_only: bool, max_ang_vel: f32, max_torque: f32, responsiveness: f32, reaction: bool, rigid: bool },
    AngularVelocity { ang_vel: Vec3, max_torque: f32, relative_to: u8, reaction: bool },
    /// JOINT I.13 [J2]: mode 0 Line / 1 Plane / 2 Vector (VelocityConstraintMode),
    /// relative_to (0 Attachment0, 1 Attachment1, 2 World), force_limit_mode (0 Magnitude,
    /// 1 PerAxis).
    LinearVelocity { mode: u32, relative_to: u32, force_limit_mode: u32, limits: bool, max_force: f32, max_axes_force: Vec3, vector_velocity: Vec3, line_direction: Vec3, line_velocity: f32, reaction: bool, plane_velocity: [f32; 2], max_planar_axes_force: [f32; 2], primary_tangent: Vec3, secondary_tangent: Vec3 },
    VectorForce { force: Vec3, relative_to: u8, at_com: bool },
    Torque { torque: Vec3, relative_to: u8 },
    LineForce { magnitude: f32, max_force: f32, inverse_square: bool, at_com: bool, reaction: bool },
    /// BodyVelocity / BodyGyro / BodyPosition / BodyAngularVelocity / BodyForce /
    /// BodyThrust / RocketPropulsion: JOINT Open 6 (parameter updates, force functions).
    LegacyMover,
    /// BodyVelocity (I.15 [J2]): ConstraintLinearVelocity (D3.20) fed by Studio.
    BodyVelocity { velocity: Vec3, max_force: Vec3, p: f32 },
    /// BodyForce (I.15 [J2]): Studio → (F, 0) into the assembly accumulator.
    BodyForce { force: Vec3 },
    /// BodyThrust ([J3] I.15): slot 78 Studio → F = R_part·Force, τ = (R_part·Location +
    /// p_part − c) × F into the assembly accumulator.
    BodyThrust { force: Vec3, location: Vec3 },
    /// BodyAngularVelocity ([J3] I.15): slot 78 Studio → LegacyGyroAngularVelocity (D3.18):
    /// = clampF(AngularVelocity), = clampF(MaxTorque), = clampF(−MaxTorque),
    /// = 1 (P is not read).
    BodyAngularVelocity { ang_vel: Vec3, max_torque: Vec3, p: f32 },
    /// BodyPosition ([J3] I.15): slot 78 Studio → LinearSpring (D3.19).
    BodyPosition { position: Vec3, max_force: Vec3, p: f32, d: f32 },
    /// BodyGyro ([J3] I.15): slot 78 Studio → ConstraintBodyAngularVelocity (D3.17, legacy).
    BodyGyro { cframe: Mat3, max_torque: Vec3, p: f32, d: f32 },
    /// Rigid 6-DOF weld between two dynamic bodies (e.g. a seated character welded to a vehicle).
    Fixed,
}

impl Class {
    pub fn id(&self) -> u8 {
        match self {
            Class::Hinge { .. } => 0,
            Class::BallSocket { .. } => 1,
            Class::Sliding { variant, .. } => 2 + *variant,
            Class::Universal { .. } => 5,
            Class::Plane => 6,
            Class::Rod { .. } => 7,
            Class::Rope { .. } => 8,
            Class::Spring { .. } => 9,
            Class::Torsion { .. } => 10,
            Class::AlignPosition { .. } => 11,
            Class::AlignOrientation { .. } => 12,
            Class::AngularVelocity { .. } => 13,
            Class::LinearVelocity { .. } => 14,
            Class::VectorForce { .. } => 15,
            Class::Torque { .. } => 16,
            Class::LineForce { .. } => 17,
            Class::LegacyMover => 18,
            Class::BodyVelocity { .. } => 19,
            Class::BodyForce { .. } => 20,
            Class::BodyThrust { .. } => 21,
            Class::BodyAngularVelocity { .. } => 22,
            Class::BodyPosition { .. } => 23,
            Class::BodyGyro { .. } => 24,
            Class::Fixed => 25,
        }
    }
}

/// CylindricalConstraint's rotation part (its second apply table Studio, sub-object
/// Inst; JOINT I.3 [J2]).
#[derive(Debug, Clone, Copy)]
pub struct CylAngular {
    pub actuator: u32,
    pub resp: f32,
    pub restitution: f32,
    pub speed: f32,
    pub velocity: f32,
    pub lower: f32,
    pub upper: f32,
    pub motor_max_acc: f32,
    pub motor_max_torque: f32,
    pub servo_max_torque: f32,
    pub target: f32,
    pub inclination: f32,
    pub limits: bool,
}

/// What physics_joints.rs reads each frame.
#[derive(Debug, Clone)]
pub struct Desc {
    pub class: Class,
    pub a: End,
    pub b: End,
    /// KJ+8 == 0xf (I.16): Enabled, both attachments resolved, not one assembly.
    pub in_world: bool,
}

impl Desc {
    pub fn class_id(&self) -> u8 {
        self.class.id()
    }
}

pub struct Inst {
    class: u8,
    pub a: End,
    pub b: End,
    /// The connectors the ctor allocates, in field order.
    pub conns: Vec<Conn>,
    /// Which of `conns` are inserted (bit i).
    pub active: u32,
    pub in_world: bool,
    pub force: Option<Force>,
    pub seen: bool,
    /// SpringConstraint: the (K, D, masses) its arbiter level was picked with (§5:
    /// picked at insertion and on every re-add; Stiffness / Damping re-add, I.8).
    spring_pick: Option<(f32, f32, f32, f32)>,
    /// The Instance's properties as last applied: the sleep stage's per-class "actuated
    /// and not at target" test reads them (`edge_actuated`, SLEEP §2.1 [S2]).
    pub props: Option<Class>,
}

/// conn is 0 at construction and assigned at the connector's first registration
/// with the solver (S.vtbl[9] Studio: `conn = ++[Studio]`, ORDER §1):
/// `apply` gives a key to each connector the first time it is active in the world.
fn key(_next: &mut u64) -> u64 {
    0
}

fn bis(next: &mut u64) -> Conn {
    Conn::new(Kind::BallInSocket { p_a: Vec3::ZERO, p_b: Vec3::ZERO, use_com_a: false }, key(next))
}
fn a2a(next: &mut u64) -> Conn {
    Conn::new(Kind::Align2Axes(d::Align2Axes::new(Vec3::X, Vec3::X, Vec3::Y, Vec3::Z)), key(next))
}
fn motor(next: &mut u64) -> Conn {
    Conn::new(Kind::Motor(d::Motor { axis_a: Vec3::X, max_torque: 0.0, target: 0.0, max_acc: f32::INFINITY, acc_cap: 500000.0 }), key(next))
}
fn servo(next: &mut u64) -> Conn {
    Conn::new(
        Kind::Servo(d::Servo { axis_a: Vec3::X, ref_b: Vec3::Y, ref_target_a: Vec3::Y, ref_zero_a: Vec3::Y, max_torque: 0.0, max_speed: 0.0, limits: false, upper: 0.0, lower: 0.0, target: 0.0, k: 0.0, d: 0.0 }),
        key(next),
    )
}
fn cone(next: &mut u64) -> Conn {
    Conn::new(Kind::Cone(d::Cone { axis_a: Vec3::X, axis_b: Vec3::X, alpha: std::f32::consts::PI, restitution: 0.0 }), key(next))
}
fn sphere(next: &mut u64) -> Conn {
    Conn::new(Kind::Sphere(d::Sphere { p_a: Vec3::ZERO, p_b: Vec3::ZERO, l0: 0.0, l1: 0.0, restitution: 0.0 }), key(next))
}
fn perp(next: &mut u64) -> Conn {
    Conn::new(Kind::Perp2Axes(d::Perp2Axes { axis_a: Vec3::X, axis_b: Vec3::X, servo: false, max_torque: 0.0, max_speed: 0.0, k: 0.0, d: 0.0 }), key(next))
}
fn orient(next: &mut u64) -> Conn {
    Conn::new(
        Kind::Orient(d::Orient { a1: Vec3::X, a2: Vec3::Y, b1: Vec3::X, b2: Vec3::Y, servo: false, max_torque: 0.0, max_speed: 0.0, k: 0.0, d: 0.0, u: Vec3::X, t1: Vec3::Y, t2: Vec3::Z }),
        key(next),
    )
}
fn body_ang_vel(next: &mut u64) -> Conn {
    Conn::new(
        Kind::BodyAngVel(d::BodyAngVel { target: Vec3::ZERO, hi: Vec3::ZERO, lo: Vec3::ZERO, basis: Mat3::IDENTITY, frame: 2, use_current: false, legacy: false, gyro: None }),
        key(next),
    )
}

impl Inst {
    /// An constraint (class ids 0..=17): its Edge sub-object () answers
    /// vtbl[0x78] (SLEEP §2.1) with Studio = true (Hinge, BallSocket, Rope, Spring,
    /// AlignPosition, AlignOrientation read; the base class's slot). The legacy body movers
    /// (18..=20) are not edges between primitives.
    pub fn is_constraint_edge(&self) -> bool {
        self.class < 18
    }
}

impl Inst {
    /// The Instance ctor: its connectors (JOINT §4.0 / I), all with fresh K records.
    pub fn new(desc: &Desc, next: &mut u64) -> Self {
        let conns = match &desc.class {
            // I.1: Align2Axes, BallInSocket, motor, servo, Cone
            Class::Hinge { .. } => vec![a2a(next), bis(next), motor(next), servo(next), cone(next)],
            // I.2: BallInSocket, Cone, Twist, friction (on demand)
            Class::BallSocket { .. } => vec![
                bis(next),
                cone(next),
                Conn::new(Kind::Twist(d::Twist { axis_a: Vec3::X, axis_b: Vec3::X, ref_a: Vec3::Y, ref_b: Vec3::Y, l0: 0.0, l1: 0.0, restitution: 0.0 }), key(next)),
                body_ang_vel(next),
            ],
            // I.3: BallOnAxis, Orientation (Prismatic), motor, servo, InsideOfSphere
            Class::Sliding { variant, .. } => {
                let mut v = vec![
                Conn::new(Kind::BallOnAxis(d::BallOnAxis { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axis: Vec3::X, s1: Vec3::Y, s2: Vec3::Z }), key(next)),
                orient(next),
                Conn::new(
                    Kind::SingleLinVel(d::SingleLinVel { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axis_a: Vec3::X, axis_b: Vec3::X, max_force: 0.0, max_acc: 0.0, target: 0.0, frame: 0, world_point: false, force_limited: true }),
                    key(next),
                ),
                Conn::new(Kind::LinServo(d::LinServo { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axis_a: Vec3::X, target: 0.0, max_speed: 0.0, max_force: 0.0, k: 0.0, d: 0.0 }), key(next)),
                sphere(next),
                ];
                // Cylindrical: + Align2Axes, AngularVelocity (kind 7), ServoMotor
                // (kind 8), Cone (kind 0x16) — the sub-object's / / /
                if *variant == 1 {
                    v.extend([a2a(next), motor(next), servo(next), cone(next)]);
                }
                v
            }
            // I.4: BallInSocket + Perp2Axes + Cone
            Class::Universal { .. } => vec![bis(next), perp(next), cone(next)],
            Class::Plane => vec![Conn::new(Kind::Plane { p_a: Vec3::ZERO, p_b: Vec3::ZERO, normal_a: Vec3::X }, key(next))],
            Class::Rod { .. } => vec![
                Conn::new(Kind::Rod(d::Rod { p_a: Vec3::ZERO, p_b: Vec3::ZERO, length: 0.0, dir: Vec3::X }), key(next)),
                Conn::new(Kind::PosCones(d::PosCones { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axis_a: Vec3::X, axis_b: Vec3::X, half_a: 0.0, half_b: 0.0 }), key(next)),
            ],
            // I.7: InsideOfSphere (the Winch at: Open 5, no connector here)
            // I.7: InsideOfSphere, Winch
            Class::Rope { .. } => vec![
                sphere(next),
                Conn::new(Kind::Winch(d::Winch { p_a: Vec3::ZERO, p_b: Vec3::ZERO, cur_len: 0.0, act_len: f32::NAN, prev_len: 0.0, target: 0.0, max_speed: 0.0, max_force: 0.0, k: 0.0, d: 0.0, length: f32::NAN }), key(next)),
            ],
            Class::Spring { .. } => vec![
                Conn::new(Kind::Spring(d::Spring { p_a: Vec3::ZERO, p_b: Vec3::ZERO, max_force: 0.0, free_len: 0.0, k: 0.0, d: 0.0, max_len: f32::INFINITY, min_len: 0.0 }), key(next)),
                sphere(next),
            ],
            Class::Torsion { .. } => vec![
                Conn::new(Kind::Torsion(d::Torsion { axis_a: Vec3::X, axis_b: Vec3::X, max_torque: 0.0, k: 0.0, d: 0.0, max_angle: f32::INFINITY }), key(next)),
                cone(next),
            ],
            // I.10: AchievePosition, BallInSocket
            Class::AlignPosition { .. } => vec![
                Conn::new(
                    Kind::Achieve(d::Achieve {
                        p_a: Vec3::ZERO,
                        p_b: Vec3::ZERO,
                        dir: Vec3::X,
                        t1: Vec3::Y,
                        t2: Vec3::Z,
                        at_com: false,
                        max_force: [0.0; 3],
                        max_speed: 0.0,
                        k: 0.0,
                        d: 0.0,
                        line: true,
                        basis: Mat3::IDENTITY,
                        frame: 2,
                    }),
                    key(next),
                ),
                bis(next),
            ],
            // I.11: Orientation, Align2Axes, Perp2Axes ( AlignAxisToChord: Open 11)
            Class::AlignOrientation { .. } => vec![orient(next), a2a(next), perp(next)],
            Class::AngularVelocity { .. } => vec![body_ang_vel(next)],
            // I.15: BodyVelocity's ConstraintLinearVelocity (Studio: kind 9, 3 rows, mask 0, flag33 0xff)
            Class::BodyVelocity { .. } => vec![Conn::new(Kind::BodyLinVel { max_force: Vec3::ZERO, target: Vec3::ZERO }, key(next))],
            // I.15: BodyAngularVelocity's creator Studio (kind 0x13, 3 rows, mask 0)
            Class::BodyAngularVelocity { .. } => vec![Conn::new(Kind::LegacyGyro { target: Vec3::ZERO, hi: Vec3::ZERO, lo: Vec3::ZERO, use_current: true }, key(next))],
            // I.15: BodyGyro's creator Studio (kind 0xf, 3 rows; frame 2,
            // = 0, legacy = 1, basis identity)
            Class::BodyGyro { .. } => {
                let mut c = body_ang_vel(next);
                if let Kind::BodyAngVel(b) = &mut c.kind {
                    b.legacy = true;
                    b.use_current = false;
                    b.frame = 2;
                    b.gyro = Some(Box::new(d::Gyro { target: Mat3::IDENTITY, max_torque: Vec3::ZERO, p: 0.0, d: 0.0, part_rot: Mat3::IDENTITY, filt: [[0.0; 4]; 3] }));
                }
                vec![c]
            }
            // I.15: BodyPosition's creator Studio (LinearSpring, 3 rows)
            Class::BodyPosition { .. } => vec![Conn::new(Kind::LinearSpring(d::LinearSpring { part: Vec3::ZERO, target: Vec3::ZERO, max_force: Vec3::ZERO, k: 0.0, d: 0.0 }), key(next))],
            // I.13 (ctor Studio): the Line (), Plane (), Vector () wrappers'
            // Single / Double / Triple-axis connectors
            Class::LinearVelocity { .. } => vec![
                Conn::new(
                    Kind::SingleLinVel(d::SingleLinVel { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axis_a: Vec3::X, axis_b: Vec3::X, max_force: 0.0, max_acc: f32::INFINITY, target: 0.0, frame: 2, world_point: false, force_limited: true }),
                    key(next),
                ),
                Conn::new(Kind::DoubleLinVel(d::DoubleLinVel::new()), key(next)),
                Conn::new(Kind::TripleLinVel(d::TripleLinVel { basis: Mat3::IDENTITY, p_a: Vec3::ZERO, p_b: Vec3::ZERO, max_force: Vec3::ZERO, target: Vec3::ZERO, frame: 2, bits: 0 }), key(next)),
            ],
            Class::Fixed => vec![bis(next), orient(next)],
            _ => Vec::new(),
        };
        let mut inst = Inst { class: desc.class_id(), a: desc.a, b: desc.b, conns, active: 0, in_world: false, force: None, seen: true, spring_pick: None, props: None };
        // Every ctor writes its connectors' fixed modes (e.g. Prismatic's Orientation is
        // the rigid lock, I.3); `apply` does the rest.
        inst.apply(desc, next);
        inst
    }

    pub fn class_id(&self) -> u8 {
        self.class
    }

    /// The property apply tables (I.1–I.14) and the add / remove rules.
    pub fn apply(&mut self, desc: &Desc, next: &mut u64) {
        self.a = desc.a;
        self.b = desc.b;
        self.in_world = desc.in_world;
        self.props = Some(desc.class.clone());
        let (a, b) = (desc.a, desc.b);
        let was = self.active;
        let mut active = 0u32;
        self.force = None;
        match &desc.class {
            Class::Hinge { actuator, ang_resp, ang_speed, ang_vel, lower, upper, motor_max_acc, motor_max_torque, restitution, servo_max_torque, target, limits } => {
                // attachment frames: Align2Axes A axis = CF0 X, B axis = CF1 X
                if let Kind::Align2Axes(c) = &mut self.conns[0].kind {
                    c.axis_a = a.x();
                    c.axis_b = b.x();
                    // [J3] Studio: s1 / s2 = Studio(B axis)
                    (c.s1_b, c.s2_b) = d::perp_pair_b(c.axis_b);
                }
                // BallInSocket = CF0.p, = CF1.p
                if let Kind::BallInSocket { p_a, p_b, .. } = &mut self.conns[1].kind {
                    *p_a = a.pos;
                    *p_b = b.pos;
                }
                if let Kind::Motor(m) = &mut self.conns[2].kind {
                    m.axis_a = a.x();
                    m.target = *ang_vel; // slot 5
                    m.max_acc = motor_max_acc.min(m.acc_cap); // slot 8
                    m.max_torque = clamp_f(*motor_max_torque); // slot 9
                }
                let tr = rad(*target);
                if let Kind::Servo(s) = &mut self.conns[3].kind {
                    s.axis_a = a.x();
                    // slot 12: = Rot(att0 Axis, rad T)·att0 SecondaryAxis, = SecondaryAxis
                    s.ref_target_a = Quat::from_axis_angle(a.x(), tr) * a.y();
                    s.ref_zero_a = a.y();
                    s.ref_b = b.y();
                    s.target = tr - ((tr + std::f32::consts::PI) / (2.0 * std::f32::consts::PI)).floor() * (2.0 * std::f32::consts::PI);
                    let (k, dd) = kd(*ang_resp); // slot 2
                    s.k = k;
                    s.d = dd;
                    s.max_speed = *ang_speed; // slot 4
                    s.limits = *limits; // slot 6
                    // Studio(servo, rad L, rad U): upper, lower
                    s.upper = rad(*upper);
                    s.lower = rad(*lower);
                    s.max_torque = clamp_f(*servo_max_torque); // slot 11
                }
                if let Kind::Cone(c) = &mut self.conns[4].kind {
                    // Cone = Rot(CF0 X, rad((U+L)/2))·CF0 Y, = CF1 Y, α = rad(|U−L|)·0.5
                    c.axis_a = Quat::from_axis_angle(a.x(), rad((upper + lower) * 0.5)) * a.y();
                    c.axis_b = b.y();
                    c.alpha = rad((upper - lower).abs()) * 0.5;
                    c.restitution = *restitution; // slot 10
                }
                active = 0b11;
                match actuator {
                    1 => active |= 1 << 2,
                    2 => active |= 1 << 3,
                    _ => {}
                }
                if *limits {
                    active |= 1 << 4;
                }
            }
            Class::BallSocket { max_friction, restitution, twist_lower, twist_upper, upper, limits, twist_limits } => {
                if let Kind::BallInSocket { p_a, p_b, .. } = &mut self.conns[0].kind {
                    *p_a = a.pos;
                    *p_b = b.pos;
                }
                if let Kind::Cone(c) = &mut self.conns[1].kind {
                    c.axis_a = a.x();
                    c.axis_b = b.x();
                    c.alpha = rad(*upper); // slot 7: the half-angle is UpperAngle itself
                    c.restitution = *restitution; // slot 3
                }
                if let Kind::Twist(t) = &mut self.conns[2].kind {
                    t.axis_a = a.x();
                    t.axis_b = b.x();
                    t.ref_a = a.y();
                    t.ref_b = b.y();
                    t.l1 = rad(*twist_lower); // slot 5 →
                    t.l0 = rad(*twist_upper); // slot 6 →
                    t.restitution = *restitution;
                }
                active = 1;
                if *limits {
                    active |= 1 << 1;
                    if *twist_limits {
                        active |= 1 << 2;
                    }
                }
                // friction (slot 2 Studio): created when T > 0 with a fresh K, freed when T ≤ 0
                if *max_friction > 0.0 {
                    if was & (1 << 3) == 0 {
                        let fresh = body_ang_vel(next);
                        self.conns[3] = fresh;
                    }
                    if let Kind::BodyAngVel(f) = &mut self.conns[3].kind {
                        f.frame = 1;
                        f.legacy = false;
                        f.target = Vec3::ZERO;
                        f.basis = b.rot; // Attachment1's rotation (identity if none)
                        f.hi = Vec3::splat(clamp_f(*max_friction));
                        f.lo = Vec3::splat(clamp_f(-*max_friction));
                    }
                    active |= 1 << 3;
                }
            }
            Class::Sliding { variant, actuator, lin_resp, lower, upper, motor_max_acc, motor_max_force, restitution, servo_max_force, speed, target, velocity, limits, ang } => {
                if let Kind::BallOnAxis(c) = &mut self.conns[0].kind {
                    c.p_a = a.pos;
                    c.p_b = b.pos;
                    c.axis = a.x();
                    c.s1 = a.y();
                    c.s2 = a.z();
                }
                if let Kind::Orient(o) = &mut self.conns[1].kind {
                    // Prismatic's rotation lock: rigid mode = 0 (I.3)
                    o.servo = false;
                    o.a1 = a.x();
                    o.a2 = a.y();
                    o.b1 = b.x();
                    o.b2 = b.y();
                }
                if let Kind::SingleLinVel(m) = &mut self.conns[2].kind {
                    m.p_a = a.pos;
                    m.p_b = b.pos;
                    m.axis_a = a.x();
                    m.axis_b = b.x();
                    m.max_acc = clamp_f(*motor_max_acc); // slot 5 (the base ctor clampF's it too)
                    m.max_force = clamp_f(*motor_max_force); // slot 6
                    m.target = *velocity; // slot 12 (not negated, unlike LinearVelocity's Line)
                    // [J2] SingleAxisLinearVelocity ctor Studio: frame 0 (A), 0 (point
                    // in A), |= 1 (force-limited), = +∞; the base ctor Studio then
                    // writes / / from the Instance 
                    m.frame = 0;
                    m.world_point = false;
                    m.force_limited = true;
                }
                if let Kind::LinServo(s) = &mut self.conns[3].kind {
                    s.p_a = a.pos;
                    s.p_b = b.pos;
                    s.axis_a = a.x();
                    let (k, dd) = kd(*lin_resp); // slot 3
                    s.k = k;
                    s.d = dd;
                    s.max_force = clamp_f(*servo_max_force); // slot 8
                    s.max_speed = *speed; // slot 9
                    s.target = *target; // slot 10
                }
                if let Kind::Sphere(s) = &mut self.conns[4].kind {
                    // slot 4: centre = att0.p + ((L+U)/2)·att0 Axis, radius = |U−L|·0.5
                    s.p_a = a.pos + a.x() * ((lower + upper) * 0.5);
                    s.p_b = b.pos;
                    s.l0 = (upper - lower).abs() * 0.5;
                    s.l1 = 0.0;
                    s.restitution = *restitution; // slot 7
                }
                active = 1;
                if *variant == 0 {
                    active |= 1 << 1;
                }
                match actuator {
                    1 => active |= 1 << 2,
                    2 => active |= 1 << 3,
                    _ => {}
                }
                if *limits {
                    active |= 1 << 4;
                }
                if let (1, Some(g)) = (*variant, ang) {
                    // [J2] apply table Studio. Rotation axis in A:
                    // ax = cos i·X0 + sin i·Y0, perp = cos i·Y0 − sin i·X0 (i = rad InclinationAngle,
                    // Inst); slot 7 Studio writes the A side from them.
                    // [J3] the B side is the attachment writer Studio (main vtable slot 90,
                    // Attachment1 branch): Align2Axes B = CF1 X, Servo / Cone =
                    // CF1 Y — no inclination on Attachment1 (lab J6: the Wheel's X swings onto the
                    // inclined axis).
                    let inc = rad(g.inclination);
                    let (si, ci) = inc.sin_cos();
                    let ax = a.x() * ci + a.y() * si;
                    let perp = a.y() * ci - a.x() * si;
                    let bx = b.x();
                    let bperp = b.y();
                    if let Kind::Align2Axes(c) = &mut self.conns[5].kind {
                        c.axis_a = ax; // Studio
                        c.axis_b = bx;
                        (c.s1_b, c.s2_b) = d::perp_pair_b(bx); // Studio
                    }
                    if let Kind::Motor(m) = &mut self.conns[6].kind {
                        m.axis_a = ax;
                        m.target = g.velocity; // slot 6 Studio →
                        m.max_acc = g.motor_max_acc.min(m.acc_cap); // slot 9 Studio
                        m.max_torque = clamp_f(g.motor_max_torque); // slot 10 Studio
                    }
                    let tr = rad(g.target);
                    if let Kind::Servo(s) = &mut self.conns[7].kind {
                        s.axis_a = ax;
                        // slot 12 Studio: = Rot(ax, rad T)·perp, = att0 Y
                        s.ref_target_a = Quat::from_axis_angle(ax, tr) * perp;
                        s.ref_zero_a = a.y();
                        s.ref_b = bperp;
                        s.target = tr - ((tr + std::f32::consts::PI) / (2.0 * std::f32::consts::PI)).floor() * (2.0 * std::f32::consts::PI);
                        let (k, dd) = kd(g.resp); // slot 3 Studio → /
                        s.k = k;
                        s.d = dd;
                        s.max_speed = g.speed; // slot 5 Studio →
                        s.limits = g.limits; // slot 2 Studio →
                        s.upper = rad(g.upper); // slot 8 Studio
                        s.lower = rad(g.lower);
                        s.max_torque = clamp_f(g.servo_max_torque); // slot 11 Studio →
                    }
                    if let Kind::Cone(c) = &mut self.conns[8].kind {
                        // slot 8: = Rot(ax, rad((U+L)/2))·perp, α () = rad|U−L|·0.5
                        c.axis_a = Quat::from_axis_angle(ax, rad((g.upper + g.lower) * 0.5)) * perp;
                        c.axis_b = bperp;
                        c.alpha = rad((g.upper - g.lower).abs()) * 0.5;
                        c.restitution = g.restitution; // slot 4 Studio →
                    }
                    active |= 1 << 5;
                    match g.actuator {
                        1 => active |= 1 << 6,
                        2 => active |= 1 << 7,
                        _ => {}
                    }
                    if g.limits {
                        active |= 1 << 8;
                    }
                }
            }
            Class::Universal { max_angle, restitution, limits } => {
                if let Kind::BallInSocket { p_a, p_b, .. } = &mut self.conns[0].kind {
                    *p_a = a.pos;
                    *p_b = b.pos;
                }
                if let Kind::Perp2Axes(p) = &mut self.conns[1].kind {
                    // [J3] attachment writer Studio (slot 90): Perp2Axes / = CF0 Y /
                    // CF1 Y (Studio copy the vector); Cone / = CF0 X / CF1 X
                    p.axis_a = a.y();
                    p.axis_b = b.y();
                    p.servo = false;
                }
                if let Kind::Cone(c) = &mut self.conns[2].kind {
                    c.axis_a = a.x(); // slot 1: Cone = Attachment0's axis
                    c.axis_b = b.x();
                    c.alpha = rad(*max_angle);
                    c.restitution = *restitution;
                }
                active = 0b11 | if *limits { 1 << 2 } else { 0 };
            }
            Class::Plane => {
                if let Kind::Plane { p_a, p_b, normal_a } = &mut self.conns[0].kind {
                    *p_a = a.pos;
                    *p_b = b.pos;
                    *normal_a = a.x();
                }
                active = 1;
            }
            Class::Rod { length, limit0, limit1, limits } => {
                if let Kind::Rod(r) = &mut self.conns[0].kind {
                    r.p_a = a.pos;
                    r.p_b = b.pos;
                    r.length = *length; // slot 0
                }
                if let Kind::PosCones(c) = &mut self.conns[1].kind {
                    c.p_a = a.pos;
                    c.p_b = b.pos;
                    c.axis_a = a.x();
                    c.axis_b = b.x();
                    c.half_a = rad(*limit0); // slots 1/2
                    c.half_b = rad(*limit1);
                }
                active = 1 | if *limits { 1 << 1 } else { 0 };
            }
            Class::Rope { length, restitution, winch, winch_force, winch_resp, winch_speed, winch_target } => {
                if let Kind::Sphere(s) = &mut self.conns[0].kind {
                    s.p_a = a.pos;
                    s.p_b = b.pos;
                    s.l0 = *length; // Lmax = Length (slot 0)
                    s.l1 = 0.0;
                    s.restitution = *restitution; // slot 1
                }
                if let Kind::Winch(w) = &mut self.conns[1].kind {
                    w.p_a = a.pos;
                    w.p_b = b.pos;
                    // slot 0: Length → (on a change only: the callback moves ℓ)
                    if w.length != *length {
                        w.length = *length;
                        w.act_len = *length;
                    }
                    w.max_force = clamp_f(*winch_force); // slot 3
                    w.k = 0.25 * winch_resp * winch_resp; // slot 4
                    w.d = *winch_resp;
                    w.max_speed = *winch_speed; // slot 5
                    w.target = *winch_target; // slot 6
                }
                // slot 2: WinchEnabled → the Winch instead of InsideOfSphere
                active = if *winch { 1 << 1 } else { 1 };
            }
            Class::Spring { damping, free_length, max_force, max_length, min_length, stiffness, limits } => {
                if let Kind::Spring(s) = &mut self.conns[0].kind {
                    s.p_a = a.pos;
                    s.p_b = b.pos;
                    s.k = *stiffness; // slot 0
                    s.d = *damping;
                    s.free_len = *free_length; // slot 1
                    s.max_force = clamp_f(*max_force); // slot 3
                    // slot 2: off → = +∞, = 0; on → MinLength / MaxLength
                    if *limits {
                        s.max_len = *max_length;
                        s.min_len = *min_length;
                    } else {
                        s.max_len = f32::INFINITY;
                        s.min_len = 0.0;
                    }
                }
                if let Kind::Sphere(s) = &mut self.conns[1].kind {
                    s.p_a = a.pos;
                    s.p_b = b.pos;
                    s.l0 = *max_length;
                    s.l1 = *min_length;
                }
                active = 1 | if *limits { 1 << 1 } else { 0 };
                // §5: the spring arbiter picks conn at insertion and on every re-add
                // (Stiffness / Damping change, I.8), from K, D and the parts' reduced mass.
                let pick = (*stiffness, *damping, a.mass, b.mass);
                if self.spring_pick != Some(pick) || was & 1 == 0 {
                    let m = a.mass * b.mass / (a.mass + b.mass);
                    self.conns[0].max_level = spring_level(*stiffness, *damping, m);
                    self.spring_pick = Some(pick);
                }
            }
            Class::Torsion { damping, max_angle, max_torque, restitution, stiffness, limits } => {
                // [J3] attachment writer Studio (slot 90): TorsionSpring and Cone / =
                // CF0 Y / CF1 Y (the secondary axes), not X
                if let Kind::Torsion(t) = &mut self.conns[0].kind {
                    t.axis_a = a.y();
                    t.axis_b = b.y();
                    t.d = *damping; // slot 0
                    t.k = *stiffness;
                    t.max_torque = clamp_f(*max_torque); // slot 3
                    if *limits {
                        t.max_angle = rad(*max_angle); // slot 2, only while LimitsEnabled
                    }
                }
                if let Kind::Cone(c) = &mut self.conns[1].kind {
                    c.axis_a = a.y();
                    c.axis_b = b.y();
                    c.alpha = rad(*max_angle);
                    c.restitution = *restitution; // slot 5
                }
                active = 1 | if *limits { 1 << 1 } else { 0 };
            }
            Class::AlignPosition { one_attachment, position, magnitude_mode, relative_to, max_force, max_axes_force, max_velocity, responsiveness, at_com, reaction, rigid } => {
                let pb = if *one_attachment { *position } else { b.pos };
                if let Kind::Achieve(c) = &mut self.conns[0].kind {
                    c.p_a = a.pos;
                    c.p_b = pb; // slot 11: OneAttachment → Position (world)
                    c.at_com = *at_com; // slot 0
                    c.max_speed = *max_velocity; // slot 9
                    let (k, dd) = kd(*responsiveness); // slot 13
                    c.k = k;
                    c.d = dd;
                    c.line = *magnitude_mode; // slot 3: Magnitude → = 1
                    if *magnitude_mode {
                        c.max_force = [*max_force; 3];
                    } else {
                        c.max_force = max_axes_force.to_array();
                        c.frame = *relative_to;
                        c.basis = match relative_to {
                            0 => a.rot,
                            1 => b.rot,
                            _ => Mat3::IDENTITY,
                        };
                    }
                }
                if let Kind::BallInSocket { p_a, p_b, .. } = &mut self.conns[1].kind {
                    *p_a = a.pos;
                    *p_b = pb;
                }
                // slot 12: ReactionForceEnabled → conn bit 1 of both
                for c in &mut self.conns {
                    c.no_reaction = !*reaction;
                }
                // slot 15: RigidityEnabled swaps AchievePosition for BallInSocket
                active = if *rigid { 1 << 1 } else { 1 };
            }
            Class::AlignOrientation { one_attachment, target, align_type, primary_only, max_ang_vel, max_torque, responsiveness, reaction, rigid } => {
                // OneAttachment: B = world, B's axes = the target frame's (I.11 has no
                // OneAttachment row: inferred like AlignPosition's, JOINT-NOTES)
                let (b1, b2) = if *one_attachment { (target.x_axis, target.y_axis) } else { (b.x(), b.y()) };
                let (k, dd) = kd(*responsiveness); // slot 12
                let servo = !*rigid; // slot 14
                if let Kind::Orient(o) = &mut self.conns[0].kind {
                    o.a1 = a.x();
                    o.a2 = a.y();
                    o.b1 = b1;
                    o.b2 = b2;
                    o.servo = servo;
                    o.max_speed = *max_ang_vel;
                    o.max_torque = clamp_f(*max_torque);
                    o.k = k;
                    o.d = dd;
                }
                if let Kind::Align2Axes(c) = &mut self.conns[1].kind {
                    c.axis_a = a.x();
                    c.axis_b = b1;
                    // [J3] Studio: s1 / s2 = Studio(B axis)
                    (c.s1_b, c.s2_b) = d::perp_pair_b(b1);
                    c.servo = servo;
                    c.max_speed = *max_ang_vel;
                    c.max_torque = clamp_f(*max_torque);
                    c.k = k;
                    c.d = dd;
                }
                if let Kind::Perp2Axes(p) = &mut self.conns[2].kind {
                    p.axis_a = a.x();
                    p.axis_b = b1;
                    p.servo = servo;
                    p.max_speed = *max_ang_vel;
                    p.max_torque = clamp_f(*max_torque);
                    p.k = k;
                    p.d = dd;
                }
                for c in &mut self.conns {
                    c.no_reaction = !*reaction; // slot 11
                }
                // which one (slot 0, Studio): legacy pair for AlignType 0/1, current API 2..5
                active = match (align_type, primary_only) {
                    (0, false) | (1, false) | (5, _) => 1,
                    (0, true) | (2, _) => 1 << 1,
                    (1, true) | (3, _) => 1 << 2,
                    _ => {
                        crate::todo_spec!("JOINT Open 11: AlignAxisToChord's helper  (AlignType PrimaryAxisLookAt) — no rows");
                        0
                    }
                };
            }
            Class::AngularVelocity { ang_vel, max_torque, relative_to, reaction } => {
                if let Kind::BodyAngVel(f) = &mut self.conns[0].kind {
                    f.target = *ang_vel; // slot 0
                    f.hi = Vec3::splat(*max_torque); // slot 1
                    f.lo = Vec3::splat(-*max_torque);
                    f.frame = *relative_to; // slot 4
                    f.basis = match relative_to {
                        0 => a.rot,
                        1 => b.rot,
                        _ => Mat3::IDENTITY,
                    };
                    f.legacy = false;
                }
                self.conns[0].no_reaction = !*reaction; // slot 2
                active = 1;
            }
            Class::LinearVelocity { mode, relative_to, force_limit_mode, limits, max_force, max_axes_force, vector_velocity, line_direction, line_velocity, reaction, plane_velocity, max_planar_axes_force, primary_tangent, secondary_tangent } => {
                // [J2] apply table Studio: slot 0 ForceLimitMode (Studio → Plane Studio,
                // Vector Studio), slot 1 ForceLimitsEnabled (Studio → bit 0 of each wrapper's
                // connector), slot 2 LineDirection (Studio: world axis, only RelativeTo World),
                // slot 3 LineVelocity (Studio: stored NEGATED in), slot 4 MaxAxesForce,
                // slot 5 MaxForce (clampF), slot 9 RelativeTo (frame Studio: 0/1/2 = A/B/world;
                // conn bit 1 cleared only for RelativeTo Attachment1 with ReactionForceEnabled),
                // slot 11 VectorVelocity (Studio: = attachment rotation (RelativeTo's) · v).
                let clamp_f = |x: f32| x.max(-3.4028235e38).min(3.4028235e38);
                let frame = (*relative_to).min(2) as u8;
                let no_reaction = !(*relative_to == 1 && *reaction);
                let att_rot = match relative_to {
                    0 => a.rot,
                    1 => b.rot,
                    _ => Mat3::IDENTITY,
                };
                if let Kind::SingleLinVel(m) = &mut self.conns[0].kind {
                    m.force_limited = *limits;
                    m.max_force = clamp_f(*max_force);
                    m.target = -*line_velocity;
                    m.frame = frame;
                    // the LineDirection setter writes the world axis only for RelativeTo World;
                    // the attachment-relative axis (frames 0 / 1) is the attachment's X (stand-in, Open 15)
                    m.axis_a = if frame == 2 { line_direction.normalize_or_zero() } else { a.x() };
                    m.axis_b = b.x();
                    m.p_a = a.pos;
                    m.p_b = b.pos;
                }
                if let Kind::TripleLinVel(m) = &mut self.conns[2].kind {
                    // Studio: Magnitude → three × MaxForce, bit 1;
                    // PerAxis → MaxAxesForce, bit 1 clear (each clampF)
                    let mut bits = if *limits { 1 } else { 0 };
                    if *force_limit_mode == 0 {
                        m.max_force = Vec3::splat(clamp_f(*max_force));
                        bits |= 2;
                    } else {
                        m.max_force = Vec3::new(clamp_f(max_axes_force.x), clamp_f(max_axes_force.y), clamp_f(max_axes_force.z));
                    }
                    m.bits = bits;
                    m.frame = frame;
                    m.target = att_rot * *vector_velocity;
                    m.p_a = a.pos;
                    m.p_b = b.pos;
                }
                if let Kind::DoubleLinVel(m) = &mut self.conns[1].kind {
                    // [J3] the Plane wrapper (), JOINT I.13 [J3]:
                    // ForceLimitMode / MaxForce / MaxPlanarAxesForce (Studio / ec620 / ec630):
                    // Magnitude → = = clampF(MaxForce), bit 1 set; PerAxis →
                    // clampF(MaxPlanarAxesForce.x / .y), bit 1 clear. ForceLimitsEnabled → bit 0
                    //. PlaneVelocity → / as is.
                    let mut bits = if *limits { 1 } else { 0 };
                    if *force_limit_mode == 0 {
                        m.max_force = [clamp_f(*max_force); 2];
                        bits |= 2;
                    } else {
                        m.max_force = [clamp_f(max_planar_axes_force[0]), clamp_f(max_planar_axes_force[1])];
                    }
                    m.bits = bits;
                    m.target = *plane_velocity;
                    // the world tangents (wrapper /): Studio
                    m.set_world_tangents(*primary_tangent, *secondary_tangent);
                    // RelativeTo: conn = Studio(v); the attachment writer
                    // Studio (main vtable slot 90 → Studio): conn = CF0.p, wrapper /
                    // = CF0 X / Y; conn = CF1.p, wrapper / = CF1 X / Y; then
                    // Studio writes the axes of the current frame, normalised, to /
                    m.frame = frame;
                    m.p_a = a.pos;
                    m.p_b = b.pos;
                    let (p, q) = match frame {
                        0 => (a.x(), a.y()),
                        1 => (b.x(), b.y()),
                        _ => (m.world_p, m.world_s),
                    };
                    m.axes = [p.normalize_or_zero(), q.normalize_or_zero()];
                }
                for c in &mut self.conns {
                    c.no_reaction = no_reaction;
                }
                if self.in_world {
                    match mode {
                        0 => active |= 1,
                        1 => active |= 1 << 1,
                        _ => active |= 1 << 2,
                    }
                }
            }
            Class::VectorForce { force, relative_to, at_com } => {
                // RelativeTo Attachment0 / 1: the force in that attachment's frame, held in
                // the body's frame so that `frame()` = the body pose rotates it
                let f = match relative_to {
                    0 => a.rot * *force,
                    1 => b.rot * *force,
                    _ => *force,
                };
                self.force = Some(Force::Linear { p_a: a.pos, p_b: b.pos, force: f, magnitude: 0.0, max_force: 0.0, at_com: *at_com, line: false, inverse_square: false, frame: *relative_to, reaction: false });
            }
            Class::Torque { torque, relative_to } => {
                let t = match relative_to {
                    0 => a.rot * *torque,
                    1 => b.rot * *torque,
                    _ => *torque,
                };
                self.force = Some(Force::Angular { torque: t, frame: *relative_to, reaction: false });
            }
            Class::LineForce { magnitude, max_force, inverse_square, at_com, reaction } => {
                self.force = Some(Force::Linear { p_a: a.pos, p_b: b.pos, force: Vec3::ZERO, magnitude: *magnitude, max_force: *max_force, at_com: *at_com, line: true, inverse_square: *inverse_square, frame: 2, reaction: *reaction });
            }
            Class::BodyVelocity { velocity, max_force, .. } => {
                if let Kind::BodyLinVel { max_force: f, target } = &mut self.conns[0].kind {
                    *target = *velocity; //
                    *f = Vec3::new(clamp_f(max_force.x), clamp_f(max_force.y), clamp_f(max_force.z)); //
                }
                if self.in_world && a.body.is_some() {
                    active = 1;
                }
            }
            Class::BodyForce { force } => {
                // world force at the centre of mass, on the body's own solve tick
                self.force = Some(Force::Linear { p_a: Vec3::ZERO, p_b: Vec3::ZERO, force: *force, magnitude: 0.0, max_force: 0.0, at_com: true, line: false, inverse_square: false, frame: 2, reaction: false });
            }
            Class::BodyThrust { force, location } => {
                // Studio: the part's frame (a = the part in its body's frame)
                self.force = Some(Force::Linear { p_a: a.pos + a.rot * *location, p_b: Vec3::ZERO, force: a.rot * *force, magnitude: 0.0, max_force: 0.0, at_com: false, line: false, inverse_square: false, frame: 0, reaction: false });
            }
            Class::BodyAngularVelocity { ang_vel, max_torque, .. } => {
                if let Kind::LegacyGyro { target, hi, lo, use_current } = &mut self.conns[0].kind {
                    *target = Vec3::new(clamp_f(ang_vel.x), clamp_f(ang_vel.y), clamp_f(ang_vel.z)); //
                    *hi = Vec3::new(clamp_f(max_torque.x), clamp_f(max_torque.y), clamp_f(max_torque.z)); //
                    *lo = Vec3::new(clamp_f(-max_torque.x), clamp_f(-max_torque.y), clamp_f(-max_torque.z)); //
                    *use_current = true; // = 1
                }
                if self.in_world && a.body.is_some() {
                    active = 1;
                }
            }
            Class::BodyPosition { position, max_force, p, d } => {
                if let Kind::LinearSpring(c) = &mut self.conns[0].kind {
                    // Studio: = clampF(MaxForce ×3); K / D ( /) = P / D
                    // through Studio(&P, &D, world dt); the points per step (from the pose, in
                    // the row build): A = the centre of mass, B = Position + (c − p_part)
                    c.max_force = Vec3::new(clamp_f(max_force.x), clamp_f(max_force.y), clamp_f(max_force.z));
                    let (k, dd) = d::mover_kd(*p, *d, d::MOVER_DT);
                    c.k = k;
                    c.d = dd;
                    c.target = *position;
                    c.part = a.pos;
                }
                if self.in_world && a.body.is_some() {
                    active = 1;
                }
            }
            Class::BodyGyro { cframe, max_torque, p, d } => {
                if let Kind::BodyAngVel(b) = &mut self.conns[0].kind {
                    if let Some(g) = b.gyro.as_mut() {
                        g.target = *cframe;
                        g.max_torque = *max_torque;
                        g.p = *p;
                        g.d = *d;
                        g.part_rot = a.rot;
                    }
                }
                if self.in_world && a.body.is_some() {
                    active = 1;
                }
            }
            Class::LegacyMover => {
                crate::todo_spec!("JOINT Open 6: legacy body movers' parameter updates (slot 82s) and force functions (vt slot 78) — no rows, no force");
            }
            Class::Fixed => {
                if let Kind::BallInSocket { p_a, p_b, .. } = &mut self.conns[0].kind {
                    *p_a = a.pos;
                    *p_b = b.pos;
                }
                if let Kind::Orient(o) = &mut self.conns[1].kind {
                    o.servo = false;
                    o.a1 = a.x();
                    o.a2 = a.y();
                    o.b1 = b.x();
                    o.b2 = b.y();
                }
                active = 0b11;
            }
        }
        // the mode setters' conn (RigidityEnabled → servo flags, [J2])
        for c in &mut self.conns {
            c.refresh_mask();
        }
        // first registration (ORDER §1): keys in the order the connectors enter the world
        if self.in_world {
            for (i, c) in self.conns.iter_mut().enumerate() {
                if active & (1 << i) != 0 && c.key == 0 {
                    *next += 1;
                    c.key = *next;
                }
            }
        }
        self.active = active;
    }
}

/// The world frame of an attachment end: `body` gives a body's pose (rotation, origin);
/// an end without a body is already in world space.
fn world_end(e: &End, body: &dyn Fn(rp::RigidBodyHandle) -> Option<(Mat3, Vec3)>) -> (Mat3, Vec3) {
    match e.body.and_then(|h| body(h)) {
        Some((r, t)) => (r * e.rot, r * e.pos + t),
        None => (e.rot, e.pos),
    }
}

/// Studio = 1e-6, Studio = 1e-12 (squared lengths), Studio = 1e-4 (squared
/// distances), Studio = 0.01 (studs), Studio = 0.1 (degrees).
const EPS: f32 = 1e-6;
const EPS2: f32 = 1e-12;
const POS2: f32 = 1e-4;
const LIN: f32 = 0.01;
const DEG: f32 = 0.1;

/// rad → deg as Studio does (·180 / π, .rdata Studio).
fn deg(x: f32) -> f32 {
    x * 180.0 / std::f32::consts::PI
}

/// Angle between two directions: atan2(|a × b|, a · b).
fn angle(a: Vec3, b: Vec3) -> f32 {
    a.cross(b).length().atan2(a.dot(b))
}

/// The servo test of Hinge Studio / Cylindrical Studio: θ = atan2((a × b)·axis, a·b);
/// the target (degrees) wrapped to [−π, π) by floorf((t + π) / 2π) (.rdata Studio = 2π);
/// not at target when |deg(θ − t')| > 0.1.
fn servo_off(a: Vec3, b: Vec3, axis: Vec3, target_deg: f32) -> bool {
    let th = a.cross(b).dot(axis).atan2(a.dot(b));
    let t = target_deg * std::f32::consts::PI / 180.0;
    let tw = t - ((t + std::f32::consts::PI) / std::f32::consts::TAU).floor() * std::f32::consts::TAU;
    deg(th - tw).abs() > DEG
}

impl Inst {
    /// SLEEP §2.1 [S2]: the joint edge's vtable slot 15 (Studio calls `e.vtbl[0x78]`).
    /// A constraint's edge is its kernel joint KJ (Inst, ctor Studio, vtable
    /// Studio) whose slot 15 Studio forwards to KJ = Inst (set by
    /// Studio from Studio) → that sub-object's slot 1, one per class; a legacy
    /// mover's edge is its sub-object, slot 15 its own. True = "actuated and not at
    /// its target": the body is kept awake while its own history is not quiet.
    /// Inst (the "attachments resolved" byte, Studio) is `in_world` here.
    pub fn edge_actuated(&self, body: &dyn Fn(rp::RigidBodyHandle) -> Option<(Mat3, Vec3)>) -> bool {
        let Some(c) = &self.props else { return false };
        let nz = |x: f32| x.abs() >= EPS;
        match c {
            // Studio. ActuatorType Inst: 1 Motor, 2 Servo.
            Class::Hinge { actuator, ang_resp, ang_speed, ang_vel, motor_max_acc, motor_max_torque, servo_max_torque, target, .. } => {
                if !self.in_world {
                    return false;
                }
                match actuator {
                    1 => nz(*ang_vel) && nz(*motor_max_acc) && nz(*motor_max_torque),
                    2 if nz(*ang_speed) && nz(*servo_max_torque) && nz(*ang_resp) => {
                        let (r0, _) = world_end(&self.a, body);
                        let (r1, _) = world_end(&self.b, body);
                        // a, b = the attachments' secondary axes,
                        // axis = Attachment0's primary
                        servo_off(r0.y_axis, r1.y_axis, r0.x_axis, *target)
                    }
                    _ => false,
                }
            }
            // Studio (Prismatic, SlidingBall); Cylindrical Studio = it, then the angular part.
            Class::Sliding { actuator, lin_resp, motor_max_acc, motor_max_force, servo_max_force, speed, target, velocity, ang, .. } => {
                if !self.in_world {
                    return false;
                }
                let (r0, p0) = world_end(&self.a, body);
                let (r1, p1) = world_end(&self.b, body);
                let lin = match actuator {
                    1 => nz(*velocity) && nz(*motor_max_acc) && nz(*motor_max_force),
                    // (p1 − p0)·X0 against TargetPosition (Inst), 0.01 studs
                    2 if nz(*speed) && nz(*servo_max_force) && nz(*lin_resp) => ((p1 - p0).dot(r0.x_axis) - *target).abs() > LIN,
                    _ => false,
                };
                if lin {
                    return true;
                }
                let Some(g) = ang else { return false };
                match g.actuator {
                    1 => nz(g.velocity) && nz(g.motor_max_acc) && nz(g.motor_max_torque),
                    2 if nz(g.speed) && nz(g.servo_max_torque) && nz(g.resp) => {
                        // a = Studio: Y0·cos φ − X0·sin φ (φ = InclinationAngle, Inst);
                        // b = Attachment1's secondary axis. The axis Studio(Inst) is not
                        // read: taken as the inclined rotation axis X0·cos φ + Y0·sin φ (lab J6).
                        let f = rad(g.inclination);
                        let a = r0.y_axis * f.cos() - r0.x_axis * f.sin();
                        let axis = r0.x_axis * f.cos() + r0.y_axis * f.sin();
                        servo_off(a, r1.y_axis, axis, g.target)
                    }
                    _ => false,
                }
            }
            // Studio.
            Class::AlignPosition { one_attachment, position, magnitude_mode, max_force, max_axes_force, max_velocity, responsiveness, rigid, .. } => {
                if !self.in_world || *rigid || *max_velocity < EPS || *responsiveness < EPS {
                    return false;
                }
                if *magnitude_mode && *max_force < EPS {
                    return false;
                }
                if !*magnitude_mode && max_axes_force.length_squared() < EPS2 {
                    return false;
                }
                let (_, p0) = world_end(&self.a, body);
                let goal = if *one_attachment { *position } else { world_end(&self.b, body).1 };
                (p0 - goal).length_squared() > POS2
            }
            // Studio.
            Class::AlignOrientation { one_attachment, target, align_type, primary_only, max_ang_vel, max_torque, responsiveness, rigid, .. } => {
                if !self.in_world || *rigid || *max_ang_vel < EPS || *responsiveness < EPS || *max_torque < EPS {
                    return false;
                }
                let (r0, _) = world_end(&self.a, body);
                let (xg, yg) = if *one_attachment {
                    (target.x_axis, target.y_axis)
                } else {
                    let (r1, _) = world_end(&self.b, body);
                    (r1.x_axis, r1.y_axis)
                };
                if *align_type == 4 {
                    // PrimaryAxisLookAt: angle(X0, LookAtPosition − p0) > 0.1°; the port keeps
                    // no LookAt rows (JOINTS-3 Open) nor its position: not tested.
                    return false;
                }
                let tx = deg(angle(r0.x_axis, xg)).abs();
                if *align_type == 3 || (*primary_only && *align_type == 1) {
                    // 90° (.rdata Studio)
                    return (tx - 90.0).abs() > DEG;
                }
                if tx > DEG {
                    return true;
                }
                *align_type == 5 && deg(angle(r0.y_axis, yg)).abs() > DEG
            }
            // Studio: no target test, no in-world test.
            Class::AngularVelocity { ang_vel, max_torque, .. } => ang_vel.length_squared() >= EPS2 && max_torque.abs() >= EPS,
            // Studio (wrapper getters Studio read by their use): a
            // velocity actuator has no target test.
            Class::LinearVelocity { mode, force_limit_mode, limits, max_force, max_axes_force, vector_velocity, line_velocity, plane_velocity, max_planar_axes_force, .. } => match mode {
                0 => line_velocity.abs() >= EPS && (!*limits || *max_force >= EPS),
                1 => {
                    if plane_velocity[0] < EPS && plane_velocity[1] < EPS {
                        return false;
                    }
                    !*limits || if *force_limit_mode == 0 { *max_force >= EPS } else { max_planar_axes_force[0] >= EPS || max_planar_axes_force[1] >= EPS }
                }
                2 => vector_velocity.length_squared() >= EPS2 && (!*limits || if *force_limit_mode == 0 { *max_force >= EPS } else { max_axes_force.length_squared() >= EPS2 }),
                _ => true,
            },
            // Studio: the winch, against WinchTarget (Inst), 0.01 studs.
            Class::Rope { winch, winch_force, winch_speed, winch_target, .. } => {
                if !*winch || !self.in_world || *winch_force < EPS || *winch_speed < EPS {
                    return false;
                }
                let (_, p0) = world_end(&self.a, body);
                let (_, p1) = world_end(&self.b, body);
                ((p0 - p1).length() - *winch_target).abs() > LIN
            }
            // Legacy movers, edge = mover; `a` = the part's frame in its body.
            // BodyGyro Studio: MaxTorque (), P (); u = R_partᵀ·(goal Y),
            // θ1 = atan2(√(ux² + uz²), uy); then v = R_partᵀ·(−goal Z), θ2 = atan2(√(vx² + vy²), −vz).
            Class::BodyGyro { cframe, max_torque, p, .. } => {
                if max_torque.length_squared() < EPS2 || *p < EPS {
                    return false;
                }
                let (r, _) = world_end(&self.a, body);
                let u = r.transpose() * cframe.y_axis;
                if deg(Vec3::new(u.x, 0.0, u.z).length().atan2(u.y)).abs() > DEG {
                    return true;
                }
                let v = r.transpose() * -cframe.z_axis;
                deg(Vec3::new(v.x, v.y, 0.0).length().atan2(-v.z)).abs() > DEG
            }
            // BodyPosition Studio: MaxForce (), P (); |p_part − Position|² > 1e-4.
            Class::BodyPosition { position, max_force, p, .. } => {
                if max_force.length_squared() < EPS2 || *p < EPS {
                    return false;
                }
                let (_, pp) = world_end(&self.a, body);
                (pp - *position).length_squared() > POS2
            }
            // BodyVelocity Studio / BodyAngularVelocity Studio: no target test.
            Class::BodyVelocity { velocity, max_force, p } => velocity.length_squared() >= EPS2 && max_force.length_squared() >= EPS2 && *p >= EPS,
            Class::BodyAngularVelocity { ang_vel, max_torque, p } => ang_vel.length_squared() >= EPS2 && max_torque.length_squared() >= EPS2 && *p >= EPS,
            // BodyForce / BodyThrust / Rocket: base slot Studio with KJ = 0; BallSocket,
            // Universal, Plane, Rod, Spring, TorsionSpring, VectorForce, Torque, LineForce, Rigid:
            // Studio (false).
            _ => false,
        }
    }
}

#[cfg(test)]
mod sleep_tests {
    use super::*;

    #[test]
    fn servo_at_target_wraps() {
        // Y0 = y, Y1 turned +30° about X0 = x: θ = 30°
        let (a, axis) = (Vec3::Y, Vec3::X);
        let b = Quat::from_axis_angle(Vec3::X, 30f32.to_radians()) * Vec3::Y;
        assert!(!servo_off(a, b, axis, 30.0));
        assert!(!servo_off(a, b, axis, 30.0 + 360.0));
        assert!(servo_off(a, b, axis, 30.2));
        assert!(servo_off(a, b, axis, -30.0));
    }
}
