//! Lighting as Roblox Studio does it (measured in an offline Studio and read from
//! Studio's own shaders; numbers and sources in docs/MECHANICS.md, "Lighting"):
//! the per-frame lighting constants, the local lights (PointLight, SpotLight,
//! SurfaceLight), the sky's faces, and the "sky visibility" map that decides
//! how much OutdoorAmbient a point gets.

use glam::Vec3;
use rbx_dom_weak::types::{Ref, Variant};

use crate::camera::Camera;
use rustudio_core::doc::{self, Document};
use rustudio_core::mech;
use crate::render::{mat_cols, Globals, LightRaw};
use crate::scene::{enum_u32, f32_of, vec3_of, xform_of};

/// RUSTUDIO_SHADE_DEBUG: one view (`albedo`), or views by run time
/// (`light@80,local@85,sun@90`: each from that many seconds after start).
fn shade_debug() -> Option<String> {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let v = std::env::var("RUSTUDIO_SHADE_DEBUG").ok()?;
    if !v.contains('@') {
        return Some(v);
    }
    let t = START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
    let mut cur = None;
    for item in v.split(',') {
        let (m, at) = item.split_once('@')?;
        if t >= at.parse::<f32>().ok()? {
            cur = Some(m.to_string());
        }
    }
    cur
}

/// RUSTUDIO_LIGHT_OVERRIDE (lighting checks): `Brightness=0;Ambient=128,128,128;nolocal;nopost;noatmos`
/// overrides Lighting properties (numbers, or colours as 0–255 r,g,b) and
/// drops local lights / post effects / the Atmosphere, in any mode.
fn light_overrides() -> &'static Vec<(String, String)> {
    static O: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    O.get_or_init(|| {
        std::env::var("RUSTUDIO_LIGHT_OVERRIDE")
            .unwrap_or_default()
            .split(';')
            .filter(|t| !t.is_empty())
            .map(|t| match t.split_once('=') {
                Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
                None => (t.trim().to_string(), String::new()),
            })
            .collect()
    })
}

fn override_flag(name: &str) -> bool {
    light_overrides().iter().any(|(k, v)| k == name && v.is_empty())
}

fn override_prop(name: &str) -> Option<Variant> {
    let (_, v) = light_overrides().iter().find(|(k, v)| k == name && !v.is_empty())?;
    let parts: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    match parts[..] {
        [x] => Some(Variant::Float32(x)),
        [r, g, b] => Some(Variant::Color3uint8(rbx_dom_weak::types::Color3uint8::new(r as u8, g as u8, b as u8))),
        _ => None,
    }
}

/// Lighting.Technology as the place saved it (None: not saved).
pub fn saved_technology(d: &Document) -> Option<u32> {
    let l = d.find_child(d.dom.root_ref(), "Lighting")?;
    let i = d.dom.get_by_ref(l)?;
    enum_u32(i.properties.get(&rbx_dom_weak::ustr("Technology")).cloned())
}

/// Whether Studio's Default tone curve applies (before Retro grading): a
/// saved Technology other than Compatibility. See `globals`.
pub fn tone_curve_on(d: &Document) -> bool {
    saved_technology(d).is_some_and(|t| !mech::TECHNOLOGY_NO_TONE_CURVE.contains(&t))
}

/// Studio's linearisation: a colour channel squared (gamma 2.0).
pub fn sq(c: [u8; 3]) -> Vec3 {
    let f = |v: u8| (v as f32 / 255.0).powi(2);
    Vec3::new(f(c[0]), f(c[1]), f(c[2]))
}

fn color_of(v: Option<Variant>) -> Option<[u8; 3]> {
    match v? {
        Variant::Color3uint8(c) => Some([c.r, c.g, c.b]),
        Variant::Color3(c) => Some([
            (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        ]),
        _ => None,
    }
}

fn boolp(d: &Document, r: Ref, name: &str, default: bool) -> bool {
    match d.prop(r, name) {
        Some(Variant::Bool(b)) => b,
        _ => default,
    }
}

/// The sun's direction (toward the sun) for a ClockTime and latitude, as
/// Lighting:GetSunDirection returns it in Studio: θ = (ClockTime − 6) × 15°
/// around the sky, tilted by φ = latitude − 23.5°.
pub fn sun_direction(clock: f32, latitude: f32) -> Vec3 {
    let th = ((clock - 6.0) * 15.0).to_radians();
    let ph = (latitude - mech::SUN_AXIAL_TILT_DEG).to_radians();
    Vec3::new(th.cos() * ph.cos(), th.sin() * ph.cos(), ph.sin())
}

/// Lighting.LightingStyle is Soft (enum 1; Realistic 0). Without a stored
/// one (old places, live joins) it is what Studio has then
/// (`doc::lighting_style_default`: the constructor's Realistic once the place
/// was migrated, else derived from Technology).
pub fn soft_style(d: &Document) -> bool {
    let Some(l) = d.find_child(d.dom.root_ref(), "Lighting") else {
        return false;
    };
    let Some(i) = d.dom.get_by_ref(l) else { return false };
    match enum_u32(i.properties.get(&rbx_dom_weak::ustr("LightingStyle")).cloned()) {
        Some(style) => style == 1,
        None => rustudio_core::doc::lighting_style_default(i).0 == 1,
    }
}

/// How bright the sky (and the light it gives) is for a sun height: full
/// by day, a dim blue after dusk (measured at 20:18, sun 0.54 below:
/// SKY_NIGHT_TINT of the day sky), nothing deep in the night (00:00).
pub fn sky_tint(sun_y: f32) -> Vec3 {
    let night = Vec3::from(mech::SKY_NIGHT_TINT)
        * ((sun_y - mech::SKY_DARK_SUN_Y) / (mech::SKY_NIGHT_TINT_SUN_Y - mech::SKY_DARK_SUN_Y))
            .clamp(0.0, 1.0);
    let day = ((sun_y - mech::SKY_NIGHT_SUN_Y) / (mech::SKY_DAY_SUN_Y - mech::SKY_NIGHT_SUN_Y))
        .clamp(0.0, 1.0);
    night.lerp(Vec3::ONE, day)
}

/// Post effects under Lighting and the camera (Enabled ones; several of a
/// kind combine per Studio `updatePostEffects`).
#[derive(Default, Clone, Copy, Debug)]
pub struct Effects {
    /// Enabled `BloomEffect` aggregate: `(intensity, size, threshold)`.
    /// `None` when no enabled `BloomEffect` exists (falls back to Studio default Neon glow).
    pub bloom: Option<(f32, f32, f32)>,
    pub cc: Option<(f32, f32, f32, Vec3)>,
    pub blur: f32,
    pub dof: Option<[f32; 4]>,
    pub sun_rays: Option<(f32, f32)>,
    pub retro: bool,
}

pub fn effects(d: &Document, cam_obj: Option<Ref>) -> Effects {
    let mut e = Effects::default();
    let root = d.dom.root_ref();
    let parents: Vec<(Ref, bool)> = d
        .find_child(root, "Lighting")
        .into_iter()
        .map(|r| (r, true))
        .chain(cam_obj.map(|r| (r, false)))
        .collect();
    let mut cc_acc: Option<(f32, f32, f32, Vec3)> = None;
    for (p, is_lighting) in parents {
        for c in d
            .dom
            .get_by_ref(p)
            .map(|i| i.children().to_vec())
            .unwrap_or_default()
        {
            let Some(class) = d.class_of(c) else { continue };
            if !boolp(d, c, "Enabled", true) {
                continue;
            }
            let num = |n: &str, dflt: f32| f32_of(d.prop(c, n)).unwrap_or(dflt);
            match class.as_str() {
                // Studio: FocusDistance = max, InFocusRadius = last wins,
                // NearIntensity = max, FarIntensity = max.
                "DepthOfFieldEffect" => {
                    let fd = num("FocusDistance", 0.05).max(0.0);
                    let r = num("InFocusRadius", 10.0).max(0.0);
                    let ni = num("NearIntensity", 0.75).clamp(0.0, 1.0);
                    let fi = num("FarIntensity", 0.75).clamp(0.0, 1.0);
                    e.dof = Some(match e.dof {
                        None => [fd, r, ni, fi],
                        Some([ofd, _, oni, ofi]) => [ofd.max(fd), r, oni.max(ni), ofi.max(fi)],
                    });
                }
                // Studio: first sets (max(0,I), max(0,S), clamp(T,0,4));
                // subsequent add Intensity, max Size, min Threshold (clamped >= 0).
                "BloomEffect" => {
                    let i = num("Intensity", 1.0);
                    let s = num("Size", 24.0);
                    let t = num("Threshold", 2.0);
                    e.bloom = Some(match e.bloom {
                        None => (i.max(0.0), s.max(0.0), t.clamp(0.0, 4.0)),
                        Some((oi, os, ot)) => ((oi + i).max(0.0), os.max(s), ot.min(t).clamp(0.0, 4.0)),
                    });
                }
                "ColorCorrectionEffect" => {
                    let tint = color_of(d.prop(c, "TintColor"))
                        .map(|t| Vec3::new(t[0] as f32, t[1] as f32, t[2] as f32) / 255.0)
                        .unwrap_or(Vec3::ONE);
                    let (b, ct, s, t) = cc_acc.unwrap_or((0.0, 0.0, 0.0, Vec3::ONE));
                    cc_acc = Some((
                        b + num("Brightness", 0.0),
                        ct + num("Contrast", 0.0),
                        s + num("Saturation", 0.0),
                        t * tint,
                    ));
                }
                "BlurEffect" => e.blur = e.blur.max(num("Size", 24.0).max(0.0)),
                // Studio: SunRays Intensity sums, Spread takes max.
                "SunRaysEffect" => {
                    let i = num("Intensity", 0.01);
                    let sp = num("Spread", 0.1).clamp(0.0, 1.0);
                    e.sun_rays = Some(match e.sun_rays {
                        None => (i.max(0.0), sp),
                        Some((oi, osp)) => ((oi + i).max(0.0), osp.max(sp)),
                    });
                }
                // Studio: ColorGradingEffect is read ONLY under Lighting,
                // never under CurrentCamera. TonemapperPreset: Default 0, Retro 1.
                "ColorGradingEffect" if is_lighting => {
                    e.retro = enum_u32(d.prop(c, "TonemapperPreset")) == Some(1);
                }
                _ => {}
            }
        }
    }
    e.cc = cc_acc;
    e
}

/// Lighting.Atmosphere (the first Atmosphere child) as Studio's renderer
/// turns it into AdvSky props and fog.
#[derive(Clone, Copy, Debug)]
pub struct Atmos {
    /// Color² (FogColor before the sky's irradiance).
    pub color_sq: Vec3,
    /// AdvSkyFS fogOffset.x (−A / ln 2) and fogOffset.y (B).
    pub fog_offset: [f32; 2],
    /// Glare, and its per-channel exponents (from Decay).
    pub glare: f32,
    pub glare_exp: Vec3,
    /// Offset: the least blur of the sky the fog fades into.
    pub offset: f32,
    /// How fast objects fade into the sky, per stud (natural log).
    pub fog_k: f32,
}

/// The Atmosphere's props as Studio computes them: the change hook at
/// (A, B from Haze), the Decay transform and the
/// fog setup.
pub fn atmosphere(d: &Document) -> Option<Atmos> {
    let l = d.find_child(d.dom.root_ref(), "Lighting")?;
    let a = d.find_child(l, "Atmosphere")?;
    let num = |n: &str, dflt: f32| f32_of(d.prop(a, n)).unwrap_or(dflt);
    // Defaults: the Atmosphere constructor's: Density 0.395,
    // Color (199, 170, 107), Decay (92, 60, 13), the rest 0.
    let density = num("Density", 0.395);
    let haze = num("Haze", 0.0).clamp(0.0, 10.0);
    let glare = num("Glare", 0.0);
    let offset = num("Offset", 0.0);
    let color = color_of(d.prop(a, "Color")).unwrap_or([199, 170, 107]);
    let decay = color_of(d.prop(a, "Decay")).unwrap_or([92, 60, 13]);
    let ca = (haze - mech::ATMOS_HAZE_THRESHOLD).clamp(mech::ATMOS_A_MIN, 1.0);
    let cb = haze * mech::ATMOS_B_HAZE + if haze != 0.0 { mech::ATMOS_B_ON } else { 0.0 };
    let dexp = |c: u8| {
        let u = 1.0 - c as f32 / 255.0;
        (u * u + mech::ATMOS_DECAY_LIN * u).exp2() - 1.0
    };
    let lin = |c: [u8; 3]| Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0;
    let c = lin(color);
    Some(Atmos {
        color_sq: c * c,
        fog_offset: [-ca / std::f32::consts::LN_2, cb],
        glare,
        glare_exp: Vec3::new(dexp(decay[0]), dexp(decay[1]), dexp(decay[2])),
        offset,
        fog_k: mech::ATMOS_FOG_DENSITY_K
            * density.clamp(0.0, 1.0).powf(mech::ATMOS_FOG_DENSITY_POW),
    })
}

/// AdvSkyFS irradiance × FogColor on Studio's desktop renderer. At
/// → byte >= 2 bypasses
/// the legacy mean-irradiance/sky-level factor and uploads Color² directly.
pub fn atmos_haze_cube(a: &Atmos, sky_irr: &[Vec3; 6]) -> [Vec3; 6] {
    sky_irr.map(|e| e * a.color_sq)
}

/// Everything the shaders need for a frame.
pub struct FrameLighting {
    pub globals: Globals,
    pub postfx: crate::postfx::PostFxParams,
    pub sun: Vec3,
    /// Toward the sun (Lighting:GetSunDirection), even at night: the light
    /// grid's sun channel flips it to the moon itself.
    pub sun_true: Vec3,
    pub shadows: bool,
    /// Lighting.ShadowSoftness: the sun's shadow blur (sun_shadow.rs).
    pub shadow_softness: f32,
}

/// Lighting's time-of-day sky levels for a ClockTime and
/// EnvironmentDiffuseScale `s`: (c0, c1, k) as Studio keeps them in its
/// Lighting members (mech:SKY_LEVELS). c0² and c1²
/// are the shaders' SkyGradientTop / Bottom.
pub fn sky_levels(clock: f32, s: f32) -> (Vec3, Vec3, f32) {
    let t = mech::SKY_LEVELS;
    let n = t.len() / mech::SKY_LEVEL_COLUMNS;
    let row = |i: usize| &t[(i % n) * mech::SKY_LEVEL_COLUMNS..(i % n + 1) * mech::SKY_LEVEL_COLUMNS];
    let h = clock.rem_euclid(24.0);
    let mut i = n - 1;
    for k in 0..n {
        if row(k)[0] <= h {
            i = k;
        }
    }
    let (a, b) = (row(i), row(i + 1));
    let tb = if i + 1 >= n { b[0] + 24.0 } else { b[0] };
    let f = if tb > a[0] { ((h - a[0]) / (tb - a[0])).clamp(0.0, 1.0) } else { 0.0 };
    let v = |j: usize| a[j] + (b[j] - a[j]) * f;
    let v3 = |j: usize| Vec3::new(v(j), v(j + 1), v(j + 2));
    let s = s.clamp(0.0, 1.0);
    let c0 = v3(1).lerp(v3(4), s);
    let c1 = v3(7).lerp(v3(10), s);
    let k = v(13).max(s * v(14));
    (c0, c1, k)
}

/// Lighting's time of day in hours, as every reader takes it: ClockTime
/// (scripts and edits keep it beside TimeOfDay), else TimeOfDay (what files
/// and Team Create carry), else 14:00; clamped and cut to whole seconds as
/// Studio stores it.
pub fn clock_hours(clock: Option<Variant>, time_of_day: Option<Variant>) -> f32 {
    f32_of(clock)
        .or_else(|| match time_of_day {
            Some(Variant::String(s)) => mech::parse_time_of_day(&s),
            _ => None,
        })
        .map(mech::clock_time)
        .unwrap_or(mech::CLOCK_TIME)
}

pub fn globals(
    d: &Document,
    cam: &Camera,
    rect: egui::Rect,
    light_count: usize,
    skybox: bool,
    sky_irr: &[Vec3; 6],
    indoor_irr: &[Vec3; 6],
) -> FrameLighting {
    let lighting = d.find_child(d.dom.root_ref(), "Lighting");
    let get = |name: &str| override_prop(name).or_else(|| lighting.and_then(|l| d.prop(l, name)));
    let clock = clock_hours(get("ClockTime"), get("TimeOfDay"));
    let lat = f32_of(get("GeographicLatitude")).unwrap_or(mech::GEOGRAPHIC_LATITUDE);
    let brightness = f32_of(get("Brightness")).unwrap_or(mech::LIGHT_BRIGHTNESS);
    let exposure = f32_of(get("ExposureCompensation")).unwrap_or(0.0);
    let env_diffuse = f32_of(get("EnvironmentDiffuseScale"))
        .unwrap_or(0.0)
        .clamp(0.0, mech::ENV_SCALE_MAX); // Studio
    let env_specular = f32_of(get("EnvironmentSpecularScale"))
        .unwrap_or(0.0)
        .clamp(0.0, mech::ENV_SCALE_MAX);
    let ambient = sq(color_of(get("Ambient")).unwrap_or(mech::AMBIENT));
    let outdoor = sq(color_of(get("OutdoorAmbient")).unwrap_or(mech::OUTDOOR_AMBIENT));
    let shift = color_of(get("ColorShift_Top")).unwrap_or([0, 0, 0]);
    let shadows = matches!(get("GlobalShadows"), Some(Variant::Bool(true)) | None);
    // Tone curve (offline Studio 2026-11-08, lightov/tone/REPORT.md): a saved
    // Legacy / Voxel / ShadowMap / Future draws Studio's Default curve;
    // Compatibility is migrated to Voxel + a Retro ColorGradingEffect
    // (≈ sqrt). A place saving no Technology at all (Place A) draws without the
    // curve too: its Studio lobby capture fits sqrt (fidelity-place_a MAE 14.5
    // vs 21.2 with the curve).
    let tech = saved_technology(d).unwrap_or(mech::TECHNOLOGY_SOFT[1]);
    let no_tone = !tone_curve_on(d);

    let sun = sun_direction(clock, lat);
    let (c0, c1, sky_k) = sky_levels(clock, env_diffuse);
    // The sun / moon (Lamp0) and the back light (Lamp1), as Studio builds
    // them: the sky level k² shifted
    // toward ColorShift_Top² / _Bottom² by EnvironmentDiffuseScale, ×
    // Brightness × 0.9 / 0.36. Lamp0 comes from the moon (−sun) once the sun
    // is below LAMP_MOON_BELOW_SUN_Y.
    let lamp_mix = |a: Vec3, b: Vec3, c: Vec3| -> Vec3 {
        let la = a.length();
        let mean = (c.x + c.y + c.z) * mech::LAMP_COLOR_MEAN;
        let dc = c - Vec3::splat(mean);
        let shifted = a + dc * la;
        let sat = dc.length();
        ((c * la - b) * sat + b - shifted) * env_diffuse + shifted
    };
    let k2 = Vec3::splat(sky_k * sky_k);
    let shift_bottom = color_of(get("ColorShift_Bottom")).unwrap_or([0, 0, 0]);
    let b = brightness.max(0.0);
    let lamp0 = lamp_mix(k2, k2, sq(shift)) * (mech::LAMP0_GAIN * b);
    let lamp1 = lamp_mix(k2, Vec3::ZERO, sq(shift_bottom)) * (mech::LAMP1_GAIN * b);
    let light_dir = if sun.y >= mech::LAMP_MOON_BELOW_SUN_Y { sun } else { -sun };
    let light_irr = ((lamp0.x + lamp0.y + lamp0.z) / 3.0).max(0.0);
    let sun_color = if light_irr > 1e-6 { lamp0 / light_irr } else { Vec3::ONE };
    let day = ((sun.y - mech::SKY_NIGHT_SUN_Y) / (mech::SKY_DAY_SUN_Y - mech::SKY_NIGHT_SUN_Y))
        .clamp(0.0, 1.0);
    // Outdoors a point gets the larger of Ambient and OutdoorAmbient, at
    // night too (measured at 00:00: side faces 130 vs 132 predicted).
    let outdoor_extra = (outdoor - ambient).max(Vec3::ZERO);
    // The low shaders use the mean irradiance (CB0[13]/[14]) instead
    // of blending directional ambient cubes per pixel.
    let mean_irradiance = |irr: &[Vec3; 6]| irr.iter().copied().sum::<Vec3>() / 6.0;
    let indoor_diffuse = mean_irradiance(indoor_irr) * env_diffuse;
    let outdoor_diffuse = mean_irradiance(sky_irr) * (0.5 * env_diffuse) * (c0 * c0 + c1 * c1);
    let low_gloss = (18.0 / (std::f32::consts::PI * mech::ENV_SPEC_ROUGHNESS.powi(2))).clamp(1.0, 255.0);

    let cam_obj = d
        .workspace()
        .and_then(|ws| match d.prop(ws, "CurrentCamera") {
            Some(Variant::Ref(c)) if d.exists(c) => Some(c),
            _ => None,
        });
    let mut fx = effects(d, cam_obj);
    if override_flag("nopost") {
        fx.cc = None;
        fx.bloom = Some((0.0, mech::DEFAULT_GLOW_SIZE_PX, mech::DEFAULT_GLOW_THRESHOLD));
        fx.blur = 0.0;
        fx.dof = None;
        fx.sun_rays = None;
    }
    let saved_tech = saved_technology(d);
    let no_tone = no_tone || fx.retro;
    let fog_start = f32_of(get("FogStart")).unwrap_or(0.0);
    let fog_end = f32_of(get("FogEnd")).unwrap_or(100_000.0);
    let fog_color = sq(color_of(get("FogColor")).unwrap_or([192, 192, 192]));
    // With an Atmosphere, Lighting's FogStart / FogEnd don't apply (Roblox
    // docs): objects fade into the sky behind them, e^(−k·d) (fog.z 3,
    // exp2 form as Studio's FogParams). Only with a Sky too: Studio's
    // Atmosphere sky/haze and fog only activate
    // when Lighting has both an Atmosphere and a Sky.
    let has_sky = lighting.and_then(|l| sky_child(d, l)).is_some();
    let atmos = if override_flag("noatmos") || !has_sky { None } else { atmosphere(d) };
    let fog = match atmos {
        Some(a) if a.fog_k > 0.0 => [0.0, a.fog_k / std::f32::consts::LN_2, 3.0, day],
        Some(_) => [0.0, 1e9, 0.0, day],
        None => [
            fog_start,
            fog_end,
            if fog_end < 99_999.0 { 1.0 } else { 0.0 },
            day,
        ],
    };
    // Studio's default sky swaps to its night faces after dark, so the haze
    // it lights dims with them (Haze 1.9 at 02:48: (3, 5, 5) on screen with
    // the default sky, (62, 77, 108) with a custom one); rustudio keeps the
    // day faces and dims them by sky_tint, so the haze follows sky_tint too.
    let default_sky = sky_faces(d)[0].starts_with("rbxasset://textures/sky/sky512");
    // AdvSky's irradiance stays in the source sky frame.
    // Re-express the ambient cube supplied by the rotated reflection sky.
    // Axis-aligned rotations are exact; general rotations use the same
    // squared-axis reconstruction as AdvSkyFS.
    let orient = sky_orientation(d);
    let r = glam::Mat3::from_rotation_z(orient.z.to_radians())
        * glam::Mat3::from_rotation_x(orient.x.to_radians())
        * glam::Mat3::from_rotation_y(orient.y.to_radians());
    let axes = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
    let atmos_irr = axes.map(|axis| {
        let v = r * axis;
        sky_irr[if v.x >= 0.0 { 0 } else { 1 }] * v.x * v.x
            + sky_irr[if v.y >= 0.0 { 2 } else { 3 }] * v.y * v.y
            + sky_irr[if v.z >= 0.0 { 4 } else { 5 }] * v.z * v.z
    });
    let haze_cube = atmos.map(|a| {
        let tint = if default_sky { sky_tint(sun.y) } else { Vec3::ONE };
        let cube = atmos_haze_cube(&a, &atmos_irr);
        cube.map(|c| c * tint)
    });
    let stars = d
        .find_child(d.dom.root_ref(), "Lighting")
        .and_then(|l| sky_child(d, l))
        .map(|s| f32_of(d.prop(s, "StarCount")).unwrap_or(3000.0) > 0.0)
        .unwrap_or(true);

    // Without a BloomEffect Studio still glows what is over-bright (Neon):
    // tools/fidelity control shots show a ~12 px halo around a 1-stud Neon bar
    // in a place with no post effects.
    let (bloom_i, bloom_size, bloom_thr) = fx.bloom.unwrap_or((
        mech::DEFAULT_GLOW_INTENSITY,
        mech::DEFAULT_GLOW_SIZE_PX,
        mech::DEFAULT_GLOW_THRESHOLD,
    ));
    let (ccb, ccc, ccs, cct) = fx.cc.unwrap_or((0.0, 0.0, 0.0, Vec3::ONE));
    // Light debug views read raw: no curve, colour correction or bloom,
    // so the screen is sqrt(light / 4).
    let raw_debug = matches!(shade_debug().as_deref(), Some("light" | "local" | "sun" | "ambient" | "albedo" | "shadowmap" | "voxel-sun" | "sky-raw"));
    let no_tone = no_tone || raw_debug;
    let bloom_i = if raw_debug { 0.0 } else { bloom_i };
    let fx_cc_on = fx.cc.is_some() && !raw_debug;
    let globals = Globals {
        view_proj: mat_cols(cam.view_proj_reverse_z(rect)),
        inv_view_proj: mat_cols(cam.inv_view_proj_rel(rect)),
        cam_pos: cam.pos.extend(1.0).to_array(),
        sun_dir: light_dir.extend(light_irr).to_array(),
        // w: Lighting.LightingStyle (Realistic 0, Soft 1).
        sun_color: sun_color
            .extend(if soft_style(d) { 1.0 } else { 0.0 })
            .to_array(),
        ambient: ambient.extend(1.0).to_array(),
        outdoor: outdoor_extra.extend(1.0).to_array(),
        low_ambient: (ambient + indoor_diffuse).extend(low_gloss).to_array(),
        low_outdoor: (outdoor_extra + outdoor_diffuse - indoor_diffuse).extend(0.0).to_array(),
        sky_top: (sq(mech::SKY_GRADIENT_TOP)).extend(1.0).to_array(),
        sky_horizon: (sq(mech::SKY_GRADIENT_HORIZON)).extend(1.0).to_array(),
        params: [
            light_count as f32,
            2f32.powf(exposure),
            if no_tone { 1.0 } else { 0.0 },
            if skybox { 1.0 } else { 0.0 },
        ],
        fog,
        fog_color: fog_color.extend(1.0).to_array(),
        cc: [ccb, ccc, ccs, if fx_cc_on { 1.0 } else { 0.0 }],
        cc_tint: [cct.x, cct.y, cct.z, bloom_i],
        bloom: [
            bloom_thr,
            bloom_size,
            if shadows && light_irr > 0.0 { 1.0 } else { 0.0 },
            0.0,
        ],
        sun_casc: [[[0.0; 4]; 4]; 4],
        sun_sphere: [[0.0; 4]; 4],
        sun_rect: [[0.0; 4]; 4],
        // z: RUSTUDIO_SHADE_DEBUG=normals|albedo shows those instead of light.
        sky_rot: [
            if stars { 1.0 } else { 0.0 },
            fx.blur,
            match shade_debug().as_deref() {
                Some("normals") => 1.0,
                Some("albedo") => 2.0,
                Some("uv") => 3.0,
                Some("skylight") => 4.0,
                Some("light") => 5.0,
                Some("local") => 6.0,
                Some("sun") => 7.0,
                Some("ambient") => 8.0,
                Some("shadowmap") => 9.0,
                Some("voxel-sun") => 10.0,
                Some("sky-raw") => 11.0,
                _ => 0.0,
            },
            // w: 1 = the drawn sky takes Studio's time-of-day sky levels
            // (shader.wgsl sky_level): a custom Sky's faces dim and redden
            // at dusk and go dark at night as in Studio (they were drawn at
            // full day brightness at any ClockTime, so a game's
            // graveyard night, ClockTime 2.8, kept the afternoon sky). 0 =
            // Studio's default sky, dimmed by sky_tint (rustudio's stand-in
            // for its night faces) instead.
            if default_sky { 0.0 } else { 1.0 },
        ],
        // AmbientCube[0..5] + [6..11] as Studio builds them:
        // the sky's irradiance per axis × EnvironmentDiffuseScale × the sky
        // level (+Y c0², −Y c1², sides their mean); the shader blends it
        // with the indoor cube by skylight.
        env: {
            let (a, b) = (c0 * c0, c1 * c1);
            let side = (a + b) * 0.5;
            let lv = [side, side, a, b, side, side];
            std::array::from_fn(|i| (sky_irr[i] * lv[i] * env_diffuse).extend(1.0).to_array())
        },
        env_indoor: indoor_irr.map(|e| (e * env_diffuse).extend(1.0).to_array()),
        sky_grad: [
            (c0 * c0).extend(env_diffuse).to_array(),
            (c1 * c1).extend(env_specular).to_array(),
        ],
        lamp1: lamp1.extend(1.0).to_array(),
        ssao: {
            // Studio's SSAOParams.xy: half the projection's diagonal.
            let f = 1.0 / (cam.fov_deg.to_radians() * 0.5).tan();
            let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
            // (Off for an orthographic camera: the AO reconstructs a
            // perspective view.)
            [0.5 * f / aspect, 0.5 * f, mech::NEAR, if cam.ortho.is_some() { 0.0 } else { 1.0 }]
        },
        // w: 1 dims the drawn skybox by the tint (Studio's default sky, whose
        // night faces rustudio stands in for); a custom Sky's faces take
        // Studio's time-of-day levels in the shader instead (sky_rot.w).
        sky_tint: sky_tint(sun.y).extend(if default_sky { 1.0 } else { 0.0 }).to_array(),
        lightgrid: [0.0; 4],
        // w 1 = an Atmosphere: rgb its haze toward the horizon (world GUI fog).
        atmos: haze_cube
            .map(|c| ((c[0] + c[1] + c[4] + c[5]) * 0.25).extend(1.0).to_array())
            .unwrap_or([0.0; 4]),
        atmos_cube: haze_cube
            .map(|c| c.map(|e| e.extend(1.0).to_array()))
            .unwrap_or([[0.0; 4]; 6]),
        atmos_sky: atmos
            .map(|a| [a.fog_offset[0], a.fog_offset[1], a.glare, a.offset])
            .unwrap_or([0.0; 4]),
        atmos_glare: atmos
            .map(|a| a.glare_exp.extend(0.0).to_array())
            .unwrap_or([0.0; 4]),
        // Only ShadowMap / Future part shaders reflect the sky and draw the
        // sun's highlight (Voxel / Compatibility's FS uses the scale for
        // the sun highlight only).
        env_spec: if !soft_style(d) || mech::TECHNOLOGY_QUALITY.contains(&tech) {
            [env_specular, mech::ENV_SPEC_ROUGHNESS, 1.0, 0.0]
        } else {
            [0.0, mech::ENV_SPEC_ROUGHNESS, 0.0, 0.0]
        },
        lightocc: [0.0; 4],
        dof: fx.dof.unwrap_or([0.0; 4]),
        perf: [
            // Shading detail: 2 high, 1 medium, 0 low (the Performance page's
            // choice list is high, medium, low).
            match rustudio_core::rustudio_settings::performance_value("detail") {
                "low" => 0.0,
                "medium" => 1.0,
                _ => 2.0,
            },
            if std::env::var_os("RUSTUDIO_PROBE_FLAT").is_some() || std::env::var_os("RUSTUDIO_PROBE_FLAT_PARTS").is_some() { 1.0 } else { 0.0 },
            if std::env::var_os("RUSTUDIO_PROBE_FLAT_TERRAIN").is_some() { 1.0 } else { 0.0 },
            0.0,
        ],

    };
    if std::env::var_os("RUSTUDIO_GLOBALS_DEBUG").is_some() {
        static LAST: std::sync::Mutex<Vec<(u32, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());
        let mut last = LAST.lock().unwrap();
        let key = (clock * 100.0) as u32 ^ (tech << 24);
        let due = match last.iter_mut().find(|e| e.0 == key) {
            Some(e) if e.1.elapsed().as_secs_f32() < 5.0 => false,
            Some(e) => { e.1 = std::time::Instant::now(); true }
            None => { last.push((key, std::time::Instant::now())); true }
        };
        if due {
            eprintln!("ATMOS| irr {:?} cube {:?} c0 {:?} c1 {:?}", sky_irr, globals.atmos_cube, c0, c1);
            let gl = &globals;
            eprintln!(
                "GLOBALS| tech {tech} clock {clock} sun_dir {:?} sun_color {:?} ambient {:?} outdoor {:?} params {:?} env_spec {:?} lamp1 {:?} env {:?} env_indoor {:?} sky_grad {:?} bloom {:?} lightgrid {:?} fog {:?} cc {:?}",
                gl.sun_dir, gl.sun_color, gl.ambient, gl.outdoor, gl.params, gl.env_spec, gl.lamp1, gl.env, gl.env_indoor, gl.sky_grad, gl.bloom, gl.lightgrid, gl.fog, gl.cc
            );
        }
    }
    let tonemap_mode = if raw_debug || saved_tech.is_none() {
        crate::postfx::TonemapMode::RawSqrt
    } else if fx.retro || saved_tech.is_some_and(|t| mech::TECHNOLOGY_NO_TONE_CURVE.contains(&t)) {
        crate::postfx::TonemapMode::Retro
    } else {
        crate::postfx::TonemapMode::Default
    };
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    let tan_y = (cam.fov_deg.to_radians() * 0.5).tan().max(1e-4);
    let tan_x = tan_y * aspect;
    let sun_u = sun.normalize_or_zero();
    let cam_fwd = cam.forward();
    let sun_view = Vec3::new(sun_u.dot(cam.right()), sun_u.dot(cam.up()), -sun_u.dot(cam_fwd));
    let sun_ndc = if sun_view.z < -1e-4 {
        [
            sun_view.x / (-sun_view.z * tan_x),
            sun_view.y / (-sun_view.z * tan_y),
        ]
    } else {
        [0.0, 0.0]
    };
    let (sr_intensity, sr_spread) = fx.sun_rays.unwrap_or((0.0, 0.1));
    let postfx = crate::postfx::PostFxParams {
        exposure: 2f32.powf(exposure),
        tonemap_mode,
        raw_debug,
        terrain_capture_id: false,
        post_fx_tier: 3,
        bloom_allowed: true,
        bloom_effect_enabled: fx.bloom.is_some(),
        bloom_intensity: bloom_i,
        bloom_size,
        bloom_threshold: bloom_thr,
        blur_size: if raw_debug { 0.0 } else { fx.blur },
        dof: if raw_debug { [0.0; 4] } else { fx.dof.unwrap_or([0.0; 4]) },
        cc_enabled: fx_cc_on,
        cc_brightness: ccb,
        cc_contrast: ccc,
        cc_saturation: ccs,
        cc_tint: cct.to_array(),
        sun_rays_enabled: fx.sun_rays.is_some() && !raw_debug,
        sun_rays_intensity: sr_intensity,
        sun_rays_spread: sr_spread,
        sun_dir_world: sun_u.to_array(),
        cam_forward: cam_fwd.to_array(),
        sun_ndc,
        sun_view: sun_view.to_array(),
        inv_proj_xy: [tan_x, tan_y],
        ssao_half_diag: [globals.ssao[0], globals.ssao[1]],
        near_plane: mech::NEAR,
        is_ortho: cam.ortho.is_some(),
    };
    FrameLighting {
        globals,
        postfx,
        sun: light_dir,
        sun_true: sun,
        shadows: shadows && light_irr > 0.0,
        shadow_softness: f32_of(get("ShadowSoftness")).unwrap_or(mech::SHADOW_SOFTNESS).clamp(0.0, mech::SHADOW_SOFTNESS_MAX),
    }
}

// ------------------------------------------------------------ local lights

/// Lights in the document (rebuilt when the tree changes).
#[derive(Default)]
pub struct LightCache {
    doc_id: u64,
    /// The lights under Workspace, in tree order (then as added), kept from
    /// the document's journal: rescanning Place B's 116k instances on every
    /// tree change was most of the lighting time.
    lights: Vec<Ref>,
    set: std::collections::HashSet<Ref>,
    records: rustc_hash::FxHashMap<Ref, Option<CachedLight>>,
    dependents: rustc_hash::FxHashMap<Ref, Vec<Ref>>,
    soft: Option<bool>,
}

struct CachedLight {
    raw: LightRaw,
    unit_color: Vec3,
    brightness: f32,
}

impl CachedLight {
    fn set_brightness(&mut self, brightness: f32) {
        self.brightness = brightness;
        self.raw.color[..3].copy_from_slice(&(self.unit_color * brightness).to_array());
    }
}

fn is_light(d: &Document, r: Ref) -> bool {
    d.dom
        .get_by_ref(r)
        .is_some_and(|i| matches!(i.class.as_str(), "PointLight" | "SpotLight" | "SurfaceLight"))
}

impl LightCache {
    fn refresh(&mut self, d: &mut Document) {
        let marks = d.marks_for(doc::Reader::Lights);
        if let doc::Marks::Some(marks) = marks {
            if self.doc_id == d.id {
                let mut workspace = None;
                let mut removed = false;
                let mut topology_changed = false;
                for mark in marks {
                    match mark {
                        doc::Mark::Tree(root) => {
                            let ws = *workspace.get_or_insert_with(|| d.workspace());
                            for r in d.subtree(root) {
                                let keep = is_light(d, r) && ws.is_some_and(|w| d.in_world(w, r));
                                topology_changed |= keep || self.dependents.contains_key(&r);
                                if keep && self.set.insert(r) {
                                    self.lights.push(r);
                                } else if !keep && self.set.remove(&r) {
                                    removed = true;
                                }
                            }
                        }
                        doc::Mark::Prop(r, name) => {
                            if self.set.contains(&r) {
                                if name == "Brightness" {
                                    if let Some(Some(light)) = self.records.get_mut(&r) {
                                        light.set_brightness(f32_of(d.prop_key(r, name)).unwrap_or(1.0));
                                    }
                                } else {
                                    self.records.remove(&r);
                                }
                            } else if !appearance_only(name.as_str()) {
                                // (an ancestor a light's position reads: a
                                // part's CFrame or Size, an attachment's or
                                // bone's placement under any of its names)
                                if let Some(lights) = self.dependents.get(&r) {
                                    for light in lights {
                                        self.records.remove(light);
                                    }
                                }
                            }
                        }
                    }
                }
                if removed || topology_changed {
                    self.records.clear();
                    self.dependents.clear();
                }
                if removed {
                    let set = &self.set;
                    self.lights.retain(|r| set.contains(r));
                }
                return;
            }
        }
        self.records.clear();
        self.dependents.clear();
        self.doc_id = d.id;
        self.lights = match d.workspace() {
            Some(ws) => d
                .dom
                .descendants_of(ws)
                .filter(|i| {
                    matches!(
                        i.class.as_str(),
                        "PointLight" | "SpotLight" | "SurfaceLight"
                    )
                })
                .map(|i| i.referent())
                .filter(|r| d.in_world(ws, *r))
                .collect(),
            None => Vec::new(),
        };
        self.set = self.lights.iter().copied().collect();
    }

    /// The parts lights hang from (directly or by an Attachment): they
    /// don't block their own light (round 26, lightocc.rs).
    pub fn hosts(&mut self, d: &mut Document) -> std::collections::HashSet<Ref> {
        self.refresh(d);
        let mut out = std::collections::HashSet::new();
        for &l in &self.lights {
            let Some(mut h) = d.dom.get_by_ref(l).map(|i| i.parent()) else { continue };
            while d.class_of(h).is_some_and(|c| doc::is_a(&c, "Attachment")) {
                match d.dom.get_by_ref(h) {
                    Some(i) => h = i.parent(),
                    None => continue,
                }
            }
            out.insert(h);
        }
        out
    }

    /// Enabled lights in the shader's layout. Realistic keeps the nearest
    /// MAX_LIGHTS reaching the view; Soft gathers the whole voxel volume.
    pub fn gather(
        &mut self,
        d: &mut Document,
        cam: &Camera,
        planes: &dyn Fn(Vec3, f32) -> bool,
        voxel_lights: bool,
    ) -> Vec<LightRaw> {
        self.refresh(d);
        if override_flag("nolocal") {
            return Vec::new();
        }
        let d: &Document = d;
        let soft = voxel_lights || soft_style(d);
        if self.soft.replace(soft) != Some(soft) {
            self.records.clear();
        }
        let mut out: Vec<(f32, LightRaw)> = Vec::with_capacity(self.lights.len());
        let mut rebuilt = 0;
        for &l in &self.lights {
            let record = self.records.entry(l).or_insert_with(|| {
                rebuilt += 1;
                // Track the parents used by attachment world transforms.
                // Tree marks on ancestors also invalidate the layer's records.
                let mut parent = d.dom.get_by_ref(l).map(|i| i.parent());
                while let Some(i) = parent.and_then(|r| d.dom.get_by_ref(r)) {
                    if matches!(i.class.as_str(), "Workspace" | "DataModel") {
                        break;
                    }
                    let users = self.dependents.entry(i.referent()).or_default();
                    if !users.contains(&l) {
                        users.push(l);
                    }
                    parent = Some(i.parent());
                }
                prepare_light(d, l, soft)
            });
            let Some(light) = record else { continue };
            if light.brightness <= 0.0 {
                continue;
            }
            let raw = light.raw;
            let centre = Vec3::from_array(raw.pos_range[..3].try_into().unwrap());
            let [a, b, ..] = raw.extra;
            if !soft && !planes(centre, raw.pos_range[3] + (a * a + b * b).sqrt()) {
                continue;
            }
            let dist = if soft { 0.0 } else { (centre - cam.pos).length() };
            out.push((dist, raw));
        }
        rustudio_core::bench::add("lt_records_rebuilt", 0.0, rebuilt);
        if !soft {
            out.sort_by(|a, b| a.0.total_cmp(&b.0));
            out.truncate(mech::MAX_LIGHTS);
        }
        // Realistic: the shadowed lights whose influence sphere is nearest the
        // camera (within LOCAL_LIGHT_DIST) get a cube shadow map each (Studio's
        // Future clustered lights). Sorting by effective distance to the light's
        // influence sphere ensures large-range SurfaceLights reaching the view
        // aren't starved by distant small-range lights.
        if !soft {
            let mut order: Vec<(f32, f32, usize)> = out
                .iter()
                .enumerate()
                .filter_map(|(i, (dist, l))| {
                    if l.extra[2] <= 0.5 {
                        return None;
                    }
                    let [a, b, ..] = l.extra;
                    let radius = l.pos_range[3] + (a * a + b * b).sqrt();
                    let eff = (*dist - radius).max(0.0);
                    if eff <= mech::LOCAL_LIGHT_DIST {
                        Some((eff, *dist, i))
                    } else {
                        None
                    }
                })
                .collect();
            order.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
            for (slot, (_, _, idx)) in order.into_iter().take(mech::LSHADOW_MAX_LIGHTS).enumerate() {
                out[idx].1.extra[3] = slot as f32;
            }
        }
        if std::env::var_os("RUSTUDIO_LIGHT_DEBUG").is_some() {
            for (dist, l) in out.iter().take(8) {
                eprintln!("light d={dist:.1} {:?}", l);
            }
        }
        out.into_iter().map(|(_, l)| l).collect()
    }
}

/// Properties that can't move a light's host (a part or attachment above
/// it): a script recolouring a lamp's part every frame keeps its record.
fn appearance_only(name: &str) -> bool {
    matches!(
        name,
        "Color" | "BrickColor" | "Color3uint8" | "Material" | "MaterialVariant" | "Transparency"
            | "LocalTransparencyModifier" | "Reflectance" | "CastShadow" | "Name" | "CanCollide"
            | "CanTouch" | "CanQuery" | "Anchored" | "Massless" | "AssemblyLinearVelocity"
            | "AssemblyAngularVelocity" | "Velocity" | "RotVelocity"
    )
}

fn prepare_light(d: &Document, l: Ref, soft: bool) -> Option<CachedLight> {
    let Some(class) = d.class_of(l) else { return None };
    if !boolp(d, l, "Enabled", true) {
        return None;
    }
    let brightness = f32_of(d.prop(l, "Brightness")).unwrap_or(1.0);
    let range = f32_of(d.prop(l, "Range"))
        .unwrap_or(8.0)
        .clamp(0.0, mech::LIGHT_MAX_RANGE);
    if range <= 0.0 {
        return None;
    }
    let Some(host) = d.dom.get_by_ref(l).map(|i| i.parent()) else {
        return None;
    };
    let Some(hclass) = d.class_of(host) else {
        return None;
    };
    // Where the light sits: a part's centre, or an attachment's world position.
    let (pos, rot, size) = if doc::is_a(&hclass, "BasePart") {
        let Some(xf) = xform_of(d, host) else {
            return None;
        };
        (
            xf.pos,
            xf.rot,
            vec3_of(d.prop(host, "Size")).unwrap_or(Vec3::ONE),
        )
    } else if doc::is_a(&hclass, "Attachment") {
        let Some(xf) = rustudio_core::attachment::world(d, host, true) else { return None };
        (xf.pos, xf.rot, Vec3::ZERO)
    } else {
        return None;
    };
    let source_color = d.prop(l, "Color");
    let rgb = color_of(source_color.clone()).unwrap_or([255, 255, 255]);
    // Realistic (Future): Studio's light record holds Color^2.2 ×
    // Brightness (read from its light buffer to 4 digits).
    let unit_color = if soft {
        // Studio: round(Color * 256), saturate to a byte,
        // then square byte/255 before accumulating voxel radiance.
        let c = match source_color {
            Some(Variant::Color3(c)) => Vec3::new(c.r, c.g, c.b),
            _ => Vec3::from_array(rgb.map(|v| v as f32 / 255.0)),
        };
        let c = (c * 256.0).round().clamp(Vec3::ZERO, Vec3::splat(255.0)) / 255.0;
        c * c
    } else {
        (Vec3::new(rgb[0] as f32, rgb[1] as f32, rgb[2] as f32) / 255.0).powf(2.2)
    };
    // Face (NormalId): Right 0, Top 1, Back 2, Left 3, Bottom 4, Front 5.
    let face = |f: u32| match f {
        0 => Vec3::X,
        1 => Vec3::Y,
        2 => Vec3::Z,
        3 => -Vec3::X,
        4 => -Vec3::Y,
        _ => -Vec3::Z,
    };
    let cutoff = |angle: f32| {
        if soft {
            // CPU voxel writer's approximation to 1-cos(Angle/2),
            // coefficients at Studio, full angle in degrees.
            let a = angle * angle;
            1.0 - ((3.8077018e-5 - (2.4119384e-10 - a * 5.733313e-16) * a) * a + 1e-15)
        } else { (angle.to_radians() * 0.5).cos() }
    };
    let (kind, dir, cos_cut, centre, half) = match class.as_str() {
        "SpotLight" => {
            let n = face(enum_u32(d.prop(l, "Face")).unwrap_or(5));
            let angle = f32_of(d.prop(l, "Angle"))
                .unwrap_or(90.0)
                .clamp(0.0, mech::LIGHT_MAX_ANGLE);
            (
                1.0,
                rot * n,
                cutoff(angle),
                pos,
                [0.0, 0.0],
            )
        }
        "SurfaceLight" => {
            let fid = enum_u32(d.prop(l, "Face")).unwrap_or(5);
            let n = face(fid);
            let angle = f32_of(d.prop(l, "Angle"))
                .unwrap_or(90.0)
                .clamp(0.0, mech::LIGHT_MAX_ANGLE);
            let h = size * 0.5;
            let along = n.abs();
            let centre = pos + rot * (n * (h * along).length());
            let rest = h * (Vec3::ONE - along);
            let mut ext = [rest.x, rest.y, rest.z];
            ext.sort_by(|a, b| b.total_cmp(a));
            (
                2.0,
                rot * n,
                cutoff(angle),
                centre,
                [ext[0], ext[1]],
            )
        }
        _ => (0.0, Vec3::Y, -1.0, pos, [0.0, 0.0]),
    };
    let fid = enum_u32(d.prop(l, "Face")).unwrap_or(5) % 3;
    let (u, v) = match fid {
        0 => (Vec3::Y, Vec3::Z), 1 => (Vec3::X, Vec3::Z), _ => (Vec3::X, Vec3::Y),
    };
    let mut light = CachedLight {
        raw: LightRaw {
            surface_u: (rot * u).extend(size.dot(u) * 0.5).to_array(),
            surface_v: (rot * v).extend(size.dot(v) * 0.5).to_array(),
            pos_range: centre.extend(range).to_array(),
            color: [0.0, 0.0, 0.0, kind],
            dir: dir.normalize_or_zero().extend(cos_cut).to_array(),
            // z: Shadows on, parts block it (round 26, lightocc.rs).
            // w: its shadow-map slot (set below; −1 none).
            extra: [half[0], half[1], if boolp(d, l, "Shadows", false) { 1.0 } else { 0.0 }, -1.0],
        },
        unit_color,
        brightness,
    };
    light.set_brightness(brightness);
    Some(light)
}

// ------------------------------------------------------------- sky faces

/// Sky instances are selected by class; games commonly rename clones.
fn sky_child(d: &Document, parent: Ref) -> Option<Ref> {
    d.dom.get_by_ref(parent)?.children().iter().copied()
        .find(|r| d.class_of(*r).is_some_and(|c| c == "Sky"))
}

/// Sky.SkyboxOrientation in degrees (zero without a Sky). Studio turns the
/// whole skybox by it: the sky drawn toward a direction d is the unturned
/// sky's toward R⁻¹·d, R = Rz·Rx·Ry (each Roblox's rotation about that axis)
/// (docs/MECHANICS.md, "Skybox orientation").
pub fn sky_orientation(d: &Document) -> Vec3 {
    d.find_child(d.dom.root_ref(), "Lighting")
        .and_then(|l| sky_child(d, l))
        .and_then(|s| match d.prop(s, "SkyboxOrientation") {
            Some(Variant::Vector3(v)) => Some(Vec3::new(v.x, v.y, v.z)),
            _ => None,
        })
        .filter(|v| v.is_finite())
        .unwrap_or(Vec3::ZERO)
}

/// The six skybox images (Bk, Dn, Ft, Lf, Rt, Up order as Sky names them):
/// the Sky's own, else Studio's default sky.
pub fn sky_faces(d: &Document) -> [String; 6] {
    let sky = d
        .find_child(d.dom.root_ref(), "Lighting")
        .and_then(|l| sky_child(d, l));
    let names = [
        "SkyboxBk", "SkyboxDn", "SkyboxFt", "SkyboxLf", "SkyboxRt", "SkyboxUp",
    ];
    let defaults = ["bk", "dn", "ft", "lf", "rt", "up"];
    let mut out: [String; 6] = Default::default();
    for i in 0..6 {
        let own = sky.and_then(|s| match d.prop(s, names[i]) {
            Some(Variant::ContentId(c)) if !c.as_str().is_empty() => Some(c.as_str().to_string()),
            Some(Variant::Content(c)) => {
                c.as_uri().filter(|u| !u.is_empty()).map(|u| u.to_string())
            }
            _ => None,
        });
        out[i] =
            own.unwrap_or_else(|| format!("rbxasset://textures/sky/sky512_{}.tex", defaults[i]));
    }
    out
}

/// Light arriving from the skybox on a surface facing each axis (+X, −X,
/// +Y, −Y, +Z, −Z), from six square RGBA8 faces in the same cube order:
/// E(n) = 1/π ∫ L(ω) max(n·ω, 0) dω with texels squared (gamma 2.0). The
/// shader blends the six by n² (an "ambient cube").
pub fn sky_irradiance(faces: &[Vec<u8>; 6], size: u32) -> [Vec3; 6] {
    let axes = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
    let mut sum = [Vec3::ZERO; 6];
    let step = (size / 32).max(1);
    for (f, px) in faces.iter().enumerate() {
        for y in (0..size).step_by(step as usize) {
            for x in (0..size).step_by(step as usize) {
                let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
                // Face texel → direction (wgpu / D3D cube convention).
                let d = match f {
                    0 => Vec3::new(1.0, -v, -u),
                    1 => Vec3::new(-1.0, -v, u),
                    2 => Vec3::new(u, 1.0, v),
                    3 => Vec3::new(u, -1.0, -v),
                    4 => Vec3::new(u, -v, 1.0),
                    _ => Vec3::new(-u, -v, -1.0),
                };
                let w = 1.0 / (1.0 + u * u + v * v).powf(1.5);
                let o = ((y * size + x) * 4) as usize;
                let Some(t) = px.get(o..o + 3) else { continue };
                let l = sq([t[0], t[1], t[2]]);
                let d = d.normalize();
                for (k, a) in axes.iter().enumerate() {
                    let c = a.dot(d);
                    if c > 0.0 {
                        sum[k] += l * c * w;
                    }
                }
            }
        }
    }
    // Each sample covers (2·step/size)² of the face; the whole sphere is 4π.
    let texel = (2.0 * step as f32 / size as f32).powi(2);
    sum.map(|s| s * texel / std::f32::consts::PI)
}


#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::{InstanceBuilder, WeakDom};

    /// The kept light records give what a fresh gather gives after
    /// brightness, light, host and appearance writes and a reparent.
    #[test]
    fn kept_light_records_match_a_fresh_gather() {
        use rbx_dom_weak::types::{CFrame, Matrix3, Vector3};
        use rbx_dom_weak::ustr;
        let mut d = Document::empty();
        let root = d.dom.root_ref();
        let ws = d.insert(root, InstanceBuilder::new("Workspace"));
        let at = |x: f32| CFrame::new(Vector3::new(x, 2.0, 0.0), Matrix3::identity());
        let part = d.insert(ws, InstanceBuilder::new("Part")
            .with_property("Size", Vector3::new(4.0, 2.0, 1.0))
            .with_property("CFrame", at(0.0)));
        let other = d.insert(ws, InstanceBuilder::new("Part")
            .with_property("Size", Vector3::new(2.0, 2.0, 2.0))
            .with_property("CFrame", at(30.0)));
        let point = d.insert(part, InstanceBuilder::new("PointLight").with_property("Range", 12.0f32));
        let att = d.insert(part, InstanceBuilder::new("Attachment")
            .with_property("CFrame", CFrame::new(Vector3::new(1.0, 0.5, 0.0), Matrix3::identity())));
        d.insert(att, InstanceBuilder::new("SpotLight").with_property("Brightness", 3.0f32));
        d.insert(part, InstanceBuilder::new("SurfaceLight").with_property("Face", rbx_dom_weak::types::Enum::from_u32(1)));
        let cam = Camera::default();
        let all = |_: Vec3, _: f32| true;
        for voxel in [false, true] {
            let mut kept = LightCache::default();
            let mut check = |d: &mut Document, what: &str| {
                let a = kept.gather(d, &cam, &all, voxel);
                let b = LightCache::default().gather(d, &cam, &all, voxel);
                assert_eq!(bytemuck::cast_slice::<_, u8>(&a), bytemuck::cast_slice::<_, u8>(&b), "{what} voxel {voxel}");
                a.len()
            };
            assert_eq!(check(&mut d, "start"), 3);
            d.set_prop_live(point, ustr("Brightness"), Variant::Float32(2.5));
            check(&mut d, "brightness");
            d.set_prop_live(point, ustr("Brightness"), Variant::Float32(0.0));
            assert_eq!(check(&mut d, "dark"), 2);
            d.set_prop_live(point, ustr("Brightness"), Variant::Float32(1.0));
            d.set_prop_live(point, ustr("Color"), Variant::Color3(rbx_dom_weak::types::Color3::new(1.0, 0.5, 0.0)));
            check(&mut d, "light colour");
            d.set_prop_live(part, ustr("Color"), Variant::Color3(rbx_dom_weak::types::Color3::new(0.0, 0.5, 0.0)));
            check(&mut d, "part colour");
            d.set_prop_live(part, ustr("CFrame"), Variant::CFrame(at(5.0)));
            check(&mut d, "part moved");
            d.set_prop_live(part, ustr("Size"), Variant::Vector3(Vector3::new(6.0, 1.0, 1.0)));
            check(&mut d, "part resized");
            d.set_prop_live(att, ustr("Position"), Variant::Vector3(Vector3::new(0.0, 3.0, 0.0)));
            d.set_prop_live(att, ustr("CFrame"), Variant::CFrame(CFrame::new(Vector3::new(0.0, 3.0, 0.0), Matrix3::identity())));
            check(&mut d, "attachment moved");
            d.reparent(&[att], other);
            check(&mut d, "reparented");
            d.set_prop_live(point, ustr("Enabled"), Variant::Bool(false));
            assert_eq!(check(&mut d, "disabled"), 2);
            d.set_prop_live(point, ustr("Enabled"), Variant::Bool(true));
            d.reparent(&[att], part);
        }
    }

    #[test]
    fn renamed_sky_and_camera_effects() {
        let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(
            InstanceBuilder::new("Lighting").with_name("Lighting")
                .with_child(InstanceBuilder::new("Sky").with_name("FactorySky")
                    .with_property("SkyboxBk", rbx_dom_weak::types::ContentId::from("rbxassetid://123")))
                .with_child(InstanceBuilder::new("DepthOfFieldEffect")
                    .with_property("FocusDistance", 4.72f32)
                    .with_property("FarIntensity", 0.567f32)
                    .with_property("NearIntensity", 0.0f32)
                    .with_property("InFocusRadius", 0.0f32))
                .with_child(InstanceBuilder::new("DepthOfFieldEffect").with_property("Enabled", false).with_property("FocusDistance", 999.0f32))));
        let d = Document::from_play_dom(dom, "effects".into());
        assert!(soft_style(&d), "live documents bypass file-load lighting migration");
        assert_eq!(sky_faces(&d)[0], "rbxassetid://123");
        assert_eq!(effects(&d, None).dof, Some([4.72, 0.0, 0.0, 0.567]));

    }

    #[test]
    fn post_effects_stacking_and_parenting_match_studio() {
        use rbx_dom_weak::types::{Color3, Enum};
        let dom = WeakDom::new(
            InstanceBuilder::new("DataModel")
                .with_child(
                    InstanceBuilder::new("Lighting")
                        .with_name("Lighting")
                        .with_child(
                            InstanceBuilder::new("BloomEffect")
                                .with_property("Intensity", 0.5f32)
                                .with_property("Size", 16.0f32)
                                .with_property("Threshold", 1.5f32),
                        )
                        .with_child(
                            InstanceBuilder::new("BloomEffect")
                                .with_property("Enabled", false)
                                .with_property("Intensity", 9.0f32),
                        )
                        .with_child(
                            InstanceBuilder::new("DepthOfFieldEffect")
                                .with_property("FocusDistance", 30.0f32)
                                .with_property("InFocusRadius", 25.0f32)
                                .with_property("NearIntensity", 0.2f32)
                                .with_property("FarIntensity", 0.8f32),
                        )
                        .with_child(
                            InstanceBuilder::new("SunRaysEffect")
                                .with_property("Intensity", 0.1f32)
                                .with_property("Spread", 0.3f32),
                        )
                        .with_child(
                            InstanceBuilder::new("BlurEffect").with_property("Size", 8.0f32),
                        )
                        .with_child(
                            InstanceBuilder::new("ColorCorrectionEffect")
                                .with_property("Brightness", 0.1f32)
                                .with_property("Contrast", 0.2f32)
                                .with_property("Saturation", -0.3f32)
                                .with_property("TintColor", Color3::new(1.0, 0.5, 0.5)),
                        ),
                )
                .with_child(
                    InstanceBuilder::new("Workspace")
                        .with_name("Workspace")
                        .with_child(
                            InstanceBuilder::new("Camera")
                                .with_name("Camera")
                                .with_child(
                                    InstanceBuilder::new("BloomEffect")
                                        .with_property("Intensity", 0.75f32)
                                        .with_property("Size", 40.0f32)
                                        .with_property("Threshold", 0.9f32),
                                )
                                .with_child(
                                    InstanceBuilder::new("DepthOfFieldEffect")
                                        .with_property("FocusDistance", 20.0f32)
                                        .with_property("InFocusRadius", 8.0f32)
                                        .with_property("NearIntensity", 0.6f32)
                                        .with_property("FarIntensity", 0.3f32),
                                )
                                .with_child(
                                    InstanceBuilder::new("SunRaysEffect")
                                        .with_property("Intensity", 0.15f32)
                                        .with_property("Spread", 0.9f32),
                                )
                                .with_child(
                                    InstanceBuilder::new("BlurEffect").with_property("Size", 20.0f32),
                                )
                                .with_child(
                                    InstanceBuilder::new("ColorCorrectionEffect")
                                        .with_property("Brightness", -0.05f32)
                                        .with_property("Contrast", 0.3f32)
                                        .with_property("Saturation", 0.5f32)
                                        .with_property("TintColor", Color3::new(0.5, 1.0, 0.5)),
                                )
                                // ColorGradingEffect under Camera must be ignored (Lighting only).
                                .with_child(
                                    InstanceBuilder::new("ColorGradingEffect")
                                        .with_property("TonemapperPreset", Enum::from_u32(1)),
                                ),
                        ),
                ),
        );
        let d = Document::from_play_dom(dom, "stack".into());
        let ws = d.workspace().unwrap();
        let cam = d.find_child(ws, "Camera").unwrap();
        let fx = effects(&d, Some(cam));
        // Bloom: sum intensity, max size, min threshold
        let (bi, bs, bt) = fx.bloom.unwrap();
        assert!((bi - 1.25).abs() < 1e-5);
        assert!((bs - 40.0).abs() < 1e-5);
        assert!((bt - 0.9).abs() < 1e-5);
        // DOF: max FocusDistance (30), LAST InFocusRadius (8), max Near (0.6), max Far (0.8)
        let dof = fx.dof.unwrap();
        assert!((dof[0] - 30.0).abs() < 1e-5);
        assert!((dof[1] - 8.0).abs() < 1e-5);
        assert!((dof[2] - 0.6).abs() < 1e-5);
        assert!((dof[3] - 0.8).abs() < 1e-5);
        // SunRays: sum intensity, max spread
        let (sri, srs) = fx.sun_rays.unwrap();
        assert!((sri - 0.25).abs() < 1e-5);
        assert!((srs - 0.9).abs() < 1e-5);
        // Blur: max size
        assert!((fx.blur - 20.0).abs() < 1e-5);
        // CC: sum B/C/S, multiply tint
        let (b, c, s, t) = fx.cc.unwrap();
        assert!((b - 0.05).abs() < 1e-5);
        assert!((c - 0.5).abs() < 1e-5);
        assert!((s - 0.2).abs() < 1e-5);
        assert!((t.x - 0.5).abs() < 1e-2 && (t.y - 0.5).abs() < 1e-2 && (t.z - 0.25).abs() < 1e-2);
        // ColorGradingEffect under Camera is ignored
        assert!(!fx.retro);
    }

    /// A Bone's Transform moves what hangs under it (selftest/attachment.rs
    /// covers the frames themselves; this is the light that reads them).
    #[test]
    fn bone_light_uses_transformed_frame() {
        use rustudio_core::math::Xform;
        use glam::Mat3;
        use rbx_dom_weak::InstanceBuilder;
        let cf = |p: Vec3, r: Mat3| Variant::CFrame(Xform { pos: p, rot: r }.to_cframe());
        let mut d = Document::new_place();
        let ws = d.find_child(d.dom.root_ref(), "Workspace").unwrap();
        let p = d.insert(ws, InstanceBuilder::new("Part").with_property("CFrame", cf(Vec3::new(10., 0., 0.), Mat3::IDENTITY)));
        let b = d.insert(p, InstanceBuilder::new("Bone").with_property("CFrame", cf(Vec3::Y, Mat3::IDENTITY)).with_property("Transform", cf(Vec3::X, Mat3::IDENTITY)));
        d.insert(b, InstanceBuilder::new("PointLight"));
        let mut cache = LightCache::default();
        assert!(cache.hosts(&mut d).contains(&p));
        let lights = cache.gather(&mut d, &Camera::default(), &|_, _| true, false);
        assert_eq!(lights.len(), 1);
        assert_eq!(&lights[0].pos_range[..3], &[11., 1., 0.]);
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    #[test]
    fn time_of_day_alone_sets_the_clock() {
        // A live Team Create Lighting carries only TimeOfDay.
        let h = clock_hours(None, Some(Variant::String("02:48:00".into())));
        assert!((h - 2.8).abs() < 1e-4, "{h}");
        assert_eq!(clock_hours(Some(Variant::Float32(7.75)), Some(Variant::String("02:48:00".into()))), 7.75);
        assert_eq!(clock_hours(None, None), mech::CLOCK_TIME);
        // The sun is below the horizon at 02:48, above it at 14:00 (latitude 60).
        assert!(sun_direction(clock_hours(None, Some(Variant::String("02:48:00".into()))), 60.0).y < 0.0);
        assert!(sun_direction(clock_hours(None, None), 60.0).y > 0.0);
    }
}
