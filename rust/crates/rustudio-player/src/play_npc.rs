//! Every other Humanoid in Workspace as Studio's Humanoid too
//! (checks/humanoid-physics SPEC.md §4.1 item 8): a character that isn't the
//! player's — an NPC a server Script walks with Humanoid:Move / MoveTo, a
//! dummy that just stands — is the same dynamic assembly
//! (physics_humanoid.rs) pushed by the same controller (rustudio_physics::humanoid) as
//! the player's (play_humanoid.rs), simulated by the server: its Humanoid's
//! values are the server's, its joint transforms the server Animator's, its
//! pose is written to the server's DataModel and the client's copy.
//!
//! Before, such a character was a plain rigid body with its rotation
//! locked that `game_tick_end` pushed by setting its velocity for MoveTo;
//! Humanoid:Move did nothing to it, and two characters walking into each
//! other passed at full speed (probe T11).
//!
//! Not here yet: seats, ladders and water for these Humanoids (they stand,
//! walk, jump, fall, tip over and get up).

use super::*;
use rustudio_physics::humanoid::{FrameIn, Humanoid, StepIn};
use rustudio_physics::physics::hum_body::HumCtl;
use rapier3d::rayon::prelude::*;

/// Frames between looks for new / gone Humanoids under Workspace.
const SCAN_EVERY: u32 = 30;

pub(crate) struct Npc {
    humanoid: Ref,
    root: Ref,
    rig: rig::Rig,
    r6: bool,
    shape: rustudio_physics::physics::HumShape,
    hum: Humanoid,
    had_floor: bool,
    floor_part: Option<Ref>,
    transforms: rustc_hash::FxHashMap<Ref, Xform>,
    last_speed: f32,
    last_root: Option<Xform>,
    floor_material: u32,
    /// Humanoid.Jump / :ChangeState(Jumping) asked for a jump.
    jump: bool,
    parts: Vec<Ref>,
    /// What the body is made of as the document had it when last looked at
    /// (None: look again). Kept until the change journal says something it
    /// was read from changed (`Npcs::journal`).
    view: Option<NpcView>,
    /// The rig's own pose (no Transforms), relative to the root: fixed for
    /// the rig read at the start.
    rest: Option<rustc_hash::FxHashMap<Ref, Xform>>,
    /// The body `transforms` were last put on (`npc_pose`): the same
    /// Transforms on the same body put every collider where it already is.
    posed_on: Option<rapier3d::prelude::RigidBodyHandle>,
}

/// One character's parts and the values read from them, cached between
/// frames: the walk over its descendants, the part signature hash
/// (`Physics::refresh_humanoid`) and the CanCollide map were ~200 000
/// instructions per character per frame on a 230-NPC place (the NPC test place).
struct NpcView {
    /// Every BasePart of the model (descendant order).
    parts: Vec<Ref>,
    own: std::collections::HashSet<Ref, rustc_hash::FxBuildHasher>,
    collide: HashMap<Ref, bool>,
    legs: f32,
    head: Option<Ref>,
    /// Every descendant of the model (parts and the rest), to see which
    /// character a change journal entry is about.
    members: Vec<Ref>,
    /// The body's signature as `refresh_humanoid` last found it matching
    /// the document, when everything it hashes is final (no mesh still
    /// loading or being decomposed): the check can be skipped while the
    /// body still carries it and nothing changed.
    verified: Option<u64>,
    /// The journal says something it was read from changed.
    stale: bool,
    /// Collider settings already applied to this body.
    applied_collide: Option<rapier3d::prelude::RigidBodyHandle>,
    applied_freefall: Option<(rapier3d::prelude::RigidBodyHandle, bool)>,
}

pub(super) enum NpcEvent {
    State(Ref, State, State),
    Running(Ref, f32),
    Move(Ref, &'static str, f32),
}

#[derive(Default)]
pub(crate) struct Npcs {
    // (FxHash: NPCs step in this map's order; see physics.rs)
    by_model: rustc_hash::FxHashMap<Ref, Npc>,
    scan_in: u32,
    pub(super) events: Vec<NpcEvent>,
    /// Descendant of a driven model → (the model, whether it is a BasePart).
    member_of: rustc_hash::FxHashMap<Ref, (Ref, bool)>,
    /// Every Humanoid seen under Workspace since the last full scan (some may
    /// have left since), kept from the journal's tree changes up to
    /// `cands_pos`: a scan reads these instead of walking the whole
    /// Workspace (a large test place: ~116k instances every 30 frames, ~2.5% of the
    /// Play step). None: walk it again.
    cands: Option<rustc_hash::FxHashSet<Ref>>,
    cands_pos: u64,
}

impl Npcs {
    /// The Humanoid is one this module drives.
    pub fn has_humanoid(&self, h: Ref) -> bool {
        self.by_model.values().any(|n| n.humanoid == h)
    }

    /// A script asked the Humanoid to jump.
    pub fn jump(&mut self, h: Ref) {
        if let Some(n) = self.by_model.values_mut().find(|n| n.humanoid == h) {
            n.jump = true;
        }
    }

    /// Each driven Humanoid's state name (GetState).
    pub fn states(&self) -> impl Iterator<Item = (Ref, &'static str)> + '_ {
        self.by_model.values().map(|n| (n.humanoid, n.hum.state.enum_name()))
    }
}

fn senses_floor(s: State) -> bool {
    matches!(s, State::Jumping | State::Freefall | State::Landed | State::Running | State::Climbing)
}

impl Game {
    /// Finds the Humanoids to drive: models under Workspace with a Humanoid
    /// and an unanchored HumanoidRootPart, alive, not the player's character.
    fn npc_scan(&mut self) {
        let Some(ws) = self.doc.workspace() else { return };
        let mine = self.play.as_ref().and_then(|s| s.character.as_ref()).map(|c| c.model);
        let doc = &self.doc;
        let drivable = |h: Ref| -> Option<(Ref, Ref, Ref)> {
            let model = doc.dom.get_by_ref(h)?.parent();
            if Some(model) == mine || !doc.class_of(model).is_some_and(|c| c == "Model") {
                return None;
            }
            let root = doc.find_named(model, "HumanoidRootPart")?;
            if !doc.class_of(root).is_some_and(|c| doc::is_a(&c, "BasePart")) {
                return None;
            }
            let health = match doc.prop(h, "Health") {
                Some(Variant::Float32(h)) => h,
                _ => 100.0,
            };
            // (Health_XML is how files store it.)
            let anchored = matches!(doc.prop(root, "Anchored"), Some(Variant::Bool(true)));
            if health <= 0.0 || anchored {
                return None;
            }
            Some((model, h, root))
        };
        // The known Humanoids, brought up to date with the tree changes the
        // journal (npc_journal) hasn't handed over yet, still under Workspace.
        let known = self.play.as_mut().and_then(|s| s.npcs.cands.take()).and_then(|mut cands| {
            for m in doc.marks_since(self.play.as_ref()?.npcs.cands_pos)? {
                if let doc::Mark::Tree(t) = *m {
                    cands.extend(doc.subtree(t).into_iter().filter(|r| doc.class_of(*r).is_some_and(|c| c == "Humanoid")));
                }
            }
            cands.retain(|&h| doc.class_of(h).is_some_and(|c| c == "Humanoid") && doc.is_ancestor(ws, h));
            Some(cands)
        });
        let by_model = self.play.as_ref().map(|s| &s.npcs.by_model);
        let quick = known.as_ref().and_then(|cands| {
            let found: Vec<(Ref, Ref, Ref)> = cands.iter().filter_map(|&h| drivable(h)).collect();
            // A new character's place in the scan order decides when it is
            // added: with more than one new, walk in document order.
            let new = found.iter().filter(|f| by_model.is_none_or(|m| !m.contains_key(&f.0))).count();
            (new <= 1).then_some(found)
        });
        // RUSTUDIO_NPC_SCAN_CHECK=1: the full walk as well, compared.
        static CHECK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if let (Some(q), true) = (&quick, *CHECK.get_or_init(|| std::env::var_os("RUSTUDIO_NPC_SCAN_CHECK").is_some())) {
            let full: Vec<(Ref, Ref, Ref)> =
                doc.dom.descendants_of(ws).filter(|i| i.class == "Humanoid").filter_map(|i| drivable(i.referent())).collect();
            let key = |v: &[(Ref, Ref, Ref)]| {
                let mut k: Vec<String> = v.iter().map(|f| format!("{:?}", f)).collect();
                k.sort();
                k
            };
            rustudio_core::bench::add("npc_scan_checked", 0.0, 1);
            if key(q) != key(&full) {
                rustudio_core::bench::add("npc_scan_mismatch", 0.0, 1);
                eprintln!("NPC_SCAN_CHECK mismatch: quick {} full {}", q.len(), full.len());
            }
        }
        let (found, cands) = match quick {
            Some(found) => (found, known.unwrap_or_default()),
            None => {
                let mut found: Vec<(Ref, Ref, Ref)> = Vec::new();
                let mut cands = rustc_hash::FxHashSet::default();
                for i in doc.dom.descendants_of(ws) {
                    if i.class != "Humanoid" {
                        continue;
                    }
                    cands.insert(i.referent());
                    found.extend(drivable(i.referent()));
                }
                (found, cands)
            }
        };
        if let Some(s) = self.play.as_mut() {
            s.npcs.cands = Some(cands);
        }
        let keep: HashSet<Ref> = found.iter().map(|f| f.0).collect();
        let gone: Vec<Ref> = self
            .play
            .as_ref()
            .map(|s| s.npcs.by_model.keys().copied().filter(|m| !keep.contains(m)).collect())
            .unwrap_or_default();
        for m in gone {
            self.npc_remove(m);
        }
        for (model, humanoid, root) in found {
            if self.play.as_ref().is_some_and(|s| s.npcs.by_model.contains_key(&model)) {
                continue;
            }
            let parts: Vec<Ref> = self
                .doc
                .dom
                .descendants_of(model)
                .filter(|i| doc::is_a(&i.class, "BasePart"))
                .map(|i| i.referent())
                .collect();
            // A rig whose parts are welded to something anchored, or with
            // anchored parts of its own, isn't a free character.
            if parts.iter().any(|p| matches!(self.doc.prop(*p, "Anchored"), Some(Variant::Bool(true)))) {
                continue;
            }
            let rig = rig::Rig::read(&self.doc, model);
            let r6 = self.doc.find_named(model, "UpperTorso").is_none();
            let shape = hum_shape(&self.doc, model, 0.0);
            let npc = Npc {
                humanoid,
                root,
                rig,
                r6,
                shape,
                hum: Humanoid { state: State::Freefall, bal_p: rustudio_physics::humanoid::BAL_P_FREEFALL, ..Default::default() },
                had_floor: false,
                floor_part: None,
                transforms: Default::default(),
                last_speed: -1.0,
                last_root: None,
                floor_material: mech::MATERIAL_AIR,
                jump: false,
                parts: parts.clone(),
                view: None,
                rest: None,
                posed_on: None,
            };
            if rustudio_physics::physics::hum_debug() {
                let name = |r: Ref| self.doc.dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default();
                eprintln!("HUMDBG npc added: {} {:?} at {:?} (player's character: {:?})", name(model), model, rustudio_render::scene::xform_of(&self.doc, root).map(|x| x.pos), mine.map(name));
            }
            if let Some(s) = self.play.as_mut() {
                // Out of the general assemblies: the character is one body
                // of its own (physics_humanoid.rs).
                s.physics.skip.extend(parts.iter().copied());
                s.physics.refresh(parts);
                s.npcs.by_model.insert(model, npc);
            }
        }
    }

    /// Hands a character back to the general physics (it died, went away,
    /// was anchored).
    fn npc_remove(&mut self, model: Ref) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(n) = s.npcs.by_model.remove(&model) else { return };
        if let Some(v) = &n.view {
            for m in &v.members {
                s.npcs.member_of.remove(m);
            }
        }
        let v = s.physics.humanoid_root(model).map(|(_, v, _)| v);
        s.physics.remove_humanoid(model);
        s.physics.skip.retain(|x| !n.parts.contains(x));
        let live: Vec<Ref> = n.parts.iter().copied().filter(|p| self.doc.exists(*p)).collect();
        s.physics.refresh(live.iter().copied());
        if let Some(v) = v {
            for r in &live {
                s.physics.set_velocity(*r, v);
            }
        }
    }

    /// stepHumanoid for every other Humanoid (the player's: `hum_pre`).
    pub(super) fn npc_pre(&mut self, dt: f32) {
        {
            let Some(s) = self.play.as_mut() else { return };
            s.physics.prune_humanoids(&self.doc);
            if s.npcs.scan_in == 0 {
                s.npcs.scan_in = SCAN_EVERY;
                self.npc_scan();
            } else {
                s.npcs.scan_in -= 1;
            }
        }
        self.npc_journal();
        let models: Vec<Ref> = self.play.as_ref().map(|s| s.npcs.by_model.keys().copied().collect()).unwrap_or_default();
        let gravity = self.play.as_ref().map(|s| s.physics.gravity).unwrap_or(mech::GRAVITY);
        // The ownership boundary: characters a script kept on the server
        // (SetNetworkOwner(nil) on their root) against the player's.
        {
            let kept = rustudio_engine::runtime::domains::world::part::server_owned_roots(self.doc.id);
            let s = self.play.as_mut().expect("playing");
            let server: Vec<Ref> = s.npcs.by_model.iter().filter(|(_, n)| kept.contains(&n.root)).map(|(m, _)| *m).collect();
            let client = s.character.as_ref().filter(|c| !c.dead).map(|c| c.model);
            s.physics.set_ownership_split(client, &server);
        }
        let workspace = self.doc.workspace();
        // Build bodies and apply script teleports before taking the frame's
        // floor snapshot. Every sensor reads the same collision world.
        for &model in &models {
            let Some((humanoid, root)) = self.play.as_ref().and_then(|s| s.npcs.by_model.get(&model)).map(|n| (n.humanoid, n.root)) else {
                continue;
            };
            if !self.doc.exists(humanoid) || !self.doc.exists(root) || !workspace.is_some_and(|ws| self.doc.is_ancestor(ws, model)) {
                self.npc_remove(model);
                continue;
            }
            let health = match self.doc.prop_key(humanoid, rustudio_core::ukey!("Health")) {
                Some(Variant::Float32(v)) => v,
                Some(Variant::Float64(v)) => v as f32,
                _ => 100.0,
            };
            if health <= 0.0 || matches!(self.doc.prop_key(root, rustudio_core::ukey!("Anchored")), Some(Variant::Bool(true))) {
                self.npc_remove(model);
                continue;
            }
            if self.play.as_ref().and_then(|s| s.npcs.by_model.get(&model)).is_some_and(|n| n.view.as_ref().is_none_or(|v| v.stale)) {
                if let Some(s) = self.play.as_mut() {
                    npc_view(&self.doc, &mut s.npcs, model);
                    // Parts welded on (or taken off) since: the rig carries
                    // them from their limbs (Rig::refresh_attached).
                    if let Some(n) = s.npcs.by_model.get_mut(&model) {
                        n.rig.refresh_attached(&self.doc, model);
                        n.rest = None;
                        n.posed_on = None;
                    }
                }
            }
            let doc_root = rustudio_render::scene::xform_of(&self.doc, root);
            // The body, at the parts' poses (only when it is built or its
            // parts may have changed).
            let posed = |doc: &Document, list: &[Ref]| -> Vec<(Ref, Xform)> {
                list.iter().filter_map(|r| rustudio_render::scene::xform_of(doc, *r).map(|x| (*r, x))).collect()
            };
            {
                let s = self.play.as_mut().expect("playing");
                let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
                let view = n.view.as_mut().expect("npc view");
                if !s.physics.has_humanoid(model) {
                    let parts = posed(&self.doc, &view.parts);
                    s.physics.add_humanoid(&self.doc, model, root, &parts);
                    view.verified = None;
                    n.last_root = doc_root;
                } else {
                    let sig = s.physics.humanoid_sig_of(model);
                    if view.verified.is_none() || view.verified != sig {
                        let parts = posed(&self.doc, &view.parts);
                        s.physics.refresh_humanoid(&self.doc, model, root, &parts);
                        view.verified = rustudio_physics::physics::Physics::humanoid_parts_settled(&self.doc, &view.parts)
                            .then(|| s.physics.humanoid_sig_of(model))
                            .flatten();
                    }
                    // A script moved the root: the whole assembly goes there.
                    let last = n.last_root;
                    if let (Some(last), Some(xf)) = (last, doc_root) {
                        if (xf.pos - last.pos).length() > 1e-4 || !(xf.rot - last.rot).abs_diff_eq(Mat3::ZERO, 1e-4) {
                            s.physics.teleport_humanoid(model, &xf);
                            n.last_root = Some(xf);
                        }
                    }
                }
                if let Some(ctl) = s.physics.humanoid_ctl(model) {
                    n.hum = ctl.hum.clone();
                }
                if let Some(body) = s.physics.humanoid_body_handle(model) {
                    if view.applied_collide != Some(body) {
                        s.physics.sync_humanoid_collide(model, &view.collide);
                        view.applied_collide = Some(body);
                    }
                }
            }
        }
        let sensed = {
            let _scope = rustudio_core::bench::scope("npc_floor");
            let s = self.play.as_ref().expect("playing");
            let physics = &*s.physics;
            let npcs = &s.npcs.by_model;
            let doc = &self.doc;
            let sense = |model: Ref| {
                let npc = npcs.get(&model)?;
                let view = npc.view.as_ref()?;
                let (root, v, w) = physics.humanoid_root(model)?;
                let floor = if senses_floor(npc.hum.state) {
                    physics.humanoid_floor(
                        doc, model, root.pos, glam::Quat::from_mat3(&root.rot),
                        &npc.shape, npc.had_floor, &view.own,
                    )
                } else {
                    None
                };
                Some((model, npc.humanoid, npc.root, root, v, w, floor))
            };
            match rig::character_pool(npcs.len()) {
                Some(pool) => pool.install(|| models.into_par_iter().map(sense).collect::<Vec<_>>()),
                None => models.into_iter().map(sense).collect::<Vec<_>>(),
            }
        };
        // Humanoids whose Jump latch the state machine used up this frame.
        let mut jump_spent: Vec<Ref> = Vec::new();
        for (model, humanoid, root, root_xf, root_v, root_w, floor) in sensed.into_iter().flatten() {
            let f = |name: rbx_dom_weak::Ustr, d: f32| match self.doc.prop_key(humanoid, name) {
                Some(Variant::Float32(v)) => v,
                Some(Variant::Float64(v)) => v as f32,
                _ => d,
            };
            let b = |name: rbx_dom_weak::Ustr, d: bool| match self.doc.prop_key(humanoid, name) {
                Some(Variant::Bool(v)) => v,
                _ => d,
            };
            // The Humanoid's values (the server's).
            let speed = f(rustudio_core::ukey!("WalkSpeed"), mech::WALK_SPEED);
            let jump_velocity = if b(rustudio_core::ukey!("UseJumpPower"), true) {
                f(rustudio_core::ukey!("JumpPower"), mech::JUMP_POWER)
            } else {
                ((2.0 * gravity * f(rustudio_core::ukey!("JumpHeight"), mech::JUMP_HEIGHT)).max(0.0).sqrt() / mech::JUMP_TAKEOFF_FACTOR).clamp(0.0, mech::JUMP_SPEED_MAX)
            };
            let max_slope = f(rustudio_core::ukey!("MaxSlopeAngle"), mech::MAX_SLOPE_DEG);
            let hip_height = f(rustudio_core::ukey!("HipHeight"), 0.0);
            let auto_rotate = b(rustudio_core::ukey!("AutoRotate"), true);
            let sit = b(rustudio_core::ukey!("Sit"), false);
            let platform = b(rustudio_core::ukey!("PlatformStand"), false);
            let jump_prop = b(rustudio_core::ukey!("Jump"), false);
            let move_dir = match self.doc.prop_key(humanoid, rustudio_core::ukey!("MoveDirection")) {
                Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
                _ => Vec3::ZERO,
            };
            let move_dir = if move_dir.is_finite() { move_dir.clamp_length_max(1.0) } else { Vec3::ZERO };
            let s = self.play.as_mut().expect("playing");
            let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
            let Some(view) = n.view.as_ref() else { continue };
            // Humanoid:MoveTo's walk (play_game.rs keeps the target).
            let mut dir = move_dir;
            let mut mag = move_dir.length().min(1.0);
            if Vec3::new(dir.x, 0.0, dir.z) == Vec3::ZERO {
                if let Some(w) = s.game.walk_target(humanoid) {
                    let mut to = w - root_xf.pos;
                    to.y = 0.0;
                    if to.length() >= mech::MOVE_TO_REACHED {
                        dir = to.normalize();
                        mag = 1.0;
                    }
                }
            }
            n.had_floor = floor.is_some();
            n.floor_part = floor.and_then(|f| f.1);
            let full_hip = n.shape.root_size.y * 0.5 + view.legs + hip_height;
            let jump = std::mem::take(&mut n.jump) || jump_prop;
            let inp = FrameIn {
                dt,
                move_dir: dir,
                move_mag: mag,
                walk_speed: speed,
                auto_rotate,
                camera_relative: None,
                jump,
                jump_velocity,
                sit,
                has_seat: false,
                platform_stand: platform,
                dead: false,
                anchored: false,
                floor: floor.map(|f| f.0),
                full_hip,
                cos_max_slope: max_slope.to_radians().cos(),
                ladder: false,
                in_water: false,
                root_rot: root_xf.rot,
                root_pos: root_xf.pos,
                root_v,
                root_w,
                torso_up: Some(root_xf.rot.y_axis),
            };
            let change = n.hum.frame(&inp).changed;
            // Humanoid.Jump is a latch, not a held input: Studio sets it back
            // to false as the jump starts (Jumping), and a Jump set in the air
            // is dropped on landing (Landed) without a second jump. Left true
            // it would jump again on every landing.
            if jump_prop && matches!(change, Some((_, State::Jumping | State::Landed))) {
                jump_spent.push(humanoid);
            }
            // (the arbiter answers for the new state and targets: see hum_pre)
            let level = match s.physics.humanoid_body(model) {
                Some(body) => n.hum.level(&body, s.physics.floor_static(n.floor_part), s.physics.humanoid_touching_moving(model), false),
                None => 1,
            };
            if let Some((from, to)) = change {
                n.last_speed = -1.0;
                s.npcs.events.push(NpcEvent::State(humanoid, from, to));
            }
            let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
            // Humanoid.Running / Swimming / Climbing (Studio: see hum_pre).
            let sp = match n.hum.state {
                State::Running => Some(Vec3::new(root_v.x, 0.0, root_v.z).length()),
                State::Swimming => Some(root_v.length()),
                State::Climbing => Some(root_v.y),
                _ => None,
            };
            let mut ev = None;
            if let Some(sp) = sp {
                let sp = if sp.abs() < 0.5 { 0.0 } else { sp };
                let last = n.last_speed;
                let sign = (last < 0.0 && sp > 0.0) || (last > 0.0 && sp < 0.0);
                if sp.abs() < last.abs() * 0.95 || last.abs() * 1.05 < sp.abs() || sign {
                    n.last_speed = sp;
                    ev = Some(if n.hum.state == State::Running {
                        NpcEvent::Running(humanoid, sp)
                    } else {
                        NpcEvent::Move(humanoid, n.hum.state.enum_name(), sp)
                    });
                }
            }
            let step = StepIn { floor: floor.map(|f| f.0), full_hip, cos_max_slope: inp.cos_max_slope, ladder: false, jump_velocity, accel_scale: 1.0, ceiling: false };
            let ctl = HumCtl { hum: n.hum.clone(), step, floor_part: n.floor_part, hrp: root, last: Default::default(), level, swim: None };
            let freefall = n.hum.state == State::Freefall;
            s.npcs.events.extend(ev);
            s.physics.set_humanoid_ctl(model, Some(ctl));
            if let (Some(body), Some(view)) = (
                s.physics.humanoid_body_handle(model),
                n.view.as_mut(),
            ) {
                if view.applied_freefall != Some((body, freefall)) {
                    let parts = [root, view.head.unwrap_or(root)];
                    s.physics.humanoid_air_friction(model, &parts, freefall);
                    view.applied_freefall = Some((body, freefall));
                }
            }
        }
        for humanoid in jump_spent {
            if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                inst.properties.insert(ustr("Jump"), Variant::Bool(false));
            }
            self.doc.mark_prop(humanoid, ustr("Jump"));
            self.record(RepOp::Prop(humanoid, "Jump".into()));
        }
    }

    /// What changed in the document since the last frame: the characters
    /// whose parts (or anything read from them) it touched look again.
    /// Motion (CFrame, velocities, joint transforms) and looks don't change
    /// what a body is made of; anything else on a part, a rename of any
    /// descendant, and any tree change inside a model does.
    fn npc_journal(&mut self) {
        let marks = self.doc.marks_for(doc::Reader::Npcs);
        let Some(s) = self.play.as_mut() else { return };
        let npcs = &mut s.npcs;
        // The scan's Humanoids: new ones arrive by tree changes.
        npcs.cands_pos = self.doc.journal_end();
        match (&marks, npcs.cands.as_mut()) {
            (doc::Marks::All, _) => npcs.cands = None,
            (doc::Marks::Some(list), Some(cands)) => {
                for m in list {
                    if let doc::Mark::Tree(t) = *m {
                        cands.extend(self.doc.subtree(t).into_iter().filter(|r| self.doc.class_of(*r).is_some_and(|c| c == "Humanoid")));
                    }
                }
            }
            _ => {}
        }
        let mut dirty: Vec<Ref> = Vec::new();
        // The player's character: something put in it (a part welded on, an
        // accessory) gets carried by its rig too (Rig::refresh_attached).
        let mine = s.character.as_ref().map(|c| c.model);
        let mut mine_dirty = matches!(marks, doc::Marks::All);
        match marks {
            doc::Marks::All => dirty.extend(npcs.by_model.keys().copied()),
            doc::Marks::Some(list) => {
                thread_local! {
                    static MOTION: Vec<rbx_dom_weak::Ustr> = [
                        "CFrame", "Velocity", "RotVelocity", "AssemblyLinearVelocity", "AssemblyAngularVelocity",
                        "Position", "Orientation", "Rotation", "Transform", "LocalTransparencyModifier",
                        "Transparency", "Color", "BrickColor", "Color3uint8", "Reflectance", "CastShadow", "Locked",
                    ]
                    .iter()
                    .map(|n| ustr(n))
                    .collect();
                }
                let name_key = ustr("Name");
                MOTION.with(|motion| {
                    for m in list {
                        match m {
                            doc::Mark::Prop(r, name) => {
                                let Some(&(model, part)) = npcs.member_of.get(&r) else { continue };
                                let relevant = if part { !motion.contains(&name) } else { name == name_key };
                                if relevant {
                                    dirty.push(model);
                                }
                            }
                            doc::Mark::Tree(r) => {
                                if let Some(&(model, _)) = npcs.member_of.get(&r) {
                                    dirty.push(model);
                                }
                                // Something new under a model (or moved into one).
                                let mut cur = r;
                                while let Some(i) = self.doc.dom.get_by_ref(cur) {
                                    if npcs.by_model.contains_key(&cur) {
                                        dirty.push(cur);
                                    }
                                    if Some(cur) == mine {
                                        mine_dirty = true;
                                    }
                                    cur = i.parent();
                                }
                            }
                        }
                    }
                });
            }
        }
        if mine_dirty {
            if let Some(ch) = s.character.as_mut().filter(|c| self.doc.exists(c.model)) {
                ch.rig.refresh_attached(&self.doc, ch.model);
                // A part welded on since spawn joins the character's assembly
                // (Roblox moves it as one body with it): out of the loose
                // simulation, as the spawn's parts are. Left there, it was
                // simulated on its own as well and fell away from the body (an
                // accessory's welded copy was destroyed below
                // FallenPartsDestroyHeight while its original stayed hidden).
                // A part that leaves the character goes back to the loose simulation.
                if !ch.dead {
                    let now: HashSet<Ref> = ch.rig.attached.iter().map(|a| a.handle).collect();
                    let mut changed: Vec<Ref> = Vec::new();
                    for r in &now {
                        if !s.physics.skip.contains(r) {
                            s.physics.skip.insert(*r);
                            ch.carried.insert(*r);
                            changed.push(*r);
                        }
                    }
                    let gone: Vec<Ref> = ch.carried.iter().copied().filter(|r| !now.contains(r) && !self.doc.is_ancestor(ch.model, *r)).collect();
                    for r in gone {
                        ch.carried.remove(&r);
                        s.physics.skip.remove(&r);
                        if self.doc.exists(r) {
                            changed.push(r);
                        }
                    }
                    if !changed.is_empty() {
                        s.physics.refresh(changed);
                    }
                }
            }
        }
        for m in dirty {
            // (npc_view reads it again; the members map follows there.)
            if let Some(v) = npcs.by_model.get_mut(&m).and_then(|n| n.view.as_mut()) {
                v.stale = true;
            }
        }
    }

    /// After the Animator: poses server-side AnimationController rigs and
    /// anchored Humanoid rigs (everything not driven by dynamic NPC / player
    /// Humanoid physics) so server scripts read animated Part.CFrames as in Studio.
    pub(super) fn server_pose_rigs(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let mut skip: rustc_hash::FxHashSet<Ref> = s.npcs.by_model.keys().copied().collect();
        if let Some(c) = &s.character {
            skip.insert(c.model);
        }
        s.server_joints.solve_server_rigs(&mut self.doc, &skip);
    }

    /// After the Animator: the colliders follow each character's animated
    /// pose (the server's joint transforms).
    pub(super) fn npc_pose(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        let key = ustr("Transform");
        for (model, n) in s.npcs.by_model.iter_mut() {
            let mut transforms = rustc_hash::FxHashMap::with_capacity_and_hasher(n.rig.joints.len(), Default::default());
            for j in &n.rig.joints {
                if let Some(Variant::CFrame(cf)) = self.doc.dom.get_by_ref(j.motor).and_then(|i| i.properties.get(&key)) {
                    transforms.insert(j.motor, Xform::from_cframe(cf));
                }
            }
            let body = s.physics.humanoid_body_handle(*model);
            // (Unchanged: set_humanoid_pose would find every collider and
            // the mass already where they are.)
            if body.is_some() && n.posed_on == body && transforms == n.transforms {
                continue;
            }
            let local: rustc_hash::FxHashMap<Ref, Xform> = n.rig.solve(ident, &transforms).into_iter().collect();
            let rest = n.rest.get_or_insert_with(|| n.rig.solve(ident, &rustc_hash::FxHashMap::default()).into_iter().collect());
            // (The rest mass once per body.)
            s.physics.set_humanoid_pose_with(*model, &local, (n.posed_on != body || body.is_none()).then_some(&*rest));
            n.posed_on = body;
            n.transforms = transforms;
        }
    }

    /// After the world steps: each character's pose in both DataModels and
    /// its FloorMaterial.
    pub(super) fn npc_post(&mut self) {
        let Some(s) = self.play.as_ref() else { return };
        let fallen_height = s.physics.fallen_height;
        let poses = {
            let _scope = rustudio_core::bench::scope("npc_solve_pose");
            let inputs: Vec<_> = s.npcs.by_model.iter().filter_map(|(&model, npc)| {
                let (root, _, _) = s.physics.humanoid_root(model)?;
                Some((model, root, &npc.rig, &npc.transforms))
            }).collect();
            let solve = |(model, root, rig, transforms): (Ref, Xform, &rig::Rig, &rustc_hash::FxHashMap<Ref, Xform>)| {
                (model, root, rig.solve(root, transforms))
            };
            // A rig solve is pure. Batch large crowds across a bounded pool
            // and keep document publication in the original traversal order.
            match rig::character_pool(inputs.len()) {
                Some(pool) => pool.install(|| inputs.into_par_iter().map(solve).collect::<Vec<_>>()),
                None => inputs.into_iter().map(solve).collect::<Vec<_>>(),
            }
        };
        for (model, root_xf, pose) in poses {
            let Some(s) = self.play.as_mut() else { return };
            let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
            if let Some(c) = s.physics.humanoid_ctl(model) {
                n.hum = c.hum.clone();
            }
            n.last_root = Some(root_xf);
            let state = n.hum.state;
            let humanoid = n.humanoid;
            let floor = match n.floor_part.filter(|_| senses_floor(state) && n.had_floor) {
                None => mech::MATERIAL_AIR,
                Some(p) => rustudio_render::scene::enum_u32(self.doc.prop(p, "Material")).unwrap_or(256),
            };
            let floor_changed = (floor != n.floor_material).then(|| {
                n.floor_material = floor;
                floor
            });
            if let Some(cdoc) = s.other_doc.as_mut() {
                rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
            }
            rig::write_pose(&mut self.doc, &pose, None);
            if let Some(m) = floor_changed {
                if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                    inst.properties.insert(ustr("FloorMaterial"), Variant::Enum(rbx_dom_weak::types::Enum::from_u32(m)));
                }
                self.doc.mark_prop(humanoid, ustr("FloorMaterial"));
                self.record(RepOp::Prop(humanoid, "FloorMaterial".into()));
            }
            // Fell out of the world: Studio destroys what falls past
            // FallenPartsDestroyHeight; the general physics does that once
            // the parts are its own again.
            if root_xf.pos.y < fallen_height {
                self.npc_remove(model);
            }
        }
    }
}

/// Reads a character's parts into its `NpcView`.
fn npc_view(doc: &Document, npcs: &mut Npcs, model: Ref) {
    let Some(n) = npcs.by_model.get_mut(&model) else { return };
    let mut members = Vec::new();
    let mut parts = Vec::new();
    for i in doc.dom.descendants_of(model) {
        let r = i.referent();
        let part = doc::is_a(&i.class, "BasePart");
        members.push(r);
        npcs.member_of.insert(r, (model, part));
        // (A part without a pose isn't part of the body.)
        if part && rustudio_render::scene::xform_of(doc, r).is_some() {
            parts.push(r);
        }
    }
    if let Some(old) = n.view.take() {
        for m in old.members {
            if !members.contains(&m) && npcs.member_of.get(&m).is_some_and(|(o, _)| *o == model) {
                npcs.member_of.remove(&m);
            }
        }
    }
    let legs = if n.r6 {
        ["Left Leg", "Right Leg"]
            .iter()
            .filter_map(|name| doc.find_named(model, name))
            .filter_map(|r| rustudio_render::scene::vec3_of(doc.prop(r, "Size")))
            .map(|s| s.y)
            .fold(0.0f32, f32::max)
    } else {
        0.0
    };
    let own: std::collections::HashSet<Ref, rustc_hash::FxBuildHasher> = parts.iter().copied().collect();
    let collide: HashMap<Ref, bool> = parts.iter().map(|r| (*r, !matches!(doc.prop(*r, "CanCollide"), Some(Variant::Bool(false))))).collect();
    let head = doc.find_named(model, "Head");
    n.view = Some(NpcView { parts, own, collide, legs, head, members, verified: None, stale: false, applied_collide: None, applied_freefall: None });
}


#[cfg(test)]
mod tests {
    use crate::game::Game;
    use rustudio_engine::runtime::{RunMode, Side};

    /// Humanoid.Jump on a server NPC is a latch (Studio 0.747, server NPC on
    /// a Baseplate): set once it jumps once and reads false from the jump on;
    /// set in the air it stays true until the landing, which clears it
    /// without a second jump. Left set, a game's bots hopped without end.
    #[test]
    fn humanoid_jump_is_a_latch_spent_by_the_jump() {
        let mut game = Game::new(None);
        game.start_play(RunMode::Run, &mut (), false, None);
        assert!(game.is_playing());
        let setup = r#"
            local floor = Instance.new("Part")
            floor.Anchored = true
            floor.Size = Vector3.new(100, 1, 100)
            floor.CFrame = CFrame.new(5000, 500, 5000)
            floor.Parent = workspace
            local m = Instance.new("Model")
            m.Name = "JumpLatchNpc"
            local root = Instance.new("Part")
            root.Name = "HumanoidRootPart"
            root.Size = Vector3.new(2, 2, 1)
            root.CFrame = CFrame.new(5000, 503.5, 5000)
            root.Parent = m
            local h = Instance.new("Humanoid")
            h.HipHeight = 2.5
            h.Parent = m
            m.PrimaryPart = root
            m.Parent = workspace
            _G.latch = { h = h, jumps = 0, landed = 0 }
            h.StateChanged:Connect(function(_, new)
                if new == Enum.HumanoidStateType.Jumping then _G.latch.jumps += 1 end
                if new == Enum.HumanoidStateType.Landed then _G.latch.landed += 1 end
            end)
        "#;
        assert_eq!(game.run_luau(Side::Server, setup), None);
        let ctx = egui::Context::default();
        let mut tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        // Settle on the floor, then one Jump.
        tick(&mut game, 120);
        assert_eq!(game.run_luau(Side::Server, "assert(_G.latch.jumps == 0, _G.latch.jumps); _G.latch.h.Jump = true"), None);
        tick(&mut game, 240);
        assert_eq!(
            game.run_luau(Side::Server, "local l = _G.latch; assert(l.jumps == 1, 'jumps ' .. l.jumps); assert(l.h.Jump == false, 'Jump still set')"),
            None
        );
        // Jump set while in the air: no second jump on landing, and cleared.
        assert_eq!(game.run_luau(Side::Server, "_G.latch.h.Jump = true"), None);
        let mut airborne = false;
        for _ in 0..30 {
            tick(&mut game, 1);
            if game.run_luau(Side::Server, "assert(_G.latch.h:GetState() == Enum.HumanoidStateType.Freefall)").is_none() {
                airborne = true;
                break;
            }
        }
        assert!(airborne, "the second jump never left the ground");
        assert_eq!(game.run_luau(Side::Server, "_G.latch.jumps = 0; _G.latch.landed = 0; _G.latch.h.Jump = true"), None);
        tick(&mut game, 240);
        assert_eq!(
            game.run_luau(Side::Server, "local l = _G.latch; assert(l.landed >= 1, 'never landed'); assert(l.jumps == 0, 'jumped again on landing: ' .. l.jumps); assert(l.h.Jump == false, 'Jump still set')"),
            None
        );
    }
}
