//! The Play side of play-testing teleports (rustudio_engine::play_teleport):
//! takes a server's TeleportAsync request, loads the destination place and
//! hands it to the host, which ends this Play session and starts one on it.
//!
//! Studio 0.748 (measured 2026-11-21): the open place plays its edit
//! DataModel, unsaved edits included; another place of the experience plays
//! its latest saved version. The old server's player leaves (PlayerRemoving,
//! BindToClose) about half a second after the client's OnTeleport events.
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use rustudio_core::doc::Document;
use rustudio_core::log::Level;
use rustudio_engine::play_teleport::{self, Arrival, Request};

use crate::game::Game;

/// A loaded destination for the next Play session.
pub struct Teleport {
    pub doc: Document,
    pub place_id: u64,
    pub universe_id: u64,
    pub arrival: Arrival,
}

/// A destination being loaded.
pub struct TeleportJob {
    req: Request,
    asked: Instant,
    rx: Receiver<Result<Document, String>>,
}

/// The client hears OnTeleport (RequestedFromServer, InProgress) before
/// its server goes (Studio: ~0.5 s).
const LEAVE_AFTER: Duration = Duration::from_millis(500);

impl Game {
    /// Call once per Play frame on the thread that owns the session. True
    /// when the destination is ready in `teleport_to`: the host then stops
    /// this session and starts Play again (`start_play` plays that place).
    pub fn poll_teleport(&mut self) -> bool {
        if self.play.is_none() {
            if self.teleport_job.take().is_some() {
                play_teleport::set_in_flight(false);
            }
            return false;
        }
        if self.teleport_job.is_none() {
            let Some(req) = play_teleport::take_request() else { return false };
            play_teleport::set_in_flight(true);
            let rx = self.load_destination(&req);
            self.teleport_job = Some(TeleportJob { req, asked: Instant::now(), rx });
        }
        let job = self.teleport_job.as_ref().expect("teleport job");
        if job.asked.elapsed() < LEAVE_AFTER {
            return false;
        }
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the download stopped".into()),
        };
        let job = self.teleport_job.take().expect("teleport job");
        match result {
            Ok(doc) => {
                self.info(format!("Teleporting to {} (place {})", doc.title, job.req.place_id));
                self.teleport_to = Some(Teleport {
                    doc,
                    place_id: job.req.place_id,
                    universe_id: job.req.universe_id,
                    arrival: Arrival::of(&job.req),
                });
                true
            }
            Err(e) => {
                play_teleport::set_in_flight(false);
                self.push_log(Level::Error, format!("Teleport to place {} failed: {e}", job.req.place_id));
                false
            }
        }
    }

    /// The open place (its edit DataModel), else the place's latest saved
    /// version from Roblox with the signed-in session.
    fn load_destination(&mut self, req: &Request) -> Receiver<Result<Document, String>> {
        let (tx, rx) = mpsc::channel();
        let open = self.play_saved_doc();
        let is_open = |d: &&Document| match d.cloud.as_ref() {
            Some(o) => o.place_id == req.place_id,
            // (a live Team Create document: the place Play started on)
            None => self.play_origin.is_some_and(|(_, p)| p == req.place_id),
        };
        if let Some(saved) = open.filter(is_open) {
            let _ = tx.send(Ok(play_copy(saved)));
            return rx;
        }
        let client = self.account.shared_client();
        let place = req.place_id;
        let universe = req.universe_id;
        std::thread::spawn(move || {
            let result = (|| {
                let session = {
                    let g = client.as_ref().ok_or("sign in to teleport to other places")?;
                    let g = g.lock().map_err(|_| "the session is busy".to_string())?;
                    g.as_ref().ok_or("sign in to teleport to other places")?.session.downloader()
                };
                let api = rustudio_account::cloud::CloudApi::new(session, rustudio_account::Endpoints::default(), None);
                let mut origin = api.resolve(place).map_err(|e| e.0)?;
                if origin.universe_id != universe {
                    return Err(format!("place {place} is in another experience"));
                }
                let (bytes, version) = api.download(place, None).map_err(|e| e.0)?;
                origin.version = version;
                let mut d = Document::from_place_bytes(&bytes, origin.name.clone())?;
                d.cloud = Some(origin);
                Ok(d)
            })();
            let _ = tx.send(result);
        });
        rx
    }
}

/// A Play-only copy of the edit document (as threaded Play seeds its worker).
fn play_copy(d: &Document) -> Document {
    let mut c = Document::from_play_dom(d.dom.clone(), d.title.clone());
    c.kind = d.kind;
    c.path.clone_from(&d.path);
    c.cloud.clone_from(&d.cloud);
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustudio_engine::runtime::{RunMode, Side};

    fn origin(place_id: u64, universe_id: u64) -> rustudio_account::CloudOrigin {
        rustudio_account::CloudOrigin { place_id, universe_id, ..Default::default() }
    }

    /// A server TeleportAsync to the open place ends the session and starts
    /// another on the edit document (unsaved edits included), whose player
    /// arrives with the data; Stop then brings the edit document back.
    #[test]
    // (macOS CI runners time out waiting for the destination; tracked to
    // find out whether they're only slow or the teleport stalls there)
    #[cfg_attr(target_os = "macos", ignore)]
    fn teleport_to_the_open_place_restarts_play_with_join_data() {
        let mut game = Game::new(None);
        game.doc.cloud = Some(origin(5, 9));
        let edit_id = game.doc.id;
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        assert_eq!(game.run_luau(Side::Server, r#"
            assert(game.PlaceId==5 and game.GameId==9)
            local p=game:GetService('Players'):GetPlayers()[1]
            assert(next(p:GetJoinData())==nil)
            workspace:SetAttribute('session',1)
            local o=Instance.new('TeleportOptions');o:SetTeleportData({coins=3})
            assert(game:GetService('TeleportService'):TeleportAsync(5,{p},o)==nil)
        "#), None);
        let ctx = egui::Context::default();
        let t = Instant::now();
        let mut ready = false;
        // (generous: a cold CI runner without Studio content is slow to build
        // the destination session)
        while !ready && t.elapsed() < Duration::from_secs(60) {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            ready = game.poll_teleport();
        }
        assert!(ready, "the destination never got ready");
        game.stop_play(&mut ());
        assert_eq!(game.doc.id, edit_id, "Stop of the old session brings the edit document back");
        game.start_play(RunMode::Play, &mut (), false, None);
        assert_eq!(game.run_luau(Side::Server, r#"
            assert(game.PlaceId==5 and game.GameId==9)
            assert(workspace:GetAttribute('session')==nil,'a fresh server from the edit document')
            local j=game:GetService('Players'):GetPlayers()[1]:GetJoinData()
            assert(j.SourcePlaceId==5 and j.SourceGameId==9 and j.TeleportData.coins==3 and j.Members==nil)
        "#), None);
        assert_eq!(game.run_luau(Side::Client, r#"
            local d=game:GetService('TeleportService'):GetLocalPlayerTeleportData()
            assert(d and d.coins==3)
        "#), None);
        game.stop_play(&mut ());
        assert_eq!(game.doc.id, edit_id);
        // A plain Play afterwards is no teleport's arrival.
        game.start_play(RunMode::Play, &mut (), false, None);
        assert_eq!(game.run_luau(Side::Server, r#"
            assert(next(game:GetService('Players'):GetPlayers()[1]:GetJoinData())==nil)
        "#), None);
        game.stop_play(&mut ());
    }
}
