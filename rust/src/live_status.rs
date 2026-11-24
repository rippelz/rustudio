//! The live Team Create session as the person sees it: the status bar's
//! connection indicator, and what leaving the live document would lose.
//!
//! Studio's Team Create has no "unsaved changes" for a live place: an edit is
//! on the server as soon as it is made. rustudio's document stays dirty (its
//! local revision moves on), so closing asked to discard changes that were
//! already on the server, and the person could not tell. Leaving now asks
//! only when something is not on the server: an edit that could not be sent,
//! a script draft not committed, a commit the server has not confirmed, or a
//! session that ended (every edit since is only in this window).
//!
//! A child of cloud_ui so it reads the private `LiveView` without widening it.

use rustudio_engine::theme;

use super::{LiveView, UNCONFIRMED_WINDOW};

/// A live session that ended while its document stays open.
pub struct EndedLive {
    pub document_id: u64,
    pub reason: String,
    /// Edits that never reached the server, or were sent just before the end.
    pub lost: Vec<String>,
}

/// What leaving the open document needs.
#[derive(Debug, PartialEq)]
pub enum Leave {
    /// Nothing would be lost (a live document whose edits are on the server).
    Free,
    /// Ask first. `lacking`: what the Team Create server doesn't have (empty
    /// for an ordinary unsaved document); `note`: why.
    Ask { lacking: Vec<String>, note: String },
}

/// The status bar's Team Create state.
#[derive(Debug, PartialEq)]
pub enum Indicator {
    Joining,
    /// `pending`: edits not on the server yet (drafts, commits in flight,
    /// edits that could not be sent).
    Connected { pending: usize },
    /// Play shows another document; the session stays and catches up after.
    Paused { pending: usize },
    Ended { reason: String },
}

/// What the live session holds that the server lacks. Starts from the
/// drafts and commits in flight (a handful), not the place's instances: the
/// status bar asks every frame, and a big live place has hundreds of
/// thousands of them.
fn lacking(doc: &rustudio_core::doc::Document, live: &LiveView) -> Vec<String> {
    let mut out: Vec<String> = live.unsynced.clone();
    let mut ids: Vec<u64> = live.drafts.ids().collect();
    ids.extend(live.script_commits.values().map(|(script, _, _)| *script));
    ids.sort_unstable();
    ids.dedup();
    let mut found: Vec<(String, String)> = Vec::new();
    for id in ids {
        let Some(r) = live.model.reference(id) else { continue };
        let Some(inst) = doc.dom.get_by_ref(r) else { continue };
        if !matches!(inst.class.as_str(), "Script" | "LocalScript" | "ModuleScript") {
            continue;
        }
        let note = if live.script_commits.values().any(|(script, _, _)| *script == id) {
            "script commit not confirmed by the server yet"
        } else if let Some(d) = live.drafts.get(id) {
            if d.rejected {
                "script draft the server rejected"
            } else if !d.submitted {
                "script draft not committed"
            } else {
                continue;
            }
        } else {
            continue;
        };
        found.push((crate::live_sync::path(doc, r), note.to_string()));
    }
    found.sort();
    out.extend(found.into_iter().map(|(path, note)| format!("{path}: {note}")));
    out
}

/// The decision, from what is known (kept apart for the tests).
pub fn leave_decision(dirty: bool, live: Option<Vec<String>>, ended: Option<&EndedLive>) -> Leave {
    if let Some(lacking) = live {
        return if lacking.is_empty() {
            Leave::Free
        } else {
            Leave::Ask {
                note: format!("{} edit(s) are not on the Team Create server. Closing loses them; Save As keeps a local copy.", lacking.len()),
                lacking,
            }
        };
    }
    if let Some(ended) = ended {
        if dirty || !ended.lost.is_empty() {
            return Leave::Ask {
                lacking: ended.lost.clone(),
                note: format!(
                    "Team Create disconnected ({}). Edits made since then are only in this window, and those listed may not be on the server.",
                    ended.reason
                ),
            };
        }
        return Leave::Free;
    }
    if dirty {
        Leave::Ask { lacking: Vec::new(), note: String::new() }
    } else {
        Leave::Free
    }
}

impl crate::app::Editor {
    /// The live session whose document is shown, or put aside by Play.
    fn live_of_this_document(&self) -> Option<&LiveView> {
        let live = self.cloud.live.as_ref()?;
        let id = live.document_id?;
        (id == self.game.doc.id || self.game.play_saved_doc().is_some_and(|d| d.id == id)).then_some(live)
    }

    /// What leaving the open document (closing, quitting, opening another)
    /// needs. See the module comment.
    pub(crate) fn leave_check(&self) -> Leave {
        let doc = self.game.play_saved_doc().unwrap_or(&self.game.doc);
        let live = self.live_of_this_document().map(|l| lacking(doc, l));
        let ended = self.cloud.ended_live.as_ref().filter(|e| e.document_id == doc.id);
        leave_decision(doc.dirty(), live, ended)
    }

    pub(crate) fn live_indicator(&self) -> Option<Indicator> {
        if let Some(live) = self.cloud.live.as_ref() {
            let Some(id) = live.document_id else { return Some(Indicator::Joining) };
            if id == self.game.doc.id {
                return Some(Indicator::Connected { pending: lacking(&self.game.doc, live).len() });
            }
            if let Some(doc) = self.game.play_saved_doc().filter(|d| d.id == id) {
                return Some(Indicator::Paused { pending: lacking(doc, live).len() });
            }
            return None;
        }
        let doc = self.game.play_saved_doc().unwrap_or(&self.game.doc);
        self.cloud
            .ended_live
            .as_ref()
            .filter(|e| e.document_id == doc.id)
            .map(|e| Indicator::Ended { reason: e.reason.clone() })
    }

    /// The status bar's Team Create item (nothing for a document that was
    /// never live).
    pub(crate) fn live_status_ui(&mut self, ui: &mut egui::Ui) {
        let Some(state) = self.live_indicator() else { return };
        let pending = |n: usize| if n == 0 { String::new() } else { format!(" · {n} not on the server") };
        let (text, color, tip) = match &state {
            Indicator::Joining => ("● Team Create: joining…".to_string(), theme::WARN, "Downloading the live place".to_string()),
            Indicator::Connected { pending: n } => (
                format!("● Team Create: connected{}", pending(*n)),
                if *n == 0 { egui::Color32::from_rgb(90, 200, 120) } else { theme::WARN },
                if *n == 0 {
                    "Every edit is on the server; closing needs no save".to_string()
                } else {
                    "Some edits are not on the server (script drafts to commit, or edits that could not be sent); see team_create_status / the Output".to_string()
                },
            ),
            Indicator::Paused { pending: n } => (
                format!("● Team Create: connected (paused for Play){}", pending(*n)),
                theme::WARN,
                "Server changes are applied when Play stops".to_string(),
            ),
            Indicator::Ended { reason } => (
                "● Team Create: disconnected".to_string(),
                theme::ERROR,
                format!("{reason}. Edits since then are only in this window: keep a local copy (Save As), then reopen the live place"),
            ),
        };
        ui.label(egui::RichText::new(text).color(color)).on_hover_text(tip);
        ui.separator();
    }

    /// Remembers that the shown live session ended, for the indicator and
    /// for leaving the document (what the server may lack).
    pub(super) fn note_live_ended(&mut self, live: &LiveView, reason: &str) {
        let Some(document_id) = live.document_id else { return };
        let mut lost = live.unsynced.clone();
        lost.extend(live.sent.iter().filter(|(at, _)| at.elapsed() <= UNCONFIRMED_WINDOW).map(|(_, l)| format!("{l} (sent just before the disconnect)")));
        self.cloud.ended_live = Some(EndedLive { document_id, reason: reason.to_string(), lost });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synced_live_document_closes_without_asking() {
        assert_eq!(leave_decision(true, Some(Vec::new()), None), Leave::Free);
    }

    #[test]
    fn edits_the_server_lacks_are_listed() {
        let Leave::Ask { lacking, note } = leave_decision(true, Some(vec!["Workspace.Sign.Source: script draft not committed".into()]), None) else {
            panic!("must ask")
        };
        assert_eq!(lacking.len(), 1);
        assert!(note.contains("not on the Team Create server"), "{note}");
    }

    #[test]
    fn an_ended_session_asks_and_says_so() {
        let ended = EndedLive { document_id: 1, reason: "stream reset".into(), lost: vec![] };
        let Leave::Ask { lacking, note } = leave_decision(true, None, Some(&ended)) else { panic!("must ask") };
        assert!(lacking.is_empty());
        assert!(note.contains("disconnected (stream reset)"), "{note}");
        assert_eq!(leave_decision(false, None, Some(&ended)), Leave::Free);
    }

    #[test]
    fn ordinary_documents_keep_the_unsaved_prompt() {
        assert_eq!(leave_decision(true, None, None), Leave::Ask { lacking: Vec::new(), note: String::new() });
        assert_eq!(leave_decision(false, None, None), Leave::Free);
    }
}
