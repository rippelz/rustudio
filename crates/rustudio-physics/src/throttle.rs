//! Studio's environmental physics throttle (Workspace.PhysicsEnvironmentalThrottle).
//!
//! When the simulation cannot keep up, Studio steps the "environment" (every
//! assembly without a Humanoid) on only some 60 Hz frames and keeps Humanoid
//! assemblies at full rate. Measured on the low-end reference laptop with Place B
//! (`workspace:GetPhysicsThrottling()` 25, free-falling plain parts gaining 0.25 g
//! per game second, Humanoid models 1.0 g, near and far alike).
//!
//! Frame loop Studio (public Windows build): one decision per 60 Hz frame,
//! from the mode or, for Default, from the adaptive level 0..8
//! (world) through table Studio = [1e8, 8, 4, 3, 2, −3, −4, −8, −16]:
//! N > 0 throttles one frame in N, N < 0 all frames but one in |N|. A throttled
//! frame steps only the bodies of solver set S (Studio: body clear).
//! The adaptive level moves one step per frame: up when
//! the physics cost × the stepped fraction reaches 0.8 of the frame
//! (PhysicsImprovedCyclicExecutiveThrottleThresholdTenth 8) or when two or more
//! steps are owed; down when the next lower level's fraction would still fit.

/// Table Studio.
const TABLE: [i32; 9] = [100_000_000, 8, 4, 3, 2, -3, -4, -8, -16];
/// PhysicsImprovedCyclicExecutiveThrottleThresholdTenth (Studio = 8) / 10.
const BUDGET: f32 = 0.8;
/// Steps owed at which the level rises regardless of cost (Studio = 2).
const LATE_STEPS: u32 = 2;
/// The stepped fraction at or below which a frame runs only
/// mech::THROTTLED_MAX_PHYSICS_STEPS world steps (Studio 0.746: the float 0.4 at
/// 0x108f46868 compared in `World::updateStepsRequiredForCyclicExecutive`).
const THROTTLED_STEPS_FRACTION: f32 = 0.4;

/// Workspace.PhysicsEnvironmentalThrottle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Default,
    Disabled,
    Always,
    /// Skip2 / Skip4 / Skip8 / Skip16: one frame in N steps the environment.
    Skip(u32),
}

impl Mode {
    /// From the enum's value (Default 0, Disabled 1, Always 2, Skip2 3 … Skip16 6).
    pub fn from_value(v: u32) -> Mode {
        match v {
            1 => Mode::Disabled,
            2 => Mode::Always,
            3 => Mode::Skip(2),
            4 => Mode::Skip(4),
            5 => Mode::Skip(8),
            6 => Mode::Skip(16),
            _ => Mode::Default,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Throttle {
    pub mode: Mode,
    /// The adaptive level (world), 0 = never throttled.
    level: u8,
    /// 60 Hz frames since Play started (world).
    frame: u32,
    /// This frame's decision.
    throttled: bool,
    /// Wall ms of one unthrottled 60 Hz frame (moving average).
    full_ms: f32,
    /// This frame's ticks so far: count and wall ms.
    ticks: u8,
    tick_ms: f32,
}

impl Throttle {
    fn factor(&self) -> i32 {
        match self.mode {
            Mode::Default => TABLE[self.level as usize],
            Mode::Disabled => 1,
            Mode::Always => 100_000_000,
            Mode::Skip(n) => n as i32,
        }
    }

    /// Starts the next 60 Hz frame: whether the environment is throttled in it.
    pub fn begin_frame(&mut self) -> bool {
        if self.ticks == 4 && !self.throttled {
            self.record_full_frame(self.tick_ms);
        }
        self.ticks = 0;
        self.tick_ms = 0.0;
        let f = self.factor();
        let c = self.frame as i32;
        self.throttled = match self.mode {
            Mode::Disabled => false,
            Mode::Always => true,
            Mode::Skip(_) => c % f != 0,
            // Level 0 never throttles (its 1e8 only matches frame 0).
            Mode::Default => self.level > 0 && ((f > 0) == (c % f == 0)),
        };
        self.frame = self.frame.wrapping_add(1);
        self.throttled
    }

    pub fn throttled(&self) -> bool {
        self.throttled
    }

    /// The environment sits out (the rest of) this frame although the level let it step
    /// (physics.rs: a hurried frame). The frame is not an unthrottled frame's cost sample.
    pub fn cut(&mut self) {
        self.throttled = true;
    }

    /// The share of 60 Hz frames that step the environment.
    pub fn fraction(&self) -> f32 {
        match self.mode {
            Mode::Default => fraction_of(self.factor()),
            Mode::Disabled => 1.0,
            Mode::Always => 0.0,
            Mode::Skip(n) => 1.0 / n as f32,
        }
    }

    /// workspace:GetPhysicsThrottling(): the stepped share in percent.
    pub fn percent(&self) -> f64 {
        (f64::from(self.fraction()) * 100.0).round()
    }

    /// World steps one frame may run (Studio 0.746 `World::updateStepsRequiredForCyclicExecutive`
    /// 0x100309f1e: `DFInt::MaxMissedWorldStepsRemembered` (12, at least 12) while more than
    /// 0.4 of the frames step the environment, else 4, one 60 Hz frame's worth). Studio's
    /// rule reads the throttle factor; Always and Disabled have none here, so they keep 12.
    pub fn max_world_steps(&self) -> usize {
        let throttled = matches!(self.mode, Mode::Default | Mode::Skip(_)) && self.fraction() <= THROTTLED_STEPS_FRACTION;
        if throttled {
            rustudio_core::mech::THROTTLED_MAX_PHYSICS_STEPS
        } else {
            rustudio_core::mech::MAX_PHYSICS_STEPS
        }
    }

    /// One world tick of the current frame took `ms` of wall time.
    pub fn record_tick(&mut self, ms: f32) {
        self.ticks = self.ticks.saturating_add(1);
        self.tick_ms += ms;
    }

    /// One unthrottled frame took `ms` of wall time.
    pub fn record_full_frame(&mut self, ms: f32) {
        self.full_ms = if self.full_ms == 0.0 { ms } else { 0.8 * self.full_ms + 0.2 * ms };
    }

    /// The adaptive decision after one batch: `frame_ms` is the wall
    /// time one 60 Hz frame of game time may take, `owed` the 60 Hz steps the batch
    /// could not run.
    pub fn adapt(&mut self, frame_ms: f32, owed: u32) {
        if self.mode != Mode::Default || self.full_ms == 0.0 {
            return;
        }
        let budget = BUDGET * frame_ms;
        if budget <= self.full_ms * self.fraction() || owed >= LATE_STEPS {
            if self.level < 8 {
                self.level += 1;
            }
        } else if self.level > 0 && self.full_ms * fraction_of(TABLE[self.level as usize - 1]) < budget {
            self.level -= 1;
        }
    }

    pub fn level(&self) -> u8 {
        self.level
    }
}

fn fraction_of(f: i32) -> f32 {
    if f >= 100_000_000 {
        1.0
    } else if f > 0 {
        (f - 1) as f32 / f as f32
    } else {
        1.0 / (-f) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_match_studio_percentages() {
        let mut t = Throttle::default();
        let mut seen = Vec::new();
        for level in 0..9u8 {
            t.level = level;
            seen.push(t.percent());
        }
        // GetPhysicsThrottling read 75 / 33 / 25 on the reference laptop.
        assert_eq!(seen, vec![100.0, 88.0, 75.0, 67.0, 50.0, 33.0, 25.0, 13.0, 6.0]);
    }

    #[test]
    fn negative_factor_steps_one_frame_in_n() {
        let mut t = Throttle { level: 6, ..Default::default() };
        let stepped: Vec<bool> = (0..8).map(|_| !t.begin_frame()).collect();
        assert_eq!(stepped, vec![true, false, false, false, true, false, false, false]);
    }

    #[test]
    fn modes() {
        let mut t = Throttle { mode: Mode::Disabled, level: 8, ..Default::default() };
        assert!((0..16).all(|_| !t.begin_frame()));
        let mut t = Throttle { mode: Mode::Skip(4), ..Default::default() };
        let stepped = (0..8).filter(|_| !t.begin_frame()).count();
        assert_eq!(stepped, 2);
        let mut t = Throttle { mode: Mode::Always, ..Default::default() };
        assert!((0..16).all(|_| t.begin_frame()));
        let mut t = Throttle::default();
        assert!((0..16).all(|_| !t.begin_frame()));
    }

    #[test]
    fn adapts_to_the_budget() {
        let mut t = Throttle::default();
        // 60 ms per full frame against a 16.7 ms frame: rises until it fits.
        t.begin_frame();
        for _ in 0..4 {
            t.record_tick(15.0);
        }
        t.begin_frame();
        for _ in 0..20 {
            t.adapt(1000.0 / 60.0, 0);
        }
        // 60 × 1/8 fits 13.3 ms, 60 × 1/4 does not.
        assert_eq!(t.level(), 7);
        assert_eq!(t.percent(), 13.0);
        // Cheap again: falls back step by step to unthrottled.
        for _ in 0..50 {
            t.record_full_frame(2.0);
            t.adapt(1000.0 / 60.0, 0);
        }
        assert_eq!(t.level(), 0);
        // Owed steps raise it even when cheap.
        t.adapt(1000.0 / 60.0, 3);
        assert_eq!(t.level(), 1);
    }

    /// Studio's frame step cap: 12 world steps, 4 once the throttle steps the
    /// environment on 40 % of frames or fewer (levels 5..8: 33 %, 25 %, 13 %, 6 %).
    #[test]
    fn heavy_throttle_caps_a_frame_at_four_steps() {
        let caps: Vec<usize> = (0..9u8).map(|level| Throttle { level, ..Default::default() }.max_world_steps()).collect();
        assert_eq!(caps, vec![12, 12, 12, 12, 12, 4, 4, 4, 4]);
        assert_eq!(Throttle { mode: Mode::Skip(2), ..Default::default() }.max_world_steps(), 12);
        assert_eq!(Throttle { mode: Mode::Skip(4), ..Default::default() }.max_world_steps(), 4);
        assert_eq!(Throttle { mode: Mode::Always, ..Default::default() }.max_world_steps(), 12);
    }
}
