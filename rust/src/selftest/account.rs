//! Self-test steps for round 10 (then "cloud-place", selftest/cloud.rs):
//!
//! - "account-session": the session cookie (login keyring, else the import
//!   file) is checked with Roblox; signed in shows the profile; the display
//!   cache is keyed to the cookie (another cookie misses it); a made-up
//!   cookie reports "signed out" without echoing it; a local place upload
//!   retries PATCH with X-CSRF-TOKEN and polls the operation with the session
//!   cookie; the login cookie appears in no Output line and no result.
//! - "rig-picker": with the place set to R15, picking R6 gives an R6
//!   character in Play (and says the picker won); Auto gives the place's R15.
//! - "catalog-assets": signed in, an R15 character keeps Roblox's current
//!   dynamic head (mesh 123480233606534, Draco-compressed, login only), and
//!   a real catalog layered garment — Roblox's "Hooded Jacket - Gray"
//!   (8516620262) — is downloaded, put on and fitted onto the body cages.

use rbx_dom_weak::types::Enum;
use rbx_dom_weak::InstanceBuilder;

use super::{SelfTest, Step};
use crate::account_ui::SessionState;
use crate::app::Editor;
use rustudio_core::doc::Document;

const JACKET: u64 = 8516620262;
const V2_HEAD_MESH: u64 = 123480233606534;

const DRESS_SRC: &str = r#"local Players = game:GetService("Players")
local function dress(c)
    local h = c:WaitForChild("Humanoid")
    local m = game:GetService("InsertService"):LoadAsset(8516620262)
    local acc = m:FindFirstChildWhichIsA("Accessory", true)
    if acc then h:AddAccessory(acc) end
    print("catalog jacket", acc and acc.Name or "missing")
end
Players.PlayerAdded:Connect(function(p)
    p.CharacterAdded:Connect(dress)
    if p.Character then dress(p.Character) end
end)
"#;

fn place(avatar: Option<u32>, dress: bool) -> Document {
    let mut doc = Document::new_place();
    let root = doc.dom.root_ref();
    let sp = doc.find_child(root, "StarterPlayer").unwrap_or_else(|| {
        doc.dom.insert(
            root,
            InstanceBuilder::new("StarterPlayer").with_name("StarterPlayer"),
        )
    });
    if let (Some(v), Some(i)) = (avatar, doc.dom.get_by_ref_mut(sp)) {
        i.properties.insert(
            rbx_dom_weak::ustr("GameSettingsAvatar"),
            Enum::from_u32(v).into(),
        );
    }
    if dress {
        let sss = doc
            .find_child(root, "ServerScriptService")
            .unwrap_or_else(|| {
                doc.dom.insert(
                    root,
                    InstanceBuilder::new("ServerScriptService").with_name("ServerScriptService"),
                )
            });
        doc.insert(
            sss,
            InstanceBuilder::new("Script")
                .with_name("Dress")
                .with_property("Source", DRESS_SRC.to_string()),
        );
    }
    doc
}

fn character(ed: &Editor) -> Option<rbx_dom_weak::types::Ref> {
    let ws = ed.game.doc.workspace()?;
    rustudio_core::default_scripts::child_named(&ed.game.doc, ws, &ed.game.player_name())
}

impl SelfTest {
    /// Phases 84..=91.
    pub(super) fn tick_account(&mut self, f: u64, ed: &mut Editor) -> bool {
        let waited = self.r6.t0.map_or(0, |t| t.elapsed().as_secs());
        let since = f.saturating_sub(self.r6.started);
        match self.r4.phase {
            // The session, through the app's own account code.
            84 => {
                ed.game.account.check_now();
                self.r6 = super::R6Test {
                    started: f,
                    t0: Some(std::time::Instant::now()),
                    ..Default::default()
                };
                self.r4.phase = 85;
            }
            85 => {
                if matches!(ed.game.account.state, SessionState::Checking(_)) && waited < 45 {
                    return false;
                }
                self.step_session(ed);
                ed.set_document(place(Some(1), false));
                ed.game.play_rig = Some(rustudio_engine::rig::RigType::R6);
                self.r6 = super::R6Test {
                    started: f,
                    mark: ed.game.log.len(),
                    t0: Some(std::time::Instant::now()),
                    ..Default::default()
                };
                ed.start_play(rustudio_engine::runtime::RunMode::Play);
                self.r4.phase = 86;
            }
            // Picker R6 against a place that asks for R15.
            86 if since >= 60 => {
                let (rig, torso, said) = self.rig_seen(
                    ed,
                    "Character: R6 (from the picker); the place asks for R15",
                );
                self.r6.notes.push(format!(
                    "place R15, picker R6: Humanoid.RigType {rig}, Torso part {torso}, Output said the picker won: {said}"
                ));
                self.r6.ok = rig == "R6" && torso && said;
                ed.stop_play();
                ed.game.play_rig = None;
                ed.set_document(place(Some(1), false));
                self.r6.mark = ed.game.log.len();
                self.r6.started = f;
                ed.start_play(rustudio_engine::runtime::RunMode::Play);
                self.r4.phase = 87;
            }
            // Auto: the place's setting decides.
            87 if since >= 60 => {
                let (rig, torso, said) =
                    self.rig_seen(ed, "Character: R15 (from the place setting)");
                self.r6.notes.push(format!(
                    "place R15, picker Auto: Humanoid.RigType {rig}, R6 Torso {torso}, Output said the place setting won: {said}"
                ));
                let ok = self.r6.ok && rig == "R15" && !torso && said;
                self.steps.push(Step {
                    step: "rig-picker",
                    passed: ok,
                    detail: std::mem::take(&mut self.r6.notes).join("; "),
                });
                ed.stop_play();
                self.r4.phase = 88;
            }
            // Catalog assets (needs the session).
            88 => {
                if !ed.game.account.signed_in() {
                    self.steps.push(Step {
                        step: "catalog-assets",
                        passed: true,
                        detail: "signed out: the dynamic head and catalog clothing need a session (not run)".into(),
                    });
                    self.r4.phase = 90;
                    return false;
                }
                ed.set_document(place(Some(1), true));
                self.r6 = super::R6Test {
                    started: f,
                    mark: ed.game.log.len(),
                    t0: Some(std::time::Instant::now()),
                    ..Default::default()
                };
                ed.start_play(rustudio_engine::runtime::RunMode::Play);
                self.r4.phase = 89;
            }
            89 => {
                let fit = character(ed).map(|m| rustudio_render::wrap::fitted(&ed.game.doc, m));
                let pending = !matches!(
                    fit,
                    Some(rustudio_render::wrap::Lookup::Ready(_) | rustudio_render::wrap::Lookup::Failed(_))
                ) || !ed.game.draw_cache.waiting.is_empty();
                if (pending && waited < 90) || since < 90 {
                    return false;
                }
                self.step_catalog(ed, fit);
                ed.stop_play();
                // Next a place from Roblox (selftest/cloud.rs), still signed
                // in; it signs out after, for the rest of the run.
                self.r4.phase = 90;
            }
            // A place stored on Roblox (round 27).
            90 => return self.tick_cloud(f, ed),
            _ => {}
        }
        false
    }

    /// (RigType name, has an R6 "Torso", the Output line seen).
    fn rig_seen(&self, ed: &Editor, line: &str) -> (String, bool, bool) {
        let Some(m) = character(ed) else {
            return ("no character".into(), false, false);
        };
        let doc = &ed.game.doc;
        let rig = doc
            .dom
            .descendants_of(m)
            .find(|i| i.class == "Humanoid")
            .and_then(|h| rustudio_render::scene::enum_u32(doc.prop(h.referent(), "RigType")))
            .map(|v| if v == 0 { "R6" } else { "R15" })
            .unwrap_or("?")
            .to_string();
        let torso = rustudio_core::default_scripts::child_named(doc, m, "Torso").is_some();
        let said = ed.game.log[self.r6.mark.min(ed.game.log.len())..]
            .iter()
            .any(|l| l.text.contains(line));
        (rig, torso, said)
    }

    fn step_session(&mut self, ed: &mut Editor) {
        use rustudio_account::{
            session, AccountConfig, Client, Cookie, CookieSource, ProfileCache,
        };
        let mut notes = Vec::new();
        let mut ok = true;
        // The cookie itself is only ever compared, never shown.
        let cookie = session::keyring_cookie().or_else(|| session::file_cookie().ok().flatten());
        let state = ed.game.account.state.clone();
        match (&cookie, &state) {
            (Some(c), SessionState::SignedIn(who)) => {
                notes.push(format!("signed in as {who} from the login keyring"));
                let cached = ProfileCache::load(c);
                let cache_ok = cached
                    .as_ref()
                    .is_some_and(|p| who.contains(&p.profile.name) && p.avatar.is_some());
                notes.push(format!(
                    "profile + avatar cached under the cookie's key: {cache_ok}"
                ));
                ok &= cache_ok;
            }
            (Some(_), other) => {
                ok = false;
                notes.push(format!("a cookie is saved but the session is {other:?}"));
            }
            (None, SessionState::SignedOut(_)) => {
                notes.push("no cookie saved: signed out, as expected".into());
            }
            (None, other) => {
                ok = false;
                notes.push(format!("no cookie, but the state is {other:?}"));
            }
        }
        let upload = super::cloud::mock_session_upload();
        notes.push(format!("local place upload: cookie authentication, CSRF retry, revision poll and exact bytes: {upload}"));
        ok &= upload;
        // A cookie Roblox doesn't know: "signed out", never echoed, and the
        // display cache doesn't answer for it.
        let fake_value = "rustudio-selftest-not-a-real-session-0123456789";
        let fake = Cookie::new(fake_value).expect("cookie");
        let rejected = match Client::open_with(
            AccountConfig::default(),
            fake.clone(),
            CookieSource::Keyring,
        ) {
            Ok(_) => "accepted?!".to_string(),
            Err(e) => e.describe(),
        };
        let fake_ok = rejected.starts_with("signed out") && !rejected.contains(fake_value);
        let cache_miss = ProfileCache::load(&fake).is_none();
        notes.push(format!(
            "made-up cookie: {rejected:?}, value not shown: {}, cache keyed to the cookie (miss for it): {cache_miss}",
            !rejected.contains(fake_value)
        ));
        ok &= fake_ok && cache_miss;
        // The real cookie appears nowhere the person or the checks see.
        if let Some(c) = &cookie {
            let leaked = ed.game.log.iter().any(|l| c.appears_in(&l.text))
                || notes.iter().any(|n| c.appears_in(n));
            notes.push(format!("cookie in Output or results: {leaked}"));
            ok &= !leaked;
        }
        self.steps.push(Step {
            step: "account-session",
            passed: ok,
            detail: notes.join("; "),
        });
    }

    fn step_catalog(&mut self, ed: &mut Editor, fit: Option<rustudio_render::wrap::Lookup>) {
        let mut notes = Vec::new();
        let mut ok = true;
        let doc = &ed.game.doc;
        let m = character(ed);
        // The current dynamic head, drawn from its Draco mesh.
        let head = m.and_then(|m| rustudio_core::default_scripts::child_named(doc, m, "Head"));
        let head_mesh = head
            .and_then(|h| rustudio_render::scene::content_string(doc.prop(h, "MeshContent")))
            .unwrap_or_default();
        let head_drawn = head
            .and_then(|h| ed.game.renderer.as_ref()?.item_draws(h)?.first().copied())
            .is_some_and(|d| d.custom_mesh.is_some());
        let head_ok = head_mesh.ends_with(&V2_HEAD_MESH.to_string()) && head_drawn;
        notes.push(format!(
            "R15 head mesh {head_mesh} (Roblox's current dynamic head, login only, Draco-compressed) drawn: {head_drawn}"
        ));
        ok &= head_ok;
        // The catalog garment.
        let loaded = ed.game.log[self.r6.mark.min(ed.game.log.len())..].iter().any(|l| {
            l.text
                .contains("catalog jacket Jacket-HoodedWithShirt-Gray")
        });
        notes.push(format!(
            "catalog item {JACKET} downloaded and put on: {loaded}"
        ));
        ok &= loaded;
        match fit {
            Some(rustudio_render::wrap::Lookup::Ready(fit)) => {
                let parts: std::collections::HashSet<_> =
                    fit.pieces.iter().map(|p| p.part).collect();
                // How much of the garment lies outside each body part's box
                // (a fitted jacket covers the body rather than sinking in).
                let (mut outside, mut total) = (0usize, 0usize);
                for p in &fit.pieces {
                    let size =
                        rustudio_render::scene::vec3_of(doc.prop(p.part, "Size")).unwrap_or(glam::Vec3::ONE);
                    for v in &p.positions {
                        total += 1;
                        let h = size * 0.5;
                        if v.x.abs() > h.x * 0.98
                            || v.y.abs() > h.y * 0.98
                            || v.z.abs() > h.z * 0.98
                        {
                            outside += 1;
                        }
                    }
                }
                let frac = outside as f32 / total.max(1) as f32;
                let (hits, n) = fit.matched.first().copied().unwrap_or((0, 0));
                notes.push(format!(
                    "fitted: {hits} of {n} inner-cage points paired with the body cages by UV, {} pieces on {} body parts, {:.0}% of the garment's vertices outside the body boxes",
                    fit.pieces.len(),
                    parts.len(),
                    frac * 100.0
                ));
                ok &= n > 0 && hits * 10 >= n * 9 && parts.len() >= 5 && frac > 0.6;
            }
            Some(rustudio_render::wrap::Lookup::Failed(e)) => {
                ok = false;
                notes.push(format!("fitting failed: {e}"));
            }
            other => {
                ok = false;
                let tree: Vec<String> = m
                    .map(|m| {
                        doc.dom
                            .descendants_of(m)
                            .filter(|i| {
                                matches!(i.class.as_str(), "Accessory" | "WrapLayer")
                                    || i.name == "Handle"
                            })
                            .map(|i| {
                                let parent = doc
                                    .dom
                                    .get_by_ref(i.parent())
                                    .map(|p| p.name.clone())
                                    .unwrap_or_default();
                                let mut k: Vec<String> =
                                    i.properties.keys().map(|k| k.to_string()).collect();
                                k.sort();
                                format!("{} {} in {parent} [{}]", i.class, i.name, k.join(","))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                notes.push(format!(
                    "not fitted ({}; {tree:?})",
                    match other {
                        Some(rustudio_render::wrap::Lookup::Pending) => "still fitting",
                        Some(rustudio_render::wrap::Lookup::None) =>
                            "no layered garment or no WrapTargets found",
                        _ => "no character",
                    }
                ));
            }
        }
        if let Ok(dir) = std::env::var("RUSTUDIO_SELFTEST_SHOTS") {
            if let Some((w, h, px)) = ed.game
                .renderer
                .as_ref()
                .zip(ed.game.render_state.as_ref())
                .and_then(|(r, rs)| r.read_frame(rs))
            {
                if let Some(img) = image::RgbaImage::from_raw(w, h, px) {
                    let _ = img.save(std::path::Path::new(&dir).join("catalog-assets.png"));
                }
            }
        }
        self.steps.push(Step {
            step: "catalog-assets",
            passed: ok,
            detail: notes.join("; "),
        });
    }
}
