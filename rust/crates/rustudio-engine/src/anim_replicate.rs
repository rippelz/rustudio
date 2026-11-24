//! Animator track replication, as Studio does it: each Animator has 16
//! replicated hidden slots, `AnimationId{i}`, `AnimTrackMetadata{i}`,
//! `AnimTrackPlayState{i}` and `AnimTrackWeight{i}` (API dump; reflected
//! get/set pairs, e.g. slot 0's PlayState get Studio / set Studio,
//! registered at Studio). The server writes a slot when one of its
//! tracks starts, stops, changes speed / weight / looping / priority or
//! ends, not every frame; the slot replicates like any property and the
//! client runs its own track from it (the setter re-syncs the
//! client Animator's tracks). Transform never replicates: each side poses
//! its own Motor6Ds.
//!
//! Slot layouts, from the getters / setters (16 / 16 / 4 bytes, int64):
//! - AnimTrackPlayState: f64 time position, f32 speed, u8 playing
//! - AnimTrackWeight: f64 (written: the server time of the change), f32
//!   target weight, f32 fade time
//! - AnimTrackMetadata: u16 priority, u8 looped
//! - AnimationId: Studio's is the asset number (int64); rustudio keeps the
//!   whole content id string (local and rbxtemp ids have no number).
//!
//! Here the structs are stored as BinaryString values in those layouts.

use std::collections::HashMap;

use mlua::{Function, Lua, Value};
use rbx_dom_weak::types::{BinaryString, Ref, Variant};
use rbx_dom_weak::ustr;

use crate::runtime::domains::anim::anim_tracks;
use crate::runtime::{RepOp, Side};

/// Animator slots (AnimationId0..15 in the API dump).
pub const SLOTS: usize = 16;

/// What a slot says, minus the time position (which moves every frame and
/// is only sent with a change).
#[derive(Clone, PartialEq, Debug, Default)]
struct Snap {
    id: String,
    playing: bool,
    speed: f32,
    target: f32,
    fade: f32,
    looped: bool,
    priority: u16,
    /// Play restart counter (24-bit) so calling Play() on an already-playing
    /// track restarts the mirrored track and fires AnimationPlayed again.
    play_gen: u32,
}

/// Server side: the tracks in each Animator's slots and what was sent.
#[derive(Default)]
struct Published {
    slots: HashMap<Ref, Vec<Option<(Ref, Snap)>>>,
}

/// Client side: the local track running each replicated slot.
#[derive(Default)]
struct Mirrored {
    slots: HashMap<(Ref, usize), (Value, Snap)>,
    helper: Option<Function>,
}

/// Server side: local tracks mirrored from the owning client's character Animator.
#[derive(Default)]
struct ClientMirrored {
    by_animator: HashMap<Ref, HashMap<Ref, (Value, Snap)>>,
    helper: Option<Function>,
}

/// One client-played track snapshot carried from Client -> Server for the local
/// player's character Animator.
#[derive(Clone, Debug)]
pub struct ClientTrackSnap {
    pub track: Ref,
    pub id: String,
    pub playing: bool,
    pub speed: f32,
    pub target: f32,
    pub fade: f32,
    pub time: f64,
    pub looped: bool,
    pub priority: u16,
    pub play_gen: u32,
}

pub fn is_slot_prop(name: &str) -> Option<usize> {
    for p in ["AnimationId", "AnimTrackMetadata", "AnimTrackPlayState", "AnimTrackWeight"] {
        if let Some(n) = name.strip_prefix(p) {
            return n.parse::<usize>().ok().filter(|i| *i < SLOTS);
        }
    }
    None
}

fn play_state(time: f64, speed: f32, playing: bool, play_gen: u32) -> Variant {
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&time.to_le_bytes());
    b.extend_from_slice(&speed.to_le_bytes());
    b.push(playing as u8);
    let g = play_gen & 0x00FF_FFFF;
    b.push((g & 0xFF) as u8);
    b.push(((g >> 8) & 0xFF) as u8);
    b.push(((g >> 16) & 0xFF) as u8);
    Variant::BinaryString(BinaryString::from(b))
}

fn gen_from_ps(b: &[u8]) -> u32 {
    let b0 = b.get(13).copied().unwrap_or(0) as u32;
    let b1 = b.get(14).copied().unwrap_or(0) as u32;
    let b2 = b.get(15).copied().unwrap_or(0) as u32;
    b0 | (b1 << 8) | (b2 << 16)
}

fn weight(at: f64, target: f32, fade: f32) -> Variant {
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&at.to_le_bytes());
    b.extend_from_slice(&target.to_le_bytes());
    b.extend_from_slice(&fade.to_le_bytes());
    Variant::BinaryString(BinaryString::from(b))
}

fn metadata(priority: u16, looped: bool) -> Variant {
    let mut b = priority.to_le_bytes().to_vec();
    b.push(looped as u8);
    b.push(0);
    Variant::BinaryString(BinaryString::from(b))
}

fn bytes(v: Option<&Variant>) -> Vec<u8> {
    match v {
        Some(Variant::BinaryString(b)) => AsRef::<[u8]>::as_ref(b).to_vec(),
        _ => Vec::new(),
    }
}

fn f64_at(b: &[u8], i: usize) -> f64 {
    b.get(i..i + 8).map(|s| f64::from_le_bytes(s.try_into().unwrap())).unwrap_or(0.0)
}

fn f32_at(b: &[u8], i: usize) -> f32 {
    b.get(i..i + 4).map(|s| f32::from_le_bytes(s.try_into().unwrap())).unwrap_or(0.0)
}

/// Server: writes the slots of every Animator whose tracks changed. Run
/// once a frame after the Animator step, before replication.
pub fn publish(lua: &Lua) {
    let Ok(w) = crate::runtime::world(lua) else { return };
    if w.side != Side::Server || !w.has_peer {
        return;
    }
    let now = w.time;
    drop(w);
    let reps = anim_tracks::replicated_state(lua);
    if reps.is_empty() && lua.app_data_ref::<Published>().is_none_or(|p| p.slots.is_empty()) {
        return;
    }
    if lua.app_data_ref::<Published>().is_none() {
        lua.set_app_data(Published::default());
    }
    let Ok(mut w) = crate::runtime::world(lua) else { return };
    let mut p = lua.app_data_mut::<Published>().expect("published slots");
    let mut by_owner: HashMap<Ref, Vec<&anim_tracks::TrackRep>> = HashMap::new();
    for r in &reps {
        if r.mirrored {
            continue;
        }
        if w.d().class_of(r.owner).is_some_and(|c| c == "Animator") {
            by_owner.entry(r.owner).or_default().push(r);
        }
    }
    let mut owners: Vec<Ref> = p.slots.keys().copied().collect();
    owners.extend(by_owner.keys().copied().filter(|o| !p.slots.contains_key(o)));
    let mut writes: Vec<(Ref, String, Variant)> = Vec::new();
    for owner in owners {
        let tracks = by_owner.remove(&owner).unwrap_or_default();
        let slots = p.slots.entry(owner).or_insert_with(|| vec![None; SLOTS]);
        // Tracks keep their slot; ended ones free it; new ones take the
        // lowest free one (more than 16 at once: the rest aren't sent).
        let mut fresh: Vec<Option<(Ref, Snap, f64)>> = vec![None; SLOTS];
        let mut unplaced = Vec::new();
        for t in &tracks {
            let id = t
                .anim
                .and_then(|a| rustudio_render::scene::content_string(w.d().prop(a, "AnimationId")))
                .unwrap_or_default();
            let snap = Snap {
                id,
                playing: t.playing,
                speed: t.speed as f32,
                target: t.target as f32,
                fade: t.fade as f32,
                looped: t.looped,
                priority: t.priority as u16,
                play_gen: (t.play_gen & 0x00FF_FFFF) as u32,
            };
            match slots.iter().position(|s| s.as_ref().is_some_and(|s| s.0 == t.track)) {
                Some(i) => fresh[i] = Some((t.track, snap, t.time)),
                None => unplaced.push((t.track, snap, t.time)),
            }
        }
        for u in unplaced {
            if let Some(i) = fresh.iter().position(|s| s.is_none()) {
                fresh[i] = Some(u);
            }
        }
        for i in 0..SLOTS {
            let old = slots[i].as_ref().map(|s| (s.0, &s.1));
            let new = fresh[i].as_ref().map(|s| (s.0, &s.1));
            if old == new {
                continue;
            }
            match &fresh[i] {
                Some((track, snap, time)) => {
                    writes.push((owner, format!("AnimationId{i}"), Variant::String(snap.id.clone())));
                    writes.push((owner, format!("AnimTrackMetadata{i}"), metadata(snap.priority, snap.looped)));
                    writes.push((owner, format!("AnimTrackWeight{i}"), weight(now, snap.target, snap.fade)));
                    writes.push((owner, format!("AnimTrackPlayState{i}"), play_state(*time, snap.speed, snap.playing, snap.play_gen)));
                    slots[i] = Some((*track, snap.clone()));
                }
                None => {
                    writes.push((owner, format!("AnimationId{i}"), Variant::String(String::new())));
                    writes.push((owner, format!("AnimTrackPlayState{i}"), play_state(0.0, 0.0, false, 0)));
                    slots[i] = None;
                }
            }
        }
        if slots.iter().all(|s| s.is_none()) {
            p.slots.remove(&owner);
        }
    }
    drop(p);
    for (owner, name, v) in writes {
        let d = w.dm();
        let Some(i) = d.dom.get_by_ref_mut(owner) else { continue };
        i.properties.insert(ustr(&name), v);
        d.mark_prop(owner, ustr(&name));
        w.record(RepOp::Prop(owner, name));
    }
}

/// The client side of a slot: a local AnimationTrack loaded, played,
/// stopped and adjusted through the same API a LocalScript uses, so the
/// client poses the rig and fires AnimationPlayed / Stopped itself.
const HELPER: &str = r#"
return function(op, animator, track, id, looped, priority, fade, weight, speed, time)
    if op == "load" then
        local a = Instance.new("Animation")
        a.AnimationId = id
        local t = animator:LoadAnimation(a)
        t.Looped = looped
        for _, e in Enum.AnimationPriority:GetEnumItems() do
            if e.Value == priority then t.Priority = e end
        end
        return t
    elseif op == "meta" then
        track.Looped = looped
        for _, e in Enum.AnimationPriority:GetEnumItems() do
            if e.Value == priority then track.Priority = e end
        end
    elseif op == "play" then
        track:Play(fade, weight, speed)
        track.TimePosition = time
    elseif op == "stop" then
        track:Stop(fade)
    elseif op == "speed" then
        track:AdjustSpeed(speed)
    elseif op == "weight" then
        track:AdjustWeight(weight, fade)
    end
end
"#;

/// Client: brings the local tracks of these (Animator, slot)s in line with
/// what the server sent.
pub fn sync(lua: &Lua, dirty: Vec<(Ref, usize)>) {
    if dirty.is_empty() {
        return;
    }
    if lua.app_data_ref::<Mirrored>().is_none() {
        lua.set_app_data(Mirrored::default());
    }
    let helper = {
        let mut m = lua.app_data_mut::<Mirrored>().expect("mirrored slots");
        if m.helper.is_none() {
            m.helper = lua.load(HELPER).set_name("=rustudio.anim_replicate").eval::<Function>().ok();
        }
        m.helper.clone()
    };
    let Some(helper) = helper else { return };
    let mut seen = std::collections::HashSet::new();
    for (animator, slot) in dirty {
        if !seen.insert((animator, slot)) {
            continue;
        }
        if let Err(e) = sync_slot(lua, &helper, animator, slot) {
            if let Ok(mut w) = crate::runtime::world(lua) {
                w.output.push((rustudio_core::log::Level::Warn, format!("Animator replication: {e}")));
            }
        }
    }
}

fn sync_slot(lua: &Lua, helper: &Function, animator: Ref, slot: usize) -> mlua::Result<()> {
    let (snap, time) = {
        let w = crate::runtime::world(lua)?;
        let Some(i) = w.d().dom.get_by_ref(animator) else { return Ok(()) };
        let prop = |n: &str| i.properties.get(&ustr(&format!("{n}{slot}")));
        let id = match prop("AnimationId") {
            Some(Variant::String(s)) => s.clone(),
            _ => String::new(),
        };
        let ps = bytes(prop("AnimTrackPlayState"));
        let wt = bytes(prop("AnimTrackWeight"));
        let md = bytes(prop("AnimTrackMetadata"));
        (
            Snap {
                id,
                playing: ps.get(12).is_some_and(|b| *b != 0),
                speed: f32_at(&ps, 8),
                target: f32_at(&wt, 8),
                fade: f32_at(&wt, 12),
                looped: md.get(2).is_some_and(|b| *b != 0),
                priority: md.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0),
                play_gen: gen_from_ps(&ps),
            },
            f64_at(&ps, 0),
        )
    };
    let me = crate::runtime::inst_value(lua, animator)?;
    let old = lua.app_data_mut::<Mirrored>().and_then(|mut m| m.slots.remove(&(animator, slot)));
    let call = |op: &str, track: &Value, s: &Snap| -> mlua::Result<Value> {
        helper.call((op, me.clone(), track.clone(), s.id.clone(), s.looped, s.priority, s.fade, s.target, s.speed, time))
    };
    if snap.id.is_empty() {
        // The track ended on the server (faded out): end ours.
        if let Some((track, s)) = old {
            if s.playing {
                call("stop", &track, &Snap { fade: 0.0, ..s })?;
            }
        }
        return Ok(());
    }
    let (track, prev) = match old {
        Some((track, s)) if s.id == snap.id => (track, Some(s)),
        other => {
            if let Some((track, s)) = other {
                call("stop", &track, &Snap { fade: 0.0, ..s })?;
            }
            let t = call("load", &Value::Nil, &snap)?;
            anim_tracks::mark_mirrored(lua, &t);
            (t, None)
        }
    };
    let prev_playing = prev.as_ref().is_some_and(|p| p.playing);
    if let Some(p) = &prev {
        if p.looped != snap.looped || p.priority != snap.priority {
            call("meta", &track, &snap)?;
        }
    }
    match (prev_playing, snap.playing) {
        (false, true) => {
            call("play", &track, &snap)?;
        }
        (true, false) => {
            call("stop", &track, &snap)?;
        }
        (true, true) => {
            let p = prev.as_ref().expect("prev");
            if p.play_gen != snap.play_gen {
                call("play", &track, &snap)?;
            } else {
                if p.speed != snap.speed {
                    call("speed", &track, &snap)?;
                }
                if p.target != snap.target || p.fade != snap.fade {
                    call("weight", &track, &snap)?;
                }
            }
        }
        (false, false) => {}
    }
    if let Some(mut m) = lua.app_data_mut::<Mirrored>() {
        m.slots.insert((animator, slot), (track, snap));
    }
    Ok(())
}

/// Client: collects non-mirrored active tracks playing on the local player's
/// character Animator so they replicate back to the server Animator (Studio
/// lets the client own its character's Animator and replicates tracks to server).
pub fn collect_client_tracks(lua: &Lua, animator: Ref) -> Vec<ClientTrackSnap> {
    let Ok(w) = crate::runtime::world(lua) else { return Vec::new() };
    if w.side != Side::Client {
        return Vec::new();
    }
    drop(w);
    let reps = anim_tracks::replicated_state(lua);
    let Ok(w) = crate::runtime::world(lua) else { return Vec::new() };
    let mut out = Vec::new();
    for t in reps {
        if t.owner != animator || t.mirrored {
            continue;
        }
        let id = t
            .anim
            .and_then(|a| rustudio_render::scene::content_string(w.d().prop(a, "AnimationId")))
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        out.push(ClientTrackSnap {
            track: t.track,
            id,
            playing: t.playing,
            speed: t.speed as f32,
            target: t.target as f32,
            fade: t.fade as f32,
            time: t.time,
            looped: t.looped,
            priority: t.priority as u16,
            play_gen: (t.play_gen & 0x00FF_FFFF) as u32,
        });
        if out.len() >= SLOTS {
            break;
        }
    }
    out
}

/// Server: mirrors the owning client's character tracks onto `server_animator`,
/// marking them `mirrored = true` so `publish` never echoes them back to the client.
pub fn sync_from_client(lua: &Lua, server_animator: Ref, tracks: &[ClientTrackSnap]) {
    let exists = crate::runtime::world(lua)
        .is_ok_and(|w| w.side == Side::Server && w.d().class_of(server_animator).is_some_and(|c| c == "Animator"));
    if !exists {
        return;
    }
    if tracks.is_empty()
        && lua
            .app_data_ref::<ClientMirrored>()
            .is_none_or(|m| m.by_animator.get(&server_animator).is_none_or(|s| s.is_empty()))
    {
        return;
    }
    if lua.app_data_ref::<ClientMirrored>().is_none() {
        lua.set_app_data(ClientMirrored::default());
    }
    let helper = {
        let mut m = lua.app_data_mut::<ClientMirrored>().expect("client mirrored");
        if m.helper.is_none() {
            m.helper = lua.load(HELPER).set_name("=rustudio.anim_replicate_c2s").eval::<Function>().ok();
        }
        m.helper.clone()
    };
    let Some(helper) = helper else { return };
    if let Err(e) = sync_from_client_inner(lua, &helper, server_animator, tracks) {
        if let Ok(mut w) = crate::runtime::world(lua) {
            w.output.push((rustudio_core::log::Level::Warn, format!("Animator C->S replication: {e}")));
        }
    }
}

fn sync_from_client_inner(
    lua: &Lua,
    helper: &Function,
    server_animator: Ref,
    tracks: &[ClientTrackSnap],
) -> mlua::Result<()> {
    let me = crate::runtime::inst_value(lua, server_animator)?;
    let mut existing = lua
        .app_data_mut::<ClientMirrored>()
        .and_then(|mut m| m.by_animator.remove(&server_animator))
        .unwrap_or_default();
    let mut next: HashMap<Ref, (Value, Snap)> = HashMap::with_capacity(tracks.len());
    let call = |op: &str, track: &Value, s: &Snap, time: f64| -> mlua::Result<Value> {
        helper.call((op, me.clone(), track.clone(), s.id.clone(), s.looped, s.priority, s.fade, s.target, s.speed, time))
    };
    for ct in tracks {
        let snap = Snap {
            id: ct.id.clone(),
            playing: ct.playing,
            speed: ct.speed,
            target: ct.target,
            fade: ct.fade,
            looped: ct.looped,
            priority: ct.priority,
            play_gen: ct.play_gen,
        };
        let (track, prev) = match existing.remove(&ct.track) {
            Some((track, s)) if s.id == snap.id => (track, Some(s)),
            other => {
                if let Some((track, s)) = other {
                    if s.playing {
                        call("stop", &track, &Snap { fade: 0.0, ..s }, 0.0)?;
                    }
                }
                let t = call("load", &Value::Nil, &snap, ct.time)?;
                anim_tracks::mark_mirrored(lua, &t);
                (t, None)
            }
        };
        let prev_playing = prev.as_ref().is_some_and(|p| p.playing);
        if let Some(p) = &prev {
            if p.looped != snap.looped || p.priority != snap.priority {
                call("meta", &track, &snap, ct.time)?;
            }
        }
        match (prev_playing, snap.playing) {
            (false, true) => {
                call("play", &track, &snap, ct.time)?;
            }
            (true, false) => {
                call("stop", &track, &snap, ct.time)?;
            }
            (true, true) => {
                let p = prev.as_ref().expect("prev");
                if p.play_gen != snap.play_gen {
                    call("play", &track, &snap, ct.time)?;
                } else {
                    if p.speed != snap.speed {
                        call("speed", &track, &snap, ct.time)?;
                    }
                    if p.target != snap.target || p.fade != snap.fade {
                        call("weight", &track, &snap, ct.time)?;
                    }
                }
            }
            (false, false) => {}
        }
        next.insert(ct.track, (track, snap));
    }
    // End tracks that are no longer active on the client.
    for (_, (track, s)) in existing {
        if s.playing {
            call("stop", &track, &Snap { fade: 0.0, ..s }, 0.0)?;
        }
    }
    if !next.is_empty() {
        if let Some(mut m) = lua.app_data_mut::<ClientMirrored>() {
            m.by_animator.insert(server_animator, next);
        }
    }
    Ok(())
}
