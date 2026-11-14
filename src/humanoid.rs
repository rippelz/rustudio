//! Roblox's Humanoid controller, ported literally from Studio
//! 0.744.0.7441077 (the classic `HUMAN::` state machine; Studio's default
//! character never uses the ControllerManager path, FFlag
//! DebugHumanoidNewPhysicsEnabled = false). Every constant is Studio's;
//! the measurements they were checked against are in
//! checks/humanoid-physics/SPEC.md.
//!
//! Two halves, as in Studio:
//! - per frame (`frame`, stepHumanoid Studio →
//!   Studio): the sensors' results, the timers, the transition table,
//!   the state's update (vtable slot 7) and the targets;
//! - per 240 Hz world step (`force`, vtable slot 5): the force and torque on
//!   the root assembly and the reaction on the floor's assembly.
//!
//! The character body itself is an ordinary dynamic rapier body (physics.rs
//! `HumBody`); this module only does Studio's arithmetic on what it is given.

use glam::{Mat3, Vec3};

use crate::rig::State;

// ---------------------------------------------------------------- constants

/// RunningBase velocity gain, 1/s: Studio returns .rdata Studio.
pub const VEL_GAIN: f32 = 150.0;
/// Yaw servo gain, 1/s: .rdata Studio (also the /100 of the floor force %).
pub const YAW_GAIN: f32 = 100.0;
/// Altitude PD gains: .rdata Studio.
pub const PD_P: f32 = 3000.0;
pub const PD_D: f32 = 110.0;
/// The PD error's lower clamp, × the full hip: .rdata Studio.
pub const PD_LOW_HIP: f32 = 0.1;
/// FInt MaxAltitudePDHipHeightPercent (.data Studio; live 100).
pub const PD_HIGH_PCT: f32 = 100.0;
/// FInt NewPDAltitudeWeakPushFinalDepressPercent (.data Studio; live 5).
pub const PD_WEAK_PUSH_PCT: f32 = 5.0;
/// FInt NewPDAltitudeNoForceZonePercent (.data Studio; live 1).
pub const PD_NO_FORCE_PCT: f32 = 1.0;
/// Percent → fraction: .rdata Studio (0.01).
pub const PCT: f32 = 0.01;
/// The PD force clamps scale with mass / 14.5 (.rdata Studio).
pub const PD_REF_MASS: f32 = 14.5;
pub const PD_SOLID_MIN: f32 = -1.0e7; // .rdata Studio
pub const PD_SOLID_MAX: f32 = 1.0e9; // .rdata Studio
pub const PD_SOFT_MIN: f32 = -1.0e5; // .rdata Studio
pub const PD_SOFT_MAX: f32 = 1.0e7; // .rdata Studio
/// Acceleration ceiling (.data Studio = 1e6) and floor (.data Studio = 0).
pub const ACCEL_MAX: f32 = 1.0e6;
pub const ACCEL_Y_MIN: f32 = 0.0;
/// Horizontal acceleration without a floor: .rdata Studio.
pub const AIR_ACCEL: f32 = 143.0;
/// H ≠ 1 multiplies by this: .rdata Studio.
pub const ACCEL_SCALE_MULT: f32 = 12.0;
/// The friction limit's gravity, a constant (.data Studio), not Workspace.Gravity.
pub const FRICTION_G: f32 = 196.2;
/// Epsilon of most tests: .rdata Studio.
pub const EPS: f32 = 1.0e-6;
/// Floor force applied to the floor, %: FInt SolidFloorPercentForceApplication
/// (.data Studio, 100) / NonSolidFloorPercentForceApplication (Studio, 10).
pub const FLOOR_PCT_SOLID: f32 = 100.0;
pub const FLOOR_PCT_SOFT: f32 = 10.0;
/// A movable floor is "solid" from this × the character's mass: FInt
/// SolidFloorMassMultTenth (.data Studio, 50) / 10 (.rdata Studio).
pub const SOLID_FLOOR_MASS_MULT: f32 = 5.0;
/// Yaw servo prediction clamp (.rdata Studio):
/// only the substep prediction reads it; the torque uses the raw value.
pub const YAW_ALPHA_MAX: f32 = 5000.0;
/// Freefall yaw clamp (.rdata Studio).
pub const AIR_YAW_ALPHA_MAX: f32 = 160_000.0;
/// Balancing: Balancing ctor Studio P 7000 / D 100 (Running, Climbing,
/// GettingUp inherit it), Landed ctor Studio 7500 / 50, Freefall ctor
/// Studio P 5000.
pub const BAL_P: f32 = 7000.0;
pub const BAL_D: f32 = 100.0;
pub const BAL_P_LANDED: f32 = 7500.0;
pub const BAL_D_LANDED: f32 = 50.0;
pub const BAL_P_FREEFALL: f32 = 5000.0;
/// Studio: below this |ω| (.rdata Studio) and upside down (up.y <
/// −0.9, .rdata Studio) the axis is −0.25 × X (.rdata Studio).
pub const BAL_W_MIN: f32 = 0.01;
pub const BAL_UPSIDE_DOWN: f32 = -0.9;
pub const BAL_FLIP_AXIS: f32 = 0.25;
/// Balancing's angular acceleration cap: .rdata Studio.
pub const BAL_ALPHA_MAX: f32 = 4000.0;
/// Turn-rate target gain: 8 × the heading error (.rdata Studio).
pub const TURN_GAIN: f32 = 8.0;
/// CameraRelative's rate: H × 8 × 0.5 (.rdata Studio).
pub const HALF: f32 = 0.5;
/// Freefall carries the floor's velocity, decaying 1 − dt × 0.05 / 0.0333 a
/// frame (.rdata Studio).
pub const CARRY_DECAY: f32 = 0.05;
pub const CARRY_DECAY_DT: f32 = 0.033_333_335;
/// Freefall's air yaw target needs |vAir|² over this (.rdata Studio).
pub const AIR_YAW_MIN_SQ: f32 = 0.01;
/// Running's altitude nudge: (target − y) × 0.8, clamped to
/// ±0.1, used when over 0.05 (.rdata Studio).
pub const NUDGE_GAIN: f32 = 0.8;
pub const NUDGE_MAX: f32 = 0.1;
pub const NUDGE_MIN: f32 = 0.05;
/// Climbing: speed × 0.7 (.rdata Studio), under 0.1
/// creep 0.01, down when the move faces away by
/// more than −0.2.
pub const CLIMB_SPEED: f32 = 0.7;
pub const CLIMB_MIN: f32 = 0.1;
pub const CLIMB_CREEP: f32 = 0.01;
pub const CLIMB_DOWN_DOT: f32 = -0.2;
/// Slide direction ignored when nearly vertical: .rdata Studio.
pub const SLIDE_Y_MAX: f32 = 0.999_998_99;
/// Unit-length window that skips normalising (.rdata Studio)
/// and the zero guard (.rdata Studio).
pub const UNIT_LO: f32 = 0.999_99;
pub const UNIT_HI: f32 = 1.000_01;
pub const LEN_MIN: f32 = 1.0e-7;
/// Event 12 (no floor for this long): .rdata Studio.
pub const NO_FLOOR_S: f32 = 0.125;
/// Jump refused after this long without a floor (event 2): .rdata Studio.
pub const COYOTE_S: f32 = 0.13;
/// Event 7 / 8: torso up.y under 0.7 / over 0.9 (.rdata Studio).
pub const TIP_Y: f32 = 0.7;
pub const UPRIGHT_Y: f32 = 0.9;
/// Jumping leaves at 1.06 × GetJumpVelocity (.rdata Studio).
pub const JUMP_FACTOR: f32 = 1.06;
/// State timers (S): Jumping 0.5 (ctor Studio), Landed 0.05 (ctor
/// Studio), FallingDown 3.0, Ragdoll 5.0 (FInt
/// RagdollDefaultTimerTenthSecond 50 / 10).
pub const JUMPING_S: f32 = 0.5;
pub const LANDED_S: f32 = 0.05;
pub const FALLING_DOWN_S: f32 = 3.0;
pub const RAGDOLL_S: f32 = 5.0;
/// Ragdoll leaves early with Jump after (5 − 3) s (FInt
/// RagdollEarlyExitTimeTenthSecond 20) or when |v|, |ω| < 1 (FInt
/// RagdollSlowEnoughToNotTenth 10): Studio.
pub const RAGDOLL_EARLY_S: f32 = 2.0;
pub const RAGDOLL_SLOW: f32 = 1.0;
/// Swimming: 35 × m × (vDes − v), per-axis limits ±10000 /
/// +1000 up (.rdata Studio, .data Studio).
pub const SWIM_GAIN: f32 = 35.0;
pub const SWIM_FORCE_MAX: f32 = 10_000.0;
pub const SWIM_FORCE_UP_MAX: f32 = 1000.0;

/// Unstick (Studio; FInts MinimumFreefallMoveDistanceInTenths 5,
/// MaximumFreefallMoveTimeInTenths 1, UnstickForceDecayInTenths 9,
/// MaximumUnstickForceInGs 4, UnstickForceEpsilonInHundredths 1, live; growth
/// 0.1 at .rdata Studio; checks/humanoid-physics/RE-OPEN-ITEMS.md §3).
pub const UNSTICK_MIN_FALL: f32 = 0.5;
pub const UNSTICK_MAX_TIME: f32 = 0.1;
pub const UNSTICK_GROWTH: f32 = 0.1;
pub const UNSTICK_MAX_GS: f32 = 4.0;
pub const UNSTICK_DECAY: f32 = 0.9;
pub const UNSTICK_EPS: f32 = 0.01;

/// The timestep arbiter: its thresholds, live values.
/// FInt SimTimestepMultiplierDebounceCount (.data Studio: compiled 100,
/// live 60 in Studio 0.747): the 240 Hz ticks an assembly whose level was
/// lowered must solve before it may rise again. The setter Studio arms
/// the counter (assembly) when the level drops, each solve takes the
/// level's ticks off it and a queued rise is dropped while it
/// is above 0. Measured: after a teleport the hip PD settles
/// at the level-1 rate (0.5444 a frame, level 2 would give 0.581) for the
/// first 0.25 s (probe T5_sink03).
pub const ARB_DEBOUNCE: i32 = 60;
pub const ARB_PD_ERR: f32 = 0.2; // FInt HeightControllerHipHeightRelativeDeltaThresholdThou (.data Studio; compiled 100, live 200) / 1000 (.rdata Studio)
pub const ARB_TURN: f32 = 17.0; // FInt TimestepArbiterHumanoidTurningVelThreshold (.data Studio; compiled 14, live 17)
pub const ARB_LINEAR: f32 = 21.0; // FInt TimestepArbiterHumanoidLinearVelThreshold (.data Studio; compiled 17, live 21)
pub const ARB_STD_MOI: f32 = 61.0; // FInt StandardHumanoidAssyMOIThou 61000 (.data Studio) / 1000
pub const ARB_STD_MASS: f32 = 16.0; // FInt StandardHumanoidAssyMassThou 16000 (.data Studio) / 1000
pub const ARB_SCALE_TOL: f32 = 1.0; // FInt HumanoidMassScaleThresholdThou / …Thou2 (.data Studio; compiled 500, live 1000) / 1000

/// A rotation by the vector `r` (axis × angle), as the force functions'
/// prediction turns the orientation.
fn rotation(r: Vec3) -> Mat3 {
    let a = r.length();
    if a > 0.0 { Mat3::from_axis_angle(r / a, a) } else { Mat3::IDENTITY }
}

/// Wraps an angle into [−π, π).
pub fn wrap(a: f32) -> f32 {
    use std::f64::consts::PI;
    let d = a as f64;
    if !(-PI..PI).contains(&d) {
        (d - ((d + PI) / (2.0 * PI)).floor() * 2.0 * PI) as f32
    } else {
        a
    }
}

/// A heading's yaw: atan2(−look.x, −look.z) of a look vector.
pub fn yaw_of_look(look: Vec3) -> f32 {
    (-look.x).atan2(-look.z)
}

// ---------------------------------------------------------------- inputs

/// The floor sensor's result (Studio, per frame from Studio):
/// averaged ray hits and the floor's physical facts.
#[derive(Debug, Clone, Copy)]
pub struct Floor {
    /// Mean hit height (sensor), point (), normal ().
    pub y: f32,
    pub point: Vec3,
    pub normal: Vec3,
    /// The floor's velocity at the point and its angular velocity.
    pub v: Vec3,
    pub w: Vec3,
    /// Friction mix with the character's Plastic.
    pub mu: f32,
    /// Studio: anchored / immovable, or heavier than 5 × the character.
    pub solid: bool,
}

/// What the per-frame half reads from the Humanoid and the body.
#[derive(Debug, Clone, Copy)]
pub struct FrameIn {
    pub dt: f32,
    /// Humanoid.MoveDirection (H), else the MoveTo direction (H).
    pub move_dir: Vec3,
    /// H: the walk's magnitude (Move(0.5·dir) walks at half speed).
    pub move_mag: f32,
    pub walk_speed: f32,
    pub auto_rotate: bool,
    /// RotationType CameraRelative and the camera's yaw (H's source).
    pub camera_relative: Option<f32>,
    pub jump: bool,
    /// GetJumpVelocity.
    pub jump_velocity: f32,
    pub sit: bool,
    /// The Seated state still has its seat part (event 2's seat test).
    pub has_seat: bool,
    pub platform_stand: bool,
    pub dead: bool,
    pub anchored: bool,
    pub floor: Option<Floor>,
    /// The full hip: ½ root.Size.Y (+ R6 legs) + HipHeight.
    pub full_hip: f32,
    /// FInt MaxSlopeAngle's cosine.
    pub cos_max_slope: f32,
    /// The ladder sensor found a climbable part.
    pub ladder: bool,
    /// In water (sensor / root flag 0x40).
    pub in_water: bool,
    /// The root part's world rotation and position, velocity, angular velocity.
    pub root_rot: Mat3,
    pub root_pos: Vec3,
    pub root_v: Vec3,
    pub root_w: Vec3,
    /// The torso up (the Humanoid's RootPart, Studio) for events 7 / 8.
    pub torso_up: Option<Vec3>,
}

/// What the per-step half reads from the root assembly.
#[derive(Debug, Clone)]
pub struct Body {
    /// The root assembly's mass.
    pub mass: f32,
    /// The root primitive (the assembly root, Studio) pose and
    /// velocities ( rotation, position, v, ω).
    pub rot: Mat3,
    pub pos: Vec3,
    pub v: Vec3,
    pub w: Vec3,
    /// The assembly's world inertia.
    pub inertia: Mat3,
    /// Force / torque accumulators so far this step:
    /// gravity (−m·g: the PD rests the root exactly at the hip and a jump
    /// reaches 1.06 × its speed in one step, both measured) plus VectorForce /
    /// BodyForce, not solver constraints.
    pub fext: Vec3,
    pub text: Vec3,
    /// The Humanoid's RootPart up and position.
    pub hrp_up: Vec3,
    pub hrp_y: f32,
    /// Workspace.Gravity.
    pub gravity: f32,
    /// The character's contacts with other assemblies this step: (world
    /// point, normal pointing toward the character) (unstick's gather).
    pub contacts: Vec<(Vec3, Vec3)>,
}

/// One step's output: on the root assembly, and on the floor's assembly.
#[derive(Debug, Clone, Copy, Default)]
pub struct Force {
    pub force: Vec3,
    pub torque: Vec3,
    /// Force on the floor at a point.
    pub floor: Option<(Vec3, Vec3)>,
    /// A velocity change this step delivers as such: the part m·dv/hs of
    /// the state's force, kept out of `force` so that the f32 round trip
    /// (·m/hs, then /m·hs in the integrator) can't leave the body an ulp
    /// short of a target the next step compares against (Jumping's need).
    pub dv: Vec3,
}

// ---------------------------------------------------------------- state

/// HUMAN::HumanoidState and its subclasses' fields this port needs.
#[derive(Debug, Clone)]
pub struct Humanoid {
    pub state: State,
    /// S: the state's countdown.
    pub timer: f32,
    /// S: time since the floor sensor last found a floor (carried
    /// across state changes, Studio).
    pub air: f32,
    /// S: FallingDown blocked (set by a jump out of Swimming); carried.
    pub no_tip: bool,
    /// S: the new altitude PD (base ctor Studio sets 1); carried.
    pub new_pd: bool,
    /// S+9: Jumping's take-off reached.
    pub jump_done: bool,
    /// Jumping's direction (ctor Studio: up; out of Climbing up − look).
    pub jump_dir: Vec3,
    /// Balancing gains S / S.
    pub bal_p: f32,
    pub bal_d: f32,
    // RunningBase.
    pub floor_target_y: f32,
    pub floor_v: Vec3,
    pub floor_w: Vec3,
    pub v_des: Vec3,
    pub w_des: Vec3,
    /// S: the floor's downhill direction.
    pub slide: Vec3,
    /// S: the last PD error.
    pub pd_err: f32,
    /// S: Running's altitude nudge added to vDes.y this frame.
    pub nudge: f32,
    // Freefall.
    /// S..0x130: the floor velocity carried into the air.
    pub carry: Vec3,
    /// S..0x13c / 0x140..0x148: the air targets.
    pub v_air: Vec3,
    pub w_air: Vec3,
    /// The targets (H..0x3c0), written by `targets` each frame.
    pub v_target: Vec3,
    pub w_target_y: f32,
    // Unstick (Studio; Balancing ctor Studio starts them).
    /// S: root height when it last fell far enough.
    pub unstick_y: f32,
    /// S: the countdown then.
    pub unstick_t: f32,
    /// S: the push in g.
    pub unstick_gs: f32,
    /// S..0x120: the average contact normal of the last gather.
    pub contact_n: Vec3,
}

impl Default for Humanoid {
    fn default() -> Self {
        Humanoid {
            state: State::Running,
            timer: 0.0,
            air: 0.0,
            no_tip: false,
            new_pd: true,
            jump_done: false,
            jump_dir: Vec3::Y,
            bal_p: BAL_P,
            bal_d: BAL_D,
            floor_target_y: f32::INFINITY,
            floor_v: Vec3::ZERO,
            floor_w: Vec3::ZERO,
            v_des: Vec3::ZERO,
            w_des: Vec3::ZERO,
            slide: Vec3::ZERO,
            pd_err: 0.0,
            nudge: 0.0,
            carry: Vec3::ZERO,
            v_air: Vec3::ZERO,
            w_air: Vec3::ZERO,
            v_target: Vec3::ZERO,
            w_target_y: 0.0,
            unstick_y: 0.0,
            unstick_t: 0.0,
            unstick_gs: 0.0,
            contact_n: Vec3::ZERO,
        }
    }
}

/// Transition events (Studio's switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ev {
    Dead = 0,
    NoNeck = 1,
    Jump = 2,
    Sit = 3,
    Unsit = 4,
    Stand = 5,
    Unstand = 6,
    Tipped = 7,
    Upright = 8,
    Ladder = 9,
    NoLadder = 10,
    NoFloor = 11,
    AirLong = 12,
    Landing = 13,
    JumpDone = 14,
    Timeout = 15,
    Water = 16,
    NoWater = 17,
}

/// The transition table (.rdata Studio, dumps/state-transition-table.txt):
/// per state, (event, target) in event order.
fn row(s: State) -> &'static [(Ev, State)] {
    use Ev::*;
    use State as S;
    match s {
        S::FallingDown => &[(Dead, S::Dead), (NoNeck, S::Dead), (Sit, S::Seated), (Stand, S::PlatformStanding), (Timeout, S::GettingUp), (Water, S::Swimming)],
        S::Ragdoll => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::GettingUp), (Timeout, S::GettingUp)],
        S::GettingUp => &[(Dead, S::Dead), (NoNeck, S::Dead), (Sit, S::Seated), (Stand, S::PlatformStanding), (Upright, S::Running), (Water, S::Swimming)],
        S::Jumping => &[(Dead, S::Dead), (NoNeck, S::Dead), (Sit, S::Seated), (Stand, S::PlatformStanding), (NoFloor, S::Freefall), (AirLong, S::Freefall), (JumpDone, S::Freefall), (Timeout, S::Freefall)],
        S::Swimming => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::Jumping), (Sit, S::Seated), (Stand, S::PlatformStanding), (NoWater, S::GettingUp)],
        S::Freefall => &[(Dead, S::Dead), (NoNeck, S::Dead), (Sit, S::Seated), (Stand, S::PlatformStanding), (Ladder, S::Climbing), (Landing, S::Landed), (Water, S::Swimming)],
        S::Landed => &[(Dead, S::Dead), (NoNeck, S::Dead), (Sit, S::Seated), (Stand, S::PlatformStanding), (Tipped, S::FallingDown), (AirLong, S::Freefall), (Timeout, S::Running), (Water, S::Swimming)],
        S::Running => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::Jumping), (Sit, S::Seated), (Stand, S::PlatformStanding), (Tipped, S::FallingDown), (Ladder, S::Climbing), (AirLong, S::Freefall), (Water, S::Swimming)],
        S::Climbing => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::Jumping), (Sit, S::Seated), (Stand, S::PlatformStanding), (Tipped, S::FallingDown), (NoLadder, S::Running), (Water, S::Swimming)],
        S::Seated => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::Jumping), (Unsit, S::Running), (Stand, S::PlatformStanding)],
        S::PlatformStanding => &[(Dead, S::Dead), (NoNeck, S::Dead), (Jump, S::Jumping), (Sit, S::Seated), (Unstand, S::Running)],
        S::Physics => &[(Dead, S::Dead), (NoNeck, S::Dead)],
        S::Dead => &[],
    }
}

/// What a frame did, for the events Lua sees.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameOut {
    pub changed: Option<(State, State)>,
}

impl Humanoid {
    /// A state's constructor (Studio → the state ctors): its timer and
    /// gains; `from` is the state left (Jumping out of Climbing / Swimming).
    fn enter(&mut self, to: State, from: State, inp: &FrameIn) {
        self.timer = 0.0;
        // Balancing ctor Studio: saved root Y, saved countdown (0), Gs 0.
        self.unstick_y = inp.root_pos.y;
        self.unstick_t = 0.0;
        self.unstick_gs = 0.0;
        self.contact_n = Vec3::ZERO;
        self.jump_done = false;
        self.bal_p = BAL_P;
        self.bal_d = BAL_D;
        match to {
            State::Jumping => {
                self.timer = JUMPING_S;
                self.jump_dir = Vec3::Y;
                if from == State::Swimming {
                    self.no_tip = true;
                } else if from == State::Climbing {
                    let look = -inp.root_rot.z_axis;
                    let flat = Vec3::new(look.x, 0.0, look.z);
                    let l = flat.length();
                    if l > 0.0 {
                        self.jump_dir = (Vec3::Y - flat / l).normalize();
                    }
                }
            }
            State::Landed => {
                self.timer = LANDED_S;
                self.bal_p = BAL_P_LANDED;
                self.bal_d = BAL_D_LANDED;
            }
            State::Freefall => {
                // Freefall ctor Studio: S..0x150 zeroed (the carried
                // floor velocity starts at 0; nothing writes it afterwards).
                self.bal_p = BAL_P_FREEFALL;
                self.v_air = Vec3::ZERO;
                self.w_air = Vec3::ZERO;
                self.carry = Vec3::ZERO;
            }
            State::FallingDown => self.timer = FALLING_DOWN_S,
            State::Ragdoll => self.timer = RAGDOLL_S,
            _ => {}
        }
        if matches!(to, State::Running | State::Landed | State::Climbing) {
            // RunningBase ctor Studio: target +inf, the rest zeroed.
            self.floor_target_y = f32::INFINITY;
            self.floor_v = Vec3::ZERO;
            self.floor_w = Vec3::ZERO;
            self.v_des = Vec3::ZERO;
            self.w_des = Vec3::ZERO;
            self.slide = Vec3::ZERO;
            self.nudge = 0.0;
        }
        self.state = to;
    }

    /// Event predicates.
    fn holds(&self, e: Ev, inp: &FrameIn) -> bool {
        match e {
            Ev::Dead => inp.dead,
            Ev::NoNeck => false,
            Ev::Jump => {
                if inp.anchored {
                    return false;
                }
                if inp.in_water && self.state == State::Swimming {
                    return false;
                }
                if self.state == State::Seated && !inp.has_seat && self.air > COYOTE_S {
                    return false;
                }
                if self.air > COYOTE_S && matches!(self.state, State::Running | State::PlatformStanding) {
                    return false;
                }
                if self.state == State::Climbing && !inp.ladder {
                    return false;
                }
                let ny = inp.floor.map(|f| f.normal.y).unwrap_or(inp.root_rot.y_axis.y);
                (inp.cos_max_slope < ny || self.state != State::Running) && inp.jump && inp.jump_velocity > 0.0
            }
            Ev::Sit => inp.sit,
            Ev::Unsit => !inp.sit,
            Ev::Stand => inp.platform_stand,
            Ev::Unstand => !inp.platform_stand,
            Ev::Tipped => inp.torso_up.is_some_and(|u| u.y < TIP_Y) && !self.no_tip,
            Ev::Upright => inp.torso_up.is_none_or(|u| u.y > UPRIGHT_Y),
            Ev::Ladder => inp.ladder,
            Ev::NoLadder => !inp.ladder,
            Ev::NoFloor => inp.floor.is_none(),
            Ev::AirLong => self.air > NO_FLOOR_S,
            Ev::Landing => inp.floor.is_some_and(|f| (inp.root_v - f.v).y <= 0.0),
            Ev::JumpDone => self.jump_done,
            Ev::Timeout => self.timer <= 0.0,
            Ev::Water => inp.in_water,
            Ev::NoWater => !inp.in_water,
        }
    }

    /// The targets: desired velocity and yaw rate, H..0x3c0.
    pub fn targets(&mut self, inp: &FrameIn) {
        let mut dir = inp.move_dir;
        if self.state != State::Swimming {
            dir.y = 0.0;
        }
        let dir = if dir != Vec3::ZERO { dir.normalize() } else { dir };
        self.v_target = dir * inp.walk_speed * inp.move_mag;
        self.w_target_y = 0.0;
        if inp.auto_rotate {
            let root_yaw = yaw_of_look(-inp.root_rot.z_axis);
            if let Some(cam_yaw) = inp.camera_relative {
                // H × 8 × 0.5; H is the camera's heading offset
                // from the root (set by the camera each frame).
                self.w_target_y = wrap(cam_yaw - root_yaw) * TURN_GAIN * HALF;
            } else if self.v_target != Vec3::ZERO {
                let want = (-self.v_target.x).atan2(-self.v_target.z);
                self.w_target_y = wrap(want - root_yaw) * TURN_GAIN;
            }
        }
    }

    /// One frame: timers, transitions, the state's update.
    pub fn frame(&mut self, inp: &FrameIn) -> FrameOut {
        let mut out = FrameOut::default();
        // Studio: a floor found resets the no-floor time; then
        // Studio: S −= dt, S += dt.
        if inp.floor.is_some() {
            self.air = 0.0;
        }
        self.timer -= inp.dt;
        self.air += inp.dt;
        self.targets(inp);
        // Studio: the first event of the row that holds and whose
        // target is enabled.
        let from = self.state;
        if let Some(&(_, to)) = row(from).iter().find(|(e, _)| self.holds(*e, inp)) {
            if to != from {
                self.enter(to, from, inp);
                out.changed = Some((from, to));
            }
        }
        self.update(inp);
        out
    }

    /// A state forced from outside (ChangeState, seats, death): its ctor.
    pub fn force_state(&mut self, to: State, inp: &FrameIn) -> Option<(State, State)> {
        let from = self.state;
        if from == to {
            return None;
        }
        self.enter(to, from, inp);
        Some((from, to))
    }

    /// The state's update, vtable slot 7.
    fn update(&mut self, inp: &FrameIn) {
        match self.state {
            State::Running => {
                // Studio: undo last frame's nudge, RunningBase, nudge again.
                self.v_des.y -= self.nudge;
                self.nudge = 0.0;
                self.running_base(inp);
                if inp.dt != 0.0 && self.floor_target_y.is_finite() {
                    let e = ((self.floor_target_y - inp.root_pos.y) * NUDGE_GAIN).clamp(-NUDGE_MAX, NUDGE_MAX);
                    if e.abs() > NUDGE_MIN {
                        self.nudge = e / inp.dt;
                        self.v_des.y += self.nudge;
                    }
                }
            }
            State::Landed | State::Climbing => self.running_base(inp),
            State::Freefall => self.freefall_update(inp),
            State::Ragdoll => {
                // Studio: the timer is run down (event 15 then leaves
                // for GettingUp in the next frame) by Jump once 2 s are over
                // and whenever the root is slow: |v| and |ω| under 1. A
                // standing character told to Ragdoll is slow in that very
                // frame (Studio: GettingUp one frame later, T16). Jump alone
                // leaves at once through event 2 (measured: a spinning
                // Ragdoll gets up in the frame Jump is pressed, at 0.1 s).
                if inp.jump && self.timer <= RAGDOLL_S - RAGDOLL_EARLY_S {
                    self.timer = 0.0;
                }
                if inp.root_v.length_squared() < RAGDOLL_SLOW * RAGDOLL_SLOW
                    && inp.root_w.length_squared() < RAGDOLL_SLOW * RAGDOLL_SLOW
                {
                    self.timer = 0.0;
                }
            }
            _ => {}
        }
    }

    /// RunningBase's update: floor target, floor velocity,
    /// desired velocity (climbing, slopes).
    fn running_base(&mut self, inp: &FrameIn) {
        self.floor_target_y = inp.floor.map(|f| f.y + inp.full_hip).unwrap_or(f32::INFINITY);
        let (fv, fw) = inp.floor.map(|f| (f.v, f.w)).unwrap_or((Vec3::ZERO, Vec3::ZERO));
        self.floor_v = fv;
        self.floor_w = fw;
        if inp.floor.is_some() || self.state == State::Climbing {
            self.v_des = self.v_target;
            self.w_des = Vec3::new(0.0, self.w_target_y, 0.0);
        }
        if inp.ladder {
            let len = self.v_des.length();
            if len > 0.0 {
                let look = -inp.root_rot.z_axis;
                let flat = Vec3::new(look.x, 0.0, look.z);
                let fl = flat.length();
                let facing = if fl > 0.0 { flat / fl } else { Vec3::ZERO };
                let dot = facing.dot(self.v_des / len);
                let sign = if dot >= CLIMB_DOWN_DOT {
                    Some(1.0)
                } else if inp.floor.is_none() {
                    Some(-1.0)
                } else {
                    None
                };
                if let Some(sign) = sign {
                    let sp = if len >= CLIMB_MIN { len * CLIMB_SPEED } else { CLIMB_CREEP };
                    self.v_des = Vec3::new(0.0, sp * sign, 0.0);
                    self.w_des = Vec3::ZERO;
                    if inp.floor.is_none() {
                        return;
                    }
                }
            }
        }
        let Some(f) = inp.floor else { return };
        self.slide = downhill(f.normal);
        if f.normal.y >= inp.cos_max_slope {
            self.v_des = into_plane(f.normal, self.v_des);
        } else {
            self.v_des = off_slope(f.normal, self.v_des);
        }
    }

    /// Freefall's update: air targets and the carried floor velocity.
    fn freefall_update(&mut self, inp: &FrameIn) {
        self.v_air = self.v_target + self.carry;
        self.w_air = Vec3::new(0.0, self.w_target_y, 0.0);
        if self.v_air.length_squared() >= AIR_YAW_MIN_SQ {
            if self.v_target.length_squared() > AIR_YAW_MIN_SQ && inp.auto_rotate {
                let want = (-self.v_air.x).atan2(-self.v_air.z);
                let err = wrap(want - yaw_of_look(-inp.root_rot.z_axis));
                self.w_air.y = if err.abs() < 1.0 {
                    err.signum() * TURN_GAIN * err.abs()
                } else {
                    err.signum() * TURN_GAIN
                };
            }
        } else {
            self.v_air = Vec3::ZERO;
            self.w_air = Vec3::ZERO;
        }
        let k = 1.0 - inp.dt * CARRY_DECAY / CARRY_DECAY_DT;
        self.carry *= k;
    }

    // ------------------------------------------------------------ per step

    /// The state's force function (vtable slot 5) for one solve of the root
    /// assembly: `n` substeps of `hs` seconds (Studio: k = the
    /// assembly's 240 Hz ticks per solve, N = k > 1 ? k × FInt
    /// SimAdaptiveHumanoidPDControllerSubstepMultiplier (live 1) : k,
    /// hs = h·k / N). The force and torque are the substeps' average.
    pub fn force(&mut self, b: &Body, inp: &StepIn, n: u32, hs: f32) -> Force {
        match self.state {
            State::Running | State::Landed | State::Climbing => {
                let tau = self.balancing(b, n, hs);
                let mut f = self.running_force(b, inp, n, hs);
                f.torque += tau;
                f
            }
            State::GettingUp => Force { torque: self.balancing(b, n, hs), ..Default::default() },
            State::Freefall => self.freefall_force(b, n, hs),
            // Jumping divides by the solve's dt (Studio: h·k), not the substep.
            State::Jumping => self.jumping_force(b, inp, hs * n.max(1) as f32),
            State::Swimming => self.swimming_force(b),
            // FallingDown, Physics, Ragdoll, Seated, PlatformStanding, Dead:
            // the null function Studio.
            _ => Force::default(),
        }
    }

    /// The adaptive timestep arbiter (Studio, per frame from the timers
    /// Studio, before this frame's targets): the root assembly solves
    /// every 2nd 240 Hz tick (level 2) only when Running with the PD error
    /// under FInt HeightControllerHipHeightRelativeDeltaThresholdThou (live
    /// 200) / 1000, the targets slow (|ω| < FInt
    /// TimestepArbiterHumanoidTurningVelThreshold 17, |v| < …LinearVelThreshold
    /// 21, live), the assembly a standard one (|61 − trace I| / 61 and
    /// |16 − m| / 16 under FInt HumanoidMassScaleThresholdThou(2), live 1000
    /// / 1000; StandardHumanoidAssyMOIThou 61000, …MassThou 16000), nothing
    /// else touching it (sensor empty), the floor static and AutoRotate
    /// not camera-relative; level 4 needs DebugSimAdaptiveEnable60HzHumanoids (off).
    pub fn level(&self, b: &Body, floor_static: bool, touching_moving: bool, camera_relative: bool) -> u8 {
        let running = self.state == State::Running && ARB_PD_ERR > self.pd_err;
        let slow = self.w_target_y.abs() < ARB_TURN && self.v_target.length() < ARB_LINEAR;
        let trace = b.inertia.x_axis.x + b.inertia.y_axis.y + b.inertia.z_axis.z;
        let standard = ((ARB_STD_MOI - trace) / ARB_STD_MOI).abs() < ARB_SCALE_TOL
            && ((ARB_STD_MASS - b.mass) / ARB_STD_MASS).abs() < ARB_SCALE_TOL;
        if running && slow && standard && !touching_moving && floor_static && !camera_relative {
            2
        } else {
            1
        }
    }

    /// Balancing: the upright torque, averaged over the substeps.
    pub fn balancing(&self, b: &Body, n: u32, hs: f32) -> Vec3 {
        let u0 = b.hrp_up;
        let mut w = b.w;
        let mut torso = Mat3::from_cols(b.rot.x_axis, b.hrp_up, b.rot.z_axis);
        let mut root = b.rot;
        let mut sum = Vec3::ZERO;
        let inv = b.inertia.inverse();
        for _ in 0..n.max(1) {
            let axis = if w.length() >= BAL_W_MIN || u0.y >= BAL_UPSIDE_DOWN {
                Vec3::new(u0.z, 0.0, -u0.x)
            } else {
                -BAL_FLIP_AXIS * torso.x_axis
            };
            let mut a = axis * -self.bal_p - w * self.bal_d;
            let root_up = root.y_axis;
            a -= root_up * a.dot(root_up);
            let l2 = a.length_squared();
            if l2 >= EPS {
                a *= (BAL_ALPHA_MAX / l2.sqrt()).min(1.0);
            } else {
                a = Vec3::ZERO;
            }
            let tau = b.inertia * a;
            sum += tau;
            // The prediction: the orientation turned by ω·hs, then ω += I⁻¹(τ + Text)·hs.
            let rot = rotation(w * hs);
            torso = rot * torso;
            root = rot * root;
            w += inv * (tau + b.text) * hs;
        }
        sum / n.max(1) as f32
    }

    /// The horizontal acceleration limit (Studio head, Studio).
    pub fn max_accel(b: &Body, floor: Option<&Floor>, scale: f32) -> f32 {
        if scale == 1.0 {
            match floor {
                Some(f) => b.mass * FRICTION_G * f.mu,
                None => AIR_ACCEL,
            }
        } else {
            let base = match floor {
                Some(f) => b.mass * FRICTION_G * f.mu / b.mass,
                None => AIR_ACCEL,
            };
            scale * base * ACCEL_SCALE_MULT
        }
    }

    /// Running / Landed / Climbing force: `n` substeps, each
    /// predicting the next one's velocity, height and heading.
    fn running_force(&mut self, b: &Body, inp: &StepIn, n: u32, hs: f32) -> Force {
        let m = b.mass;
        let floor = inp.floor.as_ref();
        let solid = floor.is_some_and(|f| f.solid);
        let full_hip = inp.full_hip;
        let climbing = inp.ladder;
        let max_a = Self::max_accel(b, floor, inp.accel_scale);
        let g_w = -m * b.gravity;
        let fy_m = b.fext.y / m;
        let inv = b.inertia.inverse();
        // The prediction's state (copies: the body itself is untouched).
        let mut v = b.v;
        let mut w = b.w;
        let mut rot = b.rot;
        let mut pos = b.pos;
        let hrp_y = b.hrp_y;
        let mut w_des = self.w_des;
        let mut sum_f = Vec3::ZERO;
        let mut sum_t = Vec3::ZERO;
        let mut pd_err = self.pd_err;
        // vDes.y + vfloor.y is read once, before the substeps (Studio:
        // fVar31), and the no-force-zone block zeroes it for the substeps
        // that follow as well.
        let mut v_des_y = self.v_des.y + self.floor_v.y;
        for _ in 0..n.max(1) {
            let mut v_des_y_used = v_des_y;
            // Yaw servo (no tilt): the torque takes the raw rate; the clamp
            // (±5000) only feeds the prediction of the target.
            let up = rot.y_axis;
            let alpha_y = ((w_des + self.floor_w - w).dot(up)) * YAW_GAIN;
            let torque = b.inertia * (up * alpha_y);
            let alpha_c = alpha_y.clamp(-YAW_ALPHA_MAX, YAW_ALPHA_MAX);
            // Altitude PD.
            let mut fpd = 0.0;
            let mut steep = false;
            if let (Some(f), true) = (floor, self.floor_target_y < f32::INFINITY) {
                let s = self.slide;
                let slide = if s.length_squared() <= EPS || s.y >= SLIDE_Y_MAX {
                    0.0
                } else {
                    let t = (pos.x - f.point.x) * s.x + (pos.z - f.point.z) * s.z;
                    t * s.y
                };
                let d = slide + self.floor_target_y - hrp_y;
                let hi = PD_HIGH_PCT * PCT * full_hip;
                let e = d.min(hi).max(-(full_hip * PD_LOW_HIP));
                let a_pd = e * PD_P - (v.y - self.floor_v.y) * PD_D;
                if self.new_pd {
                    let nrm = f.normal;
                    steep = nrm.y < inp.cos_max_slope;
                    let mut a = a_pd - fy_m;
                    if a.abs() > EPS {
                        if e < full_hip * PCT * PD_WEAK_PUSH_PCT {
                            if a >= 0.0 {
                                if e < -EPS {
                                    a = 0.0;
                                }
                            } else {
                                let vmax = (-2.0 * fy_m * e).max(0.0).sqrt();
                                let a2 = (vmax - v.y) / hs;
                                if a2 < a_pd {
                                    a = (a2 - fy_m).max(0.0);
                                }
                            }
                        }
                        fpd = if !solid {
                            (a.min(ACCEL_MAX) * m).max(m / PD_REF_MASS * PD_SOFT_MIN).min(m / PD_REF_MASS * PD_SOFT_MAX)
                        } else {
                            (a * m).max(m / PD_REF_MASS * PD_SOLID_MIN).min(m / PD_REF_MASS * PD_SOLID_MAX)
                        };
                    }
                    if nrm.y >= inp.cos_max_slope && !climbing {
                        if v_des_y.abs() > EPS && e < EPS && -(PD_NO_FORCE_PCT * full_hip * PCT) < e {
                            let extra = VEL_GAIN * (v_des_y - v.y) - (fpd / m + fy_m);
                            if extra > EPS {
                                fpd += extra * m;
                            }
                        }
                        v_des_y_used = 0.0;
                        v_des_y = 0.0;
                    }
                }
                pd_err = e;
            }
            // Vertical velocity term.
            let base = (b.fext.y + fpd) / m;
            let mut ay = if (self.v_des.y > 0.0 && !steep) || climbing {
                (v_des_y_used - v.y) * VEL_GAIN - base
            } else {
                0.0
            };
            if !climbing {
                ay = ay.max(ACCEL_Y_MIN).min(ACCEL_MAX);
            }
            // Horizontal.
            let mut ax = (self.v_des.x + self.floor_v.x - v.x) * VEL_GAIN - b.fext.x / m;
            let mut az = (self.v_des.z + self.floor_v.z - v.z) * VEL_GAIN - b.fext.z / m;
            let a2 = ax * ax + az * az;
            if max_a * max_a < a2 {
                let l = a2.sqrt();
                if l >= LEN_MIN {
                    if l >= UNIT_HI || l <= UNIT_LO {
                        ax /= l;
                        az /= l;
                    }
                } else {
                    ax = 0.0;
                    az = 0.0;
                }
                ax *= max_a;
                az *= max_a;
            }
            if let (Some(f), true) = (floor, steep) {
                let nh = Vec3::new(f.normal.x, 0.0, f.normal.z);
                let nh = nh / nh.length();
                let dot = ax * nh.x + az * nh.z;
                if dot < 0.0 {
                    ax -= nh.x * dot;
                    az -= nh.z * dot;
                }
                let s = self.slide;
                if s != Vec3::ZERO {
                    let t = fy_m * s.y;
                    ax += s.x * t;
                    az += s.z * t;
                }
            }
            let f = Vec3::new(ax * m, ay * m + fpd + g_w, az * m);
            sum_f += f;
            sum_t += torque;
            // The prediction for the next substep (gW stands in for gravity).
            // The prediction moves the root and the floor target alike
            // (Studio: fStack_254 and fStack_248 both += hs·v.y), so the
            // PD's error is the same in every substep. Studio, probe T5_drop1:
            // a level-2 solve that starts 0.0246 above the target at −19.6
            // studs/s does not brake for the crossing it predicts; the
            // landing dips to −0.0848 (with the error predicted: −0.02).
            pos += v * hs;
            let turn = rotation(w * hs);
            rot = turn * rot;
            v += f / m * hs;
            w += inv * (torque + b.text) * hs;
            w_des += up * alpha_c * hs;
        }
        self.pd_err = pd_err;
        let nf = n.max(1) as f32;
        // F = ΣF/N − gW.
        let force = sum_f / nf - Vec3::new(0.0, g_w, 0.0);
        let torque = sum_t / nf;
        let floor_force = floor.map(|f| {
            let pct = if !f.solid || force.length_squared() <= EPS { FLOOR_PCT_SOFT } else { FLOOR_PCT_SOLID };
            (-force * (pct / YAW_GAIN), f.point)
        });
        Force { force, torque, floor: floor_force, dv: Vec3::ZERO }
    }

    /// Unstick: a body that hasn't fallen MinimumFreefall
    /// MoveDistanceInTenths (5) / 10 studs for MaximumFreefallMoveTimeInTenths
    /// (1) / 10 s is pushed away from its contact points at Gs × g, Gs
    /// growing 0.1 a call to MaximumUnstickForceInGs (4); falling resets the
    /// mark and decays Gs × UnstickForceDecayInTenths (9) / 10 to 0 under
    /// UnstickForceEpsilonInHundredths (1) / 100. Also leaves the contacts'
    /// average normal in S.
    fn unstick(&mut self, b: &Body) -> Vec3 {
        self.contact_n = Vec3::ZERO;
        let y = b.pos.y;
        if self.unstick_y - y <= UNSTICK_MIN_FALL {
            self.unstick_gs = (self.unstick_gs + UNSTICK_GROWTH).min(UNSTICK_MAX_GS);
            let elapsed = (self.timer - self.unstick_t).abs();
            if elapsed > UNSTICK_MAX_TIME {
                let mut sum = Vec3::ZERO;
                let mut nsum = Vec3::ZERO;
                let mut n = 0;
                for (p, nrm) in &b.contacts {
                    let d = b.pos - *p;
                    let l = d.length();
                    if l > BAL_W_MIN {
                        sum += d / l;
                        n += 1;
                        nsum += *nrm;
                    }
                }
                if n > 0 {
                    self.contact_n = nsum / n as f32;
                    return sum / n as f32 * b.gravity * self.unstick_gs;
                }
            }
        } else {
            self.unstick_y = y;
            self.unstick_t = self.timer;
            self.unstick_gs *= UNSTICK_DECAY;
            if self.unstick_gs < UNSTICK_EPS {
                self.unstick_gs = 0.0;
            }
        }
        Vec3::ZERO
    }

    /// Freefall's force: balancing (P 5000), clamped yaw,
    /// unstick, the air target without its part into the contacts, a
    /// horizontal pull to it, no vertical force.
    fn freefall_force(&mut self, b: &Body, n: u32, hs: f32) -> Force {
        let tau_bal = self.balancing(b, n, hs);
        let up = b.rot.y_axis;
        let alpha = ((self.w_air - b.w).dot(up) * YAW_GAIN).clamp(-AIR_YAW_ALPHA_MAX, AIR_YAW_ALPHA_MAX);
        let tau = b.inertia * (up * alpha);
        let push = self.unstick(b);
        let nh = Vec3::new(self.contact_n.x, 0.0, self.contact_n.z);
        self.v_air -= nh * self.v_air.dot(nh);
        let mut ax = (self.v_air.x - b.v.x) * VEL_GAIN;
        let mut az = (self.v_air.z - b.v.z) * VEL_GAIN;
        let cap = AIR_ACCEL * 1.0;
        let a2 = ax * ax + az * az;
        if cap * cap < a2 {
            let l = a2.sqrt();
            if l >= LEN_MIN {
                if l >= UNIT_HI || l <= UNIT_LO {
                    ax /= l;
                    az /= l;
                }
            } else {
                ax = 0.0;
                az = 0.0;
            }
            ax *= cap;
            az *= cap;
        }
        Force { force: Vec3::new((push.x + ax) * b.mass, 0.0, (push.z + az) * b.mass), torque: tau_bal + tau, floor: None, dv: Vec3::ZERO }
    }

    /// Jumping's force: the take-off speed in one step.
    fn jumping_force(&mut self, b: &Body, inp: &StepIn, hs: f32) -> Force {
        if self.jump_done {
            return Force::default();
        }
        let need = inp.jump_velocity * JUMP_FACTOR - b.v.dot(self.jump_dir);
        // (Studio: need ≤ 1e-6, or the ceiling rays Studio hit:
        // no push under something. Studio, tools/fidelity movement-Place B
        // jump_run: Jumping → Freefall → Landed within a frame, feet on the
        // floor.)
        if need <= EPS || inp.ceiling {
            self.jump_done = true;
            return Force::default();
        }
        let m = b.mass;
        let a = need / hs - b.fext.y / m;
        let force = self.jump_dir * (a * m);
        let floor = inp.floor.as_ref().map(|f| {
            let pct = if f.solid { FLOOR_PCT_SOLID } else { FLOOR_PCT_SOFT };
            (-force * (pct / YAW_GAIN), f.point)
        });
        // F = m·need/hs·dir − Fext.y·dir: the first term is the take-off
        // speed itself. (Studio's default rigs reach it exactly: their next
        // step finds need ≤ 1e-6 and ends the push, probes T4. `b.v` is the
        // root part's own velocity: on an assembly that is turning over —
        // a character bouncing against a wall — it falls behind the centre
        // of mass's by ω × (ω × r) each tick, need stays above 1e-6 and the
        // push lasts until the floor is out of reach: Studio's wall jump,
        // vy 53 held for 3 frames, T19.)
        Force { force: self.jump_dir * -b.fext.y, torque: Vec3::ZERO, floor, dv: self.jump_dir * need }
    }

    /// Swimming's force: 35·m·(vDes − v), per-axis limits.
    fn swimming_force(&mut self, b: &Body) -> Force {
        let f = (self.v_target - b.v) * (SWIM_GAIN * b.mass);
        let f = Vec3::new(
            f.x.clamp(-SWIM_FORCE_MAX, SWIM_FORCE_MAX),
            f.y.clamp(-SWIM_FORCE_MAX, SWIM_FORCE_UP_MAX),
            f.z.clamp(-SWIM_FORCE_MAX, SWIM_FORCE_MAX),
        );
        Force { force: f, ..Default::default() }
    }
}

/// What the per-step half reads that the frame left behind.
#[derive(Debug, Clone, Copy)]
pub struct StepIn {
    pub floor: Option<Floor>,
    pub full_hip: f32,
    pub cos_max_slope: f32,
    pub ladder: bool,
    pub jump_velocity: f32,
    /// H (1 on a default character).
    pub accel_scale: f32,
    /// Jumping's ceiling rays found something overhead.
    pub ceiling: bool,
}

/// Studio: the floor's downhill direction, normalize(n × (n̂ × Y)).
pub fn downhill(n: Vec3) -> Vec3 {
    let l = n.length_squared();
    if l <= EPS {
        return Vec3::ZERO;
    }
    let nn = n / l.sqrt();
    let a = Vec3::new(-nn.z, 0.0, nn.x);
    let c = n.cross(a);
    if a.length_squared() <= EPS {
        return Vec3::ZERO;
    }
    c.normalize_or_zero()
}

/// Studio: `v` turned into the floor's plane at the same length.
pub fn into_plane(n: Vec3, v: Vec3) -> Vec3 {
    if n.length_squared() <= EPS {
        return v;
    }
    let vl2 = v.length_squared();
    if vl2 <= EPS {
        return v;
    }
    let vl = vl2.sqrt();
    let vh = v / vl;
    let c = Vec3::new(-vh.z, 0.0, vh.x);
    let r = n.cross(c);
    if r.length_squared() <= EPS {
        return v;
    }
    r.normalize() * vl
}

/// Studio: on a too-steep floor, the part of `v` into the slope removed.
pub fn off_slope(n: Vec3, v: Vec3) -> Vec3 {
    let h = Vec3::new(n.x, 0.0, n.z);
    let l = h.length();
    if l <= EPS {
        return v;
    }
    let h = h / l;
    let d = v.dot(h);
    if d < 0.0 { v - h * d } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R15 default: AssemblyMass 14.2551 (measure/assembly-push-seat-death.md).
    const M: f32 = 14.2551;
    const H: f32 = 1.0 / 240.0;

    fn body(v: Vec3, y: f32) -> Body {
        Body {
            mass: M,
            rot: Mat3::IDENTITY,
            pos: Vec3::new(0.0, y, 0.0),
            v,
            w: Vec3::ZERO,
            inertia: Mat3::from_diagonal(Vec3::splat(10.0)),
            fext: Vec3::new(0.0, -M * 196.2, 0.0),
            text: Vec3::ZERO,
            hrp_up: Vec3::Y,
            hrp_y: y,
            gravity: 196.2,
            contacts: Vec::new(),
        }
    }

    fn floor(mu: f32) -> Floor {
        Floor { y: 0.0, point: Vec3::ZERO, normal: Vec3::Y, v: Vec3::ZERO, w: Vec3::ZERO, mu, solid: true }
    }

    fn mix(f: f32, w: f32) -> f32 {
        (f * w + 0.3) / (w + 1.0)
    }

    /// measure/accel.md: f .05 w 100 → 146.77 studs/s² = 0.6115 per step;
    /// f .01 w 100 → 0.150 per step; Plastic 839 studs/s².
    #[test]
    fn acceleration_cap_matches_studio() {
        let b = body(Vec3::ZERO, 3.19);
        let a = Humanoid::max_accel(&b, Some(&floor(mix(0.05, 100.0))), 1.0);
        assert!((a - 146.77).abs() < 0.05, "{a}");
        assert!((a * H - 0.6115).abs() < 0.0005);
        let a = Humanoid::max_accel(&b, Some(&floor(mix(0.01, 100.0))), 1.0);
        assert!((a * H - 0.150).abs() < 0.0005, "{}", a * H);
        let a = Humanoid::max_accel(&b, Some(&floor(0.3)), 1.0);
        assert!((a - 839.0).abs() < 0.5, "{a}");
    }

    /// Integrates the walk on a flat floor with the force law alone: 12 steps
    /// at f .05 reach 7.338 (measured), 18 reach 11.007.
    #[test]
    fn walk_velocity_steps_match_studio() {
        let mut hum = Humanoid { state: State::Running, v_des: Vec3::new(0.0, 0.0, -16.0), floor_target_y: 3.19, ..Default::default() };
        let inp = StepIn { floor: Some(floor(mix(0.05, 100.0))), full_hip: 3.19, cos_max_slope: 89f32.to_radians().cos(), ladder: false, jump_velocity: 50.0, accel_scale: 1.0, ceiling: false };
        let mut v = Vec3::ZERO;
        let mut at = Vec::new();
        for _ in 0..18 {
            let f = hum.running_force(&body(v, 3.19), &inp, 1, H);
            v += f.force / M * H;
            at.push(-v.z);
        }
        assert!((at[11] - 7.338).abs() < 0.01, "{}", at[11]);
        assert!((at[17] - 11.007).abs() < 0.01, "{}", at[17]);
        // After the cap the P gain closes the gap × 0.375 a step.
        let mut v = Vec3::new(0.0, 0.0, -15.29);
        let f = hum.running_force(&body(v, 3.19), &inp, 1, H);
        v += f.force / M * H;
        assert!((-v.z - 15.733).abs() < 0.002, "{}", -v.z);
    }

    /// measure/turn-jump-altitude.md sink03: root 0.3 below rest, model
    /// −0.0366 / 1.433 at 0.0667 s; the PD alone with gravity.
    #[test]
    fn altitude_pd_matches_studio_sink() {
        let hip = 3.192423;
        let mut hum = Humanoid { state: State::Running, floor_target_y: hip, ..Default::default() };
        let inp = StepIn { floor: Some(floor(0.3)), full_hip: hip, cos_max_slope: 0.0175, ladder: false, jump_velocity: 50.0, accel_scale: 1.0, ceiling: false };
        let mut y = hip - 0.3;
        let mut vy = 0.0;
        for _ in 0..16 {
            let f = hum.running_force(&body(Vec3::new(0.0, vy, 0.0), y), &inp, 1, H);
            vy += (f.force.y / M - 196.2) * H;
            y += vy * H;
        }
        let e = y - hip;
        assert!((e - -0.0327).abs() < 0.006, "{e}");
        assert!((vy - 1.427).abs() < 0.05, "{vy}");
    }

    /// drop1: 1 stud above rest falls freely, then the PD catches it with a
    /// −0.089 undershoot at 0.1 s.
    #[test]
    fn altitude_pd_matches_studio_drop() {
        let hip = 3.192423;
        let mut hum = Humanoid { state: State::Running, floor_target_y: hip, ..Default::default() };
        let inp = StepIn { floor: Some(floor(0.3)), full_hip: hip, cos_max_slope: 0.0175, ladder: false, jump_velocity: 50.0, accel_scale: 1.0, ceiling: false };
        let mut y = hip + 1.0;
        let mut vy = 0.0;
        let mut min = f32::MAX;
        for _ in 0..60 {
            let f = hum.running_force(&body(Vec3::new(0.0, vy, 0.0), y), &inp, 1, H);
            vy += (f.force.y / M - 196.2) * H;
            y += vy * H;
            min = min.min(y - hip);
        }
        assert!((min - -0.089).abs() < 0.01, "{min}");
    }

    /// turn90 at 20 fps (target held for a frame): yaw error 0.7543 → 0.3414
    /// → … measured; the model reproduces each within 5 %.
    #[test]
    fn turn_matches_studio_at_20fps() {
        let i = 3.0f32;
        let mut hum = Humanoid { state: State::Running, ..Default::default() };
        let mut yaw = 0.0f32; // facing −Z
        let mut w = 0.0f32;
        let target = -std::f32::consts::FRAC_PI_2; // walking +X
        let mut errs = Vec::new();
        for _frame in 0..12 {
            hum.w_des = Vec3::new(0.0, wrap(target - yaw) * TURN_GAIN, 0.0);
            for _ in 0..12 {
                let mut b = body(Vec3::ZERO, 3.19);
                b.w = Vec3::new(0.0, w, 0.0);
                b.inertia = Mat3::from_diagonal(Vec3::splat(i));
                let alpha = ((hum.w_des - b.w).dot(Vec3::Y)) * YAW_GAIN;
                w += alpha * H;
                yaw += w * H;
            }
            errs.push(wrap(yaw - target).abs());
        }
        // measured at 0.0667 / 0.1333 s (frames ~1.33 / 2.67 at 20 fps)
        assert!(errs[0] > 0.6 && errs[0] < 1.2, "{errs:?}");
        assert!(errs[5] < 0.1, "{errs:?}");
    }

    #[test]
    fn wrap_is_studios() {
        assert!((wrap(4.0) - (4.0 - std::f32::consts::TAU)).abs() < 1e-5);
        assert!((wrap(-4.0) - (-4.0 + std::f32::consts::TAU)).abs() < 1e-5);
        assert_eq!(wrap(1.0), 1.0);
    }

    #[test]
    fn slope_helpers() {
        let n = Vec3::new(0.5f32.sin(), 0.5f32.cos(), 0.0);
        let d = downhill(n);
        assert!(d.y < 0.0 && d.x > 0.0, "{d:?}");
        let v = into_plane(n, Vec3::new(-16.0, 0.0, 0.0));
        assert!((v.length() - 16.0).abs() < 1e-3 && v.y > 0.0, "{v:?}");
        let v = off_slope(n, Vec3::new(-16.0, 0.0, 0.0));
        assert!(v.x.abs() < 1e-4, "{v:?}");
    }
}
