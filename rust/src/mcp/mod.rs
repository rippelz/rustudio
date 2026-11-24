//! RuStudio's built-in MCP server (round 19): an AI agent drives a window
//! that is already open — edit mode, Play, a live Team Create session —
//! without restarting it.
//!
//! - **Transport** (server.rs): Streamable HTTP on 127.0.0.1, a bearer
//!   token, one port per window (44755 and up). `rustudio mcp` (bridge.rs)
//!   is a stdio server that finds the open windows (sessions.rs) and
//!   forwards to one, for clients that start their servers themselves.
//! - **Tools** (tools.rs, exec.rs): the place tree, properties, scripts,
//!   Luau with return values in the edit / server / client DataModels,
//!   projects and models applied, plugins loaded at runtime, Play / Stop and
//!   input, screenshots, Output with a cursor, place files, Team Create
//!   status / drafts / waiting.
//! - **Safety**: localhost only, the token, tool groups the person turns
//!   on in the panel (live Team Create writes off by default), optional
//!   confirmation in the editor, and errors rather than partial writes.
//! - **UI** (ui.rs): the MCP button beside Collaborate (status at a glance)
//!   and its panel; the settings persist (settings.rs).
//!
//! Calls arrive on connection threads and run on the UI thread between
//! frames (`Editor::mcp_tick`, at the end of every frame): most answer at
//! once, the rest (yielding Luau, Play, screenshots, loads, waits) are
//! polled each frame until done.

pub mod bridge;
mod exec;
pub mod project_apply;
pub mod server;
pub mod sessions;
pub mod settings;
pub mod tools;
pub mod tree;
pub mod ui;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustudio_core::log::{Level, LogLine};
use serde_json::{json, Value as J};

use server::{Job, Server, Shared, ToolResult};
use settings::{Confirm, Group, McpSettings};

/// Calls kept in the activity log.
const ACTIVITY: usize = 200;
/// Output lines kept for read_output.
const OUTPUT_KEEP: usize = 20_000;
/// How long a call waits for the person to allow it.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// One call in the activity log.
pub struct Activity {
    pub id: u64,
    pub at: String,
    pub client: String,
    pub tool: String,
    pub args: String,
    /// None while running or waiting for the person.
    pub ok: Option<bool>,
    pub result: String,
    pub started: Instant,
    pub ms: Option<u128>,
}

/// A stored Output line with its cursor.
pub struct OutLine {
    pub seq: u64,
    pub at: String,
    pub level: Level,
    /// "print", "info", "warn" or "error", as the Output panel colours it.
    pub kind: &'static str,
    pub text: String,
}

/// The Output panel's lines with stable sequence numbers (the panel's own
/// list is trimmed and cleared).
#[derive(Default)]
pub struct OutputMirror {
    pub lines: VecDeque<OutLine>,
    pub next: u64,
    /// Lines of the game log consumed, counting trimmed ones.
    seen: usize,
    last: Option<(String, String)>,
}

impl OutputMirror {
    pub fn sync(&mut self, log: &[LogLine], drained: usize) {
        let rel = self.seen.checked_sub(drained);
        let follows = rel.is_some_and(|rel| {
            rel <= log.len() && (rel == 0 || self.last.as_ref().is_some_and(|(at, t)| log[rel - 1].at == *at && log[rel - 1].text == *t))
        });
        // Cleared (or trimmed past what was seen): everything there is new.
        let start = if follows { rel.unwrap_or(0) } else { 0 };
        for l in &log[start..] {
            let kind = match crate::output_panel::parse(l).kind {
                crate::output_panel::Kind::Print => "print",
                crate::output_panel::Kind::Info => "info",
                crate::output_panel::Kind::Warn => "warn",
                crate::output_panel::Kind::Error => "error",
            };
            self.lines.push_back(OutLine { seq: self.next, at: l.at.clone(), level: l.level, kind, text: l.text.clone() });
            self.next += 1;
        }
        while self.lines.len() > OUTPUT_KEEP {
            self.lines.pop_front();
        }
        self.seen = drained + log.len();
        self.last = log.last().map(|l| (l.at.clone(), l.text.clone()));
    }

    /// Lines from `seq` on.
    pub fn since(&self, seq: u64) -> impl Iterator<Item = &OutLine> {
        let first = self.lines.front().map_or(0, |l| l.seq);
        let skip = seq.saturating_sub(first) as usize;
        self.lines.iter().skip(skip)
    }

    pub fn text_since(&self, seq: u64, max: usize) -> Vec<String> {
        let mut v: Vec<String> = self.since(seq).map(|l| l.text.clone()).collect();
        if v.len() > max {
            let cut = v.len() - max;
            v.drain(..cut);
            v.insert(0, format!("… {cut} earlier lines"));
        }
        v
    }
}

pub(crate) enum Op {
    /// Luau running; `side` None = the edit DataModel.
    /// `done`: the outcome and frames since (prints arrive a frame or two later).
    Eval { side: Option<rustudio_engine::runtime::Side>, id: u64, out_from: u64, done: Option<(J, u32)> },
    Shot { save: Option<PathBuf>, requested: bool, settled: bool, asked: Instant },
    Play { want: bool },
    Input { until: f32 },
    /// A place opening (live: a Team Create join).
    Load { live: bool, frames: u32 },
    /// `failed_before`: the session's sync_failed when the wait began.
    TcWait { quiet: f32, out_from: u64, failed_before: bool },
}

pub(crate) struct Pending {
    pub reply: Sender<ToolResult>,
    pub act: u64,
    pub op: Op,
    pub deadline: Instant,
}

pub struct Mcp {
    pub settings: McpSettings,
    /// Where the settings are kept (None: this run doesn't keep them).
    settings_file: Option<PathBuf>,
    pub(crate) server: Option<Server>,
    shared: Arc<Mutex<Shared>>,
    /// Why the server isn't listening (port taken, …).
    pub error: Option<String>,
    pub activity: VecDeque<Activity>,
    pub(crate) pending: Vec<Pending>,
    /// Calls waiting for the person: (job, activity id, asked at).
    pub(crate) confirm: VecDeque<(Job, u64, Instant)>,
    pub panel_open: bool,
    pub tab: ui::Tab,
    pub output: OutputMirror,
    next_id: u64,
    /// The person allowed every call until the window closes.
    pub allow_all: bool,
    started: bool,
    /// `RUSTUDIO_MCP=<port>`: on for this run whatever the settings say.
    pub(crate) env_port: Option<u16>,
    session_file: Option<PathBuf>,
    session_json: String,
    session_t: Option<Instant>,
    /// (document id, revision) and when it last changed: edits reach a live
    /// server the frame the document changes, so this is when the last one
    /// was sent.
    pub(crate) doc_rev: (u64, u64, Instant),
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            settings: McpSettings::default(),
            settings_file: None,
            server: None,
            shared: Arc::new(Mutex::new(Shared::default())),
            error: None,
            activity: VecDeque::new(),
            pending: Vec::new(),
            confirm: VecDeque::new(),
            panel_open: false,
            tab: ui::Tab::Settings,
            output: OutputMirror::default(),
            next_id: 1,
            allow_all: false,
            started: false,
            env_port: None,
            session_file: None,
            session_json: String::new(),
            session_t: None,
            doc_rev: (0, 0, Instant::now()),
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if let Some(f) = self.session_file.take() {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl Mcp {
    /// Reads the settings (once, at the first frame). `keep`: this run keeps
    /// settings (a person's run, not a test).
    fn init(&mut self, keep: bool) {
        if self.started {
            return;
        }
        self.started = true;
        let file = if keep || std::env::var_os("RUSTUDIO_MCP_SETTINGS").is_some() { settings::path() } else { None };
        self.settings = McpSettings::load(file.as_deref());
        // A token made now is kept, so the snippet stays valid.
        if let Some(f) = &file {
            if !f.exists() {
                let _ = self.settings.save(f);
            }
        }
        self.settings_file = file;
        self.env_port = std::env::var("RUSTUDIO_MCP").ok().and_then(|v| match v.trim() {
            "" | "0" => None,
            "1" => Some(self.settings.port),
            p => p.parse().ok(),
        });
        // RUSTUDIO_MCP_PANEL=1 | settings | connect | activity: the panel
        // open from the start, on that page (screenshots).
        if let Ok(v) = std::env::var("RUSTUDIO_MCP_PANEL") {
            self.panel_open = !v.is_empty() && v != "0";
            self.tab = match v.as_str() {
                "connect" => ui::Tab::Connect,
                "activity" => ui::Tab::Activity,
                _ => ui::Tab::Settings,
            };
        }
        if let Ok(t) = std::env::var("RUSTUDIO_MCP_TOKEN") {
            if !t.is_empty() {
                self.settings.token = t;
            }
        }
    }

    pub fn wanted(&self) -> bool {
        self.settings.enabled || self.env_port.is_some()
    }

    /// Saves the settings (when this run keeps them).
    pub fn save_settings(&mut self) {
        if let Some(f) = &self.settings_file {
            if let Err(e) = self.settings.save(f) {
                self.error = Some(format!("Could not save the MCP settings: {e}"));
            }
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.server.as_ref().map(|s| s.port)
    }

    pub fn url(&self) -> Option<String> {
        self.port().map(|p| format!("http://127.0.0.1:{p}/mcp"))
    }

    pub fn clients(&self) -> Vec<server::ClientInfo> {
        self.shared.lock().map(|s| s.connected()).unwrap_or_default()
    }

    /// Starts or stops listening to match the settings.
    fn sync_server(&mut self, ctx: &egui::Context) {
        let want = self.wanted();
        let port = self.env_port.unwrap_or(self.settings.port);
        {
            let mut s = self.shared.lock().unwrap();
            s.token = self.settings.token.clone();
            s.require_token = self.settings.require_token;
            s.ctx = Some(ctx.clone());
        }
        // A port change restarts it.
        if self.server.as_ref().is_some_and(|s| !(port..port.saturating_add(settings::PORT_TRIES)).contains(&s.port)) {
            self.stop_server();
        }
        if want && self.server.is_none() && self.error.is_none() {
            match Server::start(port, settings::PORT_TRIES, self.shared.clone()) {
                Ok(s) => {
                    self.error = None;
                    self.server = Some(s);
                }
                Err(e) => self.error = Some(e),
            }
        } else if !want && self.server.is_some() {
            self.stop_server();
        }
        if !want {
            self.error = None;
        }
    }

    fn stop_server(&mut self) {
        self.server = None;
        self.shared.lock().unwrap().clients.clear();
        // Calls still queued or running get an answer.
        for p in self.pending.drain(..) {
            let _ = p.reply.send(ToolResult::error("rustudio's MCP server was turned off"));
        }
        for (job, _, _) in self.confirm.drain(..) {
            let _ = job.reply.send(ToolResult::error("rustudio's MCP server was turned off"));
        }
        if let Some(f) = self.session_file.take() {
            let _ = std::fs::remove_file(f);
        }
        self.session_json.clear();
    }

    /// Retries listening after an error (the panel's button, a port change).
    pub fn retry(&mut self) {
        self.error = None;
        self.stop_server();
    }

    fn log_start(&mut self, job: &Job) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut args = job.args.to_string();
        if args.len() > 300 {
            args.truncate(300);
            args.push('…');
        }
        self.activity.push_front(Activity {
            id,
            at: chrono::Local::now().format("%H:%M:%S").to_string(),
            client: job.client.clone(),
            tool: job.tool.clone(),
            args,
            ok: None,
            result: String::new(),
            started: Instant::now(),
            ms: None,
        });
        self.activity.truncate(ACTIVITY);
        id
    }

    pub(crate) fn log_end(&mut self, id: u64, r: &ToolResult) {
        if let Some(a) = self.activity.iter_mut().find(|a| a.id == id) {
            a.ok = Some(!r.is_error);
            let mut s = r.summary();
            if s.len() > 2000 {
                s.truncate(2000);
                s.push('…');
            }
            a.result = s;
            a.ms = Some(a.started.elapsed().as_millis());
        }
    }

    pub(crate) fn next_eval_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// What a tool does now: answered, or an operation to poll.
pub(crate) enum Step {
    Done(ToolResult),
    Wait(Op, Duration),
}

impl From<ToolResult> for Step {
    fn from(r: ToolResult) -> Self {
        Step::Done(r)
    }
}

impl crate::app::Editor {
    /// The MCP server's frame: settings, listening, calls, pending work.
    pub fn mcp_tick(&mut self, ctx: &egui::Context) {
        let keep = crate::layout_store::enabled(&self.harness);
        self.mcp.init(keep);
        self.mcp.sync_server(ctx);
        self.mcp.output.sync(&self.game.log, self.game.log_drained);
        let rev = (self.game.doc.id, self.game.doc.revision);
        if (self.mcp.doc_rev.0, self.mcp.doc_rev.1) != rev {
            self.mcp.doc_rev = (rev.0, rev.1, Instant::now());
        }
        self.mcp_publish_session();
        if self.mcp.server.is_none() {
            return;
        }
        let jobs: Vec<Job> = self.mcp.server.as_ref().map(|s| s.jobs.try_iter().collect()).unwrap_or_default();
        for job in jobs {
            let act = self.mcp.log_start(&job);
            match self.mcp_gate(&job) {
                Err(e) => self.mcp_finish(job.reply, act, ToolResult::error(e)),
                Ok(true) => {
                    ctx.request_repaint();
                    self.mcp.confirm.push_back((job, act, Instant::now()));
                }
                Ok(false) => self.mcp_run(job, act),
            }
        }
        // Calls the person didn't answer in time.
        while self.mcp.confirm.front().is_some_and(|(_, _, at)| at.elapsed() > CONFIRM_TIMEOUT) {
            let (job, act, _) = self.mcp.confirm.pop_front().unwrap();
            self.mcp_finish(job.reply, act, ToolResult::error("Nobody allowed this call in rustudio within 2 minutes (the MCP panel asks the person to allow writes)."));
        }
        let pending = std::mem::take(&mut self.mcp.pending);
        for mut p in pending {
            match self.mcp_poll(&mut p.op, ctx) {
                Some(r) => self.mcp_finish(p.reply, p.act, r),
                None if Instant::now() > p.deadline => {
                    let r = self.mcp_timeout(&p.op);
                    self.mcp_finish(p.reply, p.act, r);
                }
                None => self.mcp.pending.push(p),
            }
        }
        if !self.mcp.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if !self.mcp.confirm.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    pub(crate) fn mcp_finish(&mut self, reply: Sender<ToolResult>, act: u64, r: ToolResult) {
        self.mcp.log_end(act, &r);
        let _ = reply.send(r);
    }

    /// Runs a call (allowed, confirmed).
    pub(crate) fn mcp_run(&mut self, job: Job, act: u64) {
        let step = self.mcp_call(&job.tool, &job.args);
        match step {
            Step::Done(r) => self.mcp_finish(job.reply, act, r),
            Step::Wait(op, limit) => self.mcp.pending.push(Pending { reply: job.reply, act, op, deadline: Instant::now() + limit }),
        }
    }

    /// The person answered a confirmation.
    pub fn mcp_answer(&mut self, act: u64, allow: bool) {
        let Some(i) = self.mcp.confirm.iter().position(|(_, a, _)| *a == act) else { return };
        let (job, act, _) = self.mcp.confirm.remove(i).unwrap();
        if allow {
            self.mcp_run(job, act);
        } else {
            self.mcp_finish(job.reply, act, ToolResult::error("The person in rustudio declined this call."));
        }
    }

    fn mcp_is_live(&self) -> bool {
        self.live_team_create_ids().is_some() || self.mcp_live_activity().is_some_and(|l| l.joined)
    }

    /// Whether a call may run: Err(why not), Ok(needs the person's OK).
    fn mcp_gate(&self, job: &Job) -> Result<bool, String> {
        let def = tools::find(&job.tool).ok_or_else(|| format!("Unknown tool {}", job.tool))?;
        let s = &self.mcp.settings;
        let off = |g: Group| format!("The \"{}\" tool group is off in this rustudio window (top bar > MCP > Allowed tools).", g.label());
        if !s.groups.allows(def.group) {
            return Err(off(def.group));
        }
        let live = self.mcp_is_live();
        let target = job.args.get("target").and_then(|t| t.as_str()).unwrap_or("edit");
        // Luau on a Play side is Play's; in the edit DataModel it is an edit.
        let luau_play = def.name == "run_luau" && target != "edit";
        if luau_play && !s.groups.play {
            return Err(off(Group::Play));
        }
        let live_write = live && (def.doc_write || (def.name == "run_luau" && !luau_play) || def.name == "apply_project" || def.name == "ui_action" || def.name == "load_plugin" || def.name == "reload_plugin");
        if live_write && !s.groups.live {
            return Err(format!(
                "The open place is a live Team Create session: this call could change it for every collaborator, and \"{}\" is off (top bar > MCP > Allowed tools).",
                Group::Live.label()
            ));
        }
        let dry = def.name == "apply_project" && job.args.get("dry_run").and_then(|d| d.as_bool()).unwrap_or(true);
        let writes = def.write && !dry;
        let ask = !self.mcp.allow_all
            && match s.confirm {
                Confirm::Never => false,
                Confirm::Live => writes && (live_write || def.group == Group::Live),
                Confirm::Writes => writes,
            };
        Ok(ask)
    }

    /// Keeps this window's entry in the sessions folder current.
    fn mcp_publish_session(&mut self) {
        let Some(port) = self.mcp.port() else { return };
        if self.mcp.session_t.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.mcp.session_t = Some(Instant::now());
        let info = self.mcp_session_json(port);
        if let Ok(mut s) = self.mcp.shared.lock() {
            s.info = info.clone();
        }
        let text = info.to_string();
        if text == self.mcp.session_json {
            return;
        }
        if let Some(dir) = sessions::dir() {
            let file = dir.join(format!("{}.json", std::process::id()));
            if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&file, &text).is_ok() {
                self.mcp.session_file = Some(file);
                self.mcp.session_json = text;
            }
        }
    }

    pub(crate) fn mcp_mode(&self) -> &'static str {
        use rustudio_engine::runtime::RunMode;
        match self.game.play.as_ref().map(|p| p.mode) {
            None => "edit",
            Some(RunMode::Run) => "run",
            Some(_) => "play",
        }
    }

    fn mcp_session_json(&self, port: u16) -> J {
        let doc = &self.game.doc;
        let live = self.live_team_create_ids();
        json!({
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "title": doc.title,
            "file": doc.path.as_ref().map(|p| p.display().to_string()),
            "place_id": doc.cloud.as_ref().map(|o| o.place_id),
            "universe_id": doc.cloud.as_ref().map(|o| o.universe_id),
            "live": live.is_some(),
            "mode": self.mcp_mode(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at: &str, text: &str) -> LogLine {
        LogLine { at: at.into(), level: Level::Info, text: text.into() }
    }

    #[test]
    fn output_mirror_follows_appends_trims_and_clears() {
        let mut m = OutputMirror::default();
        let mut log = vec![line("1", "a"), line("2", "b")];
        m.sync(&log, 0);
        assert_eq!(m.next, 2);
        log.push(line("3", "c"));
        m.sync(&log, 0);
        assert_eq!(m.since(2).map(|l| l.text.as_str()).collect::<Vec<_>>(), ["c"]);
        // The panel trims its oldest lines: nothing is taken twice.
        log.drain(0..2);
        log.push(line("4", "d"));
        m.sync(&log, 2);
        assert_eq!(m.next, 4);
        // Cleared, then new lines.
        log.clear();
        log.push(line("5", "e"));
        m.sync(&log, 2);
        assert_eq!(m.since(4).map(|l| l.text.as_str()).collect::<Vec<_>>(), ["e"]);
        // Cleared and refilled to the same length: still seen as new.
        log.clear();
        log.push(line("6", "f"));
        m.sync(&log, 2);
        assert_eq!(m.since(5).map(|l| l.text.as_str()).collect::<Vec<_>>(), ["f"]);
        // Nothing new: nothing added.
        m.sync(&log, 2);
        assert_eq!(m.next, 6);
    }
}
