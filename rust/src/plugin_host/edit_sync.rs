//! Edits made outside the plugin runtime reach plugin scripts as ordinary
//! signals. The editor, undo / redo, file sync and Team Create change the
//! edit document directly and note each change in its journal; the plugin
//! runtime reads the journal as its own reader (doc::Reader::Plugins) once
//! a frame, before plugin code runs, and fires what a script's own write
//! would: Changed / GetPropertyChangedSignal for property marks, and
//! ChildAdded / ChildRemoved / DescendantAdded / DescendantRemoving /
//! AncestryChanged / Changed("Parent") for instances created, moved or
//! removed. Its own marks (what plugin code did this frame) are read
//! and dropped right after the frame, so nothing fires twice.
//!
//! Hierarchy marks say only "this subtree changed": the old parent comes
//! from a parent map of the DataModel kept here (built once, about 50k
//! entries for a big place, then updated from the marks). A removed
//! instance is lent back from the undo limbo (and stays: a plugin may hold
//! it, as in Studio); with Immediate signals its DescendantRemoving fires
//! with it put back in place for the moment (Studio fires it before the
//! removal, handlers read its Parent), the rest after. A place too busy for the journal (a reader
//! falls behind: Marks::All) rebuilds the map without firing.
//!
//! The selection is compared each frame too: Selection.SelectionChanged
//! fires when the editor's selection differs from the last one plugins saw.

use std::collections::{HashMap, HashSet};

use rbx_dom_weak::types::Ref;

use rustudio_core::doc::{Document, Mark, Marks, Reader};
use rustudio_engine::runtime::Runtime;

#[derive(Default)]
pub struct EditSync {
    parents: HashMap<Ref, Ref>,
    built: bool,
    selection: Vec<Ref>,
}

impl EditSync {
    /// Forgets everything (a new document).
    pub fn reset(&mut self) {
        *self = EditSync::default();
    }

    fn rebuild(&mut self, d: &Document) {
        self.parents.clear();
        let root = d.dom.root_ref();
        for i in d.dom.descendants_of(root) {
            if i.referent() != root {
                self.parents.insert(i.referent(), i.parent());
            }
        }
        self.built = true;
    }

    /// Takes the marks other writers left since the last frame, while the
    /// document is still the editor's. Call [`EditSync::fire`] with the
    /// result once the document is in the runtime.
    pub fn take(&mut self, d: &mut Document) -> Pending {
        let marks = d.marks_for(Reader::Plugins);
        if !self.built {
            self.rebuild(d);
            self.selection = d.selection.clone();
            return Pending::default();
        }
        let mut out = Pending::default();
        match marks {
            Marks::All => {
                self.rebuild(d);
            }
            Marks::Some(list) => {
                let mut seen_props: HashSet<(Ref, rbx_dom_weak::Ustr)> = HashSet::new();
                let mut seen_trees: HashSet<Ref> = HashSet::new();
                for m in list {
                    match m {
                        Mark::Prop(r, name) => {
                            if seen_props.insert((r, name)) {
                                out.props.push((r, name.to_string()));
                            }
                        }
                        Mark::Tree(r) => {
                            if seen_trees.insert(r) {
                                self.tree_change(d, r, &mut out);
                            }
                        }
                    }
                }
            }
        }
        if d.selection != self.selection {
            self.selection = d.selection.clone();
            out.selection_changed = true;
        }
        out
    }

    fn tree_change(&mut self, d: &Document, r: Ref, out: &mut Pending) {
        let root = d.dom.root_ref();
        if r == root {
            return;
        }
        let old = self.parents.get(&r).copied();
        let now = d.dom.get_by_ref(r).map(|i| i.parent());
        // In the DataModel now?
        let in_model = now.is_some() && d.is_ancestor(root, r);
        let new = if in_model { now } else { None };
        if old == new {
            return;
        }
        // r and everything under it (AncestryChanged fires on r too, as the
        // runtime's own reparent does).
        let below: Vec<Ref> = d.subtree(r);
        match new {
            Some(p) => {
                self.parents.insert(r, p);
                if let Some(dom_i) = d.dom.get_by_ref(r) {
                    let _ = dom_i;
                    for x in d.dom.descendants_of(r) {
                        if x.referent() != r {
                            self.parents.insert(x.referent(), x.parent());
                        }
                    }
                }
            }
            None => {
                self.parents.remove(&r);
                for x in &below {
                    self.parents.remove(x);
                }
            }
        }
        out.trees.push((r, old.unwrap_or_else(Ref::none), new.unwrap_or_else(Ref::none), below));
    }

    /// After the frame: drops the marks the plugin runtime made itself, and
    /// keeps the parent map current with them. Returns whether any of them
    /// was in the DataModel (a plugin edited the place), not only on
    /// plugin-owned instances outside it.
    pub fn after_frame(&mut self, d: &mut Document) -> bool {
        let root = d.dom.root_ref();
        let mut touched = false;
        if let Marks::Some(list) = d.marks_for(Reader::Plugins) {
            let mut sink = Pending::default();
            let mut seen: HashSet<Ref> = HashSet::new();
            for m in list {
                match m {
                    Mark::Tree(r) => {
                        if seen.insert(r) {
                            let before = sink.trees.len();
                            let was_in = self.parents.contains_key(&r);
                            self.tree_change(d, r, &mut sink);
                            let t = (was_in || sink.trees.len() > before) && !rustudio_core::doc::in_unsaved_service(d, r);
                            if t && std::env::var_os("RUSTUDIO_PLUGIN_DEBUG_MARKS").is_some() {
                                eprintln!("plugin touched tree {:?} {:?}", d.class_of(r), d.dom.get_by_ref(r).map(|i| i.name.clone()));
                            }
                            touched |= t;
                        }
                    }
                    Mark::Prop(r, name) => {
                        let t = (r == root || self.parents.contains_key(&r)) && !rustudio_core::doc::in_unsaved_service(d, r);
                        if t && std::env::var_os("RUSTUDIO_PLUGIN_DEBUG_MARKS").is_some() {
                            eprintln!("plugin touched {:?}.{name}", d.class_of(r));
                        }
                        touched |= t
                    }
                }
            }
        } else {
            self.rebuild(d);
            touched = true;
        }
        // (The selection is compared at the next `take`: a plugin's own
        // Selection:Set is announced then too.)
        touched
    }
}

impl EditSync {
    /// The selection plugins have heard about (their own Selection:Set
    /// fired already, by the host).
    pub fn note_selection(&mut self, d: &Document) {
        self.selection = d.selection.clone();
    }
}

/// Changes waiting to be fired once the runtime holds the document.
#[derive(Default)]
pub struct Pending {
    pub props: Vec<(Ref, String)>,
    /// (instance, old parent, new parent, its descendants).
    pub trees: Vec<(Ref, Ref, Ref, Vec<Ref>)>,
    pub selection_changed: bool,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.props.is_empty() && self.trees.is_empty() && !self.selection_changed
    }

    /// Fires the signals (the runtime's World holds the document). Removed
    /// instances are lent back from the undo limbo for the frame, so their
    /// handlers can read them (Studio's still exist); the host returns them
    /// with `relimbo` after the frame.
    pub fn fire(self, rt: &Runtime) -> Vec<Ref> {
        let mut lent = Vec::new();
        for (r, old, new, below) in self.trees {
            if new.is_none() {
                let immediate = {
                    let Ok(mut w) = rustudio_engine::runtime::world(&rt.lua) else { continue };
                    if w.dm().unlimbo(r) {
                        lent.push(r);
                    }
                    w.signal_mode == rustudio_engine::runtime::signal_timing::SignalMode::Immediate
                };
                // Immediate signals: DescendantRemoving runs while the
                // instance is still in place (Studio fires it before the
                // removal, so handlers read its Parent); the rest after.
                // Deferred: they all run later, after the removal (Studio's
                // handler of an undone insert reads Parent nil).
                if immediate && old.is_some() {
                    let back = rustudio_engine::runtime::world(&rt.lua).is_ok_and(|mut w| {
                        let ok = w.d().exists(r) && w.d().exists(old);
                        if ok {
                            w.dm().dom.transfer_within(r, old);
                        }
                        ok
                    });
                    if back {
                        if let Err(e) = rustudio_engine::runtime::fire_descendant_removing(&rt.lua, old, &below) {
                            rt.report_error(&e);
                        }
                        rt.dispatch();
                        if let Ok(mut w) = rustudio_engine::runtime::world(&rt.lua) {
                            if w.d().exists(r) {
                                w.dm().dom.transfer_within(r, Ref::none());
                            }
                        }
                        if let Err(e) = rustudio_engine::runtime::fire_parent_signals_opts(&rt.lua, r, old, new, below, false) {
                            rt.report_error(&e);
                        }
                        continue;
                    }
                }
            }
            rt.notify_parent(r, old, new, below);
        }
        for (r, name) in self.props {
            rt.notify_prop(r, &name);
        }
        if self.selection_changed {
            let sel = rustudio_engine::runtime::world(&rt.lua).ok().and_then(|w| {
                let d = w.d();
                d.find_child(d.dom.root_ref(), "Selection")
            });
            if let Some(s) = sel {
                rt.fire(s, "SelectionChanged", vec![]);
                rt.fire(s, "SelectionChangedThisFrame", vec![]);
            }
        }
        lent
    }
}
