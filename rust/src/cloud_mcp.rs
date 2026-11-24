//! The MCP server's view of the live Team Create session (src/mcp): its
//! state, and the script-draft commit / discard the script tab's buttons
//! do (`live_draft_toolbar`). A child of cloud_ui so it reads the private
//! `LiveView` without widening it; it changes nothing on its own.

use std::time::Instant;

use rbx_types::Ref;
use serde_json::{json, Value as J};

use super::UNCONFIRMED_WINDOW;

/// What `team_create_wait` watches.
pub struct LiveActivity {
    pub joined: bool,
    /// When the last document edit was queued for the server.
    pub last_sent: Option<Instant>,
    pub pending_commits: usize,
    /// Edits that were not sent at all this session.
    pub unsynced: usize,
    pub sync_failed: bool,
}

/// Where a live script's draft is (File Sync follows its commits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftState {
    /// Not on the server (or not a live document).
    NotLive,
    /// No draft: the server has what the document has.
    Clean,
    /// A draft not sent yet.
    Local,
    /// A draft the server changed under (a newer version on the server).
    Conflict,
    /// A commit sent, waiting for the server.
    Committing,
    /// Sent as a Source property (no confirmation comes).
    Submitted,
    /// The server rejected the commit.
    Rejected,
}

impl crate::app::Editor {
    pub(crate) fn live_draft_state(&self, reference: Ref) -> DraftState {
        let Some(live) = self.cloud.live.as_ref().filter(|l| l.document_id == Some(self.game.doc.id)) else { return DraftState::NotLive };
        let Some(id) = live.model.server_id(reference) else { return DraftState::NotLive };
        if live.script_commits.values().any(|(script, _, _)| *script == id) {
            return DraftState::Committing;
        }
        match live.drafts.get(id) {
            None => DraftState::Clean,
            Some(d) if d.rejected => DraftState::Rejected,
            Some(_) if live.drafts.conflict(id) => DraftState::Conflict,
            Some(d) if d.submitted => DraftState::Submitted,
            Some(_) => DraftState::Local,
        }
    }

    /// Commits a draft even over a newer server version (File Sync's
    /// "Replace server with the file", after the person chose it).
    pub(crate) fn live_commit_replacing(&mut self, reference: Ref) -> Result<String, String> {
        self.live_commit(reference, true)
    }

    /// The live session of this window (joined or joining), if any.
    pub(crate) fn mcp_live_activity(&self) -> Option<LiveActivity> {
        let live = self.cloud.live.as_ref()?;
        Some(LiveActivity {
            joined: live.document_id == Some(self.game.doc.id),
            last_sent: live.sent.back().map(|(at, _)| *at),
            pending_commits: live.script_commits.len(),
            unsynced: live.unsynced.len(),
            sync_failed: live.sync_failed,
        })
    }

    /// Scripts of the live document with a draft.
    pub(crate) fn mcp_live_drafts(&self) -> Vec<Ref> {
        let Some(live) = self.cloud.live.as_ref().filter(|l| l.document_id == Some(self.game.doc.id)) else { return Vec::new() };
        self.game
            .doc
            .dom
            .descendants()
            .filter(|i| matches!(i.class.as_str(), "Script" | "LocalScript" | "ModuleScript"))
            .map(|i| i.referent())
            .filter(|r| live.model.server_id(*r).is_some_and(|id| live.drafts.get(id).is_some()))
            .collect()
    }

    pub(crate) fn mcp_live_status(&self) -> J {
        let Some(live) = self.cloud.live.as_ref() else {
            return json!({ "state": "none", "note": "This window has no Team Create session (join_team_create opens one)." });
        };
        let joined = live.document_id == Some(self.game.doc.id);
        let state = match live.document_id {
            None => "joining",
            Some(_) if joined => "connected",
            Some(_) => "connected (another document is shown)",
        };
        let path = |r: Ref| crate::mcp::tree::path(&self.game.doc, r);
        let drafts: Vec<J> = self
            .mcp_live_drafts()
            .into_iter()
            .filter_map(|r| {
                let id = live.model.server_id(r)?;
                let d = live.drafts.get(id)?;
                Some(json!({ "path": path(r), "submitted": d.submitted, "conflict": live.drafts.conflict(id), "rejected": d.rejected }))
            })
            .collect();
        let commits: Vec<J> = live
            .script_commits
            .values()
            .map(|(id, at, _)| json!({ "script": live.labels.get(id).cloned().or_else(|| live.model.reference(*id).map(path)).unwrap_or_else(|| format!("server #{id}")), "seconds": at.elapsed().as_secs_f32() }))
            .collect();
        let recent: Vec<&String> = live.sent.iter().filter(|(at, _)| at.elapsed() <= UNCONFIRMED_WINDOW).map(|(_, l)| l).collect();
        json!({
            "state": state,
            "universe_id": live.universe,
            "place_id": live.place,
            "place_name": live.origin.as_ref().map(|o| o.name.clone()),
            "instances": live.model.instance_count(),
            "sync_failed": live.sync_failed,
            "unsynced": live.unsynced,
            "sent_last_10s": recent,
            "seconds_since_last_send": live.sent.back().map(|(at, _)| at.elapsed().as_secs_f32()),
            "drafts": drafts,
            "pending_commits": commits,
            "save_pending": live.save_pending.is_some(),
        })
    }

    /// Commits one script's draft (the script tab's Commit / Replace server
    /// button). Ok(what happened).
    pub(crate) fn mcp_commit_draft(&mut self, reference: Ref) -> Result<String, String> {
        self.live_commit(reference, false)
    }

    /// `replace`: commit over a newer server version too (else that is left
    /// to the person and the conflict is committed as the toolbar does).
    fn live_commit(&mut self, reference: Ref, replace: bool) -> Result<String, String> {
        let doc_id = self.game.doc.id;
        let live = self.cloud.live.as_mut().filter(|l| l.document_id == Some(doc_id)).ok_or("This window isn't in a live Team Create session")?;
        let id = live.model.server_id(reference).ok_or("That script isn't on the Team Create server yet")?;
        let draft = live.drafts.get(id).cloned().ok_or("That script has no draft")?;
        if draft.submitted {
            return Ok("already sent".into());
        }
        if live.script_commits.values().any(|(script, _, _)| *script == id) {
            return Ok("a commit is already waiting for the server".into());
        }
        let conflict = live.drafts.conflict(id);
        let text = live.drafts.commit_text(id, conflict || replace)?;
        let inst = self.game.doc.dom.get_by_ref(reference).ok_or("The script was removed")?;
        let guid = inst.properties.get(&"ScriptGuid".into()).and_then(|v| if let rbx_types::Variant::String(g) = v { Some(g.clone()) } else { None });
        let class = inst.class.to_string();
        if let (Some(service), Some(guid)) = (live.script_service, guid) {
            if !live.script_configuration_sent {
                live.session
                    .send_event(service, "LiveScriptingService".into(), "LiveEditingClientConfiguration".into(), vec![rustudio_net::scripts::client_configuration()])
                    .map_err(|e| e.to_string())?;
                live.script_configuration_sent = true;
            }
            let request = live.next_script_request;
            live.next_script_request = live.next_script_request.checked_add(1).ok_or("Script request counter exhausted")?;
            let base = live.drafts.source(id).ok_or("The server source is unavailable")?;
            let args = rustudio_net::scripts::replace(request, &guid, base, &text);
            live.session.send_event(service, "LiveScriptingService".into(), "SendAtomicOperation".into(), args).map_err(|e| e.to_string())?;
            live.script_commits.insert(request, (id, Instant::now(), text));
            Ok(if conflict { "commit sent, replacing a newer server version; waiting for the server".into() } else { "commit sent; waiting for the server".into() })
        } else {
            live.session.send_property(id, class, "Source".into(), rustudio_net::replicator::PropValue::String(text)).map_err(|e| e.to_string())?;
            live.drafts.submitted(id);
            Ok("draft sent as a Source property".into())
        }
    }
}
