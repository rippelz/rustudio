//! ChangeHistoryService's model on the document's one undo stack (a child
//! module of doc.rs). Everything here was measured in Studio 0.747 with
//! probe plugins (checks/plugins/services/probes/SvcH*_history.lua,
//! captures in checks/plugins/services/captures/, and REF's
//! checks/plugins/ref/captures/scenarios.txt):
//!
//! - Script changes (plugins, the edit DataModel) are recorded as they
//!   happen and wait as "pending" changes; `SetWaypoint(name)` turns
//!   everything since the previous waypoint into one undo step named
//!   `name` ("" and numbers too), and nothing when nothing changed. Every
//!   SetWaypoint clears the redo stack, even an empty one.
//! - The person's own edits (Delete, drags, Properties…) take the pending
//!   changes into their step: Ctrl+Z after a plugin rename + a Delete
//!   reverts both (Studio names that step DeleteSelectedAction).
//! - Undo (CHS:Undo or Ctrl+Z) first folds the pending changes, and an open
//!   recording's, into the last step, then reverts it. Property changes of
//!   an instance that entered the DataModel in that same step aren't
//!   replayed (Studio: the part goes, its later rename stays).
//! - Not recorded: `Source` (the script editor keeps its own undo), anything
//!   outside the DataModel, CoreGui / PluginGuiService / StudioService
//!   (UNSAVED_SERVICES), the current camera, the selection.
//! - `Destroy` makes a step but its undo can't bring the instance back
//!   (Parent stays nil and locked).
//! - Recordings: one at a time for every plugin (a second TryBeginRecording
//!   gets nil); Commit makes a step named after the recording (nothing when
//!   empty), Cancel reverts at once (with any id), Append joins the last step. SetWaypoint
//!   while one is open does nothing. ResetWaypoints keeps the recording open
//!   (and drops what it held).
//! - SetEnabled(false) clears the history and records nothing until
//!   SetEnabled(true).
//! - OnUndo / OnRedo / OnRecordingStarted / OnRecordingFinished are queued
//!   here (also for the editor's own Ctrl+Z / Ctrl+Y) and fired by the
//!   plugin host after the calling code yields, in order.

use rbx_dom_weak::types::Ref;

use super::{Change, Command, Document, UNSAVED_SERVICES};

/// Studio's FinishRecordingOperation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishOp {
    Cancel = 0,
    Commit = 1,
    Append = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HistoryEvent {
    Undo(String),
    Redo(String),
    RecordingStarted { name: String, display: Option<String> },
    RecordingFinished { name: String, display: String, id: String, op: FinishOp },
}

#[derive(Debug, Clone)]
struct Recording {
    id: String,
    name: String,
    display: Option<String>,
    changes: Vec<Change>,
}

#[derive(Debug, Default)]
pub struct History {
    pending: Vec<Change>,
    disabled: bool,
    recording: Option<Recording>,
    events: Vec<HistoryEvent>,
    /// The changes the last undo / redo applied, in the order applied (the
    /// plugin runtime fires their signals when it made the undo itself).
    applied: Vec<Change>,
    seq: u64,
}

/// How many queued events wait for a reader before the oldest go (nothing
/// reads them while no plugin runs).
const MAX_EVENTS: usize = 256;

/// The waypoint name Studio gives the person's own edits, where measured
/// (OnUndo / GetCanUndo); rustudio's other labels are its own.
pub fn studio_step_name(label: &str) -> &str {
    match label {
        "Delete" => "DeleteSelectedAction",
        l => l,
    }
}

impl History {
    fn push_event(&mut self, e: HistoryEvent) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(e);
    }
}

fn change_ref(c: &Change) -> Ref {
    match c {
        Change::Prop { r, .. }
        | Change::Name { r, .. }
        | Change::Parent { r, .. }
        | Change::Create { r, .. }
        | Change::Remove { r, .. }
        | Change::Enter { r, .. }
        | Change::Destroy { r } => *r,
    }
}

/// Drops property / name changes of instances that entered the DataModel
/// in the same step (Studio replays only their arrival).
fn coalesce(changes: &mut Vec<Change>) {
    let entered: std::collections::HashSet<Ref> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Enter { r, .. } | Change::Create { r, .. } => Some(*r),
            _ => None,
        })
        .collect();
    if entered.is_empty() {
        return;
    }
    changes.retain(|c| !matches!(c, Change::Prop { r, .. } | Change::Name { r, .. } if entered.contains(r)));
}

impl Document {
    /// Whether a script's change of `r` goes into the change history: `r`
    /// is in the DataModel (or is it), not in a session-only service, not
    /// the current camera; recording is on.
    pub fn records_change(&self, r: Ref) -> bool {
        if self.history.disabled {
            return false;
        }
        let root = self.dom.root_ref();
        if r == root {
            return true;
        }
        let mut cur = r;
        loop {
            let Some(i) = self.dom.get_by_ref(cur) else { return false };
            let p = i.parent();
            if p.is_none() {
                return false;
            }
            if p == root {
                return !UNSAVED_SERVICES.contains(&i.class.as_str());
            }
            if cur == r && i.class == "Camera" && self.workspace().is_some_and(|ws| ws == p) {
                // Workspace.CurrentCamera: the editor's own view.
                let cam = self.dom.get_by_ref(p).and_then(|w| match w.properties.get(&rbx_dom_weak::ustr("CurrentCamera")) {
                    Some(rbx_dom_weak::types::Variant::Ref(c)) => Some(*c),
                    _ => None,
                });
                if cam.is_none_or(|c| c == r) {
                    return false;
                }
            }
            cur = p;
        }
    }

    /// A script changed the document (already applied): kept for the next
    /// waypoint (or the open recording).
    pub fn note_script_change(&mut self, c: Change) {
        if self.history.disabled {
            return;
        }
        match &mut self.history.recording {
            Some(rec) => rec.changes.push(c),
            None => self.history.pending.push(c),
        }
    }

    /// The instance a script just moved out (its last Parent change) was
    /// destroyed: its step can't bring it back.
    pub fn note_script_destroyed(&mut self, r: Ref) {
        let list = match &mut self.history.recording {
            Some(rec) => &mut rec.changes,
            None => &mut self.history.pending,
        };
        if let Some(c) = list.iter_mut().rev().find(|c| matches!(c, Change::Parent { r: x, .. } | Change::Enter { r: x, .. } if *x == r)) {
            *c = Change::Destroy { r };
        }
    }

    /// The pending script changes join a step the person makes.
    pub(super) fn take_pending_for_step(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.history.pending)
    }

    /// Before an undo: what scripts changed since the last waypoint, and an
    /// open recording's changes, join the last step.
    fn fold_pending(&mut self) {
        if self.undo.is_empty() {
            return;
        }
        let mut extra = std::mem::take(&mut self.history.pending);
        if let Some(rec) = &mut self.history.recording {
            extra.append(&mut rec.changes);
        }
        if extra.is_empty() {
            return;
        }
        let last = self.undo.last_mut().expect("a step");
        last.changes.append(&mut extra);
        coalesce(&mut last.changes);
    }

    /// Takes back a transient insert (a Toolbox card dragged into the 3D
    /// view and back out before the release): `roots` are what the insert
    /// put in the DataModel, `made` them and everything under them as
    /// inserted. Only that goes: every one of those instances leaves the
    /// DataModel (also one a script moved out of the dragged model to
    /// another parent meanwhile), and every change of them leaves the
    /// history (undo and redo steps, the pending script changes, an open
    /// recording), so no undo or redo can bring them back. Everything else stays as it was: other script changes made
    /// before or during the drag (still pending, or in the step they were
    /// swept into), steps made on top of the insert mid-drag, the redo stack.
    /// Changes the insert step had swept up from scripts go back to pending
    /// when it was the last step (as before the insert), else stay as that
    /// step. An instance that isn't the insert's but sits under one of its
    /// instances (moved or created there mid-drag) stays: it goes back to
    /// where it was before (the default parent when that is gone), and its
    /// recorded moves through the insert's instances are rewritten to that
    /// place (`retract_rewrite_foreign`). No OnUndo / OnRedo (none is fired
    /// for this in rustudio; Studio's behaviour there is unmeasured). False
    /// when none of `roots` came from a recorded insert or still exists.
    pub fn retract_inserted(&mut self, roots: &[Ref], made: &std::collections::HashSet<Ref>) -> bool {
        let is_root_create = |c: &Change| matches!(c, Change::Create { r, .. } if roots.contains(r));
        let at = self.undo.iter().rposition(|c| c.changes.iter().any(is_root_create));
        let in_redo = self.redo.iter().any(|c| c.changes.iter().any(is_root_create));
        let alive = roots.iter().any(|r| self.dom.get_by_ref(*r).is_some() || self.limbo.get_by_ref(*r).is_some());
        if at.is_none() && !in_redo && !alive {
            return false;
        }
        let sel_before = at.map(|i| self.undo[i].sel_before.clone());
        // Where each foreign instance (not the insert's) stands for the time
        // it spent under the insert's instances: the parent it had before
        // (or the default parent when that is gone or none). Its history is
        // rewritten to that place, so no undo or redo puts it back under a
        // removed instance (a plugin may still hold those, lent back to the
        // DOM parentless by its edit sync); it is moved back there now.
        let back = self.retract_rewrite_foreign(made);
        let mut foreign = Vec::new();
        for &r in made {
            let Some(inst) = self.dom.get_by_ref(r) else { continue };
            foreign.extend(inst.children().iter().copied().filter(|c| !made.contains(c)));
        }
        let fallback = self.default_parent();
        for x in foreign {
            let mut to = back.get(&x).copied().filter(|p| self.exists(*p)).unwrap_or(fallback);
            if to == x || self.is_ancestor(x, to) || made.contains(&to) {
                to = fallback;
            }
            self.dom.transfer_within(x, to);
            self.mark_tree(x);
        }
        // The insert leaves the DataModel, every instance it made, wherever
        // a script moved it meanwhile (a child taken out of the dragged model
        // goes as well as the model). They wait in limbo like removed
        // instances, unreachable from any step now. (One already out of the
        // DataModel stays where it is.)
        let tops: Vec<Ref> = made
            .iter()
            .copied()
            .filter(|r| self.dom.get_by_ref(*r).is_some_and(|i| i.parent().is_some() && !made.contains(&i.parent())))
            .collect();
        for r in tops {
            if r == self.dom.root_ref() {
                continue;
            }
            self.mark_tree(r);
            let lr = self.limbo.root_ref();
            self.dom.transfer(r, &mut self.limbo, lr);
        }
        // Its changes leave the history; nothing else does.
        let keep = |c: &Change| !made.contains(&change_ref(c));
        self.history.pending.retain(keep);
        if let Some(rec) = &mut self.history.recording {
            rec.changes.retain(keep);
        }
        for cmd in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            cmd.changes.retain(keep);
        }
        if let Some(i) = at {
            if i + 1 == self.undo.len() && !self.undo[i].changes.is_empty() {
                // (what scripts had pending when it was inserted: pending again)
                let mut back = self.undo.pop().expect("the insert step").changes;
                back.append(&mut self.history.pending);
                self.history.pending = back;
            }
        }
        self.undo.retain(|c| !c.changes.is_empty());
        self.redo.retain(|c| !c.changes.is_empty());
        let had: Vec<Ref> = self.selection.clone();
        self.selection.retain(|r| !made.contains(r));
        if self.selection.is_empty() && had.len() != self.selection.len() {
            if let Some(sel) = sel_before {
                self.selection = sel;
            }
        }
        self.prune_selection();
        self.revision += 1;
        true
    }

    /// `retract_inserted`'s history rewrite of foreign instances' moves
    /// through the insert's instances (`made`): in the order the changes
    /// happened (undo steps, then pending, then an open recording; the redo
    /// steps from the state before the pending ones), a parent in `made` is
    /// replaced by the place the instance had before it went under one, and
    /// moves that become no move are dropped. Returns that place for each
    /// foreign instance as of now.
    fn retract_rewrite_foreign(&mut self, made: &std::collections::HashSet<Ref>) -> std::collections::HashMap<Ref, Ref> {
        use std::collections::HashMap;
        let fallback = self.default_parent();
        let valid = |p: Ref, d: &Document| p.is_some() && !made.contains(&p) && d.exists(p);
        let fix = |c: &mut Change, at: &mut HashMap<Ref, Ref>, d: &Document| -> bool {
            match c {
                Change::Parent { r, old, new } | Change::Enter { r, old, new } if !made.contains(r) => {
                    if !made.contains(old) && !made.contains(new) {
                        return true;
                    }
                    if made.contains(old) {
                        *old = at.get(r).copied().unwrap_or(fallback);
                    }
                    if made.contains(new) {
                        let stay = if valid(*old, d) { *old } else { fallback };
                        at.insert(*r, stay);
                        *new = stay;
                    }
                    old != new
                }
                Change::Create { r, parent } | Change::Remove { r, parent } if !made.contains(r) && made.contains(parent) => {
                    *parent = at.get(r).copied().unwrap_or(fallback);
                    true
                }
                _ => true,
            }
        };
        let mut at: HashMap<Ref, Ref> = HashMap::new();
        let mut undo = std::mem::take(&mut self.undo);
        for cmd in undo.iter_mut() {
            cmd.changes.retain_mut(|c| fix(c, &mut at, self));
        }
        let mut at_redo = at.clone();
        let mut pending = std::mem::take(&mut self.history.pending);
        pending.retain_mut(|c| fix(c, &mut at, self));
        let mut rec_changes = self.history.recording.as_mut().map(|rec| std::mem::take(&mut rec.changes));
        if let Some(list) = &mut rec_changes {
            list.retain_mut(|c| fix(c, &mut at, self));
        }
        let mut redo = std::mem::take(&mut self.redo);
        for cmd in redo.iter_mut().rev() {
            cmd.changes.retain_mut(|c| fix(c, &mut at_redo, self));
        }
        self.undo = undo;
        self.redo = redo;
        self.history.pending = pending;
        if let (Some(rec), Some(list)) = (&mut self.history.recording, rec_changes) {
            rec.changes = list;
        }
        at
    }

    /// Records changes already applied live (a drag the editor's dragger
    /// made) as one step named `label`; pending script changes join it.
    pub fn record_changes(&mut self, label: &str, changes: Vec<Change>) {
        let changes: Vec<Change> = changes
            .into_iter()
            .filter(|c| match c {
                Change::Prop { old, new, .. } => old != new,
                Change::Parent { old, new, .. } => old != new,
                _ => true,
            })
            .collect();
        self.push(label, changes, None);
    }

    /// Moves `r` under `parent` without recording it (a live drag; the
    /// step comes from `record_changes`). Returns its parent before.
    pub fn reparent_live(&mut self, r: Ref, parent: Ref) -> Option<Ref> {
        let old = self.dom.get_by_ref(r)?.parent();
        if old == parent || !self.exists(parent) || self.is_ancestor(r, parent) {
            return Some(old);
        }
        self.dom.transfer_within(r, parent);
        self.mark_tree(r);
        self.revision += 1;
        Some(old)
    }

    pub(super) fn history_undo_begin(&mut self) {
        self.fold_pending();
    }

    pub(super) fn history_after(&mut self, undo: bool, cmd: &Command) {
        let name = studio_step_name(&cmd.label).to_string();
        self.history.push_event(if undo { HistoryEvent::Undo(name) } else { HistoryEvent::Redo(name) });
        self.history.applied = if undo { cmd.changes.iter().rev().cloned().collect() } else { cmd.changes.clone() };
    }

    /// The changes the last undo / redo applied (`forward`: redo), once.
    pub fn take_applied(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.history.applied)
    }

    /// Pushes an already-applied step (no journal marks: the changes were
    /// marked when made). Returns whether a non-empty step was pushed.
    fn push_step(&mut self, label: String, display: Option<String>, mut changes: Vec<Change>) -> bool {
        coalesce(&mut changes);
        if changes.is_empty() {
            return false;
        }
        self.redo.clear();
        let sel_before = self.selection.clone();
        self.undo.push(Command {
            label,
            changes,
            merge_key: None,
            at: std::time::Instant::now(),
            sel_before,
            sel_after: Vec::new(),
            display,
        });
        if self.undo.len() > crate::mech::UNDO_LIMIT {
            self.undo.remove(0);
        }
        true
    }

    /// Commits pending script changes as one undo step when there are any
    /// (and no ChangeHistoryService recording is open). Unlike
    /// `chs_set_waypoint`, a no-op call leaves the redo stack intact.
    pub fn commit_pending_script_changes(&mut self, label: &str) -> bool {
        if self.history.recording.is_some() || self.history.pending.is_empty() {
            return false;
        }
        let changes = std::mem::take(&mut self.history.pending);
        self.push_step(label.to_string(), None, changes)
    }

    // ------------------------------------------- ChangeHistoryService calls

    pub fn chs_set_waypoint(&mut self, name: &str) {
        if self.history.recording.is_some() {
            return;
        }
        self.redo.clear();
        let changes = std::mem::take(&mut self.history.pending);
        self.push_step(name.to_string(), None, changes);
    }

    /// GetCanUndo: the last step's name.
    pub fn chs_can_undo(&self) -> Option<String> {
        self.undo.last().map(|c| studio_step_name(&c.label).to_string())
    }

    pub fn chs_can_redo(&self) -> Option<String> {
        self.redo.last().map(|c| studio_step_name(&c.label).to_string())
    }

    pub fn chs_reset(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.history.pending.clear();
        if let Some(rec) = &mut self.history.recording {
            rec.changes.clear();
        }
    }

    pub fn chs_set_enabled(&mut self, on: bool) {
        if !on {
            self.chs_reset();
        }
        self.history.disabled = !on;
    }

    pub fn chs_enabled(&self) -> bool {
        !self.history.disabled
    }

    /// TryBeginRecording: the recording's id, or None while one is open.
    pub fn chs_begin_recording(&mut self, name: &str, display: Option<&str>) -> Option<String> {
        if self.history.recording.is_some() {
            return None;
        }
        // "recording_" + a decimal u64, as Studio's (random-looking).
        self.history.seq = self.history.seq.wrapping_add(1);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut x = t ^ (self.id << 32) ^ self.history.seq.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        let id = format!("recording_{x}");
        self.history.recording = Some(Recording {
            id: id.clone(),
            name: name.to_string(),
            display: display.map(str::to_string),
            changes: Vec::new(),
        });
        self.history.push_event(HistoryEvent::RecordingStarted {
            name: name.to_string(),
            display: display.map(str::to_string),
        });
        Some(id)
    }

    pub fn chs_recording_in_progress(&self, id: Option<&str>) -> bool {
        match (&self.history.recording, id) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(r), Some(id)) => r.id == id,
        }
    }

    /// FinishRecording. Cancel reverts the recording's changes now and
    /// returns them (applied, in order) for the runtime's signals.
    pub fn chs_finish_recording(&mut self, id: &str, op: FinishOp) -> Result<Vec<Change>, &'static str> {
        // (Cancel ends the open recording whatever id it is given: Studio,
        // captures/misc2.txt `finish.wrongid.cancel`.)
        match &self.history.recording {
            None => return Err("No recording in progress."),
            Some(r) if r.id != id && op != FinishOp::Cancel => return Err("Invalid recording identifier."),
            _ => {}
        }
        let rec = self.history.recording.take().expect("recording");
        let mut reverted = Vec::new();
        match op {
            FinishOp::Commit => {
                self.redo.clear();
                self.push_step(rec.name.clone(), rec.display.clone(), rec.changes);
            }
            FinishOp::Cancel => {
                for c in rec.changes.iter().rev() {
                    self.apply(c, false);
                    reverted.push(c.clone());
                }
                self.revision += 1;
            }
            FinishOp::Append => match self.undo.last_mut() {
                Some(last) => {
                    let mut ch = rec.changes;
                    last.changes.append(&mut ch);
                    coalesce(&mut last.changes);
                    self.redo.clear();
                }
                None => {
                    self.redo.clear();
                    self.push_step(rec.name.clone(), rec.display.clone(), rec.changes);
                }
            },
        }
        self.history.push_event(HistoryEvent::RecordingFinished {
            display: rec.display.clone().unwrap_or_else(|| rec.name.clone()),
            name: rec.name,
            id: rec.id,
            op,
        });
        Ok(reverted)
    }

    /// The queued OnUndo / OnRedo / recording events, oldest first.
    pub fn take_history_events(&mut self) -> Vec<HistoryEvent> {
        std::mem::take(&mut self.history.events)
    }

    /// Instances the change history can still bring back (the plugin
    /// runtime's collector keeps them).
    pub fn history_refs(&self) -> std::collections::HashSet<Ref> {
        let mut out = std::collections::HashSet::new();
        let rec = self.history.recording.iter().flat_map(|r| r.changes.iter());
        for c in self
            .undo
            .iter()
            .chain(self.redo.iter())
            .flat_map(|c| c.changes.iter())
            .chain(self.history.pending.iter())
            .chain(rec)
        {
            out.insert(change_ref(c));
        }
        out
    }

    /// The step label the editor shows (a recording's display name).
    pub fn undo_display(&self) -> Option<&str> {
        self.undo.last().map(|c| c.display.as_deref().unwrap_or(&c.label))
    }
}
