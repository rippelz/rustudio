//! Frame-time driven viewport quality: Studio's automatic graphics level as
//! observed on Windows, mapped to this renderer's passes.

const MAX_LEVEL: u8 = 21;
const START_LEVEL: u8 = 5;
const HISTORY: usize = 32;
// The Windows stepped profile is 34 ms through level 13, then
// ramps to the 19 ms endpoint at level 16.
const TARGET_MS: [f32; 21] = [
    34., 34., 34., 34., 34., 34., 34., 34., 34., 34., 34., 34., 34., 29., 24., 19., 19., 19., 19.,
    19., 19.,
];
const DRAW_DISTANCE: [f32; 21] = [
    500., 800., 1200., 1600., 1600., 1600., 1600., 1600., 1600., 1600., 1600., 1600., 2200., 3000.,
    4000., 6000., 10000., 100000., 100000., 100000., 100000.,
];
const SPRITE_QUALITY: [u8; 21] = [
    0, 0, 0, 0, 0, 0, 1, 2, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17,
];

#[derive(Clone, Copy, Debug)]
pub struct Profile(u8);

impl Profile {
    /// The profile of quality level `level` (1..=21).
    pub fn at(level: u8) -> Self {
        Self(level.clamp(1, MAX_LEVEL))
    }
    pub fn level(self) -> u8 {
        self.0
    }
    pub fn distance(self) -> f32 {
        DRAW_DISTANCE[usize::from(self.0 - 1)]
    }
    /// Small-part screen-size cull radius in viewport pixels.
    /// Studio 0.748: `max(0.05, low + (high - low) * min(level / 21, 1)) / 1080`
    /// where `0.05` is the floor clamp, `1.0` is the
    /// high-level pixel cutoff, and `low = 8.0` is the desktop profile override
    /// (`4.0` built-in default).
    pub fn lod_radius_pixels(self, height: u32) -> f32 {
        let cutoff = (8.0 + (1.0 - 8.0) * f32::from(self.0) / f32::from(MAX_LEVEL)).max(0.05);
        0.5 * cutoff * height as f32 / 1080.0
    }
    pub fn detail(self) -> f32 {
        match self.0 {
            1..=6 => 0.0,
            7..=14 => 1.0,
            _ => 2.0,
        }
    }
    pub fn samples(self) -> u32 {
        match self.0 {
            1..=16 => 1,
            17 => 2,
            _ => 4,
        }
    }
    pub fn ssao(self) -> bool {
        self.0 >= 20
    }
    pub fn shadow_tile_divisor(self) -> u32 {
        match self.0 {
            1..=3 => 0,
            4..=5 => 4,
            6..=13 => 2,
            _ => 1,
        }
    }
    pub fn grass(self) -> usize {
        // (weak-GPU mode: off below level 4, where sun shadows start)
        if self.0 <= 3 && rustudio_core::rustudio_settings::weak_gpu() {
            2
        } else if self.0 <= 6 {
            1
        } else {
            0
        }
    }
    pub fn bloom(self) -> bool {
        self.0 >= 6
    }
    /// Studio post-FX quality tier:
    /// L1..7 = 0, L8..11 = 1, L12..15 = 2, L16..21 = 3.
    pub fn post_fx_tier(self) -> u8 {
        match self.0 {
            1..=7 => 0,
            8..=11 => 1,
            12..=15 => 2,
            _ => 3,
        }
    }
    pub fn clouds(self) -> bool {
        self.0 > 10
    }
    pub fn depth_of_field(self) -> bool {
        self.post_fx_tier() >= 3
    }
    pub fn sun_rays(self) -> bool {
        self.post_fx_tier() >= 3
    }
    pub fn particle_budget(self) -> usize {
        let sprite = usize::from(SPRITE_QUALITY[usize::from(self.0 - 1)]);
        rustudio_core::mech::FX_PARTICLES_MAX * (sprite + 3) / 20
    }
}

struct History {
    data: [f32; HISTORY],
    count: usize,
    next: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            data: [0.0; HISTORY],
            count: 0,
            next: 0,
        }
    }
}

impl History {
    fn push(&mut self, value: f32) {
        self.data[self.next] = value;
        self.next = (self.next + 1) % HISTORY;
        self.count = (self.count + 1).min(HISTORY);
    }

    fn mean(&self) -> f32 {
        self.data[..self.count].iter().sum::<f32>() / self.count as f32
    }

    fn variance(&self) -> f32 {
        let average = self.mean();
        self.data[..self.count]
            .iter()
            .map(|value| (value - average).powi(2))
            .sum::<f32>()
            / self.count.saturating_sub(1).max(1) as f32
    }
}

pub struct Automatic {
    level: u8,
    times: History,
    averages: History,
    render_times: History,
    up_delay: u16,
    down_delay: u16,
    over_budget: u8,
    backoff: u16,
    last_up: bool,
    step_hill: [f32; 21],
}

/// Weak-GPU mode's first level: the lowest, with HBAO, sun and light
/// shadows, clouds, depth of field, bloom and the scene probe off, and
/// (weak GPUs only) no grass, so the first frames stay far below a Windows
/// TDR (2 s) on an HD 4600 or WARP. Automatic raises it as frame times allow.
const WEAK_START_LEVEL: u8 = 1;

impl Default for Automatic {
    fn default() -> Self {
        Self {
            level: if rustudio_core::rustudio_settings::weak_gpu() { WEAK_START_LEVEL } else { START_LEVEL },
            times: History::default(),
            averages: History::default(),
            render_times: History::default(),
            up_delay: 100,
            down_delay: 100,
            over_budget: 0,
            backoff: 1,
            last_up: false,
            step_hill: [1.0; 21],
        }
    }
}

impl Automatic {
    pub fn profile(&self) -> Profile {
        static FIXED: std::sync::OnceLock<Option<u8>> = std::sync::OnceLock::new();
        let fixed = FIXED.get_or_init(|| {
            std::env::var("RUSTUDIO_AUTO_QUALITY_LEVEL")
                .ok()?
                .parse::<u8>()
                .ok()
                .filter(|level| (1..=MAX_LEVEL).contains(level))
        });
        Profile(fixed.unwrap_or(self.level))
    }

    /// Feed the interval between consecutive rendered frames in milliseconds.
    /// An edit viewport can sleep until input arrives, so idle gaps must not
    /// become render-pressure samples.
    pub fn observe(&mut self, ms: f32, render_ms: Option<f32>) {
        if !ms.is_finite() || !(0.1..=500.0).contains(&ms) {
            self.times = History::default();
            self.averages = History::default();
            self.render_times = History::default();
            self.over_budget = 0;
            return;
        }
        self.times.push(ms);
        self.render_times.push(render_ms.unwrap_or(ms));
        self.up_delay = self.up_delay.saturating_sub(1);
        self.down_delay = self.down_delay.saturating_sub(1);

        let average = self.times.mean();
        self.averages.push(average);
        let row = usize::from(self.level - 1);
        let render_average = self.render_times.mean();
        let render_bound = render_average > average * 0.625;
        if self.level > 1 && average > 60.0 && render_bound {
            self.over_budget = self.over_budget.saturating_add(1);
            if self.over_budget == 5 {
                self.step(false, true);
                return;
            }
        } else {
            self.over_budget = 0;
        }
        if self.times.count < HISTORY
            || (self.down_delay != 0 && self.up_delay != 0)
            || self.averages.variance() > 5.0
        {
            return;
        }
        if self.down_delay == 0
            && self.level > 1
            && average > TARGET_MS[row] + 2.0
            && render_bound
        {
            self.step(false, false);
        } else if self.up_delay == 0
            && self.level < MAX_LEVEL
            && render_average < (TARGET_MS[row + 1] * 0.8 - self.step_hill[row + 1]).max(17.5)
        {
            self.step(true, false);
        }
    }

    fn step(&mut self, up: bool, fast: bool) {
        let previous = self.level;
        if std::env::var_os("RUSTUDIO_QUALITY_TRACE").is_some() {
            let next = if up { self.level + 1 } else { self.level - 1 };
            eprintln!(
                "render quality {} -> {next} (frame {:.2} ms, render {:.2} ms, fast={fast})",
                self.level, self.times.mean(), self.render_times.mean(),
            );
        }
        self.level = if up { self.level + 1 } else { self.level - 1 };
        self.times = History::default();
        self.averages = History::default();
        self.render_times = History::default();
        self.over_budget = 0;
        self.down_delay = 100;
        self.up_delay = if up { 150 } else { 150 * self.backoff };
        if self.level > 1 {
            if up == self.last_up || self.backoff >= 10 {
                self.backoff = self.backoff.saturating_sub(1).max(1);
            } else {
                if !up {
                    let hill = &mut self.step_hill[usize::from(previous - 1)];
                    *hill = (*hill + if fast { 0.5 } else { 1.0 }).min(8.0);
                }
                self.backoff += 1;
            }
        }
        self.last_up = up;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_backoff_and_slow_recovery() {
        let mut automatic = Automatic::default();
        for _ in 0..5 {
            automatic.observe(80.0, None);
        }
        assert_eq!(automatic.level, START_LEVEL - 1);
        for _ in 0..149 {
            automatic.observe(16.0, None);
        }
        assert_eq!(automatic.level, START_LEVEL - 1);
        automatic.observe(16.0, None);
        assert_eq!(automatic.level, START_LEVEL);
    }

    #[test]
    fn non_render_work_does_not_trigger_a_normal_downshift() {
        let mut automatic = Automatic::default();
        for _ in 0..100 {
            automatic.observe(50.0, Some(12.0));
        }
        assert_ne!(automatic.level, START_LEVEL - 1);
    }

    #[test]
    fn sustained_non_render_work_does_not_trigger_fast_backoff() {
        let mut automatic = Automatic::default();
        for _ in 0..32 {
            automatic.observe(90.0, Some(14.0));
        }
        assert_eq!(automatic.level, START_LEVEL);
    }

    #[test]
    fn a_long_idle_gap_does_not_lower_quality() {
        let mut automatic = Automatic::default();
        for _ in 0..200 {
            automatic.observe(1000.0, None);
        }
        assert_eq!(automatic.level, START_LEVEL);
    }

    #[test]
    fn lower_levels_reduce_draw_distance_and_optional_passes() {
        assert_eq!(Profile(1).distance(), 500.0);
        assert!(!Profile(1).ssao());
        assert!(!Profile(1).clouds());
        assert_eq!(Profile(21).distance(), 100000.0);
        assert!(Profile(21).ssao());
        assert_eq!(
            Profile(21).particle_budget(),
            rustudio_core::mech::FX_PARTICLES_MAX
        );
    }

    #[test]
    fn small_part_cull_cutoff_matches_studio_endpoints() {
        // At 1080p, diameter cutoff interpolates linearly from 8.0 px (L0) to
        // 1.0 px (L21), with radius = half diameter.
        assert!((Profile::at(21).lod_radius_pixels(1080) - 0.5).abs() < 1e-5);
        assert!((Profile::at(8).lod_radius_pixels(1080) - (0.5 * (8.0 - 7.0 * 8.0 / 21.0))).abs() < 1e-5);
        assert!((Profile::at(1).lod_radius_pixels(1080) - (0.5 * (8.0 - 7.0 / 21.0))).abs() < 1e-5);
    }

    #[test]
    fn post_fx_tier_and_cutoffs_match_studio() {
        assert_eq!(Profile::at(1).post_fx_tier(), 0);
        assert_eq!(Profile::at(7).post_fx_tier(), 0);
        assert_eq!(Profile::at(8).post_fx_tier(), 1);
        assert_eq!(Profile::at(11).post_fx_tier(), 1);
        assert_eq!(Profile::at(12).post_fx_tier(), 2);
        assert_eq!(Profile::at(15).post_fx_tier(), 2);
        assert_eq!(Profile::at(16).post_fx_tier(), 3);
        assert_eq!(Profile::at(21).post_fx_tier(), 3);
        assert!(!Profile::at(5).bloom());
        assert!(Profile::at(6).bloom());
        assert!(!Profile::at(15).depth_of_field());
        assert!(Profile::at(16).depth_of_field());
        assert!(!Profile::at(15).sun_rays());
        assert!(Profile::at(16).sun_rays());
    }
}
