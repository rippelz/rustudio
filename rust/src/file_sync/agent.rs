//! File Sync over the MCP server (src/mcp): what an agent needs to know
//! what the window has — the attached project, what waits, what differs
//! between the files and the place, the log — and to attach, apply or
//! detach. Every call is checked by the MCP gate first (tool groups, live
//! writes, the person's confirmation).

use std::path::PathBuf;

use serde_json::{json, Value as J};

use super::plan::{self, Scope};
use super::settings::Initial;
use super::Reason;
use crate::app::Editor;

fn line_json(l: &plan::Line, rel: &dyn Fn(&std::path::Path) -> String, with_diff: bool, limit: usize) -> J {
    let mut j = json!({ "change": l.kind, "path": l.path, "class": l.class });
    if let Some(p) = &l.prop {
        j["property"] = json!(p);
    }
    if let Some(f) = &l.file {
        j["file"] = json!(rel(f));
    }
    if l.instances > 1 {
        j["instances"] = json!(l.instances);
    }
    match (&l.old, &l.new, l.prop.as_deref()) {
        (Some(old), Some(new), Some("Source")) => {
            let rows = super::diff::diff(old, new, 2);
            let (plus, minus) = super::diff::stat(&rows);
            j["lines_added"] = json!(plus);
            j["lines_removed"] = json!(minus);
            if with_diff {
                let text: Vec<String> = rows
                    .iter()
                    .take(limit)
                    .map(|r| match r {
                        super::diff::Row::Same(t) => format!("  {t}"),
                        super::diff::Row::Add(t) => format!("+ {t}"),
                        super::diff::Row::Del(t) => format!("- {t}"),
                        super::diff::Row::Fold(n) => format!("@@ {n} unchanged @@"),
                    })
                    .collect();
                j["diff"] = json!(text.join("\n"));
            }
        }
        (Some(old), Some(new), _) => {
            j["from"] = json!(old);
            j["to"] = json!(new);
        }
        _ => {}
    }
    j
}

impl Editor {
    pub(crate) fn file_sync_mcp_status(&self, a: &J) -> J {
        let fs = &self.file_sync;
        let status = format!("{:?}", fs.status()).to_lowercase();
        let (_, text) = self.file_sync_status_text();
        let logs = a.get("log_lines").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
        let log: Vec<J> = fs
            .log
            .iter()
            .rev()
            .take(logs)
            .rev()
            .map(|e| json!({ "at": e.at, "level": e.settings_label(), "text": e.text, "file": e.file, "status": e.status, "ok": e.ok }))
            .collect();
        let Some(att) = &fs.att else {
            return json!({
                "status": status,
                "summary": text,
                "attached": false,
                "remembered_for_this_place": self.file_sync_place_key().and_then(|(k, _)| fs.settings.remembered_for(&k).map(|r| r.project.display().to_string())),
                "log": log,
            });
        };
        let rel = |p: &std::path::Path| att.rel(p);
        let pending = fs.pending.as_ref().map(|p| {
            json!({
                "reason": format!("{:?}", p.reason).to_lowercase(),
                "message": p.reason.text(p.lines.len(), fs.settings.threshold),
                "creates": p.counts.0, "sets": p.counts.1, "deletes": p.counts.2,
                "changes": p.lines.iter().take(200).map(|l| line_json(l, &rel, false, 0)).collect::<Vec<_>>(),
                "notes": p.notes,
                "next": "file_sync_apply applies it (planned again against the files as they are now); file_sync_apply with skip=true drops it",
            })
        });
        let commits: Vec<J> = fs.commits.iter().map(|c| json!({ "script": c.path, "sent": c.sent, "conflict": c.conflict, "seconds": c.since.elapsed().as_secs_f32() })).collect();
        json!({
            "status": status,
            "summary": text,
            "attached": true,
            "project": att.st.file.display().to_string(),
            "place": att.place_label,
            "place_key": att.place_key,
            "live_team_create": att.live,
            "instances_from_files": att.st.map.len(),
            "changes_applied": att.applied,
            "seconds_since_last_sync": att.last_sync.as_ref().map(|(t, _)| t.elapsed().as_secs_f32()),
            "last_sync": att.last_sync.as_ref().map(|(_, s)| s.clone()),
            "waiting_for_play_to_stop": att.deferred.is_some(),
            "mappings": att.mappings.iter().map(|m| json!({ "place": m.place, "disk": m.disk, "in_place": m.has, "class": m.class, "files": m.files })).collect::<Vec<_>>(),
            "pending": pending,
            "live_script_commits": commits,
            "error": fs.error,
            "settings": {
                "on_attach": fs.settings.initial.key(),
                "two_way_sync": fs.settings.two_way,
                "keep_unknowns": fs.settings.keep_unknowns_for(att.live),
                "live_hydrate": fs.settings.hydrate,
                "prompts": format!("{:?}", fs.settings.prompts).to_lowercase(),
                "threshold": fs.settings.threshold,
            },
            "log": log,
        })
    }

    pub(crate) fn file_sync_mcp_diff(&mut self, a: &J) -> Result<J, String> {
        let with_diff = a.get("source_diffs").and_then(|v| v.as_bool()).unwrap_or(false);
        let limit = a.get("diff_lines").and_then(|v| v.as_u64()).unwrap_or(200) as usize;
        let lines = self.file_sync_drift().ok_or("No project is attached (file_sync_attach), or it can't be read")?;
        let att = self.file_sync.att.as_ref().unwrap();
        let rel = |p: &std::path::Path| att.rel(p);
        Ok(json!({
            "differences": lines.len(),
            "note": "What \"files win\" would change in the place now (nothing was changed). Instances inside mapped folders that no file describes are listed as `delete unknown` only when Keep Unknowns is off.",
            "changes": lines.iter().take(500).map(|l| line_json(l, &rel, with_diff, limit)).collect::<Vec<_>>(),
        }))
    }

    pub(crate) fn file_sync_mcp_attach(&mut self, a: &J) -> Result<J, String> {
        let p = a.get("project").and_then(|v| v.as_str()).ok_or("project: a *.project.json or a folder with one")?;
        let p = PathBuf::from(p);
        if !p.is_absolute() {
            return Err("\"project\" must be an absolute path".into());
        }
        let initial = match a.get("on_attach").and_then(|v| v.as_str()) {
            Some(s) => Initial::parse(s).ok_or("on_attach: \"preview\", \"files\" or \"place\"")?,
            None => Initial::Preview,
        };
        let remember = a.get("remember").and_then(|v| v.as_bool()).unwrap_or(true);
        let before = self.file_sync.remember;
        self.file_sync.remember = remember;
        let r = self.file_sync_attach(&p, initial);
        self.file_sync.remember = before;
        let summary = r?;
        let mut out = self.file_sync_mcp_status(&json!({ "log_lines": 10 }));
        out["attached_now"] = json!(summary);
        Ok(out)
    }

    pub(crate) fn file_sync_mcp_apply(&mut self, a: &J) -> Result<J, String> {
        if self.file_sync.att.is_none() {
            return Err("No project is attached".into());
        }
        if a.get("skip").and_then(|v| v.as_bool()).unwrap_or(false) {
            if self.file_sync.pending.is_none() {
                return Err("Nothing is waiting".into());
            }
            self.file_sync_answer(false);
            return Ok(self.file_sync_mcp_status(&json!({ "log_lines": 5 })));
        }
        let all = a.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
        let log_from = self.file_sync.log.back().map_or(0, |e| e.id);
        if all {
            // The agent's own OK (the MCP gate asked the person when it should).
            self.file_sync.pending = None;
            self.file_sync_run(Scope::All, false, Some(Reason::Always));
            if self.file_sync.pending.is_none() {
                return Ok(json!({ "applied": 0, "note": "the place already has what the files have" }));
            }
        } else if self.file_sync.pending.is_none() {
            return Err("Nothing is waiting; pass all=true to make the place match every file now".into());
        }
        self.file_sync_answer(true);
        let applied: Vec<J> = self
            .file_sync
            .log
            .iter()
            .filter(|e| e.id > log_from)
            .map(|e| json!({ "level": e.settings_label(), "text": e.text, "file": e.file, "status": e.status }))
            .collect();
        let live = self.file_sync.att.as_ref().is_some_and(|a| a.live);
        Ok(json!({
            "log": applied,
            "next": if live { "Live place: script commits are followed in file_sync_status (live_script_commits); team_create_wait waits for the session to settle." } else { "" },
        }))
    }

    pub(crate) fn file_sync_mcp_detach(&mut self, a: &J) -> Result<J, String> {
        if self.file_sync.att.is_none() {
            return Err("No project is attached".into());
        }
        let forget = a.get("forget").and_then(|v| v.as_bool()).unwrap_or(false);
        self.file_sync_detach(forget);
        Ok(json!({ "detached": true, "forgotten": forget }))
    }
}

impl super::LogEntry {
    fn settings_label(&self) -> &'static str {
        match self.level {
            super::settings::LogLevel::Error => "error",
            super::settings::LogLevel::Warn => "warn",
            super::settings::LogLevel::Info => "info",
            super::settings::LogLevel::Verbose => "verbose",
        }
    }
}
