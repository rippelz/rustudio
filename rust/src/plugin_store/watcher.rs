//! Watching the local Plugins folder. Studio acts on
//! plugin files that come, change or go only when settings().Studio
//! "Reload plugins on file changed" (ReloadLocalPluginsOnChange, default
//! off) is on; with it off nothing happens until the next place open. With
//! it on, Output shows a warning and the plugin follows the file:
//! `Detected add/change: user_X; loading/reloading the plugin now!` (load,
//! or unload + load) and `Detected removal: user_X; unloading the plugin now!`.
//!
//! rustudio: a cheap poll of the folder (names, sizes, modification times)
//! about once a second from the editor's frame loop, diffed against the last
//! one. A new or changed file is acted on once it has stopped changing for
//! one poll, so a file still being written isn't loaded half-written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::runtime::PluginRuntime;
use super::{scan_local, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Changed(PathBuf),
    Removed(PathBuf),
}

type Stamp = (Option<SystemTime>, u64);

#[derive(Debug, Default)]
pub struct Watcher {
    dir: PathBuf,
    seen: BTreeMap<PathBuf, Stamp>,
    /// Changes seen once, waiting for the file to settle.
    pending: BTreeMap<PathBuf, Change>,
    last: Option<Instant>,
    pub interval: Duration,
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok();
    (m.as_ref().and_then(|m| m.modified().ok()), m.map(|m| m.len()).unwrap_or(0))
}

impl Watcher {
    /// Starts from what is in `dir` now (those are loaded at startup).
    pub fn new(dir: &Path) -> Watcher {
        let seen = scan_local(dir).into_iter().map(|p| { let s = stamp(&p); (p, s) }).collect();
        Watcher { dir: dir.to_path_buf(), seen, pending: BTreeMap::new(), last: Some(Instant::now()), interval: Duration::from_millis(1000) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Polls if the interval has passed.
    pub fn tick(&mut self) -> Vec<Change> {
        if self.last.is_some_and(|l| l.elapsed() < self.interval) {
            return Vec::new();
        }
        self.poll()
    }

    /// Polls now: settled changes since the last poll.
    pub fn poll(&mut self) -> Vec<Change> {
        self.last = Some(Instant::now());
        let now: BTreeMap<PathBuf, Stamp> = scan_local(&self.dir).into_iter().map(|p| { let s = stamp(&p); (p, s) }).collect();
        let mut raw = Vec::new();
        for (p, s) in &now {
            match self.seen.get(p) {
                None => raw.push(Change::Added(p.clone())),
                Some(old) if old != s => raw.push(Change::Changed(p.clone())),
                _ => {}
            }
        }
        for p in self.seen.keys() {
            if !now.contains_key(p) {
                raw.push(Change::Removed(p.clone()));
            }
        }
        self.seen = now;
        let mut out = Vec::new();
        let moved: Vec<PathBuf> = raw.iter().map(|c| change_path(c).clone()).collect();
        // Pending changes whose file didn't move this poll have settled.
        let pending = std::mem::take(&mut self.pending);
        for (p, c) in pending {
            if !moved.contains(&p) {
                out.push(c);
            } else {
                self.pending.insert(p, c);
            }
        }
        for c in raw {
            let p = change_path(&c).clone();
            match (self.pending.remove(&p), c) {
                // Came and went before settling: nothing happened.
                (Some(Change::Added(_)), Change::Removed(_)) => {}
                (_, Change::Removed(q)) => out.push(Change::Removed(q)),
                (Some(Change::Added(q)), _) => {
                    self.pending.insert(p, Change::Added(q));
                }
                (Some(Change::Removed(_)), _) => {
                    self.pending.insert(p.clone(), Change::Changed(p));
                }
                (_, c) => {
                    self.pending.insert(p, c);
                }
            }
        }
        out
    }
}

pub fn change_path(c: &Change) -> &PathBuf {
    match c {
        Change::Added(p) | Change::Changed(p) | Change::Removed(p) => p,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Acts on folder changes when `reload_on_change` is on (else nothing, as
/// in Studio): load / reload / unload in the runtime. Returns Output lines.
pub fn apply(store: &Store, rt: &mut dyn PluginRuntime, changes: &[Change], reload_on_change: bool) -> Vec<(Level, String)> {
    let mut out = Vec::new();
    if changes.is_empty() || !reload_on_change || !rt.enabled() {
        return out;
    }
    let list = store.list();
    for c in changes {
        let p = change_path(c);
        let id = super::local_id(&store.plugins_dir, p);
        let loaded = rt.loaded().contains(&id);
        match c {
            Change::Added(_) | Change::Changed(_) => {
                let Some(s) = list.iter().find(|s| s.id == id) else { continue };
                if !s.enabled {
                    continue;
                }
                out.push((Level::Warn, format!("Detected add/change: {id}; loading/reloading the plugin now!")));
                let r = if loaded { rt.reload(s) } else { rt.load(s) };
                if let Err(e) = r {
                    out.push((Level::Error, format!("Plugin {id} failed to load: {e}")));
                }
            }
            Change::Removed(_) => {
                out.push((Level::Warn, format!("Detected removal: {id}; unloading the plugin now!")));
                if loaded {
                    rt.unload(&id);
                }
            }
        }
    }
    out
}

/// Whether Studio Settings has "Reload plugins on file changed" on.
pub fn reload_on_change_setting() -> bool {
    matches!(
        crate::studio_settings::get("Studio", "ReloadLocalPluginsOnChange"),
        Some(crate::studio_settings::Val::Bool(true))
    )
}

#[cfg(test)]
mod tests {
    use super::super::runtime::NoRuntime;
    use super::super::tests::tmp;
    use super::*;

    fn settle(w: &mut Watcher) -> Vec<Change> {
        let mut all = w.poll();
        all.extend(w.poll());
        all
    }

    #[test]
    fn add_change_remove() {
        let root = tmp("watch");
        let dir = root.join("Plugins");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.lua"), "print(0)").unwrap();
        let mut w = Watcher::new(&dir);
        assert!(w.poll().is_empty());
        let a = dir.join("new.lua");
        std::fs::write(&a, "print(1)").unwrap();
        // First sight waits for the file to settle, then it's reported once.
        assert!(w.poll().is_empty());
        assert_eq!(w.poll(), vec![Change::Added(a.clone())]);
        assert!(w.poll().is_empty());
        std::fs::write(&a, "print(22)").unwrap();
        assert_eq!(settle(&mut w), vec![Change::Changed(a.clone())]);
        std::fs::remove_file(&a).unwrap();
        assert_eq!(w.poll(), vec![Change::Removed(a.clone())]);
        // Not a plugin file: ignored.
        std::fs::write(dir.join("readme.md"), "x").unwrap();
        assert!(settle(&mut w).is_empty());
        // Added then gone before settling: nothing.
        let b = dir.join("blip.lua");
        std::fs::write(&b, "x").unwrap();
        assert!(w.poll().is_empty());
        std::fs::remove_file(&b).unwrap();
        assert!(settle(&mut w).is_empty());
    }

    #[test]
    fn apply_follows_enable_and_setting() {
        let root = tmp("apply");
        let dir = root.join("Plugins");
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = Store::open(root.clone(), dir.clone(), 0);
        let mut rt = NoRuntime::default();
        let mut w = Watcher::new(&dir);
        let a = dir.join("A.lua");
        std::fs::write(&a, "print(1)").unwrap();
        // Setting off: nothing happens (until the next place open).
        let ch = settle(&mut w);
        assert_eq!(ch.len(), 1);
        assert!(apply(&store, &mut rt, &ch, false).is_empty());
        assert!(rt.loaded.is_empty());
        // On: add loads, change reloads, removal unloads, with Studio's lines.
        std::fs::write(&a, "print(2) -- longer").unwrap();
        let ch = settle(&mut w);
        let lines = apply(&store, &mut rt, &ch, true);
        assert_eq!(lines, vec![(Level::Warn, "Detected add/change: user_A.lua; loading/reloading the plugin now!".to_string())]);
        assert_eq!(rt.loaded, vec!["user_A.lua"]);
        std::fs::write(&a, "print(3) -- longer still").unwrap();
        let ch = settle(&mut w);
        apply(&store, &mut rt, &ch, true);
        assert_eq!(rt.log.last().unwrap(), "reload user_A.lua");
        std::fs::remove_file(&a).unwrap();
        let ch = w.poll();
        let lines = apply(&store, &mut rt, &ch, true);
        assert_eq!(lines[0].1, "Detected removal: user_A.lua; unloading the plugin now!");
        assert!(rt.loaded.is_empty());
        // A disabled file isn't loaded when it appears.
        std::fs::write(dir.join("B.lua"), "x").unwrap();
        store.set_enabled("user_B.lua", false).unwrap();
        let ch = settle(&mut w);
        assert!(apply(&store, &mut rt, &ch, true).is_empty());
        assert!(rt.loaded.is_empty());
    }
}
