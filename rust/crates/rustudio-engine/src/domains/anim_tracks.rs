//! The Animator's tracks stepped natively (frame hook after PreAnimation,
//! anim.rs `animate`), as Studio's Animator::animatorOnStepped /
//! evaluateAndAccumulateAnimationTracks / AnimationTrack::accumulate do.
//!
//! What stays in Luau (library.luau, run once per load, not per frame):
//! finding and parsing an AnimationId's KeyframeSequence / CurveAnimation
//! (`__rs_anim_load`), and sampling CurveAnimation channels
//! (`__rs_anim_sample`, called per curve joint). KeyframeSequence poses,
//! weights, fades, priorities, events and Motor6D.Transform writes are here.
//!
//! Measured in Studio (probes a_order / a_fade, client and server):
//! - The step runs after the PreAnimation handlers, before Stepped, with the
//!   simulation dt; a track advances by dt on the step after Play.
//! - Marker / KeyframeReached handlers run inside the step, before the joints
//!   are written (they see the previous pose).
//! - A non-looped track reaching its end: Stopped, then DidLoop; it then
//!   fades out linearly over 0.3 s from its current weight, whatever Play's
//!   fadeTime was. Stop() fades over 0.1 s by default. Ended fires when the
//!   weight reaches 0; the Animator lets the track go a step later.
//! - Transform = position (weighted) · slerp(identity, rotation, used weight).
//!
//! Measured in Studio 0.744 (Laser Tag blaster probes,
//! checks/laser-tag-parity/anim): Play() restarts at TimePosition 0 even
//! while the track plays (a blaster's Shoot:Play(0) every shot), fading from
//! the weight it has; a non-looped track at its end with Speed 0 has ended
//! too; Stop(0) on a playing track lets it go at once but WeightCurrent keeps
//! its value (the next Play fades from there, so a re-equipped Idle:Play()
//! snaps in); a curve clip's NamedKeyframeEvent markers are KeyframeReached
//! names; at a non-looped end the order is keyframes, Stopped, DidLoop.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use rustc_hash::FxHashSet;
use rustudio_core::doc::{Mark, Marks, Reader};

use glam::{Mat3, Quat, Vec3};
use mlua::{Function, Lua, Table, Value};
use rbx_dom_weak::types::{Ref, Variant};
use rbx_dom_weak::{ustr, Ustr};

use rustudio_core::math::Xform;
use crate::runtime::*;

struct Key {
    t: f64,
    cf: Xform,
    rotation: Quat,
    style: String,
    dir: String,
}

#[derive(Clone, Copy)]
struct FloatKey {
    t: f64,
    mode: u8,
    v: f64,
    left: f64,
    right: f64,
}

/// Studio ignores track contributions below 0.001 (measured: 0.0009 -> 0, 0.001 -> included).
const MIN_BLEND_WEIGHT: f64 = 0.001;

/// The name of Part0 and Part1, or the parent Bone and child Bone names.
type JointName = (Ustr, Ustr);
type KeyframeJoints = rustc_hash::FxHashMap<JointName, Vec<Key>>;

struct BoundJoint {
    name: JointName,
    instance: Ref,
    /// Flat clips carry only a target name. Keep their existing last-match rule.
    unqualified: bool,
}

impl BoundJoint {
    fn channel<'a, T>(&self, channels: &'a rustc_hash::FxHashMap<JointName, T>) -> Option<&'a T> {
        channels.get(&self.name).or_else(|| {
            if self.unqualified {
                channels.get(&(ustr(""), self.name.1))
            } else {
                None
            }
        })
    }
}
type Retargeter = super::anim_retarget::Retargeter;

struct Frame {
    t: f64,
    name: String,
    markers: Vec<(String, String)>,
}

pub struct Seq {
    /// Native keyframes can be sampled without the Lua VM.
    keys: KeyframeJoints,
    /// CurveAnimation channels are Lua tables and stay on the VM thread.
    curves: rustc_hash::FxHashMap<JointName, Table>,
    /// CurveAnimation FaceControls FACS channels (control name -> FloatCurve keys).
    facs: rustc_hash::FxHashMap<Ustr, Vec<FloatKey>>,
    frames: Vec<Frame>,
    length: f64,
    looped: bool,
    priority: Value,
    /// A CurveAnimation (GetTimeOfKeyframe refuses it, as Studio does).
    curve: bool,
    /// The clip's authoring rig (its AnimationRigData), for retargeting.
    rig: Option<Rc<super::anim_retarget::Rig>>,
}

struct Track {
    me: Value,
    anim: Value,
    owner: Ref,
    rig: Ref,
    seq: Option<Rc<Seq>>,
    pending: Option<String>,
    playing: bool,
    active: bool,
    ended: bool,
    end_pending: bool,
    start_at_end: bool,
    zero_weight_steps: u8,
    speed: f64,
    weight: f64,
    target: f64,
    fade: f64,
    fade_start: f64,
    fade_elapsed: f64,
    time: f64,
    looped: bool,
    looped_set: bool,
    priority: Value,
    priority_set: bool,
    /// Stop(0) let it go: not stepped, WeightCurrent kept (Studio).
    cut: bool,
    /// Bumped on every Play() / explicit TimePosition write for replication.
    play_gen: u64,
    /// True for tracks created by anim_replicate::sync (never echoed back).
    mirrored: bool,
}

#[derive(Default)]
pub struct Tracks {
    /// Load order (iteration order of the step).
    order: Vec<Ref>,
    all: HashMap<Ref, Track>,
    seqs: HashMap<String, Rc<Seq>>,
    /// Rig → cached animation bindings, refreshed when its topology changes.
    animated: rustc_hash::FxHashMap<Ref, RigBindings>,
    /// The order in which rigs began posing.
    animated_order: Vec<Ref>,
    /// rig → the rig Studio builds from it for retargeting (None: not an R15
    /// Humanoid rig), kept while it animates.
    char_rigs: HashMap<Ref, Option<Rc<super::anim_retarget::Rig>>>,
    /// (clip, rig) → its retargeter.
    retargeters: HashMap<(usize, Ref), Rc<super::anim_retarget::Retargeter>>,
    /// Last written Transform and previous Transform per joint, plus dt, for ApplyJointVelocities.
    prev_transforms: rustc_hash::FxHashMap<Ref, (Xform, Xform, f64)>,
    steps: u64,
}

struct RigBindings {
    joints: Rc<Vec<BoundJoint>>,
    face_controls: Vec<Ref>,
    /// Joint and Bone references, their ancestors inside the rig, and the
    /// endpoints that determine the binding names. A move out of the rig
    /// leaves no ancestry path back to it in the current document.
    dependencies: FxHashSet<Ref>,
}

fn tracks(lua: &Lua) -> mlua::AppDataRefMut<'_, Tracks> {
    // (One app-data lookup in the common case: this runs several times per
    // track per frame.)
    if let Some(t) = lua.app_data_mut::<Tracks>() {
        return t;
    }
    lua.set_app_data(Tracks::default());
    lua.app_data_mut::<Tracks>().expect("track state")
}

// ------------------------------------------------------------- sequences

fn xform_of(v: &Value) -> Option<Xform> {
    match v {
        Value::UserData(u) => crate::ltypes::cframe_of(u),
        _ => None,
    }
}

fn convert(lua: &Lua, t: &Table) -> mlua::Result<Seq> {
    let mut keyframes = KeyframeJoints::default();
    let mut curves = rustc_hash::FxHashMap::default();
    let jt: Table = t.get("joints")?;
    for pair in jt.pairs::<String, Table>() {
        let (parent, children) = pair?;
        for pair in children.pairs::<String, Table>() {
            let (name, list) = pair?;
            let name = (ustr(&parent), ustr(&name));
            if matches!(list.get::<Option<Table>>("curves"), Ok(Some(_))) {
                curves.insert(name, list);
                continue;
            }
            let mut keys = Vec::new();
            for k in list.sequence_values::<Table>() {
                let k = k?;
                let cf = xform_of(&k.get::<Value>("cf")?).unwrap_or_else(super::anim_ik::ident);
                keys.push(Key {
                    t: k.get("t")?,
                    cf,
                    rotation: Quat::from_mat3(&cf.rot),
                    style: k.get::<Option<String>>("style")?.unwrap_or_else(|| "Linear".into()),
                    dir: k.get::<Option<String>>("dir")?.unwrap_or_else(|| "In".into()),
                });
            }
            keyframes.insert(name, keys);
        }
    }
    let mut frames = Vec::new();
    for f in t.get::<Table>("frames")?.sequence_values::<Table>() {
        let f = f?;
        let mut markers = Vec::new();
        if let Ok(Some(ms)) = f.get::<Option<Table>>("markers") {
            for m in ms.sequence_values::<Table>() {
                let m = m?;
                markers.push((m.get::<String>(1)?, m.get::<Option<String>>(2)?.unwrap_or_default()));
            }
        }
        frames.push(Frame { t: f.get("t")?, name: f.get::<Option<String>>("name")?.unwrap_or_default(), markers });
    }
    let mut facs = rustc_hash::FxHashMap::default();
    if let Ok(Some(ft)) = t.get::<Option<Table>>("facs") {
        for pair in ft.pairs::<String, Table>() {
            let (name, list) = pair?;
            let mut keys = Vec::new();
            for k in list.sequence_values::<Table>() {
                let k = k?;
                keys.push(FloatKey {
                    t: k.get("t")?,
                    mode: k.get::<Option<u8>>("mode")?.unwrap_or(1),
                    v: k.get::<Option<f64>>("v")?.unwrap_or(0.0),
                    left: k.get::<Option<f64>>("left")?.unwrap_or(0.0),
                    right: k.get::<Option<f64>>("right")?.unwrap_or(0.0),
                });
            }
            if !keys.is_empty() {
                facs.insert(ustr(&name), keys);
            }
        }
    }
    let rig = match opt_inst(&t.get::<Value>("rig")?) {
        Some(r) => {
            let w = world(lua)?;
            super::anim_retarget::clip_rig(w.d(), r).map(Rc::new)
        }
        None => None,
    };
    Ok(Seq {
        rig,
        keys: keyframes,
        curves,
        facs,
        frames,
        length: t.get::<Option<f64>>("length")?.unwrap_or(0.0),
        looped: t.get::<Option<bool>>("loop")?.unwrap_or(false),
        priority: t.get("priority")?,
        curve: t.get::<Option<bool>>("curve")?.unwrap_or(false),
    })
}

/// The sequence for an id: (Some, _) loaded, (None, true) still
/// downloading, (None, false) unavailable.
fn load(lua: &Lua, id: &str) -> mlua::Result<(Option<Rc<Seq>>, bool)> {
    if let Some(s) = tracks(lua).seqs.get(id) {
        return Ok((Some(s.clone()), false));
    }
    let f: Function = lua.globals().get("__rs_anim_load")?;
    let (mut seq, mut still): (Value, bool) = f.call(id)?;
    // RUSTUDIO_DETERMINISTIC=1 (test runs): wait for the asset (it arrives on a background
    // thread at wall-clock time), so a track starts on the same frame on every run.
    if rbx_dom_weak::deterministic() {
        let t0 = std::time::Instant::now();
        while still && matches!(seq, Value::Nil) && t0.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            (seq, still) = f.call(id)?;
        }
    }
    let Value::Table(t) = seq else { return Ok((None, still)) };
    let s = Rc::new(convert(lua, &t)?);
    tracks(lua).seqs.insert(id.to_string(), s.clone());
    Ok((Some(s), false))
}

// --------------------------------------------------------------- poses

fn ease_in(style: &str, t: f64) -> f64 {
    use std::f64::consts::PI;
    match style {
        "Sine" => 1.0 - (t * PI / 2.0).cos(),
        "Cubic" => t * t * t,
        "Quart" => t.powi(4),
        "Quint" => t.powi(5),
        "Exponential" => {
            if t == 0.0 {
                0.0
            } else {
                2f64.powf(10.0 * (t - 1.0))
            }
        }
        "Circular" => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
        "Back" => {
            let s = 1.70158;
            t * t * ((s + 1.0) * t - s)
        }
        // Pose easing keeps the textbook 0.3 period (measured, selftest/anim.rs).
        "Elastic" => {
            if t == 0.0 || t == 1.0 {
                return t;
            }
            -(2f64.powf(10.0 * (t - 1.0))) * ((t - 1.075) * (2.0 * PI) / 0.3).sin()
        }
        "Bounce" => {
            let mut u = 1.0 - t;
            let b = if u < 1.0 / 2.75 {
                7.5625 * u * u
            } else if u < 2.0 / 2.75 {
                u -= 1.5 / 2.75;
                7.5625 * u * u + 0.75
            } else if u < 2.5 / 2.75 {
                u -= 2.25 / 2.75;
                7.5625 * u * u + 0.9375
            } else {
                u -= 2.625 / 2.75;
                7.5625 * u * u + 0.984375
            };
            1.0 - b
        }
        "Linear" => t,
        _ => t * t,
    }
}

fn ease(a: f64, style: &str, dir: &str) -> f64 {
    let a = a.clamp(0.0, 1.0);
    if style == "Linear" {
        return a;
    }
    match dir {
        "In" => ease_in(style, a),
        "InOut" => {
            if a < 0.5 {
                ease_in(style, a * 2.0) / 2.0
            } else {
                1.0 - ease_in(style, (1.0 - a) * 2.0) / 2.0
            }
        }
        _ => 1.0 - ease_in(style, 1.0 - a),
    }
}

/// Studio 0.746 measurements: only legacy Cubic reverses In / Out.
fn pose_alpha(a: f64, style: &str, dir: &str) -> f64 {
    use std::f64::consts::PI;
    match style {
        "Constant" => {
            if dir == "In" || (dir == "InOut" && a >= 0.5) {
                1.0
            } else {
                0.0
            }
        }
        "Linear" => a,
        "Cubic" => ease(a, "Cubic", match dir {
            "In" => "Out",
            "Out" => "In",
            d => d,
        }),
        "CubicV2" => ease(a, "Cubic", dir),
        "Elastic" if dir == "InOut" => {
            if a == 0.0 || a == 1.0 {
                return a;
            }
            let t = a * 2.0 - 1.0;
            if t < 0.0 {
                -0.5 * 2f64.powf(10.0 * t) * ((t - 0.1125) * 2.0 * PI / 0.45).sin()
            } else {
                1.0 + 0.5 * 2f64.powf(-10.0 * t) * ((t - 0.1125) * 2.0 * PI / 0.45).sin()
            }
        }
        // Measured in Studio: Bounce InOut uses BounceIn on both halves
        // (0.5*BounceIn(2a) for a<0.5, 0.5 + 0.5*BounceIn(2a-1) for a>=0.5).
        "Bounce" if dir == "InOut" => {
            if a < 0.5 {
                ease_in("Bounce", a * 2.0) * 0.5
            } else {
                0.5 + ease_in("Bounce", (a - 0.5) * 2.0) * 0.5
            }
        }
        _ => ease(a, style, dir),
    }
}

/// Keyframe poses: Linear uses NLERP; eased styles (Cubic/CubicV2/Elastic/Bounce)
/// use unclamped SLERP so Elastic overshoots outside [0, 1] extrapolate along the arc
/// (measured against Studio on 150° rotations across all 18 style×dir pairs).
fn pose_lerp(a: &Key, b: &Key, t: f64, slerp: bool) -> Xform {
    let qa = a.rotation;
    let mut qb = b.rotation;
    if qa.dot(qb) < 0.0 {
        qb = -qb;
    }
    let tf = t as f32;
    let rot = if slerp {
        let delta = (qa.conjugate() * qb).normalize();
        let (axis, angle) = delta.to_axis_angle();
        if angle.abs() < 1e-7 {
            Mat3::from_quat(qa.normalize())
        } else {
            let q = (qa * Quat::from_axis_angle(axis, (angle as f64 * t) as f32)).normalize();
            Mat3::from_quat(q)
        }
    } else {
        let q = qa * (1.0 - tf) + qb * tf;
        if q.length() > 1e-8 { Mat3::from_quat(q.normalize()) } else { Mat3::IDENTITY }
    };
    Xform { pos: a.cf.pos.lerp(b.cf.pos, tf), rot }
}

fn sample_float_keys(keys: &[FloatKey], t: f64) -> f64 {
    let Some(first) = keys.first() else { return 0.0 };
    if t <= first.t {
        return first.v;
    }
    let last = keys.last().unwrap();
    if t >= last.t {
        return last.v;
    }
    let next = keys.partition_point(|k| k.t <= t);
    let b = &keys[next.min(keys.len() - 1)];
    let a = &keys[next.saturating_sub(1)];
    if a.mode == 0 || b.t <= a.t {
        return a.v;
    }
    let dt = b.t - a.t;
    let u = (t - a.t) / dt;
    if a.mode == 1 {
        return a.v + (b.v - a.v) * u;
    }
    let u2 = u * u;
    let u3 = u2 * u;
    (2.0 * u3 - 3.0 * u2 + 1.0) * a.v
        + (u3 - 2.0 * u2 + u) * dt * a.right
        + (-2.0 * u3 + 3.0 * u2) * b.v
        + (u3 - u2) * dt * b.left
}

fn sample_keys(list: &[Key], t: f64) -> Option<Xform> {
    let first = list.first()?;
    if t <= first.t {
        return Some(first.cf);
    }
    // The clip loader sorts the keys by time. Exact hits use the last
    // key at that time, except at the first endpoint.
    let next = list.partition_point(|key| t.is_nan() || key.t <= t);
    let Some(b) = list.get(next) else { return list.last().map(|k| k.cf) };
    let a = &list[next.saturating_sub(1)];
    if b.t <= a.t {
        return Some(a.cf);
    }
    let alpha = pose_alpha((t - a.t) / (b.t - a.t), &a.style, &a.dir);
    let slerp = a.style != "Linear" && a.style != "Constant";
    Some(pose_lerp(a, b, alpha, slerp))
}

fn rank(priority: &Value) -> i64 {
    match priority {
        Value::UserData(u) => match u.borrow::<LEnumItem>() {
            Ok(e) if e.name != "Core" => e.value as i64,
            _ => -1,
        },
        _ => -1,
    }
}

// --------------------------------------------------------------- events

fn fire(lua: &Lua, r: Ref, name: &str, args: Vec<Value>) -> mlua::Result<()> {
    world(lua)?.fire(r, name, args);
    signal_timing::flush_immediate(lua);
    Ok(())
}

fn fade_track(st: &mut Track, target: f64, duration: f64) {
    st.target = target;
    st.fade = duration.max(0.0);
    st.fade_start = st.weight;
    st.fade_elapsed = 0.0;
}

/// Studio: a non-looped track that runs out fades over 0.3 s (a_fade).
const END_FADE: f64 = 0.3;

// ----------------------------------------------------------------- step

/// One track's time / weight / events; returns whether it poses its rig.
fn step_track(lua: &Lua, r: Ref, dt: f64) -> mlua::Result<Option<Ref>> {
    let pending = {
        let t = tracks(lua);
        let Some(st) = t.all.get(&r) else { return Ok(None) };
        st.pending.clone()
    };
    if let Some(id) = pending {
        let (seq, still) = load(lua, &id)?;
        if let Some(st) = tracks(lua).all.get_mut(&r) {
            if let Some(seq) = &seq {
                st.seq = Some(seq.clone());
                if !st.looped_set {
                    st.looped = seq.looped;
                }
                if !st.priority_set {
                    st.priority = seq.priority.clone();
                }
                if st.start_at_end {
                    st.time = seq.length;
                    st.start_at_end = false;
                }
            }
            if seq.is_some() || !still {
                st.pending = None;
            }
        }
    }
    // Check deferred releases/cuts at the start of the step:
    // - Stop(0) (`st.cut`): drops immediately on the first step, keeping WeightCurrent and not zeroing joints.
    // - Smooth fade-out (`st.end_pending` set on the step weight reached 0): fires Ended and releases next step.
    enum ImmediateEnd {
        None,
        Cut,
        FadedOut,
    }
    let imm_end = {
        let mut t = tracks(lua);
        let Some(st) = t.all.get_mut(&r) else { return Ok(None) };
        if !st.active {
            return Ok(None);
        }
        if st.cut {
            st.cut = false;
            let fire_end = !st.ended;
            st.ended = true;
            if fire_end {
                ImmediateEnd::Cut
            } else {
                st.active = false;
                return Ok(None);
            }
        } else if st.end_pending && !st.playing {
            st.end_pending = false;
            let fire_end = !st.ended;
            st.ended = true;
            if fire_end {
                ImmediateEnd::FadedOut
            } else {
                st.active = false;
                return Ok(None);
            }
        } else {
            ImmediateEnd::None
        }
    };
    match imm_end {
        ImmediateEnd::Cut | ImmediateEnd::FadedOut => {
            fire(lua, r, "Ended", vec![])?;
            if let Some(st) = tracks(lua).all.get_mut(&r) {
                if !st.playing {
                    st.active = false;
                }
            }
            return Ok(None);
        }
        ImmediateEnd::None => {}
    }

    // (frames crossed, loops, stopped_natural, stopped_zero_weight, ended_now)
    let (crossed, loops, stopped_natural, stopped_zero_weight, ended_now) = {
        let mut t = tracks(lua);
        let Some(st) = t.all.get_mut(&r) else { return Ok(None) };
        if !st.active {
            return Ok(None);
        }
        let length = st.seq.as_ref().map(|s| s.length).unwrap_or(0.0);
        let mut crossed: Vec<usize> = Vec::new();
        let mut loops = 0usize;
        let mut stopped_natural = false;
        let mut stopped_zero_weight = false;
        let mut ended_now = false;
        if length > 0.0 {
            let seq = st.seq.clone().expect("seq with length");
            let frames = &seq.frames;
            // Studio advances current fade BEFORE handling a natural end on this step
            // (so a non-looped end starts its 0.3s fade-out on the NEXT step, not this one).
            let was_zero = st.weight <= 0.0 && st.target <= 0.0;
            if st.fade > 0.0 {
                st.fade_elapsed += dt;
                let a = (st.fade_elapsed / st.fade).min(1.0);
                st.weight = st.fade_start + (st.target - st.fade_start) * a;
            } else {
                st.weight = st.target;
            }
            if st.playing && st.weight <= 0.0 && st.target <= 0.0 {
                if st.fade <= 0.0 {
                    // Play(0, 0, speed) or AdjustWeight(0, 0): stops immediately on this step.
                    st.playing = false;
                    st.ended = true;
                    stopped_zero_weight = true;
                    ended_now = true;
                } else {
                    // AdjustWeight(0, fade > 0): Studio stays playing for 2 steps at wc==0, then stops+ends.
                    if was_zero {
                        st.zero_weight_steps = st.zero_weight_steps.saturating_add(1);
                    } else {
                        st.zero_weight_steps = 0;
                    }
                    if st.zero_weight_steps >= 2 {
                        st.playing = false;
                        st.ended = true;
                        stopped_zero_weight = true;
                        ended_now = true;
                    }
                }
            } else {
                st.zero_weight_steps = 0;
            }
            if !stopped_zero_weight {
                // Studio accumulates TimePosition in f32 (even while fading out after
                // Stop(fade > 0) so looped clips keep moving smoothly during fade-out),
                // and fires markers/keyframes/DidLoop only while IsPlaying is true.
                let before = st.time as f32;
                let step_dt = (dt as f32) * (st.speed as f32);
                let after = (before + step_dt) as f64;
                let before_f64 = before as f64;
                let emit_events = st.playing;
                let events_fwd = |lo: f64, hi: f64, include_hi: bool, out: &mut Vec<usize>| {
                    if !emit_events || lo.is_nan() || hi.is_nan() || lo > hi {
                        return;
                    }
                    let first = frames.partition_point(|f| f.t < lo);
                    let end = frames.partition_point(|f| f.t < hi || (include_hi && f.t == hi));
                    out.extend(first..end);
                };
                let events_rev = |lo: f64, hi: f64, include_lo: bool, include_hi: bool, out: &mut Vec<usize>| {
                    if !emit_events || lo.is_nan() || hi.is_nan() || lo > hi {
                        return;
                    }
                    let first = frames.partition_point(|f| f.t < lo || (!include_lo && f.t == lo));
                    let end = frames.partition_point(|f| f.t < hi || (include_hi && f.t == hi));
                    out.extend((first..end).rev());
                };
                if st.looped {
                    if st.speed >= 0.0 {
                        let wraps = (after / length).floor().max(0.0) as i64;
                        let rem = after.rem_euclid(length);
                        st.time = rem;
                        let mut lo = before_f64;
                        for _ in 0..wraps {
                            events_fwd(lo, length, true, &mut crossed);
                            lo = 0.0;
                            if emit_events {
                                loops += 1;
                            }
                        }
                        if wraps > 0 {
                            // At a wrap boundary, include t == 0.0 (Start) even if rem == 0.0.
                            events_fwd(0.0, st.time, st.time == 0.0, &mut crossed);
                        } else {
                            events_fwd(lo, st.time, false, &mut crossed);
                        }
                    } else {
                        // Studio includes the departure and excludes the destination
                        // during reverse playback. Landing exactly at zero leaves
                        // both endpoint events for the next step, which wraps.
                        let wraps = (-(after / length).floor()).max(0.0) as i64;
                        let rem = after.rem_euclid(length);
                        st.time = if rem == 0.0 && wraps > 0 { length } else { rem };
                        let mut hi = before_f64;
                        for _ in 0..wraps {
                            events_rev(0.0, hi, true, true, &mut crossed);
                            hi = length;
                            if emit_events {
                                loops += 1;
                            }
                        }
                        events_rev(st.time, hi, false, true, &mut crossed);
                    }
                } else {
                    st.time = after.clamp(0.0, length);
                    if st.speed >= 0.0 {
                        let hit_end = after >= length;
                        events_fwd(before_f64, st.time, hit_end, &mut crossed);
                        if emit_events && hit_end {
                            stopped_natural = true;
                            loops += 1;
                        }
                    } else {
                        let hit_start = after <= 0.0;
                        events_rev(st.time, before_f64, true, false, &mut crossed);
                        if emit_events && hit_start {
                            stopped_natural = true;
                            loops += 1;
                        }
                    }
                }
                if st.weight <= 0.0 && !st.playing && !st.ended {
                    // Weight hit 0 during a smooth fade-out: fire Ended on the NEXT step.
                    st.end_pending = true;
                }
            }
        } else if !st.playing && st.fade <= 0.0 {
            st.weight = 0.0;
            if !st.ended {
                st.ended = true;
                ended_now = true;
            } else {
                st.active = false;
            }
        }
        (crossed, loops, stopped_natural, stopped_zero_weight, ended_now)
    };
    if !crossed.is_empty() || loops > 0 || stopped_natural || stopped_zero_weight {
        let seq = tracks(lua).all.get(&r).and_then(|s| s.seq.clone());
        if let Some(seq) = seq {
            // Studio: markers, then named keyframes (while IsPlaying is still true!),
            // then (on natural end) flip IsPlaying=false + begin 0.3s fade-out,
            // then Stopped, then DidLoop.
            for &i in &crossed {
                for (name, value) in &seq.frames[i].markers {
                    fire(lua, r, &format!("MarkerReached:{name}"), vec![Value::String(lua.create_string(value)?)])?;
                }
            }
            for &i in &crossed {
                let f = &seq.frames[i];
                if f.name != "Keyframe" {
                    fire(lua, r, "KeyframeReached", vec![Value::String(lua.create_string(&f.name)?)])?;
                }
            }
            if stopped_natural {
                if let Some(st) = tracks(lua).all.get_mut(&r) {
                    st.playing = false;
                    fade_track(st, 0.0, END_FADE);
                }
                fire(lua, r, "Stopped", vec![])?;
            } else if stopped_zero_weight {
                fire(lua, r, "Stopped", vec![])?;
            }
            for _ in 0..loops {
                fire(lua, r, "DidLoop", vec![])?;
            }
        }
    }
    if ended_now {
        fire(lua, r, "Ended", vec![])?;
        if let Some(st) = tracks(lua).all.get_mut(&r) {
            if !st.playing {
                st.active = false;
            }
        }
    }
    let t = tracks(lua);
    let Some(st) = t.all.get(&r) else { return Ok(None) };
    if !st.active || st.seq.is_none() || st.cut || st.rig.is_none() {
        return Ok(None);
    }
    let rig = st.rig;
    drop(t);
    let w = world(lua)?;
    let d = w.d();
    if !d.dom.get_by_ref(rig).is_some_and(|i| i.parent().is_some()) {
        return Ok(None);
    }
    // In a ViewportFrame a track poses its rig only under a WorldModel
    // (Studio 0.747: a rig directly under the frame stays in its rest pose;
    // checks/viewport-frame-render/studio).
    let mut cur = rig;
    while let Some(i) = d.dom.get_by_ref(cur) {
        match i.class.as_str() {
            "WorldModel" => break,
            "ViewportFrame" => return Ok(None),
            _ => cur = i.parent(),
        }
    }
    Ok(Some(rig))
}

/// Workspace.Retargeting on: Enabled,
/// or Default while DFFlagWorkspaceAnimatorRetargetingEnabledDefault (on in
/// Studio 0.744); Disabled is off. A script's write lives in the stub
/// store, a place's saved value in the document.
fn retargeting_enabled(lua: &Lua) -> mlua::Result<bool> {
    let ws = { world(lua)?.d().workspace() };
    let Some(ws) = ws else { return Ok(false) };
    let mut mode: Option<u32> = None;
    if let Ok(store) = lua.named_registry_value::<Table>("rs_stub_store") {
        if let Ok(Some(t)) = store.get::<Option<Table>>(format!("{ws:?}")) {
            if let Value::UserData(u) = t.get::<Value>("Retargeting")? {
                if let Ok(e) = u.borrow::<LEnumItem>() {
                    mode = Some(e.value);
                }
            }
        }
    }
    let mode = match mode {
        Some(m) => m,
        None => rustudio_render::scene::enum_u32(world(lua)?.d().prop(ws, "Retargeting")).unwrap_or(0),
    };
    Ok(mode == 2 || mode == 0)
}

/// The retargeter for a clip on a rig: the clip carries an AnimationRigData
/// and the rig is an R15 Humanoid character.
fn retargeter_for(lua: &Lua, seq: &Rc<Seq>, rig: Ref) -> mlua::Result<Option<Rc<super::anim_retarget::Retargeter>>> {
    let Some(src) = seq.rig.clone() else { return Ok(None) };
    let key = (Rc::as_ptr(seq) as usize, rig);
    if let Some(r) = tracks(lua).retargeters.get(&key) {
        return Ok(Some(r.clone()));
    }
    let known = tracks(lua).char_rigs.get(&rig).cloned();
    let tgt = match known {
        Some(t) => t,
        None => {
            let t = {
                let w = world(lua)?;
                let d = w.d();
                let humanoid = d.dom.get_by_ref(rig).and_then(|m| {
                    m.children().iter().copied().find(|c| d.class_of(*c).is_some_and(|k| k == "Humanoid"))
                });
                let r15 = humanoid.is_some_and(|h| rustudio_render::scene::enum_u32(d.prop(h, "RigType")) == Some(1));
                let root = d.dom.get_by_ref(rig).and_then(|m| {
                    m.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.name == "HumanoidRootPart"))
                });
                match (r15, root) {
                    (true, Some(root)) => Some(Rc::new(super::anim_retarget::build_character_rig(d, rig, root))),
                    _ => None,
                }
            };
            tracks(lua).char_rigs.insert(rig, t.clone());
            t
        }
    };
    let Some(tgt) = tgt else { return Ok(None) };
    let r = Rc::new(super::anim_retarget::initialize(&src, &tgt));
    tracks(lua).retargeters.insert(key, r.clone());
    Ok(Some(r))
}

fn joints_of(lua: &Lua, rig: Ref) -> mlua::Result<RigBindings> {
    let w = world(lua)?;
    let d = w.d();
    let mut out: Vec<BoundJoint> = Vec::new();
    let mut dependencies = FxHashSet::default();
    let note_hierarchy = |mut node, dependencies: &mut FxHashSet<Ref>| {
        while node != rig && dependencies.insert(node) {
            let Some(i) = d.dom.get_by_ref(node) else { break };
            node = i.parent();
        }
    };
    let mut nodes = Vec::new();
    let mut pending = VecDeque::from([rig]);
    while let Some(node) = pending.pop_front() {
        let Some(instance) = d.dom.get_by_ref(node) else { continue };
        // Studio's rig walk stops at the host of another
        // Humanoid/AnimationController or a nested Animator.
        let boundary = node != rig && instance.children().iter().any(|child| {
            d.class_of(*child).is_some_and(|class| {
                matches!(class.as_str(), "Animator" | "AnimationController" | "Humanoid")
            })
        });
        if boundary {
            note_hierarchy(node, &mut dependencies);
            dependencies.extend(instance.children().iter().copied());
            continue;
        }
        pending.extend(instance.children().iter().copied());
        nodes.push(instance);
    }
    for i in &nodes {
        // AnimationConstraint: Studio's upgraded avatar joint (anim domain).
        if i.class != "Motor6D" && i.class != "AnimationConstraint" {
            continue;
        }
        note_hierarchy(i.referent(), &mut dependencies);
        // An AnimationConstraint's parts come from its attachments (its
        // Part0 / Part1 getters, anim.rs prop_get).
        if i.class == "AnimationConstraint" {
            for key in ["Attachment0", "Attachment1"] {
                if let Some(Variant::Ref(a)) = d.prop(i.referent(), key) {
                    dependencies.insert(a);
                }
            }
        }
        let Some((p0, p1, _, _)) = rustudio_core::body::joint_frames(d, i.referent()) else {
            continue;
        };
        dependencies.extend([p0, p1]);
        let (Some(parent), Some(child)) = (d.dom.get_by_ref(p0), d.dom.get_by_ref(p1)) else {
            continue;
        };
        let name = (ustr(&parent.name), ustr(&child.name));
        if let Some(binding) = out.iter_mut().find(|binding| binding.name == name) {
            binding.instance = i.referent();
        } else {
            out.push(BoundJoint { name, instance: i.referent(), unqualified: false });
        }
    }
    // A skinned rig targets Bones by name within their parent. If a part and
    // a Bone share the same endpoints' names, the animated joint takes precedence.
    let mut face_controls = Vec::new();
    for i in nodes {
        if i.class == "FaceControls" {
            note_hierarchy(i.referent(), &mut dependencies);
            face_controls.push(i.referent());
            continue;
        }
        if i.class != "Bone" {
            continue;
        }
        note_hierarchy(i.referent(), &mut dependencies);
        let parent = d.dom.get_by_ref(i.parent()).map_or("", |p| p.name.as_str());
        let name = (ustr(parent), ustr(&i.name));
        if !out.iter().any(|binding| binding.name == name) {
            out.push(BoundJoint { name, instance: i.referent(), unqualified: false });
        }
    }
    let mut seen = FxHashSet::default();
    for binding in out.iter_mut().rev() {
        binding.unqualified = seen.insert(binding.name.1);
    }
    Ok(RigBindings { joints: Rc::new(out), face_controls, dependencies })
}

fn dirty_bindings(lua: &Lua) -> mlua::Result<FxHashSet<Ref>> {
    let mut w = world(lua)?;
    let t = tracks(lua);
    if t.animated.is_empty() {
        // Establish a journal position before recording the first bindings.
        w.dm().marks_for(Reader::AnimatorBindings);
        return Ok(FxHashSet::default());
    }
    let Marks::Some(marks) = w.dm().marks_for(Reader::AnimatorBindings) else {
        return Ok(t.animated.keys().copied().collect());
    };
    let d = w.d();
    let mut changed = FxHashSet::default();
    let mut dirty = FxHashSet::default();
    for mark in marks {
        match mark {
            Mark::Tree(r) => {
                changed.insert(r);
                let mut node = r;
                while let Some(i) = d.dom.get_by_ref(node) {
                    if t.animated.contains_key(&node) {
                        dirty.insert(node);
                    }
                    node = i.parent();
                }
            }
            Mark::Prop(r, key) if matches!(key.as_str(), "Part0" | "Part1" | "Attachment0" | "Attachment1") => {
                changed.insert(r);
            }
            _ => {}
        }
    }
    if !changed.is_empty() {
        dirty.extend(t.animated.iter().filter_map(|(rig, bindings)| {
            bindings.dependencies.iter().any(|r| changed.contains(r)).then_some(*rig)
        }));
    }
    Ok(dirty)
}

fn write_transforms(lua: &Lua, dt: f64, pose: impl IntoIterator<Item = (Ref, Xform)>) -> mlua::Result<()> {
    // Transform goes to the engine's readers without firing Changed or
    // replicating. Apply each completed pose on the VM thread.
    let mut w = world(lua)?;
    let d = w.dm();
    let key = rbx_dom_weak::ustr("Transform");
    let mut history: Vec<(Ref, Xform, Xform)> = Vec::new();
    for (motor, xf) in pose {
        let Some(i) = d.dom.get_by_ref_mut(motor) else { continue };
        let prev = match i.properties.get(&key) {
            Some(Variant::CFrame(c)) => Xform::from_cframe(c),
            _ => super::anim_ik::ident(),
        };
        history.push((motor, prev, xf));
        let value = Variant::CFrame(xf.to_cframe());
        if i.parent().is_some() && i.properties.get(&key) != Some(&value) {
            i.properties.insert(key, value);
            d.mark_prop(motor, key);
        }
    }
    drop(w);
    if !history.is_empty() && dt > 0.0 {
        let mut t = tracks(lua);
        for (m, prev, curr) in history {
            t.prev_transforms.insert(m, (prev, curr, dt));
        }
    }
    Ok(())
}

/// Last Animator Transform delta for a joint (prev, curr, dt), for ApplyJointVelocities.
pub fn last_joint_delta(lua: &Lua, joint: Ref) -> Option<(Xform, Xform, f64)> {
    tracks(lua).prev_transforms.get(&joint).copied()
}

/// Blend scalar values (FACS FaceControls) in priority order with Roblox's weighted blend rules.
fn blend_scalar(samples: &mut [(i64, f64, f64)]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_by(|a, b| b.0.cmp(&a.0));
    let (mut remaining, mut acc) = (1.0f64, 0.0f64);
    for band in samples.chunk_by(|a, b| a.0 == b.0) {
        let total: f64 = band.iter().filter(|p| p.2 >= MIN_BLEND_WEIGHT).map(|p| p.2).sum();
        let take = total.min(remaining);
        if take > 0.0 {
            for (_, v, pw) in band {
                if *pw >= MIN_BLEND_WEIGHT {
                    acc += *v * (*pw * take / total);
                }
            }
            remaining -= take;
        }
    }
    Some(acc.clamp(0.0, 1.0))
}

fn write_facs(lua: &Lua, face_controls: &[Ref], tracks_list: &[PoseTrack]) -> mlua::Result<()> {
    if face_controls.is_empty() || tracks_list.iter().all(|t| t.seq.facs.is_empty()) {
        return Ok(());
    }
    let mut keys: FxHashSet<Ustr> = FxHashSet::default();
    for tr in tracks_list {
        keys.extend(tr.seq.facs.keys().copied());
    }
    if keys.is_empty() {
        return Ok(());
    }
    let mut samples: Vec<(i64, f64, f64)> = Vec::with_capacity(tracks_list.len());
    let mut writes: Vec<(Ustr, f32)> = Vec::with_capacity(keys.len());
    for key in keys {
        samples.clear();
        for tr in tracks_list {
            if let Some(fkeys) = tr.seq.facs.get(&key) {
                let v = sample_float_keys(fkeys, tr.time);
                samples.push((tr.rank, v, tr.weight));
            }
        }
        if let Some(v) = blend_scalar(&mut samples) {
            writes.push((key, v as f32));
        }
    }
    if writes.is_empty() {
        return Ok(());
    }
    let mut dirty_fcs = Vec::new();
    {
        let mut w = world(lua)?;
        let d = w.dm();
        for &fc in face_controls {
            let mut changed_keys = Vec::new();
            if let Some(i) = d.dom.get_by_ref_mut(fc) {
                for &(k, v) in &writes {
                    let val = Variant::Float32(v);
                    if i.properties.get(&k) != Some(&val) {
                        i.properties.insert(k, val);
                        changed_keys.push(k);
                    }
                }
            }
            if !changed_keys.is_empty() {
                for k in changed_keys {
                    d.mark_prop(fc, k);
                }
                dirty_fcs.push(fc);
            }
        }
    }
    if !dirty_fcs.is_empty() {
        if let Some(mut dirty) = lua.app_data_mut::<crate::runtime::domains::avatar::face::Dirty>() {
            dirty.0.extend(dirty_fcs);
        }
    }
    Ok(())
}

struct PoseTrack {
    seq: Rc<Seq>,
    weight: f64,
    rank: i64,
    time: f64,
    retargeter: Option<Rc<Retargeter>>,
}

struct NativePoseTrack<'a> {
    keys: &'a KeyframeJoints,
    weight: f64,
    rank: i64,
    time: f64,
    retargeter: Option<&'a Retargeter>,
}

fn retarget(name: &str, cf: Xform, retargeter: Option<&Retargeter>) -> Xform {
    retargeter.map_or(cf, |r| r.apply(super::anim_retarget::label_of_part(name), cf))
}

/// Blend the contributions in priority order, retaining load order within each
/// priority. Reuse the contribution storage across a rig's joints.
/// Measured in Studio: track weights < 0.001 are ignored; any remaining weight
/// `(1 - used)` blends directly with the Identity quaternion (0, 0, 0, 1) in
/// the NLERP sum rather than post-slerping from Identity.
fn blend(poses: &mut [(i64, Xform, f64)]) -> Option<Xform> {
    if poses.is_empty() {
        return None;
    }
    poses.sort_by(|a, b| b.0.cmp(&a.0));
    let (mut remaining, mut used) = (1.0f64, 0.0f64);
    let mut position = Vec3::ZERO;
    let (mut vector, mut scalar) = (glam::DVec3::ZERO, 0.0f64);
    let mut reference: Option<(glam::DVec3, f64)> = None;
    for poses in poses.chunk_by(|a, b| a.0 == b.0) {
        let total: f64 = poses.iter().filter(|p| p.2 >= MIN_BLEND_WEIGHT).map(|p| p.2).sum();
        let take = total.min(remaining);
        if take > 0.0 {
            for (_, cf, pw) in poses {
                if *pw < MIN_BLEND_WEIGHT {
                    continue;
                }
                let w = pw * take / total;
                let q = Quat::from_mat3(&cf.rot);
                // Canonical (w ≥ 0) like an axis-angle round trip.
                let q = if q.w < 0.0 { -q } else { q };
                let mut v = glam::DVec3::new(q.x as f64, q.y as f64, q.z as f64);
                let mut s = q.w as f64;
                let refq = *reference.get_or_insert((v, s));
                if refq.0.dot(v) + refq.1 * s < 0.0 {
                    v = -v;
                    s = -s;
                }
                position += cf.pos * w as f32;
                vector += v * w;
                scalar += s * w;
            }
            used += take;
            remaining -= take;
        }
    }
    if used <= 0.0 {
        return Some(super::anim_ik::ident());
    }
    let norm = (vector.dot(vector) + scalar * scalar).sqrt();
    let mut rot = if norm > 1e-8 {
        Quat::from_xyzw(
            (vector.x / norm) as f32,
            (vector.y / norm) as f32,
            (vector.z / norm) as f32,
            (scalar / norm) as f32,
        )
    } else {
        Quat::IDENTITY
    };
    // Remaining weight (1 - used) NLERPs the normalized blended quaternion with Identity.
    // Verified exact against Studio 0.748 for single-track, same-priority, and cross-priority <1.0 sums.
    if used < 1.0 {
        let u = used as f32;
        if rot.w < 0.0 {
            rot = -rot;
        }
        let iv = glam::Vec3::new(rot.x * u, rot.y * u, rot.z * u);
        let iw = (1.0 - u) + rot.w * u;
        let inorm = (iv.length_squared() + iw * iw).sqrt();
        rot = if inorm > 1e-8 {
            Quat::from_xyzw(iv.x / inorm, iv.y / inorm, iv.z / inorm, iw / inorm)
        } else {
            Quat::IDENTITY
        };
    }
    Some(Xform { pos: position, rot: Mat3::from_quat(rot) })
}

fn evaluate_native(joints: &[BoundJoint], tracks: &[NativePoseTrack<'_>]) -> Vec<(Ref, Xform)> {
    let mut contributions = Vec::with_capacity(tracks.len());
    let mut pose = Vec::with_capacity(joints.len());
    for joint in joints {
        contributions.clear();
        for track in tracks {
            if let Some(cf) = joint.channel(track.keys).and_then(|keys| sample_keys(keys, track.time)) {
                contributions.push((track.rank, retarget(joint.name.1.as_str(), cf, track.retargeter), track.weight));
            }
        }
        if let Some(cf) = blend(&mut contributions) {
            pose.push((joint.instance, cf));
        }
    }
    pose
}

/// The Animator step over every track, then each animated rig's joints.
pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    step_filtered(lua, None, dt)
}

/// Animator:StepAnimations(dt): steps only this Animator's tracks and poses its rig.
pub fn step_animator(lua: &Lua, animator: Ref, dt: f64) -> mlua::Result<()> {
    step_filtered(lua, Some(animator), dt)
}

fn step_filtered(lua: &Lua, only_owner: Option<Ref>, dt: f64) -> mlua::Result<()> {
    let _animator = rustudio_core::bench::scope("animator");
    let advance = rustudio_core::bench::scope("anim_tracks_advance");
    let order = {
        let mut t = tracks(lua);
        if only_owner.is_none() {
            t.steps += 1;
            if t.steps % 120 == 0 {
                // Tracks the collector freed (inactive and unreferenced).
                let alive: Vec<Ref> = {
                    let w = world(lua)?;
                    let d = w.d();
                    t.order.iter().copied().filter(|r| t.all.get(r).is_some_and(|s| s.active) || d.dom.get_by_ref(*r).is_some()).collect()
                };
                t.all.retain(|r, _| alive.contains(r));
                t.order = alive;
            }
        }
        if t.order.is_empty() && t.animated.is_empty() {
            return Ok(());
        }
        match only_owner {
            Some(owner) => t.order.iter().copied().filter(|r| t.all.get(r).is_some_and(|s| s.owner == owner)).collect(),
            None => t.order.clone(),
        }
    };
    // rig → tracks posing it, in step order.
    let mut by_rig: Vec<(Ref, Vec<Ref>)> = Vec::new();
    // (rig → its entry in by_rig)
    let mut rig_at: rustc_hash::FxHashMap<Ref, usize> = Default::default();
    for r in order {
        if let Some(rig) = step_track(lua, r, dt)? {
            match rig_at.get(&rig) {
                Some(&i) => by_rig[i].1.push(r),
                None => {
                    rig_at.insert(rig, by_rig.len());
                    by_rig.push((rig, vec![r]));
                }
            }
        }
    }
    drop(advance);
    let _pose = rustudio_core::bench::scope("anim_tracks_pose");
    let dirty = if by_rig.is_empty() {
        FxHashSet::default()
    } else {
        dirty_bindings(lua)?
    };
    if !dirty.is_empty() {
        let mut t = tracks(lua);
        t.char_rigs.retain(|r, _| !dirty.contains(r));
        t.retargeters.retain(|(_, r), _| !dirty.contains(r));
    }
    let mut retarget_on: Option<bool> = None;
    let mut rigs = Vec::with_capacity(by_rig.len());
    for (rig, list) in &by_rig {
        let cached = {
            let t = tracks(lua);
            t.animated.get(rig).map(|b| (b.joints.clone(), b.face_controls.clone()))
        };
        let (joints, face_controls) = match cached {
            Some(c) if !dirty.contains(rig) => c,
            previous => {
                let bindings = joints_of(lua, *rig)?;
                let out = (bindings.joints.clone(), bindings.face_controls.clone());
                let mut state = tracks(lua);
                state.animated.insert(*rig, bindings);
                if previous.is_none() {
                    state.animated_order.push(*rig);
                }
                out
            }
        };
        let mut posing: Vec<PoseTrack> = {
            let t = tracks(lua);
            list.iter()
                .filter_map(|r| t.all.get(r))
                .filter_map(|s| Some(PoseTrack {
                    seq: s.seq.clone()?,
                    weight: s.weight,
                    rank: rank(&s.priority),
                    time: s.time,
                    retargeter: None,
                }))
                .collect()
        };
        let retarget = match retarget_on {
            Some(v) => v,
            None => *retarget_on.insert(retargeting_enabled(lua)?),
        };
        if retarget {
            for track in &mut posing {
                track.retargeter = retargeter_for(lua, &track.seq, *rig)?;
            }
        }
        rigs.push((joints, face_controls, posing));
    }
    let keyframes_only = |tracks: &[PoseTrack]| tracks.iter().all(|t| t.seq.curves.is_empty());
    let native_count = rigs.iter().filter(|(_, _, tracks)| keyframes_only(tracks)).count();
    let mut worker_poses = if let Some(pool) = crate::rig::character_pool(native_count) {
        let inputs: Vec<_> = rigs.iter().filter(|(_, _, tracks)| keyframes_only(tracks)).map(|(joints, _, tracks)| {
            let tracks: Vec<_> = tracks.iter().map(|t| NativePoseTrack {
                keys: &t.seq.keys,
                weight: t.weight,
                rank: t.rank,
                time: t.time,
                retargeter: t.retargeter.as_deref(),
            }).collect();
            (joints.as_slice(), tracks)
        }).collect();
        // The worker input contains only immutable keyframes and numeric
        // blend parameters. Lua callbacks, curves and writes stay ordered.
        use rapier3d::rayon::prelude::*;
        let _worker = rustudio_core::bench::scope("anim_keyframes_parallel");
        let poses = pool.install(|| {
            inputs.into_par_iter().map(|(joints, tracks)| evaluate_native(joints, &tracks)).collect::<Vec<_>>()
        });
        Some(poses.into_iter())
    } else {
        None
    };
    if native_count == rigs.len() && worker_poses.is_some() {
        write_transforms(lua, dt, worker_poses.take().unwrap().flatten())?;
        for (_, face_controls, tracks) in &rigs {
            write_facs(lua, face_controls, tracks)?;
        }
    } else {
        let mut contributions = Vec::new();
        let mut pose = Vec::new();
        for (joints, face_controls, tracks) in rigs {
            let native = keyframes_only(&tracks);
            if let Some(worker) = worker_poses.as_mut().filter(|_| native) {
                write_transforms(lua, dt, worker.next().expect("keyframe pose in rig order"))?;
                write_facs(lua, &face_controls, &tracks)?;
                continue;
            }
            for joint in joints.iter() {
                contributions.clear();
                for track in &tracks {
                    let cf = if let Some(curves) = joint.channel(&track.seq.curves) {
                        let f: Function = lua.globals().get("__rs_anim_sample")?;
                        xform_of(&f.call::<Value>((curves.clone(), track.time))?)
                    } else {
                        joint.channel(&track.seq.keys).and_then(|keys| sample_keys(keys, track.time))
                    };
                    if let Some(cf) = cf {
                        contributions.push((track.rank, retarget(joint.name.1.as_str(), cf, track.retargeter.as_deref()), track.weight));
                    }
                }
                if let Some(cf) = blend(&mut contributions) {
                    if native {
                        pose.push((joint.instance, cf));
                    } else {
                        write_transforms(lua, dt, [(joint.instance, cf)])?;
                    }
                }
            }
            if native {
                write_transforms(lua, dt, pose.drain(..))?;
            }
            write_facs(lua, &face_controls, &tracks)?;
        }
    }
    // Drop cached rig bindings once no active tracks remain (smooth fade-outs
    // already wrote Identity on the frame weight reached 0; Stop(0) keeps the
    // last pose in Studio, so we do not force-reset Transforms here).
    if only_owner.is_none() {
        let mut t = tracks(lua);
        let mut order = std::mem::take(&mut t.animated_order);
        order.retain(|r| {
            if rig_at.contains_key(r) {
                return true;
            }
            t.animated.remove(r);
            t.char_rigs.remove(r);
            t.retargeters.retain(|k, _| k.1 != *r);
            false
        });
        t.animated_order = order;
        // The Animator holds playing and fading tracks only: a released one is
        // the collector's once no script holds it.
        for st in t.all.values_mut() {
            if !st.active && !matches!(st.me, Value::Nil) {
                st.me = Value::Nil;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ replication

/// One active (playing or fading) track, for Animator replication
/// (crate::anim_replicate). Read-only view of the native state.
pub struct TrackRep {
    pub track: Ref,
    pub owner: Ref,
    pub anim: Option<Ref>,
    pub playing: bool,
    pub speed: f64,
    pub target: f64,
    pub fade: f64,
    pub time: f64,
    pub looped: bool,
    /// The AnimationPriority's enum value (Idle 0 … Action4 5, Core 1000),
    /// as Animator replication carries it.
    pub priority: u32,
    pub play_gen: u64,
    pub mirrored: bool,
}

/// The active tracks, in load order.
pub fn replicated_state(lua: &Lua) -> Vec<TrackRep> {
    let t = tracks(lua);
    t.order
        .iter()
        .filter_map(|r| t.all.get(r).map(|s| (*r, s)))
        .filter(|(_, s)| s.active)
        .map(|(r, s)| TrackRep {
            track: r,
            owner: s.owner,
            anim: opt_inst(&s.anim),
            playing: s.playing,
            speed: s.speed,
            target: s.target,
            fade: s.fade,
            time: s.time,
            looped: s.looped,
            priority: match &s.priority {
                Value::UserData(u) => u.borrow::<LEnumItem>().map(|e| e.value as u32).unwrap_or(1000),
                _ => 1000,
            },
            play_gen: s.play_gen,
            mirrored: s.mirrored,
        })
        .collect()
}

/// Marks a track created by anim_replicate::sync so it is never echoed back.
pub fn mark_mirrored(lua: &Lua, track: &Value) {
    if let Some(r) = me_ref(track) {
        if let Some(st) = tracks(lua).all.get_mut(&r) {
            st.mirrored = true;
        }
    }
}

// ------------------------------------------------------------------ API

fn me_ref(v: &Value) -> Option<Ref> {
    opt_inst(v)
}

fn new_track(lua: &Lua, (me, anim, owner, rig, id): (Value, Value, Value, Value, String)) -> mlua::Result<()> {
    let Some(r) = me_ref(&me) else { return Ok(()) };
    let (seq, pending) = load(lua, &id)?;
    let core = LEnumItem::from_name("AnimationPriority", "Core")
        .map(|e| crate::ltypes::enum_value(lua, e))
        .transpose()?
        .unwrap_or(Value::Nil);
    let st = Track {
        me: Value::Nil,
        anim,
        owner: me_ref(&owner).unwrap_or_else(Ref::none),
        rig: me_ref(&rig).unwrap_or_else(Ref::none),
        looped: seq.as_ref().is_some_and(|s| s.looped),
        priority: seq.as_ref().map(|s| s.priority.clone()).unwrap_or(core),
        seq,
        pending: pending.then_some(id),
        playing: false,
        active: false,
        ended: false,
        end_pending: false,
        start_at_end: false,
        zero_weight_steps: 0,
        speed: 1.0,
        weight: 0.0,
        target: 0.0,
        fade: 0.1,
        fade_start: 0.0,
        fade_elapsed: 0.0,
        time: 0.0,
        looped_set: false,
        priority_set: false,
        cut: false,
        play_gen: 0,
        mirrored: false,
    };
    let mut t = tracks(lua);
    t.all.insert(r, st);
    t.order.push(r);
    Ok(())
}

/// Play: false when the Animator's track limit refuses it (the caller warns).
fn play(lua: &Lua, (me, fade, weight, speed): (Value, Option<f64>, Option<f64>, Option<f64>)) -> mlua::Result<(bool, Value, Value)> {
    let Some(r) = me_ref(&me) else { return Ok((true, Value::Nil, Value::Nil)) };
    let mut t = tracks(lua);
    let owner = match t.all.get(&r) {
        Some(st) => st.owner,
        None => return Ok((true, Value::Nil, Value::Nil)),
    };
    let count = t.all.values().filter(|s| s.active && s.owner == owner).count();
    let st = t.all.get_mut(&r).expect("track");
    let spd = speed.unwrap_or(1.0);
    // Studio restarts a playing track too; negative speed starts at Length.
    if spd < 0.0 {
        if let Some(seq) = &st.seq {
            st.time = seq.length;
            st.start_at_end = false;
        } else {
            st.time = 0.0;
            st.start_at_end = true;
        }
    } else {
        st.time = 0.0;
        st.start_at_end = false;
    }
    st.cut = false;
    st.playing = true;
    st.ended = false;
    st.end_pending = false;
    st.zero_weight_steps = 0;
    st.play_gen = st.play_gen.wrapping_add(1);
    fade_track(st, weight.unwrap_or(1.0).max(0.0), fade.unwrap_or(0.1).max(0.0));
    if !st.active {
        // Studio's insertion checks count > the configured limit (257 slots).
        if count > 256 {
            let (anim, rig) = (st.anim.clone(), st.rig);
            drop(t);
            return Ok((false, anim, inst_value(lua, rig)?));
        }
        st.active = true;
    }
    st.me = me.clone();
    st.speed = spd;
    drop(t);
    // AnimationPlayed fires on the Animator and on its Humanoid /
    // AnimationController.
    let parent = world(lua)?.d().dom.get_by_ref(owner).map(|i| i.parent()).unwrap_or_else(Ref::none);
    if parent.is_some() {
        fire(lua, owner, "AnimationPlayed", vec![me.clone()])?;
        fire(lua, parent, "AnimationPlayed", vec![me])?;
    }
    Ok((true, Value::Nil, Value::Nil))
}

fn stop(lua: &Lua, (me, fade): (Value, Option<f64>)) -> mlua::Result<()> {
    let Some(r) = me_ref(&me) else { return Ok(()) };
    let fire_it = {
        let mut t = tracks(lua);
        match t.all.get_mut(&r) {
            Some(st) if st.playing => {
                st.playing = false;
                st.end_pending = false;
                st.zero_weight_steps = 0;
                let fade = fade.unwrap_or(0.1);
                if fade <= 0.0 {
                    // Let go at once; WeightCurrent keeps its value (Studio).
                    st.cut = true;
                    st.target = 0.0;
                    st.fade = 0.0;
                } else {
                    fade_track(st, 0.0, fade);
                }
                true
            }
            _ => false,
        }
    };
    if fire_it {
        fire(lua, r, "Stopped", vec![])?;
    }
    Ok(())
}

fn adjust_speed(lua: &Lua, (me, speed): (Value, Option<f64>)) -> mlua::Result<()> {
    let Some(r) = me_ref(&me) else { return Ok(()) };
    if let Some(st) = tracks(lua).all.get_mut(&r) {
        st.speed = speed.unwrap_or(1.0);
    }
    Ok(())
}

fn adjust_weight(lua: &Lua, (me, weight, fade): (Value, Option<f64>, Option<f64>)) -> mlua::Result<()> {
    let Some(r) = me_ref(&me) else { return Ok(()) };
    if let Some(st) = tracks(lua).all.get_mut(&r) {
        if st.playing {
            st.zero_weight_steps = 0;
            fade_track(st, weight.unwrap_or(1.0).max(0.0), fade.unwrap_or(0.1).max(0.0));
        }
    }
    Ok(())
}

/// AnimationTrack properties (runtime_ext reads them through `__rs_track_get`).
fn get(lua: &Lua, (me, key): (Value, String)) -> mlua::Result<Value> {
    let Some(r) = me_ref(&me) else { return Ok(Value::Nil) };
    let t = tracks(lua);
    let Some(st) = t.all.get(&r) else { return Ok(Value::Nil) };
    Ok(match key.as_str() {
        "Length" => Value::Number(st.seq.as_ref().map(|s| s.length).unwrap_or(0.0)),
        "TimePosition" => Value::Number(st.time),
        "IsPlaying" => Value::Boolean(st.playing),
        "Speed" => Value::Number(st.speed),
        "WeightCurrent" => Value::Number(st.weight),
        "WeightTarget" => Value::Number(st.target),
        "Looped" => Value::Boolean(st.looped),
        "Priority" => st.priority.clone(),
        "Animation" => st.anim.clone(),
        _ => Value::Nil,
    })
}

fn set(lua: &Lua, (me, key, value): (Value, String, Value)) -> mlua::Result<bool> {
    let Some(r) = me_ref(&me) else { return Ok(false) };
    let mut t = tracks(lua);
    let Some(st) = t.all.get_mut(&r) else { return Ok(false) };
    match key.as_str() {
        "Looped" => {
            st.looped = matches!(value, Value::Boolean(true));
            st.looped_set = true;
        }
        "Priority" => {
            st.priority = value;
            st.priority_set = true;
        }
        "TimePosition" => {
            let length = st.seq.as_ref().map(|s| s.length).unwrap_or(0.0);
            let v = match value {
                Value::Number(n) => n,
                Value::Integer(i) => i as f64,
                Value::String(s) => s.to_str().ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0),
                _ => 0.0,
            };
            st.time = if length <= 0.0 {
                0.0
            } else if st.looped {
                v.rem_euclid(length)
            } else {
                v.clamp(0.0, length)
            };
            st.start_at_end = false;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// The tracks an Animator holds (playing or fading), in load order.
fn playing(lua: &Lua, owner: Value) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    let Some(o) = me_ref(&owner) else { return Ok(out) };
    let list: Vec<Ref> = {
        let t = tracks(lua);
        t.order.iter().copied().filter(|r| t.all.get(r).is_some_and(|s| s.active && s.owner == o)).collect()
    };
    for (i, r) in list.into_iter().enumerate() {
        out.raw_set(i + 1, inst_value(lua, r)?)?;
    }
    Ok(out)
}

/// A keyframe's time; `false` for a CurveAnimation, which Studio refuses.
fn keyframe_time(lua: &Lua, (me, name): (Value, String)) -> mlua::Result<Value> {
    let Some(r) = me_ref(&me) else { return Ok(Value::Nil) };
    let t = tracks(lua);
    let Some(q) = t.all.get(&r).and_then(|s| s.seq.as_ref()) else { return Ok(Value::Nil) };
    if q.curve {
        return Ok(Value::Boolean(false));
    }
    Ok(q.frames.iter().find(|f| f.name == name).map(|f| Value::Number(f.t)).unwrap_or(Value::Nil))
}

/// Animator:GetTrackByAnimationId: an active track of this Animator.
fn by_id(lua: &Lua, (owner, id): (Value, String)) -> mlua::Result<Value> {
    let Some(o) = me_ref(&owner) else { return Ok(Value::Nil) };
    let cands: Vec<(Ref, Value)> = {
        let t = tracks(lua);
        t.order
            .iter()
            .filter_map(|r| t.all.get(r).map(|s| (*r, s)))
            .filter(|(_, s)| s.active && s.owner == o)
            .map(|(r, s)| (r, s.anim.clone()))
            .collect()
    };
    for (r, anim) in cands {
        if let Some(a) = opt_inst(&anim) {
            if let Ok(Value::String(s)) = index(lua, a, "AnimationId") {
                if s.to_str()? == id.as_str() {
                    return inst_value(lua, r);
                }
            }
        }
    }
    Ok(Value::Nil)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set("__rs_track_new", lua.create_function(new_track)?)?;
    g.set("__rs_track_play", lua.create_function(play)?)?;
    g.set("__rs_track_stop", lua.create_function(stop)?)?;
    g.set("__rs_track_speed", lua.create_function(adjust_speed)?)?;
    g.set("__rs_track_weight", lua.create_function(adjust_weight)?)?;
    g.set("__rs_track_get", lua.create_function(get)?)?;
    g.set("__rs_track_set", lua.create_function(set)?)?;
    g.set("__rs_track_playing", lua.create_function(playing)?)?;
    g.set("__rs_track_keyframe_time", lua.create_function(keyframe_time)?)?;
    g.set("__rs_track_by_id", lua.create_function(by_id)?)?;
    g.set(
        "__rs_anim_forget",
        lua.create_function(|lua, id: String| {
            tracks(lua).seqs.remove(&id);
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframe_search_keeps_duplicate_and_endpoint_semantics() {
        let make_key = |t, x| Key {
            t,
            cf: Xform { pos: Vec3::X * x, rot: Mat3::IDENTITY },
            rotation: Quat::IDENTITY,
            style: "Linear".into(),
            dir: "In".into(),
        };
        let keys = [
            make_key(0.0, 1.0), make_key(0.0, 2.0),
            make_key(1.0, 4.0), make_key(1.0, 6.0), make_key(2.0, 10.0),
        ];
        for (time, position) in [(0.0, 1.0), (0.5, 3.0), (1.0, 6.0), (1.5, 8.0), (3.0, 10.0)] {
            assert_eq!(sample_keys(&keys, time).unwrap().pos.x, position);
        }
    }

    #[test]
    fn replication_preserves_priority_enum_values_without_changing_blend_order() {
        let lua = Lua::new();
        // API dump: Core's value is 1000, but it blends below Idle (0).
        let priorities = [
            ("Core", 1000, -1),
            ("Idle", 0, 0),
            ("Movement", 1, 1),
            ("Action", 2, 2),
            ("Action2", 3, 3),
            ("Action3", 4, 4),
            ("Action4", 5, 5),
        ];
        for (name, wire_value, blend_rank) in priorities {
            let priority = crate::ltypes::enum_value(
                &lua,
                LEnumItem::from_name("AnimationPriority", name).expect("API enum"),
            )
            .unwrap();
            assert_eq!(rank(&priority), blend_rank, "{name} blending order");
            let r = Ref::new();
            let mut t = tracks(&lua);
            t.order.push(r);
            t.all.insert(
                r,
                Track {
                    me: Value::Nil,
                    anim: Value::Nil,
                    owner: Ref::none(),
                    rig: Ref::none(),
                    seq: None,
                    pending: None,
                    playing: true,
                    active: true,
                    ended: false,
                    end_pending: false,
                    start_at_end: false,
                    zero_weight_steps: 0,
                    speed: 1.0,
                    weight: 1.0,
                    target: 1.0,
                    fade: 0.1,
                    fade_start: 1.0,
                    fade_elapsed: 0.0,
                    time: 0.0,
                    looped: false,
                    looped_set: false,
                    priority,
                    priority_set: true,
                    cut: false,
                    play_gen: 0,
                    mirrored: false,
                },
            );
            drop(t);
            let reps = replicated_state(&lua);
            assert_eq!(
                reps.last().unwrap().priority,
                wire_value,
                "{name} wire value"
            );
            assert_eq!(reps.len(), tracks(&lua).order.len());
        }
    }
}
