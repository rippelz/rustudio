//! File Sync: a Rojo / Argon project folder attached to whatever place is
//! open — a place file, a place opened from Roblox, a live Team Create
//! session — so files saved on disk reach that place, and through it the
//! Team Create server. Argon and Rojo need a server and a Studio plugin
//! because Studio can't read the disk; rustudio can, so this is a watcher
//! on the project folder, no port.
//!
//! - **Attach** (`Editor::file_sync_attach`): the project's tree is read
//!   (src/project.rs, the Project Folder reader) and matched to the place by
//!   name and class (plan.rs). What happens to the differences is the
//!   "On attach" setting: shown first (the default; Apply makes the files
//!   win), the files win at once, or the place wins (only files changed
//!   later sync). The pair is remembered per place (place id, or the file's
//!   path) and attached again when that place opens.
//! - **Files → place**: the watcher (project_sync.rs, its debounce) reports
//!   changed paths; only the nodes they feed are compared and the
//!   differences applied as ordinary undoable document edits. In a live
//!   document the Team Create planner (live_sync.rs) sends them; a script
//!   Source change there becomes a draft, which File Sync commits (the
//!   script tab's Commit, cloud_mcp.rs) and follows until the server
//!   confirms. A draft the server changed under is never replaced
//!   silently: it is reported, and replaced only from the panel.
//! - **Asking first**: a batch larger than the threshold (or every batch,
//!   per "Display prompts") waits in the panel with its diff. In live
//!   places a batch over the threshold, or one that deletes, always waits.
//! - **Place → files** (Two-Way Sync, off by default): edits made in
//!   rustudio are written to their files by the Project Folder write-back
//!   (in place, never outside the mapped paths, removed files to the trash),
//!   filtered by Syncback Properties / Only Code Mode and the project's own
//!   `syncback.ignoreGlobs`.
//! - **Log**: every change with its file, and for live places whether it was
//!   sent, confirmed or failed.
//!
//! The button left of the MCP button shows the state (ui.rs); agents get
//! the same through the MCP tools file_sync_status / _diff / _attach /
//! _apply / _detach.

mod agent;
pub mod diff;
pub mod plan;
pub mod settings;
pub mod ui;

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use rbx_dom_weak::types::{Ref, Variant};
use rustudio_core::doc::{self, Mark, Marks};

use crate::app::Editor;
use crate::cloud_ui::DraftState;
use crate::project::{FileKind, Origin, Snap};
use crate::project_sync::ProjectState;
use plan::{Opts, Scope};
use settings::{FileSyncSettings, Initial, LogLevel, Prompts};

/// Log entries kept.
const LOG_KEEP: usize = 500;
/// A live script commit the server hasn't answered in this long is reported.
const COMMIT_TIMEOUT_S: f32 = 60.0;

pub struct LogEntry {
    pub id: u64,
    pub at: String,
    pub level: LogLevel,
    pub text: String,
    /// The file it came from (relative to the project).
    pub file: Option<String>,
    /// Live places: sent / confirmed / failed, updated as it happens.
    pub status: Option<String>,
    /// Some(true) done, Some(false) failed, None waiting / not applicable.
    pub ok: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Off,
    Synced,
    Pending,
    Error,
}

pub struct Attached {
    pub st: ProjectState,
    /// The tree as last read.
    pub tree: Snap,
    /// What the place is remembered by ("place:<id>" / "file:<path>").
    pub place_key: Option<String>,
    pub place_label: String,
    pub live: bool,
    pub mappings: Vec<plan::MapRow>,
    pub attached_at: Instant,
    /// When files last reached the place, and a summary.
    pub last_sync: Option<(Instant, String)>,
    /// Changes on disk waiting for Play to stop.
    pub deferred: Option<Scope>,
    /// Argon's `syncback.ignoreGlobs`: files Two-Way Sync never writes.
    pub syncback_ignore: Vec<glob::Pattern>,
    /// Instances File Sync changed, in total.
    pub applied: usize,
}

impl Attached {
    pub fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.st.root).unwrap_or(p).display().to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// "Show diff first" at attach.
    Attach,
    /// Display prompts: Always.
    Always,
    /// More changes than the threshold.
    Threshold,
    /// The batch deletes instances (Safety > Ask Before Deletes).
    Deletes,
    /// "Never" in a live place, not confirmed yet for this project: the one
    /// confirmation, then never again.
    LiveNever,
}

impl Reason {
    pub fn text(self, n: usize, threshold: usize) -> String {
        match self {
            Reason::Attach => format!("Attaching: {n} difference(s) between the files and the place. Apply makes the files win; Skip keeps the place as it is."),
            Reason::Always => format!("{n} change(s) from the files (Display prompts: Always)."),
            Reason::Threshold => format!("{n} change(s) from the files, more than the threshold of {threshold}."),
            Reason::Deletes => format!("{n} change(s) from the files, some of them deletes (Ask Before Deletes)."),
            Reason::LiveNever => format!("{n} change(s) for a live place, where Display Prompts is Never: confirm once to send without asking."),
        }
    }
}

/// A batch waiting for the person's OK.
pub struct PendingBatch {
    pub scope: Scope,
    pub reason: Reason,
    pub opts: Opts,
    pub lines: Vec<plan::Line>,
    pub notes: Vec<String>,
    pub counts: (usize, usize, usize),
    pub since: Instant,
}

/// A live script whose Source File Sync set: committed, then followed.
pub struct Commit {
    pub r: Ref,
    pub path: String,
    pub log: u64,
    pub sent: bool,
    pub since: Instant,
    pub conflict: bool,
}

pub struct FileSync {
    pub settings: FileSyncSettings,
    settings_file: Option<PathBuf>,
    started: bool,
    /// The dock panel should open (the popover's Open panel, test runs).
    pub open_panel: bool,
    /// The top-bar popover is open.
    pub popover: bool,
    /// Settings groups folded in the panel.
    pub folded: std::collections::HashSet<&'static str>,
    /// Scripts whose diff is unfolded in the change list (by place path).
    pub unfolded: std::collections::HashSet<String>,
    pub tab: ui::Tab,
    pub att: Option<Attached>,
    pub pending: Option<PendingBatch>,
    pub log: VecDeque<LogEntry>,
    next_log: u64,
    pub commits: Vec<Commit>,
    /// The last thing that went wrong (the button is red until a sync works).
    pub error: Option<String>,
    /// The project picker's text: the open place's remembered project, or
    /// what the person picked for it (never another place's project).
    pub pick: String,
    /// The document `pick` was filled for.
    pick_doc: Option<u64>,
    /// Remember the pair when attaching from the panel.
    pub remember: bool,
    /// The document auto-attach looked at (once per document).
    auto_done: Option<u64>,
    /// RUSTUDIO_FILE_SYNC_ATTACH=<project>[#mode]: attached once (tests).
    env_attach: Option<(PathBuf, Initial)>,
    /// Team Create edits not sent, when File Sync last applied.
    live_unsynced: Option<usize>,
    /// The one confirmation for "Never" in a live place, while it shows.
    pub never_confirm: Option<NeverConfirm>,
    /// A live apply going out in batches.
    pub queue: Option<plan::LiveQueue>,
}

/// "Live place: changes will be sent to the server without asking."
pub struct NeverConfirm {
    /// Prompts before Never was picked (Cancel goes back to it); None when
    /// it was already Never and a batch is what asked.
    pub previous: Option<Prompts>,
}

impl Default for FileSync {
    fn default() -> Self {
        FileSync {
            settings: FileSyncSettings::default(),
            settings_file: None,
            started: false,
            open_panel: false,
            popover: false,
            folded: ["log"].into_iter().collect(),
            unfolded: Default::default(),
            tab: ui::Tab::Sync,
            att: None,
            pending: None,
            log: VecDeque::new(),
            next_log: 1,
            commits: Vec::new(),
            error: None,
            pick: String::new(),
            pick_doc: None,
            remember: true,
            auto_done: None,
            env_attach: None,
            live_unsynced: None,
            never_confirm: None,
            queue: None,
        }
    }
}

impl FileSync {
    fn init(&mut self, keep: bool) {
        if self.started {
            return;
        }
        self.started = true;
        let file = if keep || std::env::var_os("RUSTUDIO_FILE_SYNC_SETTINGS").is_some() { settings::path() } else { None };
        self.settings = FileSyncSettings::load(file.as_deref());
        self.settings_file = file;
        // RUSTUDIO_FILE_SYNC_PANEL=sync | settings | log: the panel open from
        // the start on that page (screenshots).
        if let Ok(v) = std::env::var("RUSTUDIO_FILE_SYNC_PANEL") {
            self.open_panel = !v.is_empty() && v != "0";
            self.tab = match v.as_str() {
                "settings" => ui::Tab::Settings,
                "log" => ui::Tab::Log,
                "mapping" => ui::Tab::Mapping,
                _ => ui::Tab::Sync,
            };
        }
        // RUSTUDIO_FILE_SYNC_POPOVER=1: the top-bar popover open (screenshots).
        self.popover = std::env::var("RUSTUDIO_FILE_SYNC_POPOVER").is_ok_and(|v| v == "1");
        if let Ok(v) = std::env::var("RUSTUDIO_FILE_SYNC_ATTACH") {
            let (p, mode) = v.rsplit_once('#').map(|(p, m)| (p.to_string(), Initial::parse(m))).unwrap_or((v.clone(), None));
            if !p.is_empty() {
                self.env_attach = Some((PathBuf::from(p), mode.unwrap_or(self.settings.initial)));
            }
        }
    }

    pub fn save_settings(&mut self) {
        if let Some(f) = &self.settings_file {
            if let Err(e) = self.settings.save(f) {
                self.error = Some(format!("Could not save the File Sync settings: {e}"));
            }
        }
    }

    pub fn status(&self) -> Status {
        if self.att.is_none() {
            return if self.error.is_some() { Status::Error } else { Status::Off };
        }
        if self.error.is_some() || self.commits.iter().any(|c| c.conflict) {
            Status::Error
        } else if self.pending.is_some() || self.queue.is_some() || self.commits.iter().any(|c| !c.conflict) || self.att.as_ref().is_some_and(|a| a.deferred.is_some()) {
            Status::Pending
        } else {
            Status::Synced
        }
    }

    fn push(&mut self, level: LogLevel, text: String, file: Option<String>) -> u64 {
        let id = self.next_log;
        self.next_log += 1;
        let at = chrono::Local::now().format("%H:%M:%S").to_string();
        self.log.push_back(LogEntry { id, at, level, text, file, status: None, ok: None });
        while self.log.len() > LOG_KEEP {
            self.log.pop_front();
        }
        id
    }

    fn set_status(&mut self, id: u64, status: impl Into<String>, ok: Option<bool>) {
        if let Some(e) = self.log.iter_mut().rev().find(|e| e.id == id) {
            e.status = Some(status.into());
            e.ok = ok;
        }
    }
}

/// Changes a script's file would make, for its kind.
fn script_file(o: &Origin) -> Option<PathBuf> {
    let script = |k: FileKind| matches!(k, FileKind::Server | FileKind::Client | FileKind::Module);
    match o {
        Origin::File { file, kind, .. } if script(*kind) => Some(file.clone()),
        Origin::Dir { init: Some((f, k)), .. } if script(*k) => Some(f.clone()),
        _ => None,
    }
}

impl Editor {
    /// A log line (also to Output when the log level shows it).
    pub(crate) fn file_sync_log(&mut self, level: LogLevel, text: impl Into<String>, file: Option<String>) -> u64 {
        let text = text.into();
        if level <= self.file_sync.settings.log_level {
            let line = match &file {
                Some(f) => format!("File Sync: {text} ({f})"),
                None => format!("File Sync: {text}"),
            };
            match level {
                LogLevel::Error => self.error(line),
                LogLevel::Warn => self.warn(line),
                _ => self.info(line),
            }
        }
        self.file_sync.push(level, text, file)
    }

    /// What the open place is remembered by, and its name.
    pub(crate) fn file_sync_place_key(&self) -> Option<(String, String)> {
        if let Some(place) = self.live_team_create_place() {
            let name = self.game.doc.cloud.as_ref().map(|c| c.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("place {place}"));
            return Some((format!("place:{place}"), format!("{name} (live)")));
        }
        if let Some(c) = &self.game.doc.cloud {
            return Some((format!("place:{}", c.place_id), if c.name.is_empty() { format!("place {}", c.place_id) } else { c.name.clone() }));
        }
        let p = self.game.doc.path.as_ref()?;
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
        Some((format!("file:{}", p.display()), p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))
    }

    /// Whether the open place is a live Team Create session.
    /// RUSTUDIO_FILE_SYNC_FAKE_LIVE=1 (checks only) treats any place as one,
    /// for File Sync's own rules (prompts, batches); nothing is sent.
    fn file_sync_live(&self) -> bool {
        self.live_team_create_place().is_some() || std::env::var_os("RUSTUDIO_FILE_SYNC_FAKE_LIVE").is_some_and(|v| v == "1")
    }

    /// Why nothing can be attached right now, if so.
    fn file_sync_blocked(&self) -> Option<&'static str> {
        if self.project.is_some() {
            return Some("this place is a project folder opened as one: its files sync already");
        }
        if self.game.play.is_some() {
            return Some("Play is running; attach after Stop");
        }
        if self.place_loading() || self.mcp_live_activity().is_some_and(|a| !a.joined) {
            return Some("the place is still loading");
        }
        None
    }

    /// Attaches a project (a *.project.json or a folder with one) to the
    /// open place. Ok(a summary).
    pub fn file_sync_attach(&mut self, project: &Path, initial: Initial) -> Result<String, String> {
        if let Some(why) = self.file_sync_blocked() {
            return Err(format!("Can't attach: {why}"));
        }
        let file = crate::mcp::project_apply::project_file(project)?;
        self.file_sync_detach(false);
        let (mut st, tree) = crate::project_sync::attach(&file, &self.game.doc)?;
        st.map = plan::match_map(&self.game.doc, &tree);
        let syncback_ignore = std::fs::read(&st.file)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|j| j.get("syncback")?.get("ignoreGlobs")?.as_array().cloned())
            .into_iter()
            .flatten()
            .filter_map(|g| g.as_str().and_then(|g| glob::Pattern::new(g).ok()))
            .collect();
        let (place_key, place_label) = match self.file_sync_place_key() {
            Some((k, l)) => (Some(k), l),
            None => (None, self.game.doc.title.clone()),
        };
        let mappings = plan::mappings(&self.game.doc, &tree, &st.root);
        let warnings = std::mem::take(&mut st.warnings);
        let live = self.file_sync_live();
        let name = st.name.clone();
        let present = mappings.iter().filter(|m| m.has).count();
        let summary = format!(
            "attached {} to {place_label}: {present} of {} mapped paths are in the place, {} instances matched to files",
            st.file.display(),
            mappings.len(),
            st.map.len()
        );
        if self.file_sync.remember {
            if let Some(k) = &place_key {
                self.file_sync.settings.remember(k, Some((st.file.clone(), place_label.clone())));
                self.file_sync.save_settings();
            }
        }
        self.file_sync.pick = st.file.display().to_string();
        self.file_sync.att = Some(Attached {
            st,
            tree,
            place_key,
            place_label,
            live,
            mappings,
            attached_at: Instant::now(),
            last_sync: None,
            deferred: None,
            syncback_ignore,
            applied: 0,
        });
        self.file_sync.error = None;
        // What attaching did isn't an edit to write back.
        let _ = self.game.doc.marks_for(doc::Reader::Project);
        self.file_sync_log(LogLevel::Info, format!("Project \"{name}\" {summary}"), None);
        for w in warnings.iter().take(20) {
            self.file_sync_log(LogLevel::Warn, w.clone(), None);
        }
        match initial {
            Initial::Place => {
                self.file_sync_log(LogLevel::Info, "Place wins: the place is kept as it is; files changed from now on sync", None);
            }
            Initial::Preview => self.file_sync_run(Scope::All, true, Some(Reason::Attach)),
            Initial::Files => self.file_sync_run(Scope::All, true, None),
        }
        Ok(summary)
    }

    /// Detaches; `forget`: the place no longer attaches it by itself.
    pub fn file_sync_detach(&mut self, forget: bool) {
        let Some(a) = self.file_sync.att.take() else { return };
        self.file_sync.pending = None;
        self.file_sync.never_confirm = None;
        if let Some(q) = self.file_sync.queue.take() {
            self.file_sync_log(LogLevel::Warn, format!("detached while sending: {} of {} instance(s) were not sent", q.total - q.sent, q.total), None);
        }
        let waiting = self.file_sync.commits.len();
        self.file_sync.commits.clear();
        if forget {
            if let Some(k) = &a.place_key {
                self.file_sync.settings.remember(k, None);
                self.file_sync.save_settings();
            }
        }
        let mut text = format!("detached {}{}", a.st.file.display(), if forget { " (forgotten for this place)" } else { "" });
        if waiting > 0 {
            text += &format!("; {waiting} script commit(s) were still being followed (the Team Create panel has their drafts)");
        }
        self.file_sync_log(LogLevel::Info, text, None);
    }

    fn file_sync_opts(&self, initial: bool) -> Opts {
        let live = self.file_sync_live();
        Opts {
            create_missing: !initial || self.file_sync.settings.hydrate,
            keep_unknowns: self.file_sync.settings.keep_unknowns_for(live),
            code_only: false,
        }
    }

    /// Reads the files again and compares the scope with the place: applied
    /// at once, or left waiting for an OK (`force_prompt`, else the prompt
    /// settings and the live guard decide).
    pub(crate) fn file_sync_run(&mut self, scope: Scope, initial: bool, force_prompt: Option<Reason>) {
        let opts = self.file_sync_opts(initial);
        let Some(a) = self.file_sync.att.as_mut() else { return };
        let Some(tree) = a.st.read_tree() else {
            let msg = format!("{} could not be read (it has no tree?); nothing changed", a.st.file.display());
            self.file_sync.error = Some(msg.clone());
            self.file_sync_log(LogLevel::Error, msg, None);
            return;
        };
        a.tree = tree;
        if scope == Scope::All {
            a.mappings = plan::mappings(&self.game.doc, &a.tree, &a.st.root);
        }
        let warnings = std::mem::take(&mut a.st.warnings);
        for w in warnings.iter().take(20) {
            self.file_sync_log(LogLevel::Warn, w.clone(), None);
        }
        let a = self.file_sync.att.as_ref().unwrap();
        let p = plan::plan(&self.game.doc, &a.tree, &scope, &a.st.map, opts);
        for n in p.notes.iter().take(20) {
            self.file_sync_log(LogLevel::Verbose, n.clone(), None);
        }
        if p.is_empty() {
            if initial {
                self.file_sync_log(LogLevel::Info, "the place already matches the files", None);
            } else {
                self.file_sync_log(LogLevel::Verbose, "files changed, nothing to change in the place", None);
            }
            self.file_sync.error = None;
            return;
        }
        let n = p.steps.len();
        let (_, _, deletes) = p.counts();
        let live = self.file_sync_live();
        // The person's settings decide (Display Prompts, Ask Before Deletes),
        // live places included. "Never" in a live place is confirmed once per
        // project, the first time it would matter.
        let mut reason = force_prompt.or(self.file_sync.settings.asks(n, deletes, live));
        if reason.is_none() && live && self.file_sync.settings.prompts == Prompts::Never && !self.file_sync_never_ok() {
            reason = Some(Reason::LiveNever);
            self.file_sync.never_confirm = Some(NeverConfirm { previous: None });
        }
        match reason {
            Some(reason) => {
                let lines = plan::lines(&self.game.doc, &p);
                let counts = p.counts();
                let notes = p.notes;
                self.file_sync.pending = Some(PendingBatch { scope, reason, opts, lines, notes, counts, since: Instant::now() });
                self.file_sync.tab = ui::Tab::Sync;
                let (c, s2, d) = counts;
                self.file_sync_log(LogLevel::Info, format!("{n} change(s) wait for your OK in the File Sync panel ({c} create, {s2} set, {d} delete)"), None);
            }
            None => self.file_sync_apply(p),
        }
    }

    /// Whether the person confirmed Never in a live place for this project.
    pub(crate) fn file_sync_never_ok(&self) -> bool {
        self.file_sync.att.as_ref().is_some_and(|a| self.file_sync.settings.live_never_ok.contains(&a.st.file))
    }

    /// Applies a plan and logs each change. In a live place a big one goes
    /// in batches (Safety > Live Batch Size), one every quarter second.
    fn file_sync_apply(&mut self, p: plan::Plan) {
        let live = self.file_sync_live();
        if live {
            self.file_sync.live_unsynced = self.mcp_live_activity().map(|a| a.unsynced);
            let batch = self.file_sync.settings.live_batch;
            let instances: usize = p.steps.iter().map(|s| if let plan::Step::Create { snap, .. } = s { crate::mcp::project_apply::count(snap) } else { 1 }).sum();
            if batch > 0 && instances > batch {
                let q = plan::LiveQueue::new(p);
                self.file_sync_log(LogLevel::Info, format!("sending {} instance(s) to the live place in batches of {batch}", q.total), None);
                self.file_sync.queue = Some(q);
                self.file_sync_send_batch();
                return;
            }
        }
        let n = p.steps.len();
        let results = plan::apply(&mut self.game.doc, p);
        self.file_sync_applied(results, n, live);
    }

    /// The next batch of a live queue (each quarter second, from the tick).
    fn file_sync_send_batch(&mut self) {
        let Some(mut q) = self.file_sync.queue.take() else { return };
        let budget = self.file_sync.settings.live_batch.max(1);
        let results = q.apply_batch(&mut self.game.doc, budget);
        let n = results.len();
        let done = q.is_done();
        let (sent, total) = (q.sent, q.total);
        if !done {
            self.file_sync.queue = Some(q);
        }
        self.file_sync_applied(results, n, true);
        if done {
            self.file_sync_log(LogLevel::Info, format!("sent {total} instance(s) to the live place"), None);
        } else {
            self.file_sync_log(LogLevel::Verbose, format!("sent {sent} of {total}"), None);
        }
    }

    /// Logs what was applied and brings the map up to date.
    fn file_sync_applied(&mut self, results: Vec<plan::Applied>, n: usize, live: bool) {
        let mut failed = 0;
        for r in results {
            let file = r.file.as_ref().and_then(|f| self.file_sync.att.as_ref().map(|a| a.rel(f)));
            match &r.error {
                Some(e) => {
                    failed += 1;
                    self.file_sync_log(LogLevel::Error, format!("{}: {} failed: {e}", r.path, r.what), file);
                }
                None => {
                    let id = self.file_sync_log(LogLevel::Info, format!("{}: {}", r.path, r.what), file);
                    if live {
                        match r.source_set {
                            Some(s) => {
                                self.file_sync.set_status(id, "draft: committing to the server…", None);
                                self.file_sync.commits.push(Commit { r: s, path: r.path.clone(), log: id, sent: false, since: Instant::now(), conflict: false });
                            }
                            None => self.file_sync.set_status(id, "sent to the server", Some(true)),
                        }
                    }
                }
            }
        }
        // Open script tabs show the new text.
        let refs: Vec<Ref> = self.scripts.bufs.keys().copied().collect();
        for r in refs {
            if let (Some(buf), Some(Variant::String(src))) = (self.scripts.bufs.get_mut(&r), self.game.doc.prop(r, "Source")) {
                if buf.text != src {
                    buf.text = src;
                }
            }
        }
        if let Some(a) = self.file_sync.att.as_mut() {
            a.st.map = plan::match_map(&self.game.doc, &a.tree);
            a.applied += n;
            a.last_sync = Some((Instant::now(), format!("{n} change(s){}", if failed > 0 { format!(", {failed} failed") } else { String::new() })));
            if a.deferred.is_none() {
                a.mappings = plan::mappings(&self.game.doc, &a.tree, &a.st.root);
            }
        }
        // What File Sync did isn't an edit to write back.
        let _ = self.game.doc.marks_for(doc::Reader::Project);
        if failed > 0 {
            self.file_sync.error = Some(format!("{failed} change(s) failed (see the log)"));
        } else if self.file_sync.queue.is_none() {
            self.file_sync.error = None;
        }
    }

    /// The waiting batch: applied (planned again against the files and the
    /// place as they are now) or skipped.
    pub fn file_sync_answer(&mut self, apply: bool) {
        let Some(b) = self.file_sync.pending.take() else { return };
        if !apply {
            let what = if b.reason == Reason::Attach { "kept the place as it is (the files win again when they change)" } else { "skipped; the place keeps its version until those files change again" };
            self.file_sync_log(LogLevel::Info, format!("{} change(s) {what}", b.lines.len()), None);
            return;
        }
        if self.game.play.is_some() {
            self.file_sync.pending = Some(b);
            return;
        }
        let Some(a) = self.file_sync.att.as_mut() else { return };
        let Some(tree) = a.st.read_tree() else { return };
        a.tree = tree;
        let a = self.file_sync.att.as_ref().unwrap();
        let p = plan::plan(&self.game.doc, &a.tree, &b.scope, &a.st.map, b.opts);
        if p.steps.len() != b.lines.len() {
            self.file_sync_log(LogLevel::Info, format!("the files or the place changed since the preview: applying {} change(s) instead of {}", p.steps.len(), b.lines.len()), None);
        }
        self.file_sync_apply(p);
    }

    /// Everything that differs between the files and the place now (a dry
    /// run of "files win", for the panel and agents).
    pub fn file_sync_drift(&mut self) -> Option<Vec<plan::Line>> {
        let opts = Opts { create_missing: true, ..self.file_sync_opts(false) };
        let a = self.file_sync.att.as_mut()?;
        a.tree = a.st.read_tree()?;
        let a = self.file_sync.att.as_ref().unwrap();
        Some(plan::lines(&self.game.doc, &plan::plan(&self.game.doc, &a.tree, &Scope::All, &a.st.map, opts)))
    }

    /// Once a frame, after the project tick.
    pub fn file_sync_tick(&mut self) {
        let keep = crate::layout_store::enabled(&self.harness);
        self.file_sync.init(keep);
        if std::mem::take(&mut self.file_sync.open_panel) {
            self.ui.panel_requests.push((crate::app::Tab::FileSync, Some(true)));
        }
        let playing = self.game.play.is_some();
        // The place closed (another one opened).
        if let Some(a) = &self.file_sync.att {
            if a.st.doc_id != self.game.doc.id && !playing {
                let label = a.place_label.clone();
                self.file_sync.att = None;
                self.file_sync.queue = None;
                self.file_sync.never_confirm = None;
                self.file_sync.pending = None;
                self.file_sync.commits.clear();
                self.file_sync_log(LogLevel::Info, format!("{label} closed: detached"), None);
            }
        }
        if !playing && self.file_sync.pick_doc != Some(self.game.doc.id) {
            self.file_sync.pick_doc = Some(self.game.doc.id);
            let attached = self.file_sync.att.as_ref().filter(|a| a.st.doc_id == self.game.doc.id).map(|a| a.st.file.clone());
            let remembered = || {
                let (key, _) = self.file_sync_place_key()?;
                self.file_sync.settings.remembered_for(&key).map(|r| r.project.clone())
            };
            self.file_sync.pick = attached.or_else(remembered).map(|p| p.display().to_string()).unwrap_or_default();
        }
        if self.file_sync.att.is_none() {
            self.file_sync_auto_attach();
            return;
        }
        if !playing {
            self.file_sync_write_back();
        }
        // Only what the project maps (a project folder also holds tools,
        // builds, captures… that change all the time).
        let changed = self.file_sync.att.as_mut().and_then(|a| a.st.poll_disk()).and_then(|mut c| {
            let a = self.file_sync.att.as_ref()?;
            c.retain(|p| a.st.writable(p));
            (!c.is_empty()).then_some(c)
        });
        if let Some(changed) = changed {
            let a = self.file_sync.att.as_ref().unwrap();
            let scope = if changed.contains(&a.st.file) { Scope::All } else { Scope::Paths(changed.clone()) };
            let mut names: Vec<String> = changed.iter().map(|p| a.rel(p)).collect();
            names.sort();
            let more = names.len().saturating_sub(6);
            names.truncate(6);
            self.file_sync_log(LogLevel::Verbose, format!("changed on disk: {}{}", names.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() }), None);
            if playing || self.file_sync.queue.is_some() {
                let a = self.file_sync.att.as_mut().unwrap();
                match a.deferred.as_mut() {
                    Some(d) => d.merge(scope),
                    None => a.deferred = Some(scope),
                }
            } else if let Some(mut b) = self.file_sync.pending.take() {
                // A preview waiting: it grows to cover the new changes.
                b.scope.merge(scope);
                let reason = b.reason;
                self.file_sync_run(b.scope, reason == Reason::Attach, Some(reason));
            } else {
                self.file_sync_run(scope, false, None);
            }
        }
        if !playing && self.file_sync.queue.as_ref().is_some_and(|q| q.last.is_none_or(|t| t.elapsed().as_millis() >= 250)) {
            self.file_sync_send_batch();
        }
        if !playing && self.file_sync.queue.is_none() {
            if let Some(scope) = self.file_sync.att.as_mut().and_then(|a| a.deferred.take()) {
                self.file_sync_log(LogLevel::Info, "applying the files changed during Play", None);
                self.file_sync_run(scope, false, None);
            }
        }
        self.file_sync_commits();
        if let Some(a) = self.file_sync.att.as_mut() {
            let logs = std::mem::take(&mut a.st.log);
            let warnings = std::mem::take(&mut a.st.warnings);
            for l in logs {
                self.file_sync_log(LogLevel::Info, format!("Two-Way Sync: {l}"), None);
            }
            for w in warnings {
                self.file_sync_log(LogLevel::Warn, w, None);
            }
        }
    }

    fn file_sync_auto_attach(&mut self) {
        if self.file_sync_blocked().is_some() {
            return;
        }
        if let Some((p, mode)) = self.file_sync.env_attach.take() {
            self.file_sync.auto_done = Some(self.game.doc.id);
            if let Err(e) = self.file_sync_attach(&p, mode) {
                self.file_sync.error = Some(e.clone());
                self.file_sync_log(LogLevel::Error, e, None);
            }
            return;
        }
        if !self.file_sync.settings.auto_attach || self.file_sync.auto_done == Some(self.game.doc.id) {
            return;
        }
        self.file_sync.auto_done = Some(self.game.doc.id);
        let Some((key, label)) = self.file_sync_place_key() else { return };
        let Some(r) = self.file_sync.settings.remembered_for(&key).cloned() else { return };
        if !r.project.exists() {
            self.file_sync_log(LogLevel::Warn, format!("{label}: its project {} is gone; not attached", r.project.display()), None);
            return;
        }
        let initial = self.file_sync.settings.initial;
        if let Err(e) = self.file_sync_attach(&r.project, initial) {
            self.file_sync.error = Some(e.clone());
            self.file_sync_log(LogLevel::Error, format!("re-attaching {}: {e}", r.project.display()), None);
        }
    }

    /// Two-Way Sync: this frame's edits to their files (filtered); without
    /// it the journal is only read past.
    fn file_sync_write_back(&mut self) {
        let Some(mut a) = self.file_sync.att.take() else { return };
        let marks = match self.game.doc.marks_for(doc::Reader::Project) {
            Marks::Some(m) => m,
            Marks::All => Vec::new(),
        };
        if self.file_sync.settings.two_way && !marks.is_empty() && a.st.doc_id == self.game.doc.id {
            let s = &self.file_sync.settings;
            let doc = &self.game.doc;
            let ignored = |r: Ref| {
                // The instance's file, or its nearest mapped ancestor's.
                let mut cur = r;
                loop {
                    if let Some(k) = a.st.map.get(&cur).and_then(|o| o.key()) {
                        let rel = k.strip_prefix(&a.st.root).unwrap_or(k);
                        return a.syncback_ignore.iter().any(|g| g.matches_path(rel));
                    }
                    match doc.dom.get_by_ref(cur) {
                        Some(i) if !i.parent().is_none() => cur = i.parent(),
                        _ => return false,
                    }
                }
            };
            let code = |r: Ref| match doc.dom.get_by_ref(r) {
                Some(_) => plan::has_code(doc, r),
                None => a.st.map.get(&r).is_some_and(|o| script_file(o).is_some() || matches!(o, Origin::Dir { .. })),
            };
            let kept: Vec<Mark> = marks
                .into_iter()
                .filter(|m| match m {
                    Mark::Prop(r, n) => {
                        let n = n.as_str();
                        let file_held = matches!(n, "Source" | "Name") || (n == "Value" && doc.class_of(*r).is_some_and(|c| c.as_str() == "StringValue"));
                        (s.syncback_props || file_held) && (!s.code_only || code(*r)) && !ignored(*r)
                    }
                    Mark::Tree(r) => (!s.code_only || code(*r)) && !ignored(*r),
                })
                .collect();
            let before = crate::project_config::active();
            crate::project_config::set_active(Some(a.st.config.clone()));
            a.st.write_marks(&mut self.game.doc, kept);
            crate::project_config::set_active(before);
            // New instances written to files are synced instances now.
            if !a.st.log.is_empty() {
                a.st.map = plan::match_map(&self.game.doc, &a.tree);
            }
        }
        self.file_sync.att = Some(a);
    }

    /// Live script commits: sent once the draft exists, then followed until
    /// the server confirms; a conflict waits for the person.
    fn file_sync_commits(&mut self) {
        if self.file_sync.commits.is_empty() {
            if let (Some(before), Some(now)) = (self.file_sync.live_unsynced, self.mcp_live_activity().map(|a| a.unsynced)) {
                if now > before {
                    self.file_sync.live_unsynced = Some(now);
                    let msg = format!("{} edit(s) could not be sent to the Team Create server (the Output says which)", now - before);
                    self.file_sync.error = Some(msg.clone());
                    self.file_sync_log(LogLevel::Error, msg, None);
                }
            }
            return;
        }
        let commits = std::mem::take(&mut self.file_sync.commits);
        let mut keep = Vec::new();
        for mut c in commits {
            let age = c.since.elapsed().as_secs_f32();
            match (c.sent, self.live_draft_state(c.r)) {
                (_, DraftState::NotLive) => {
                    if age > 10.0 {
                        self.file_sync.set_status(c.log, "not on the Team Create server; not committed", Some(false));
                        continue;
                    }
                }
                (false, DraftState::Local) => match self.mcp_commit_draft(c.r) {
                    Ok(m) => {
                        c.sent = true;
                        self.file_sync.set_status(c.log, format!("draft {m}"), None);
                    }
                    Err(e) => {
                        self.file_sync.set_status(c.log, format!("commit failed: {e}"), Some(false));
                        self.file_sync.error = Some(format!("{}: {e}", c.path));
                        continue;
                    }
                },
                (false, DraftState::Clean) => {
                    if age > 3.0 {
                        self.file_sync.set_status(c.log, "the server already has this source", Some(true));
                        continue;
                    }
                }
                (_, DraftState::Conflict) => {
                    if !c.conflict {
                        c.conflict = true;
                        self.file_sync.set_status(c.log, "conflict: the server has a newer version; not replaced (File Sync panel: Replace server)", Some(false));
                        self.file_sync_log(LogLevel::Error, format!("{}: someone changed it on the server; the file's version was NOT committed. Replace the server's from the File Sync panel, or keep theirs", c.path), None);
                    }
                }
                (true, DraftState::Committing) => {
                    if age > COMMIT_TIMEOUT_S {
                        self.file_sync.set_status(c.log, "the server hasn't confirmed the commit (the draft is kept)", Some(false));
                        continue;
                    }
                }
                (true, DraftState::Clean) => {
                    self.file_sync.set_status(c.log, "committed: confirmed by the server", Some(true));
                    continue;
                }
                (true, DraftState::Submitted) => {
                    self.file_sync.set_status(c.log, "sent as a Source property (the server doesn't confirm these)", Some(true));
                    continue;
                }
                (_, DraftState::Rejected) => {
                    self.file_sync.set_status(c.log, "rejected by the server (the draft is kept)", Some(false));
                    self.file_sync.error = Some(format!("{}: the server rejected the commit", c.path));
                    continue;
                }
                _ => {}
            }
            keep.push(c);
        }
        self.file_sync.commits = keep;
    }

    /// Replaces the server's newer version with the file's (the person chose it).
    pub fn file_sync_replace_conflict(&mut self, r: Ref) {
        let Some(i) = self.file_sync.commits.iter().position(|c| c.r == r) else { return };
        match self.live_commit_replacing(r) {
            Ok(m) => {
                let c = &mut self.file_sync.commits[i];
                c.sent = true;
                c.conflict = false;
                c.since = Instant::now();
                let (log, path) = (c.log, c.path.clone());
                self.file_sync.set_status(log, format!("replacing the server's version: {m}"), None);
                self.file_sync_log(LogLevel::Warn, format!("{path}: replacing the server's newer version with the file's"), None);
            }
            Err(e) => {
                self.file_sync_log(LogLevel::Error, format!("replace failed: {e}"), None);
            }
        }
    }

    /// Keeps the server's version (the conflict is dropped; the draft is the
    /// Team Create panel's to discard).
    pub fn file_sync_keep_server(&mut self, r: Ref) {
        if let Some(i) = self.file_sync.commits.iter().position(|c| c.r == r) {
            let c = self.file_sync.commits.remove(i);
            self.file_sync.set_status(c.log, "kept the server's version", Some(false));
            self.discard_live_draft(r);
        }
    }

    /// Open In Editor: scripts that came from files open in the OS editor.
    pub(crate) fn file_sync_open_external(&mut self) {
        if !self.file_sync.settings.open_in_editor || self.open_script.is_empty() {
            return;
        }
        let Some(a) = self.file_sync.att.as_ref() else { return };
        let mut opened = Vec::new();
        self.open_script.retain(|r| match a.st.map.get(r).and_then(script_file) {
            Some(f) => {
                opened.push(f);
                false
            }
            None => true,
        });
        let seen: HashSet<PathBuf> = opened.into_iter().collect();
        for f in seen {
            let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
            let rel = self.file_sync.att.as_ref().map(|a| a.rel(&f));
            match std::process::Command::new(opener).arg(&f).spawn() {
                Ok(_) => self.file_sync_log(LogLevel::Verbose, "opened in the default editor", rel),
                Err(e) => self.file_sync_log(LogLevel::Error, format!("couldn't open {}: {e}", f.display()), rel),
            };
        }
    }
}
