//! Edit-mode on-demand audio loading, `SoundService:PlayLocalSound`, and
//! Studio 0.748's Properties-panel **Preview** player.
//!
//! Studio 0.748 parity:
//! - Opening a place in Edit mode skips place-wide media scans (decoding every
//!   `Sound` in a large place cost >1.4 GB RSS). `EditAudio` consumes only
//!   incremental DOM marks (`doc::Reader::EditAudio`), single Explorer
//!   selection, and explicit Preview / `PlayLocalSound` actions.
//! - Setting `Sound.Playing = true` or calling `:Play()` / `:Resume()` in Edit
//!   updates flags and fires events in Lua, but does NOT start audible audio
//!   or advance `TimePosition` until entering Play/Run (where saved
//!   `Playing = true` autoplays from `TimePosition`).
//! - Audible Edit-mode preview is triggered by the Properties panel
//!   `Asset -> AudioContent -> Preview` control (for `Sound` and `AudioPlayer`),
//!   playing through the same `Audio` / `audio_scene` pipeline as Play mode
//!   while remaining invisible to `Sound` properties and signals (`Playing`,
//!   `IsPlaying`, `IsPaused`, `TimePosition`, `PlaybackLoudness`, events).
//! - `SoundService:PlayLocalSound(sound)` plays a detached audible one-shot in
//!   Edit mode without modifying `sound`'s properties or firing events.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::assets::Fetch;
use rustudio_core::doc::{self, Document, Mark, Marks};
use rustudio_core::math::Xform;
use rustudio_engine::audio::{Audio, Region};
use rustudio_engine::runtime::ext::SoundCmd;

use crate::game::Game;

/// Events produced during an Edit-mode audio tick for the Edit Lua VM to mirror.
#[derive(Default, Debug)]
pub struct EditAudioEvents {
    /// `(instance, asset_id, time_length)` that finished loading.
    pub loaded: Vec<(Ref, String, f64)>,
    /// Instances whose loaded asset was cleared or which left the DataModel.
    pub unloaded: Vec<Ref>,
}

impl EditAudioEvents {
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty() && self.unloaded.is_empty()
    }
}

struct DetachedTrack {
    sound: Ref,
    pending: Option<String>,
}

struct PreviewState {
    r: Ref,
    playing: bool,
    pos: f64,
    track_key: u64,
    pending: Option<String>,
    started: bool,
}

#[derive(Default)]
pub struct EditAudio {
    doc_id: u64,
    audio: Option<Audio>,
    detached: HashMap<u64, DetachedTrack>,
    preview: Option<PreviewState>,
    pub lengths: HashMap<Ref, f64>,
    seen_ids: HashMap<Ref, String>,
    pending_loads: HashMap<Ref, String>,
    unavailable: HashSet<String>,
    warned: HashSet<String>,
    next_key: u64,
    last_selected_audio: Option<Ref>,
    last_selection: Vec<Ref>,
    /// Number of `Audio::play` starts in Edit mode (diagnostics and tests).
    pub starts: u64,
}

impl EditAudio {
    /// True while an Edit-mode Properties preview, detached `PlayLocalSound`,
    /// or pending asset load is active.
    pub fn has_active(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.playing || p.pending.is_some())
            || !self.detached.is_empty()
            || !self.pending_loads.is_empty()
    }

    pub fn is_loaded(&self, r: Ref) -> bool {
        self.lengths.contains_key(&r)
    }

    pub fn time_length(&self, r: Ref) -> f64 {
        self.lengths.get(&r).copied().unwrap_or(0.0)
    }

    /// `(is_playing, preview_pos, time_length)` for the Properties panel
    /// Preview row of `r`.
    pub fn preview_state(&self, r: Ref) -> (bool, f64, f64) {
        let len = self.time_length(r);
        match self.preview.as_ref() {
            Some(p) if p.r == r => (p.playing, p.pos.clamp(0.0, len.max(0.0)), len),
            _ => (false, 0.0, len),
        }
    }

    pub fn preview_ref(&self) -> Option<Ref> {
        self.preview.as_ref().map(|p| p.r)
    }

    pub fn playing_count(&self) -> usize {
        self.audio.as_ref().map_or(0, |a| a.playing_count())
    }

    pub fn loaded_count(&self) -> usize {
        self.lengths.len()
    }

    pub fn set_muted(&mut self, on: bool) {
        if let Some(a) = self.audio.as_mut() {
            a.set_muted(on);
        }
    }

    /// Stops all audible Edit-mode preview and detached playback (e.g. when
    /// entering Play), preserving cached lengths for already-loaded instances.
    pub fn stop_playback(&mut self) {
        if let Some(a) = self.audio.as_mut() {
            a.stop_all();
        }
        self.detached.clear();
        self.preview = None;
    }

    /// Stops only the Properties-panel preview, resetting its thumb to 0.
    pub fn stop_preview(&mut self) {
        if let Some(p) = self.preview.take() {
            if let Some(a) = self.audio.as_mut() {
                a.stop(p.track_key);
            }
        }
    }

    /// Clears all state for a new/replaced document.
    pub fn reset(&mut self) {
        self.stop_playback();
        self.doc_id = 0;
        self.lengths.clear();
        self.seen_ids.clear();
        self.pending_loads.clear();
        self.unavailable.clear();
        self.warned.clear();
        self.last_selected_audio = None;
        self.last_selection.clear();
        self.starts = 0;
    }
}

fn is_audio_class(class: Option<&str>) -> bool {
    matches!(class, Some("Sound" | "AudioPlayer"))
}

fn in_dm(d: &Document, r: Ref) -> bool {
    d.exists(r) && d.is_ancestor(d.dom.root_ref(), r)
}

fn content_prop_str(d: &Document, r: Ref, name: &str) -> String {
    match d.prop(r, name) {
        Some(Variant::ContentId(c)) => c.as_str().to_string(),
        Some(Variant::Content(c)) => c.as_uri().unwrap_or("").to_string(),
        Some(Variant::String(s)) => s,
        _ => String::new(),
    }
}

fn asset_id_of(d: &Document, r: Ref) -> String {
    match d.class_of(r).as_deref() {
        Some("Sound") => {
            let sid = content_prop_str(d, r, "SoundId");
            if !sid.trim().is_empty() {
                sid
            } else {
                content_prop_str(d, r, "AudioContent")
            }
        }
        Some("AudioPlayer") => {
            let ac = content_prop_str(d, r, "AudioContent");
            if !ac.trim().is_empty() {
                ac
            } else {
                content_prop_str(d, r, "Asset")
            }
        }
        _ => String::new(),
    }
}

fn f32_prop(d: &Document, r: Ref, name: &str, default: f32) -> f32 {
    match d.prop(r, name) {
        Some(Variant::Float32(x)) => x,
        Some(Variant::Float64(x)) => x as f32,
        Some(Variant::Int32(x)) => x as f32,
        _ => default,
    }
}

fn f64_prop(d: &Document, r: Ref, name: &str, default: f64) -> f64 {
    match d.prop(r, name) {
        Some(Variant::Float64(x)) => x,
        Some(Variant::Float32(x)) => x as f64,
        Some(Variant::Int32(x)) => x as f64,
        Some(Variant::Int64(x)) => x as f64,
        _ => default,
    }
}

fn sound_region(d: &Document, r: Ref) -> Region {
    let range = |k: &str| match d.prop(r, k) {
        Some(Variant::NumberRange(n)) => Some((n.min as f64, n.max as f64)),
        _ => None,
    };
    match d.class_of(r).as_deref() {
        Some("Sound") => {
            if matches!(d.prop(r, "PlaybackRegionsEnabled"), Some(Variant::Bool(true))) {
                Region {
                    play: range("PlaybackRegion"),
                    looped: range("LoopRegion"),
                }
            } else {
                Region::default()
            }
        }
        Some("AudioPlayer") => {
            let play = range("PlaybackRegion").filter(|(a, b)| *b > *a);
            let looped = range("LoopRegion").filter(|(a, b)| *b > *a);
            Region { play, looped }
        }
        _ => Region::default(),
    }
}

fn preview_params(d: &Document, r: Ref) -> (f32, f32, bool, Region) {
    match d.class_of(r).as_deref() {
        Some("AudioPlayer") => {
            let vol = f32_prop(d, r, "Volume", 1.0).clamp(0.0, 10.0);
            let speed = f32_prop(d, r, "PlaybackSpeed", 1.0).clamp(0.0, 20.0);
            let looped = matches!(d.prop(r, "Looping"), Some(Variant::Bool(true)));
            (vol, speed, looped, sound_region(d, r))
        }
        _ => {
            let vol = f32_prop(d, r, "Volume", 0.5).clamp(0.0, 10.0);
            let speed = f32_prop(d, r, "PlaybackSpeed", 1.0);
            let looped = matches!(d.prop(r, "Looped"), Some(Variant::Bool(true)));
            (vol, speed, looped, sound_region(d, r))
        }
    }
}

fn next_track_key(ea: &mut EditAudio, r: Ref) -> u64 {
    ea.next_key = ea.next_key.wrapping_add(1);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    r.hash(&mut h);
    ea.next_key.hash(&mut h);
    h.finish() | 1
}

impl Game {
    fn edit_sound_bytes(&mut self, id: &str) -> Fetch {
        let dir = self
            .doc
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        if let Some(path) = rustudio_core::assets::resolve(id, dir.as_deref()) {
            return match std::fs::read(&path) {
                Ok(b) => Fetch::Ready(Arc::new(b)),
                Err(e) => Fetch::Unavailable(format!("{}: {e}", path.display())),
            };
        }
        match rustudio_core::assets::asset_ref(id) {
            Some(a) => self.assets.fetch(a),
            None => Fetch::Unavailable("no SoundId".into()),
        }
    }

    /// Loads `SoundId` / `AudioContent` / `Asset` for a single touched `Sound`
    /// or `AudioPlayer` on demand (never place-wide) so `IsLoaded`/`IsReady`
    /// and `TimeLength` update without playing.
    fn edit_register_audio_load(&mut self, r: Ref, events: &mut EditAudioEvents) {
        let class = self.doc.class_of(r);
        if !is_audio_class(class.as_deref()) {
            return;
        }
        if !in_dm(&self.doc, r) {
            self.edit_clear_audio_load(r, events);
            return;
        }
        let id = asset_id_of(&self.doc, r);
        if self.edit_audio.seen_ids.get(&r) == Some(&id)
            && (self.edit_audio.lengths.contains_key(&r)
                || self.edit_audio.pending_loads.contains_key(&r)
                || id.trim().is_empty()
                || self.edit_audio.unavailable.contains(&id))
        {
            return;
        }
        self.edit_audio.seen_ids.insert(r, id.clone());
        self.edit_audio.pending_loads.remove(&r);
        let had_len = self.edit_audio.lengths.remove(&r).is_some();
        if id.trim().is_empty() || self.edit_audio.unavailable.contains(&id) {
            if had_len {
                events.unloaded.push(r);
            }
            return;
        }
        match self.edit_sound_bytes(&id) {
            Fetch::Ready(bytes) => {
                if let Some(len) = Audio::length(&bytes) {
                    self.edit_audio.lengths.insert(r, len);
                    events.loaded.push((r, id, len));
                } else if had_len {
                    events.unloaded.push(r);
                }
            }
            Fetch::Pending => {
                self.edit_audio.pending_loads.insert(r, id);
                if had_len {
                    events.unloaded.push(r);
                }
            }
            Fetch::Unavailable(_) => {
                self.edit_audio.unavailable.insert(id);
                if had_len {
                    events.unloaded.push(r);
                }
            }
        }
    }

    fn edit_clear_audio_load(&mut self, r: Ref, events: &mut EditAudioEvents) {
        let had = self.edit_audio.lengths.remove(&r).is_some();
        self.edit_audio.seen_ids.remove(&r);
        self.edit_audio.pending_loads.remove(&r);
        if had {
            events.unloaded.push(r);
        }
    }

    /// Toggles Studio 0.748's Properties-panel **Preview** button on `r`
    /// (`Sound` or `AudioPlayer`): idle -> play, playing -> pause (keeping
    /// scrubber position), paused -> resume. Invisible to `Sound` scripts.
    pub fn edit_audio_preview_toggle(&mut self, r: Ref) {
        if self.play.is_some() || !in_dm(&self.doc, r) || !is_audio_class(self.doc.class_of(r).as_deref()) {
            return;
        }
        let mut ev = EditAudioEvents::default();
        self.edit_register_audio_load(r, &mut ev);
        self.edit_audio.last_selection = self.doc.selection.clone();

        if let Some(p) = self.edit_audio.preview.as_mut().filter(|p| p.r == r) {
            if p.playing {
                if let Some(a) = self.edit_audio.audio.as_mut() {
                    a.pause(p.track_key);
                    p.pos = a.position(p.track_key);
                }
                p.playing = false;
                return;
            }
            // Resume from paused/scrubbed position.
            let pos = p.pos;
            let key = p.track_key;
            let can_resume = p.started
                && self
                    .edit_audio
                    .audio
                    .as_ref()
                    .is_some_and(|a| a.position(key) > 0.0 || a.is_playing(key));
            if can_resume {
                if let Some(a) = self.edit_audio.audio.as_mut() {
                    a.resume(key);
                }
                p.playing = true;
                return;
            }
            p.playing = true;
            let _ = pos;
            self.start_edit_preview_track();
            return;
        }

        // Start a fresh preview for `r`.
        self.edit_audio.stop_preview();
        let key = next_track_key(&mut self.edit_audio, r);
        self.edit_audio.preview = Some(PreviewState {
            r,
            playing: true,
            pos: 0.0,
            track_key: key,
            pending: None,
            started: false,
        });
        self.start_edit_preview_track();
    }

    /// Seeks the Properties-panel Preview scrubber for `r` to `secs` seconds.
    /// Does not modify `Sound.TimePosition` or fire any signals.
    pub fn edit_audio_preview_seek(&mut self, r: Ref, secs: f64) {
        if self.play.is_some() || !in_dm(&self.doc, r) || !is_audio_class(self.doc.class_of(r).as_deref()) {
            return;
        }
        let len = self.edit_audio.time_length(r);
        let t = if secs.is_finite() { secs.clamp(0.0, len.max(0.0)) } else { 0.0 };
        self.edit_audio.last_selection = self.doc.selection.clone();

        if let Some(p) = self.edit_audio.preview.as_mut().filter(|p| p.r == r) {
            p.pos = t;
            if p.started {
                if let Some(a) = self.edit_audio.audio.as_mut() {
                    let _ = a.seek(p.track_key, t);
                }
            }
        } else {
            self.edit_audio.stop_preview();
            let key = next_track_key(&mut self.edit_audio, r);
            self.edit_audio.preview = Some(PreviewState {
                r,
                playing: false,
                pos: t,
                track_key: key,
                pending: None,
                started: false,
            });
        }
    }

    fn start_edit_preview_track(&mut self) {
        let Some((r, key, start)) = self
            .edit_audio
            .preview
            .as_ref()
            .map(|p| (p.r, p.track_key, p.pos))
        else {
            return;
        };
        if !in_dm(&self.doc, r) {
            self.edit_audio.stop_preview();
            return;
        }
        let id = asset_id_of(&self.doc, r);
        if id.trim().is_empty() || self.edit_audio.unavailable.contains(&id) {
            if let Some(p) = self.edit_audio.preview.as_mut() {
                p.playing = false;
                p.pending = None;
            }
            return;
        }
        let (volume, speed, looped, region) = preview_params(&self.doc, r);
        let is_sound = self.doc.class_of(r).as_deref() == Some("Sound");
        match self.edit_sound_bytes(&id) {
            Fetch::Ready(bytes) => {
                let len = Audio::length(&bytes);
                let (groups, settings) = if is_sound {
                    rustudio_engine::audio_scene::settings(&self.doc, r, self.cam.pos, self.cam.right(), None)
                } else {
                    (Vec::new(), Default::default())
                };
                let audio = self.edit_audio.audio.get_or_insert_with(Audio::new);
                audio.configure(key, groups, settings);
                if let Err(e) = audio.play(key, bytes, volume, speed, looped, start, region) {
                    if self.edit_audio.warned.insert(id.clone()) {
                        self.warn(format!("Failed to load sound {id}: {e}"));
                    }
                    if let Some(p) = self.edit_audio.preview.as_mut() {
                        p.playing = false;
                        p.pending = None;
                    }
                    return;
                }
                self.edit_audio.starts += 1;
                if let Some(len) = len {
                    self.edit_audio.lengths.insert(r, len);
                }
                if let Some(p) = self.edit_audio.preview.as_mut() {
                    p.pending = None;
                    p.started = true;
                    p.playing = true;
                }
            }
            Fetch::Pending => {
                if let Some(p) = self.edit_audio.preview.as_mut() {
                    p.pending = Some(id.clone());
                }
                self.edit_audio.pending_loads.insert(r, id);
            }
            Fetch::Unavailable(why) => {
                self.edit_audio.unavailable.insert(id.clone());
                if self.edit_audio.warned.insert(id.clone()) {
                    self.warn(format!("Sound {id} isn't available: {why}"));
                }
                if let Some(p) = self.edit_audio.preview.as_mut() {
                    p.playing = false;
                    p.pending = None;
                }
            }
        }
    }

    fn start_edit_detached_sound(
        &mut self,
        key: u64,
        listener: Option<(u32, Option<Xform>, Option<Ref>)>,
    ) {
        let Some(r) = self.edit_audio.detached.get(&key).map(|t| t.sound) else {
            return;
        };
        if !self.doc.exists(r) {
            self.edit_audio.detached.remove(&key);
            return;
        }
        let id = asset_id_of(&self.doc, r);
        if id.trim().is_empty() || self.edit_audio.unavailable.contains(&id) {
            self.edit_audio.detached.remove(&key);
            return;
        }
        let volume = f32_prop(&self.doc, r, "Volume", 0.5).clamp(0.0, 10.0);
        let speed = f32_prop(&self.doc, r, "PlaybackSpeed", 1.0);
        let region = sound_region(&self.doc, r);
        let start = f64_prop(&self.doc, r, "TimePosition", 0.0).max(0.0);
        match self.edit_sound_bytes(&id) {
            Fetch::Ready(bytes) => {
                let (groups, settings) = rustudio_engine::audio_scene::settings(
                    &self.doc,
                    r,
                    self.cam.pos,
                    self.cam.right(),
                    listener,
                );
                let audio = self.edit_audio.audio.get_or_insert_with(Audio::new);
                audio.configure(key, groups, settings);
                if let Err(e) = audio.play(key, bytes, volume, speed, false, start, region) {
                    if self.edit_audio.warned.insert(id.clone()) {
                        self.warn(format!("Failed to load sound {id}: {e}"));
                    }
                    self.edit_audio.detached.remove(&key);
                    return;
                }
                self.edit_audio.starts += 1;
                if let Some(t) = self.edit_audio.detached.get_mut(&key) {
                    t.pending = None;
                }
            }
            Fetch::Pending => {
                if let Some(t) = self.edit_audio.detached.get_mut(&key) {
                    t.pending = Some(id);
                }
            }
            Fetch::Unavailable(why) => {
                self.edit_audio.detached.remove(&key);
                self.edit_audio.unavailable.insert(id.clone());
                if self.edit_audio.warned.insert(id.clone()) {
                    self.warn(format!("Sound {id} isn't available: {why}"));
                }
            }
        }
    }

    /// Steps Edit-mode on-demand audio loading, `PlayLocalSound` detached
    /// copies, and Properties-panel Preview playback for one frame.
    ///
    /// Never runs a place-wide scan: only Sounds / AudioPlayers touched via
    /// incremental DOM marks, single selection, or Preview / PlayLocalSound
    /// are loaded or played.
    pub fn edit_audio_tick(
        &mut self,
        lua_cmds: Vec<(Ref, SoundCmd)>,
        _lua_seeks: Vec<(Ref, f64)>,
        listener: Option<(u32, Option<Xform>, Option<Ref>)>,
    ) -> EditAudioEvents {
        let mut events = EditAudioEvents::default();
        if self.play.is_some() {
            return events;
        }
        if self.edit_audio.doc_id != self.doc.id {
            self.edit_audio.reset();
            self.edit_audio.doc_id = self.doc.id;
            // Discard initial place-open marks so opening a large place
            // never scans or decodes its Sounds.
            self.doc.discard_marks_for(doc::Reader::EditAudio);
            return events;
        }

        // 1. Apply `SoundService:PlayLocalSound` commands queued by Edit Lua.
        //    Normal Play/Pause/Resume/Stop in Edit only update flags & signals
        //    (Studio 0.748) and do not start audible playback.
        for (r, cmd) in lua_cmds {
            if cmd == SoundCmd::PlayLocal && self.doc.exists(r) {
                let key = next_track_key(&mut self.edit_audio, r);
                self.edit_audio.detached.insert(key, DetachedTrack { sound: r, pending: None });
                self.start_edit_detached_sound(key, listener);
            }
        }

        // 2. Consume incremental DOM marks (Properties panel, MCP set_properties,
        //    create/delete, undo/redo). `Marks::All` is intentionally ignored to
        //    prevent place-wide Sound decoding.
        if let Marks::Some(marks) = self.doc.marks_for(doc::Reader::EditAudio) {
            let mut seen_trees: HashSet<Ref> = HashSet::new();
            let mut seen_props: HashSet<(Ref, rbx_dom_weak::Ustr)> = HashSet::new();
            let mut tree_refs: Vec<Ref> = Vec::new();
            let mut prop_edits: Vec<(Ref, rbx_dom_weak::Ustr)> = Vec::new();
            for m in marks {
                match m {
                    Mark::Tree(r) => {
                        if seen_trees.insert(r) {
                            tree_refs.push(r);
                        }
                    }
                    Mark::Prop(r, name) => {
                        if seen_props.insert((r, name)) {
                            prop_edits.push((r, name));
                        }
                    }
                }
            }
            for r in tree_refs {
                if !is_audio_class(self.doc.class_of(r).as_deref()) {
                    continue;
                }
                if in_dm(&self.doc, r) {
                    self.edit_register_audio_load(r, &mut events);
                } else {
                    if self.edit_audio.preview_ref() == Some(r) {
                        self.edit_audio.stop_preview();
                    }
                    self.edit_clear_audio_load(r, &mut events);
                }
            }
            for (r, prop) in prop_edits {
                if !is_audio_class(self.doc.class_of(r).as_deref()) {
                    continue;
                }
                match prop.as_str() {
                    "SoundId" | "AudioContent" | "Asset" => {
                        let id = asset_id_of(&self.doc, r);
                        let changed = self.edit_audio.seen_ids.get(&r) != Some(&id);
                        self.edit_register_audio_load(r, &mut events);
                        if changed && self.edit_audio.preview_ref() == Some(r) {
                            self.edit_audio.stop_preview();
                        }
                    }
                    "Playing" => {
                        // Touching Playing in Edit ensures IsLoaded/TimeLength are ready
                        // without starting audio (Studio 0.748 parity).
                        self.edit_register_audio_load(r, &mut events);
                    }
                    _ => {}
                }
            }
        }

        // 3. Selection changes:
        //    - Selecting a single Sound or AudioPlayer loads its IsLoaded/IsReady
        //      and TimeLength on demand for the Properties panel.
        //    - Deselecting the previewed instance (or selecting something else)
        //      stops Properties preview and resets its thumb to 0 (Studio 0.748).
        if self.doc.selection != self.edit_audio.last_selection {
            if let Some(pr) = self.edit_audio.preview_ref() {
                if self.doc.selection.as_slice() != [pr] {
                    self.edit_audio.stop_preview();
                }
            }
            self.edit_audio.last_selection = self.doc.selection.clone();
        }
        if self.doc.selection.len() == 1 {
            let sel = self.doc.selection[0];
            if self.edit_audio.last_selected_audio != Some(sel) {
                self.edit_audio.last_selected_audio = Some(sel);
                if is_audio_class(self.doc.class_of(sel).as_deref()) && in_dm(&self.doc, sel) {
                    self.edit_register_audio_load(sel, &mut events);
                }
            }
        } else {
            self.edit_audio.last_selected_audio = None;
        }

        // 4. Clean up any loaded/previewed instance whose ancestor left the DataModel.
        if let Some(pr) = self.edit_audio.preview_ref() {
            if !in_dm(&self.doc, pr) {
                self.edit_audio.stop_preview();
            }
        }
        let gone_loaded: Vec<Ref> = self
            .edit_audio
            .lengths
            .keys()
            .copied()
            .filter(|r| !in_dm(&self.doc, *r))
            .collect();
        for r in gone_loaded {
            self.edit_clear_audio_load(r, &mut events);
        }

        // 5. Retry pending cloud asset loads.
        let pending_loads: Vec<(Ref, String)> = self
            .edit_audio
            .pending_loads
            .iter()
            .map(|(r, id)| (*r, id.clone()))
            .collect();
        for (r, id) in pending_loads {
            if !in_dm(&self.doc, r) || asset_id_of(&self.doc, r) != id {
                self.edit_audio.pending_loads.remove(&r);
                continue;
            }
            match self.edit_sound_bytes(&id) {
                Fetch::Ready(bytes) => {
                    self.edit_audio.pending_loads.remove(&r);
                    if let Some(len) = Audio::length(&bytes) {
                        self.edit_audio.lengths.insert(r, len);
                        events.loaded.push((r, id, len));
                    }
                }
                Fetch::Pending => {}
                Fetch::Unavailable(_) => {
                    self.edit_audio.pending_loads.remove(&r);
                    self.edit_audio.unavailable.insert(id);
                }
            }
        }
        if self.edit_audio.preview.as_ref().is_some_and(|p| p.pending.is_some()) {
            self.start_edit_preview_track();
        }
        let pending_detached: Vec<u64> = self
            .edit_audio
            .detached
            .iter()
            .filter(|(_, t)| t.pending.is_some())
            .map(|(k, _)| *k)
            .collect();
        for k in pending_detached {
            self.start_edit_detached_sound(k, listener);
        }

        // 6. Poll finished audio tracks.
        let done = match self.edit_audio.audio.as_mut() {
            Some(a) => {
                let _ = a.looped_since_poll();
                a.poll_finished()
            }
            None => Vec::new(),
        };
        for k in done {
            if self.edit_audio.detached.remove(&k).is_some() {
                continue;
            }
            if let Some(p) = self.edit_audio.preview.as_mut().filter(|p| p.track_key == k) {
                // Non-looped preview ended: reset thumb to 0 and toggle back to ▶ (Studio 0.748).
                p.playing = false;
                p.started = false;
                p.pos = 0.0;
            }
        }

        // 7. Advance preview scrubber & apply live Volume / PlaybackSpeed /
        //    Looped / SoundGroup / SoundEffects / 3D rolloff while previewing.
        let cam = self.cam.pos;
        let right = self.cam.right();
        if let Some((r, key)) = self
            .edit_audio
            .preview
            .as_ref()
            .filter(|p| p.playing && p.started)
            .map(|p| (p.r, p.track_key))
        {
            if let Some(pos) = self.edit_audio.audio.as_ref().filter(|a| a.is_playing(key)).map(|a| a.position(key)) {
                if let Some(p) = self.edit_audio.preview.as_mut() {
                    p.pos = pos;
                }
            }
            let (vol, speed, looped, _region) = preview_params(&self.doc, r);
            let is_sound = self.doc.class_of(r).as_deref() == Some("Sound");
            let (groups, settings) = if is_sound {
                rustudio_engine::audio_scene::settings(&self.doc, r, cam, right, None)
            } else {
                (Vec::new(), Default::default())
            };
            if let Some(a) = self.edit_audio.audio.as_mut() {
                a.configure(key, groups, settings);
                a.set_volume(key, vol);
                a.set_speed(key, speed);
                a.set_looped(key, looped);
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::{ustr, InstanceBuilder};
    use std::thread::sleep;
    use std::time::Duration;

    fn wav_bytes(secs: f64, rate: u32) -> Vec<u8> {
        let n = (secs * rate as f64).round() as u32;
        let data_len = n * 2;
        let mut b = Vec::with_capacity(44 + data_len as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let v: i16 = if (i / 20) % 2 == 0 { 4000 } else { -4000 };
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    fn write_test_sound(name: &str, secs: f64) -> String {
        let dir = rustudio_core::assets::content_dir().join("sounds");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), wav_bytes(secs, 16000));
        format!("rbxasset://sounds/{name}")
    }

    fn new_edit_game() -> Game {
        let mut g = Game::new(None);
        g.edit_audio.audio = Some(Audio::silent());
        // First tick establishes doc_id and discards initial marks.
        g.edit_audio_tick(vec![], vec![], None);
        g
    }

    #[test]
    fn dom_playing_loads_but_does_not_play_or_advance_in_edit_mode() {
        let id = write_test_sound("edit-parity-noop.wav", 0.30);
        let mut g = new_edit_game();
        let ws = g.doc.workspace().unwrap();
        let s = g.doc.insert(
            ws,
            InstanceBuilder::new("Sound")
                .with_name("SilentInEdit")
                .with_property("SoundId", Variant::ContentId(id.into()))
                .with_property("Looped", Variant::Bool(true)),
        );
        // Unplayed parented sound loads IsLoaded / TimeLength on demand.
        let ev = g.edit_audio_tick(vec![], vec![], None);
        assert_eq!(ev.loaded.len(), 1);
        assert!(g.edit_audio.is_loaded(s));
        assert!((g.edit_audio.time_length(s) - 0.30).abs() < 0.02);

        // Setting Playing=true via DOM (Properties panel / MCP) does NOT start audio in Edit (Studio 0.748).
        g.doc
            .set_props("Set Playing", vec![(s, ustr("Playing"), Variant::Bool(true))], None);
        g.edit_audio_tick(vec![], vec![], None);
        sleep(Duration::from_millis(80));
        g.edit_audio_tick(vec![], vec![], None);
        assert_eq!(g.edit_audio.starts, 0, "Playing=true must not start Edit audio");
        assert_eq!(g.edit_audio.playing_count(), 0);
        assert_eq!(g.doc.prop(s, "Playing"), Some(Variant::Bool(true)));
        assert_eq!(g.doc.prop(s, "TimePosition"), Some(Variant::Float64(0.0)));
        assert!(!g.doc.dom.get_by_ref(s).unwrap().properties.contains_key(&ustr("TimePosition")));
    }

    #[test]
    fn properties_preview_plays_pauses_resumes_seeks_loops_and_stops_on_deselect() {
        let id = write_test_sound("edit-preview-ctrl.wav", 0.35);
        let mut g = new_edit_game();
        let ws = g.doc.workspace().unwrap();
        let sg = g.doc.insert(
            g.doc.find_child(g.doc.dom.root_ref(), "SoundService").unwrap(),
            InstanceBuilder::new("SoundGroup").with_property("Volume", Variant::Float32(0.5)),
        );
        let s = g.doc.insert(
            ws,
            InstanceBuilder::new("Sound")
                .with_name("PreviewSound")
                .with_property("SoundId", Variant::ContentId(id.into()))
                .with_property("Looped", Variant::Bool(true))
                .with_property("Volume", Variant::Float32(0.8))
                .with_property("SoundGroup", Variant::Ref(sg)),
        );
        g.doc.selection = vec![s];
        g.edit_audio_tick(vec![], vec![], None);
        assert!(g.edit_audio.is_loaded(s));

        // 1. Click Preview ▶ -> plays audibly, advances scrubber, invisible to Sound properties.
        g.edit_audio_preview_toggle(s);
        assert_eq!(g.edit_audio.starts, 1);
        sleep(Duration::from_millis(80));
        g.edit_audio_tick(vec![], vec![], None);
        let (playing, pos1, len) = g.edit_audio.preview_state(s);
        assert!(playing && pos1 > 0.02 && (len - 0.35).abs() < 0.02, "playing={playing} pos1={pos1} len={len}");
        assert_ne!(g.doc.prop(s, "Playing"), Some(Variant::Bool(true)), "Preview must not set Sound.Playing");
        assert_eq!(g.doc.prop(s, "TimePosition"), Some(Variant::Float64(0.0)), "Preview must not mutate Sound.TimePosition");
        assert!(!g.doc.dom.get_by_ref(s).unwrap().properties.contains_key(&ustr("TimePosition")));

        // 2. Click ⏸ -> pauses at current scrubber position; click ▶ -> resumes without restarting.
        g.edit_audio_preview_toggle(s);
        let (playing, paused_at, _) = g.edit_audio.preview_state(s);
        assert!(!playing && paused_at > 0.02);
        sleep(Duration::from_millis(60));
        g.edit_audio_tick(vec![], vec![], None);
        assert!((g.edit_audio.preview_state(s).1 - paused_at).abs() < 1e-6);
        g.edit_audio_preview_toggle(s);
        assert!(g.edit_audio.preview_state(s).0);
        assert_eq!(g.edit_audio.starts, 1, "resume must not restart");

        // 3. Scrubbing moves preview position without touching Sound.TimePosition.
        g.edit_audio_preview_seek(s, 0.20);
        assert!((g.edit_audio.preview_state(s).1 - 0.20).abs() < 0.02);
        assert_eq!(g.doc.prop(s, "TimePosition"), Some(Variant::Float64(0.0)));
        assert!(!g.doc.dom.get_by_ref(s).unwrap().properties.contains_key(&ustr("TimePosition")));

        // 4. Deselecting stops preview and resets thumb to 0 (Studio 0.748).
        g.doc.selection.clear();
        g.edit_audio_tick(vec![], vec![], None);
        let (playing, pos_after, _) = g.edit_audio.preview_state(s);
        assert!(!playing && pos_after == 0.0);
        assert_eq!(g.edit_audio.playing_count(), 0);

        // 5. Non-looped at 2x speed ends and resets thumb to 0.
        g.doc.set_props(
            "Set OneShot",
            vec![
                (s, ustr("Looped"), Variant::Bool(false)),
                (s, ustr("PlaybackSpeed"), Variant::Float32(2.0)),
            ],
            None,
        );
        g.doc.selection = vec![s];
        g.edit_audio_tick(vec![], vec![], None);
        g.edit_audio_preview_toggle(s);
        let mut finished = false;
        for _ in 0..30 {
            sleep(Duration::from_millis(30));
            g.edit_audio_tick(vec![], vec![], None);
            if !g.edit_audio.preview_state(s).0 {
                finished = true;
                break;
            }
        }
        assert!(finished, "non-looped preview should finish");
        assert_eq!(g.edit_audio.preview_state(s).1, 0.0);
    }

    #[test]
    fn audio_player_loads_and_previews_in_edit() {
        let id = write_test_sound("edit-preview-ap.wav", 0.30);
        let mut g = new_edit_game();
        let ws = g.doc.workspace().unwrap();
        let ap = g.doc.insert(
            ws,
            InstanceBuilder::new("AudioPlayer")
                .with_name("AP")
                .with_property("Asset", Variant::ContentId(id.into()))
                .with_property("Looping", Variant::Bool(true)),
        );
        g.doc.selection = vec![ap];
        g.edit_audio_tick(vec![], vec![], None);
        assert!(g.edit_audio.is_loaded(ap));
        assert!((g.edit_audio.time_length(ap) - 0.30).abs() < 0.02);

        g.edit_audio_preview_toggle(ap);
        sleep(Duration::from_millis(80));
        g.edit_audio_tick(vec![], vec![], None);
        let (playing, pos, _) = g.edit_audio.preview_state(ap);
        assert!(playing && pos > 0.02, "AudioPlayer preview should advance: playing={playing} pos={pos}");
    }

    #[test]
    fn opening_place_with_many_sounds_loads_zero_in_edit_mode() {
        let id = write_test_sound("edit-preview-bulk.wav", 0.50);
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        for i in 0..200 {
            doc.dom.insert(
                ws,
                InstanceBuilder::new("Sound")
                    .with_name(format!("S{i}"))
                    .with_property("SoundId", Variant::ContentId(id.clone().into()))
                    .with_property("Playing", Variant::Bool(true)),
            );
        }
        let mut g = Game::new(None);
        g.doc = doc;
        g.edit_audio.audio = Some(Audio::silent());
        for _ in 0..5 {
            g.edit_audio_tick(vec![], vec![], None);
        }
        assert_eq!(g.edit_audio.loaded_count(), 0, "Edit open must not scan/decode place sounds");
        assert_eq!(g.edit_audio.playing_count(), 0, "Edit open must not autoplay saved sounds");
    }

    #[test]
    fn play_local_sound_and_stop_playback_on_play() {
        let id = write_test_sound("edit-preview-local.wav", 0.50);
        let mut g = new_edit_game();
        let ws = g.doc.workspace().unwrap();
        let s = g.doc.insert(
            ws,
            InstanceBuilder::new("Sound")
                .with_property("SoundId", Variant::ContentId(id.into()))
                .with_property("Playing", Variant::Bool(true)),
        );
        g.doc.selection = vec![s];
        g.edit_audio_tick(vec![], vec![], None);

        // PlayLocalSound plays a detached copy without toggling Sound.Playing.
        g.edit_audio_tick(vec![(s, SoundCmd::PlayLocal)], vec![], None);
        assert_eq!(g.edit_audio.playing_count(), 1);

        // Preview + stop_playback (entering Play) silences preview while keeping doc Playing=true.
        g.edit_audio_preview_toggle(s);
        assert!(g.edit_audio.preview_state(s).0);
        g.edit_audio.stop_playback();
        assert_eq!(g.edit_audio.playing_count(), 0);
        assert!(!g.edit_audio.preview_state(s).0);
        assert_eq!(g.doc.prop(s, "Playing"), Some(Variant::Bool(true)));
        assert!(g.edit_audio.is_loaded(s));

        // Removing Sound from DataModel stops preview and unloads it.
        g.edit_audio_preview_toggle(s);
        assert!(g.edit_audio.preview_state(s).0);
        g.doc.remove(&[s]);
        g.edit_audio_tick(vec![], vec![], None);
        assert!(!g.edit_audio.preview_state(s).0);
        assert_eq!(g.edit_audio.playing_count(), 0);
        assert!(!g.edit_audio.is_loaded(s));
    }
}
