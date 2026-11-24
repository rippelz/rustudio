//! Self-test step for round 27, "cloud-place": a place from Roblox.
//!
//! - Links and IDs parse (roblox.com/games/…, create.roblox.com/…/places/…).
//! - Signed in: Roblox's public Baseplate template (place 95206881,
//!   universe 28220420, by @Templates 998796) opens from Roblox through
//!   the app's own path, lands in the cache and in Recent, and knows its
//!   origin; the Team Create status is asked for; a place the account
//!   can't read (100000005) gives a clear error and leaves the document.
//! - Play reports the place's ids (game.PlaceId, GameId, CreatorId,
//!   CreatorType, PlaceVersion).
//! - Ctrl+S against a local mock of Roblox (nothing is sent to Roblox):
//!   someone saved in between, so it stops with the conflict; Overwrite
//!   saves the next revision, then Publish sends a published revision.
//! - Publish As relinks the document and saves only the submitted snapshot.
//!   The next Save sends the edits made while Publish As was pending.
//! - Signed out, the cached copy reopens.
//!
//! Signed out, the Baseplate part is skipped and a made-up origin is used.

use std::sync::{Arc, Mutex};

use rbx_dom_weak::InstanceBuilder;
use rustudio_account::mock::{MockReply, MockRequest, MockServer};
use rustudio_account::{
    parse_place_ref, CloudApi, CloudOrigin, Cookie, CreatorKind, Endpoints, PlaceCache, Profile,
    Session, VersionType,
};

use super::{SelfTest, Step};
use crate::app::Editor;
use rustudio_core::doc::Document;

const BASEPLATE: u64 = 95206881;
const BASEPLATE_UNIVERSE: u64 = 28220420;
const TEMPLATES: u64 = 998796;
const AS_PLACE: u64 = 101;
const AS_UNIVERSE: u64 = 102;
/// Another account's private place.
const NO_ACCESS: u64 = 100000005;
const MOCK_COOKIE: &str = ".ROBLOSECURITY=rustudio-selftest-mock-session";
const MOCK_CSRF: &str = "rustudio-selftest-csrf";
const ASSETS: &str = "/apis/assets/user-auth/v1/assets";
const OPERATIONS: &str = "/apis/assets/user-auth/v1/operations";

#[derive(Default)]
pub(super) struct CloudTest {
    stage: u32,
    started: u64,
    t0: Option<std::time::Instant>,
    notes: Vec<String>,
    ok: bool,
    mark: usize,
    /// The version Baseplate opened at.
    version: u64,
    mock: Option<MockServer>,
}

fn mock_api(base: &str) -> crate::cloud_ui::ApiFactory {
    let base = base.to_string();
    Arc::new(move || {
        let session = Session::unchecked(
            Cookie::new(MOCK_COOKIE)?,
            Profile {
                id: 1,
                name: "selftest".into(),
                display_name: "selftest".into(),
            },
        );
        Some(CloudApi::new(session, Endpoints::mock(&base), None))
    })
}

fn session_authenticated(r: &MockRequest) -> bool {
    r.header("cookie") == Some(MOCK_COOKIE)
        && r.header("x-api-key").is_none()
}

fn upload_parts(r: &MockRequest) -> Option<(serde_json::Value, &[u8])> {
    let boundary = r.header("content-type")?
        .strip_prefix("multipart/form-data; boundary=")?
        .trim_matches('"');
    let separator = format!("\r\n--{boundary}\r\n");
    let body = r.body.strip_prefix(format!("--{boundary}\r\n").as_bytes())?
        .strip_suffix(format!("\r\n--{boundary}--\r\n").as_bytes())?;
    let split = body.windows(separator.len()).position(|w| w == separator.as_bytes())?;
    let part = |bytes: &'_ [u8]| {
        bytes.windows(4).position(|w| w == b"\r\n\r\n")
    };
    let request = &body[..split];
    let file = &body[split + separator.len()..];
    let (request_head, file_head) = (part(request)?, part(file)?);
    if !std::str::from_utf8(&request[..request_head]).ok()?.contains("name=\"request\"")
        || !std::str::from_utf8(&file[..file_head]).ok()?.contains("name=\"fileContent\"")
    {
        return None;
    }
    Some((serde_json::from_slice(&request[request_head + 4..]).ok()?, &file[file_head + 4..]))
}

fn operation_id(revision: u64, published: bool) -> String {
    format!("{revision}-{published}")
}

/// Loopback asset delivery, place upload and operation polling.
fn mock_roblox(place: u64, version: u64) -> Option<MockServer> {
    let server = Mutex::new((version, None));
    MockServer::start(Arc::new(move |r: &MockRequest| {
        let mut state = server.lock().unwrap();
        let (revision, last_published) = &mut *state;
        if r.path.starts_with(&format!("/assetdelivery/v2/assetId/{place}")) {
            return MockReply::json(200, r#"{"locations":[{"location":"http://127.0.0.1:9/none"}]}"#)
                .with_header("roblox-assetversionnumber", &revision.to_string());
        }
        if r.path == format!("/apis/universes/v1/places/{AS_PLACE}/universe") {
            return MockReply::json(200, &format!(r#"{{"universeId":{AS_UNIVERSE}}}"#));
        }
        if r.path == format!("/develop/v1/universes/{AS_UNIVERSE}") {
            return MockReply::json(200, &format!(r#"{{"id":{AS_UNIVERSE},"name":"Upload destination","rootPlaceId":{AS_PLACE}}}"#));
        }
        if r.path.starts_with(&format!("/assetdelivery/v2/assetId/{AS_PLACE}")) {
            return MockReply::json(200, r#"{"locations":[{"location":"http://127.0.0.1:9/none"}]}"#)
                .with_header("roblox-assetversionnumber", &revision.to_string());
        }
        let upload = r.method == "PATCH"
            && (r.path == format!("{ASSETS}/{place}") || r.path == format!("{ASSETS}/{AS_PLACE}"));
        let poll = r.method == "GET" && r.path.starts_with(&format!("{OPERATIONS}/"));
        if (upload || poll) && !session_authenticated(r) {
            return MockReply::json(401, r#"{"message":"mock: session required"}"#);
        }
        if upload {
            if r.header("x-csrf-token") != Some(MOCK_CSRF) {
                return MockReply::json(403, r#"{"message":"mock: CSRF token required"}"#)
                    .with_header("x-csrf-token", MOCK_CSRF);
            }
            if let Some(published) = upload_parts(r).and_then(|(metadata, _)| metadata["published"].as_bool()) {
                *revision += 1;
                *last_published = Some(published);
                let id = operation_id(*revision, published);
                return MockReply::json(200, &format!(r#"{{"path":"operations/{id}","done":false}}"#));
            }
            return MockReply::json(400, r#"{"message":"mock: expected multipart place upload with published flag"}"#);
        }
        if let Some(published) = *last_published {
            if poll && r.path == format!("{OPERATIONS}/{}", operation_id(*revision, published)) {
                return MockReply::json(200, &format!(
                    r#"{{"done":true,"response":{{"revisionId":"{revision}","isPublished":{published}}}}}"#
                ));
            }
        }
        MockReply::json(404, r#"{"errors":[{"code":0,"message":"mock: no route"}]}"#)
    }))
    .ok()
}

/// Checks the write, its CSRF retry, and the authenticated operation poll in order.
fn uploaded_bytes(requests: &[MockRequest], place: u64, revision: u64, published: bool) -> Option<&[u8]> {
    let [initial, retry, poll] = requests else { return None };
    let path = format!("{ASSETS}/{place}");
    if initial.method != "PATCH" || retry.method != "PATCH"
        || initial.path != path || retry.path != path
        || initial.header("x-csrf-token").is_some()
        || retry.header("x-csrf-token") != Some(MOCK_CSRF)
        || initial.body != retry.body
        || poll.method != "GET"
        || poll.path != format!("{OPERATIONS}/{}", operation_id(revision, published))
        || !requests.iter().all(session_authenticated)
    {
        return None;
    }
    let (metadata, bytes) = upload_parts(retry)?;
    (metadata["assetType"].as_str() == Some("Place")
        && metadata["assetId"].as_u64() == Some(place)
        && metadata["published"].as_bool() == Some(published))
        .then_some(bytes)
}

pub(super) fn mock_session_upload() -> bool {
    let Some(mock) = mock_roblox(AS_PLACE, 1) else { return false };
    let Some(api) = mock_api(&mock.base)() else { return false };
    let bytes = b"<roblox!\x00\xff";
    let revision = api.upload_place(AS_UNIVERSE, AS_PLACE, bytes, false, VersionType::Saved);
    matches!(revision, Ok(2))
        && uploaded_bytes(&mock.requests(), AS_PLACE, 2, false) == Some(bytes.as_slice())
}

impl SelfTest {
    fn cloud_wait(&self) -> u64 {
        self.cloud.t0.map_or(0, |t| t.elapsed().as_secs())
    }

    fn cloud_next(&mut self, f: u64, stage: u32) {
        self.cloud.stage = stage;
        self.cloud.started = f;
        self.cloud.t0 = Some(std::time::Instant::now());
    }

    fn cloud_note(&mut self, ok: bool, note: String) {
        self.cloud.ok &= ok;
        self.cloud.notes.push(note);
    }

    /// Phase 90.
    pub(super) fn tick_cloud(&mut self, f: u64, ed: &mut Editor) -> bool {
        let since = f.saturating_sub(self.cloud.started);
        let waited = self.cloud_wait();
        match self.cloud.stage {
            0 => {
                self.cloud.ok = true;
                // A throwaway cache (RUSTUDIO_CACHE_DIR is the self-test's).
                ed.cloud.cache = PlaceCache::default();
                let _ = std::fs::remove_dir_all(&ed.cloud.cache.root);
                let cases = [
                    ("95206881", Some(BASEPLATE)),
                    ("https://www.roblox.com/games/95206881/Baseplate", Some(BASEPLATE)),
                    ("https://create.roblox.com/dashboard/creations/experiences/28220420/places/95206881/configure", Some(BASEPLATE)),
                    ("roblox.com/games/start?placeId=95206881", Some(BASEPLATE)),
                    ("https://example.com/games/95206881", None),
                    ("Baseplate", None),
                ];
                let bad: Vec<&str> = cases
                    .iter()
                    .filter(|(s, want)| parse_place_ref(s).ok().map(|p| p.place_id) != *want)
                    .map(|(s, _)| *s)
                    .collect();
                self.cloud_note(bad.is_empty(), format!("place IDs and links parse ({} cases; wrong: {bad:?})", cases.len()));
                if ed.game.account.signed_in() {
                    ed.perform_open_cloud(BASEPLATE, None, false);
                    self.cloud_next(f, 1);
                } else {
                    self.cloud.notes.push("signed out: the Baseplate download is skipped; a made-up origin stands in".into());
                    let mut d = Document::new_place();
                    d.title = "Baseplate".into();
                    d.cloud = Some(CloudOrigin {
                        universe_id: BASEPLATE_UNIVERSE,
                        place_id: BASEPLATE,
                        version: 71,
                        name: "Baseplate".into(),
                        experience: "Baseplate".into(),
                        root_place_id: BASEPLATE,
                        creator_id: TEMPLATES,
                        creator_type: CreatorKind::User,
                        creator_name: "Templates".into(),
                        ..Default::default()
                    });
                    self.cloud.version = 71;
                    ed.set_document(d);
                    self.cloud_next(f, 4);
                }
            }
            // The download.
            1 => {
                let got = ed.game.doc.cloud.as_ref().is_some_and(|o| o.place_id == BASEPLATE);
                if !got && ed.cloud.open_error.is_none() && waited < 60 {
                    return false;
                }
                match ed.game.doc.cloud.clone().filter(|_| got) {
                    Some(o) => {
                        let ok = o.universe_id == BASEPLATE_UNIVERSE
                            && o.creator_id == TEMPLATES
                            && o.creator_type == CreatorKind::User
                            && o.version > 0
                            && o.name == "Baseplate";
                        self.cloud_note(ok, format!(
                            "opened from Roblox: {:?} place {} version {} universe {} by {} ({} {}); tab title {:?}",
                            o.name, o.place_id, o.version, o.universe_id, o.creator_label(), o.creator_type.as_str(), o.creator_id, ed.game.doc.title
                        ));
                        self.cloud.version = o.version;
                        let cached = ed.cloud.cache.cached_versions(BASEPLATE);
                        let recent = ed.cloud.cache.recent().first().map(|r| r.origin.place_id);
                        self.cloud_note(
                            cached.contains(&o.version) && recent == Some(BASEPLATE),
                            format!("cached versions {cached:?} at {}; first in Recent: {recent:?}", ed.cloud.cache.dir(BASEPLATE).display()),
                        );
                        let parts = ed.game.doc.dom.descendants().count();
                        self.cloud_note(parts > 5, format!("{parts} instances"));
                    }
                    None => self.cloud_note(false, format!(
                        "Baseplate didn't open in {waited} s: {:?} (cloud jobs running: {}, signed in: {})",
                        ed.cloud.open_error, ed.cloud.busy(), ed.game.account.signed_in()
                    )),
                }
                self.cloud_next(f, 2);
            }
            // The Team Create status of its universe.
            2 => {
                let st = ed.cloud.status.clone().filter(|(u, _)| *u == BASEPLATE_UNIVERSE);
                if st.is_none() && waited < 30 {
                    return false;
                }
                match st {
                    // Baseplate isn't ours: Roblox doesn't say whether Team
                    // Create is on, and we may not edit it.
                    Some((_, s)) => self.cloud_note(
                        s.can_edit() == Some(false) && s.can_collaborate == Some(false),
                        format!("Team Create: {:?} (can collaborate {:?}, {:?})", s.describe(), s.can_collaborate, s.reason),
                    ),
                    None => self.cloud_note(false, "no Team Create status within 30 s".into()),
                }
                ed.cloud.open_error = None;
                ed.perform_open_cloud(NO_ACCESS, None, false);
                self.cloud_next(f, 3);
            }
            // A place this account can't read.
            3 => {
                if ed.cloud.open_error.is_none() && waited < 45 {
                    return false;
                }
                let e = ed.cloud.open_error.clone().unwrap_or_default();
                let still = ed.game.doc.cloud.as_ref().is_some_and(|o| o.place_id == BASEPLATE);
                self.cloud_note(
                    e.contains("won't give this account place 100000005") && still,
                    format!("no access: {e:?}; the open document stays: {still}"),
                );
                self.cloud_next(f, 4);
            }
            // Play reports the ids.
            4 => {
                let root = ed.game.doc.dom.root_ref();
                let sss = ed.game.doc.find_child(root, "ServerScriptService").unwrap_or_else(|| {
                    ed.game.doc.dom.insert(root, InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"))
                });
                ed.game.doc.insert(
                    sss,
                    InstanceBuilder::new("Script").with_name("CloudIds").with_property(
                        "Source",
                        "print(\"cloud ids\", game.PlaceId, game.GameId, game.CreatorId, game.CreatorType, game.PlaceVersion)".to_string(),
                    ),
                );
                self.cloud.mark = ed.game.log.len();
                ed.start_play(rustudio_engine::runtime::RunMode::Run);
                self.cloud_next(f, 5);
            }
            5 if since >= 30 => {
                let want = format!(
                    "cloud ids {BASEPLATE} {BASEPLATE_UNIVERSE} {TEMPLATES} Enum.CreatorType.User {}",
                    self.cloud.version
                );
                let line = ed.game.log[self.cloud.mark.min(ed.game.log.len())..]
                    .iter()
                    .find(|l| l.text.contains("cloud ids"))
                    .map(|l| l.text.clone());
                self.cloud_note(line.as_deref().is_some_and(|l| l.contains(&want)), format!("Play printed {line:?}"));
                ed.stop_play();
                // Ctrl+S with someone else's newer version on (mock) Roblox.
                self.cloud.mock = mock_roblox(BASEPLATE, self.cloud.version + 1);
                let Some(base) = self.cloud.mock.as_ref().map(|m| m.base.clone()) else {
                    self.cloud_note(false, "the mock server didn't start".into());
                    self.cloud_next(f, 9);
                    return false;
                };
                ed.cloud.api_override = Some(mock_api(&base));
                ed.cloud.conflict = None;
                ed.cloud.last_saved = None;
                ed.run_action("save");
                self.cloud_next(f, 6);
            }
            6 => {
                if ed.cloud.conflict.is_none() && ed.cloud.save_error.is_none() && waited < 20 {
                    return false;
                }
                let uploads = self.mock_upload_requests();
                let c = ed.cloud.conflict;
                self.cloud_note(
                    c.is_some_and(|c| c.opened == self.cloud.version && c.server == self.cloud.version + 1) && uploads.is_empty(),
                    format!("Ctrl+S after someone saved: conflict {c:?} (asked the user; {} upload(s) sent)", uploads.len()),
                );
                // "Overwrite".
                ed.cloud.conflict = None;
                ed.save_cloud(VersionType::Saved, true);
                self.cloud_next(f, 7);
            }
            7 => {
                if ed.cloud.last_saved.is_none() && ed.cloud.save_error.is_none() && waited < 20 {
                    return false;
                }
                let uploads = self.mock_upload_requests();
                let want = self.cloud.version + 2;
                let v = ed.game.doc.cloud.as_ref().map(|o| o.version);
                let upload = uploaded_bytes(&uploads, BASEPLATE, want, false);
                let bytes_ok = upload.and_then(|bytes| Document::from_place_bytes(bytes, String::new()).ok())
                    .is_some_and(|doc| doc.dom.descendants().any(|i| i.name == "CloudIds"));
                let ok = ed.cloud.last_saved == Some((want, VersionType::Saved))
                    && v == Some(want)
                    && !ed.game.doc.dirty()
                    && bytes_ok;
                self.cloud_note(ok, format!(
                    "Overwrite: saved revision {:?}, document at {v:?}, dirty={}, session upload snapshot={bytes_ok}, error {:?}",
                    ed.cloud.last_saved, ed.game.doc.dirty(), ed.cloud.save_error
                ));
                ed.ui_action(&egui::Context::default(), "publish");
                self.cloud_next(f, 8);
            }
            8 => {
                let want = self.cloud.version + 3;
                let complete = ed.cloud.last_saved == Some((want, VersionType::Published));
                if !complete && ed.cloud.save_error.is_none() && waited < 20 {
                    return false;
                }
                let uploads = self.mock_upload_requests();
                let published = uploads.get(3..)
                    .and_then(|requests| uploaded_bytes(requests, BASEPLATE, want, true))
                    .is_some();
                let version = ed.game.doc.cloud.as_ref().map(|o| o.version);
                self.cloud_note(
                    complete && published && version == Some(want) && !ed.game.doc.dirty(),
                    format!("Publish: session upload and poll={published}, result={:?}, document at {version:?}, error {:?}",
                        ed.cloud.last_saved, ed.cloud.save_error),
                );
                // Signed out: the cached copy reopens.
                ed.cloud.api_override = Some(Arc::new(|| None));
                self.cloud.mark = ed.game.log.len();
                ed.cloud.open_error = None;
                ed.perform_open_cloud(BASEPLATE, None, false);
                self.cloud_next(f, 9);
            }
            9 => {
                let reopened = ed.game.log[self.cloud.mark.min(ed.game.log.len())..]
                    .iter()
                    .any(|l| l.text.contains("this is the copy cached at"));
                if !reopened && ed.cloud.open_error.is_none() && waited < 20 && self.cloud.mock.is_some() {
                    return false;
                }
                if self.cloud.mock.is_some() {
                    let v = ed.game.doc.cloud.as_ref().map(|o| o.version);
                    self.cloud_note(
                        reopened && v == Some(self.cloud.version + 3),
                        format!("signed out, reopened from the cache: {reopened} (version {v:?}); error {:?}", ed.cloud.open_error),
                    );
                }
                if let Some(base) = self.cloud.mock.as_ref().map(|m| m.base.clone()) {
                    ed.cloud.api_override = Some(mock_api(&base));
                    let ctx = egui::Context::default();
                    ed.ui_action(&ctx, "save_roblox_as");
                    let saved_mode = ed.cloud_upload_mode() == Some(VersionType::Saved);
                    ed.ui_action(&ctx, "publish_as");
                    let published_mode = ed.cloud_upload_mode() == Some(VersionType::Published);
                    let accepted = ed.submit_cloud_upload(AS_UNIVERSE, AS_PLACE, VersionType::Published);
                    self.cloud_note(saved_mode && published_mode && accepted.is_ok() && ed.cloud_upload_pending(),
                        format!("As routes: Saved={saved_mode}, Published={published_mode}, submission={accepted:?}"));
                    ed.game.doc.insert(ed.game.doc.dom.root_ref(), InstanceBuilder::new("Folder").with_name("EditAfterSubmit"));
                    self.cloud_next(f, 10);
                    return false;
                }
                self.cloud_next(f, 12);
            }
            10 => {
                if ed.cloud_upload_pending() && waited < 20 {
                    return false;
                }
                let want = self.cloud.version + 4;
                let linked = ed.game.doc.cloud.as_ref().is_some_and(|o|
                    (o.universe_id, o.place_id, o.version) == (AS_UNIVERSE, AS_PLACE, want));
                let recent = ed.cloud.cache.recent().first().is_some_and(|r| r.origin.place_id == AS_PLACE);
                let uploads = self.mock_upload_requests();
                let bytes = uploads.get(6..)
                    .and_then(|requests| uploaded_bytes(requests, AS_PLACE, want, true));
                let cached = ed.cloud.cache.load(AS_PLACE, Some(want))
                    .is_some_and(|(_, cached, _)| bytes == Some(cached.as_slice()));
                let snapshot = bytes.and_then(|bytes| Document::from_place_bytes(bytes, String::new()).ok())
                    .is_some_and(|doc| {
                        doc.dom.descendants().any(|i| i.name == "CloudIds")
                            && !doc.dom.descendants().any(|i| i.name == "EditAfterSubmit")
                    });
                self.cloud_note(linked && recent && cached && snapshot && ed.game.doc.dirty()
                    && ed.cloud_upload_result() == Some(&Ok(Some(want))),
                    format!("Publish As: relinked={linked}, recent={recent}, cached upload bytes={cached}, published snapshot={snapshot}, dirty={}, result={:?}",
                        ed.game.doc.dirty(), ed.cloud_upload_result()));
                ed.close_cloud_upload();
                ed.ui_action(&egui::Context::default(), "save_roblox");
                self.cloud_next(f, 11);
            }
            11 => {
                if ed.cloud_upload_pending() && waited < 20 {
                    return false;
                }
                let uploads = self.mock_upload_requests();
                let want = self.cloud.version + 5;
                let saved = uploads.get(9..)
                    .and_then(|requests| uploaded_bytes(requests, AS_PLACE, want, false))
                    .and_then(|bytes| Document::from_place_bytes(bytes, String::new()).ok())
                    .is_some_and(|doc| doc.dom.descendants().any(|i| i.name == "EditAfterSubmit"));
                let linked = ed.game.doc.cloud.as_ref().is_some_and(|o|
                    (o.universe_id, o.place_id, o.version) == (AS_UNIVERSE, AS_PLACE, want));
                self.cloud_note(saved && linked && ed.cloud.last_saved == Some((want, VersionType::Saved))
                    && !ed.game.doc.dirty(), format!("Save after relinking: saved later edit={saved}, linked={linked}, dirty={}", ed.game.doc.dirty()));
                self.cloud_next(f, 12);
            }
            12 => {
                ed.cloud.api_override = None;
                self.cloud.mock = None;
                self.steps.push(Step {
                    step: "cloud-place",
                    passed: self.cloud.ok,
                    detail: std::mem::take(&mut self.cloud.notes).join("; "),
                });
                ed.game.account.drop_session();
                self.r4.phase = 94;
            }
            _ => {}
        }
        false
    }

    fn mock_upload_requests(&self) -> Vec<MockRequest> {
        self.cloud
            .mock
            .as_ref()
            .map(|m| {
                m.requests().into_iter().filter(|r| {
                    r.method == "PATCH"
                        || r.path.starts_with(OPERATIONS)
                        || (r.method == "POST" && r.path.contains("/versions"))
                }).collect()
            })
            .unwrap_or_default()
    }
}
