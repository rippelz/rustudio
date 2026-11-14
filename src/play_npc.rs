//! Every other Humanoid in Workspace as Studio's Humanoid too
//! (checks/humanoid-physics SPEC.md §4.1 item 8): a character that isn't the
//! player's — an NPC a server Script walks with Humanoid:Move / MoveTo, a
//! dummy that just stands — is the same dynamic assembly
//! (physics_humanoid.rs) pushed by the same controller (crate::humanoid) as
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
use crate::humanoid::{FrameIn, Humanoid, StepIn};
use crate::physics::hum_body::HumCtl;

/// Frames between looks for new / gone Humanoids under Workspace.
const SCAN_EVERY: u32 = 30;

pub(crate) struct Npc {
    humanoid: Ref,
    root: Ref,
    rig: rig::Rig,
    r6: bool,
    shape: crate::physics::HumShape,
    hum: Humanoid,
    had_floor: bool,
    floor_part: Option<Ref>,
    transforms: HashMap<Ref, Xform>,
    last_speed: f32,
    last_root: Option<Xform>,
    floor_material: u32,
    /// Humanoid.Jump / :ChangeState(Jumping) asked for a jump.
    jump: bool,
    parts: Vec<Ref>,
}

pub(super) enum NpcEvent {
    State(Ref, State, State),
    Running(Ref, f32),
    Move(Ref, &'static str, f32),
}

#[derive(Default)]
pub(crate) struct Npcs {
    by_model: HashMap<Ref, Npc>,
    scan_in: u32,
    pub(super) events: Vec<NpcEvent>,
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

impl Editor {
    /// Finds the Humanoids to drive: models under Workspace with a Humanoid
    /// and an unanchored HumanoidRootPart, alive, not the player's character.
    fn npc_scan(&mut self) {
        let Some(ws) = self.doc.workspace() else { return };
        let mine = self.play.as_ref().and_then(|s| s.character.as_ref()).map(|c| c.model);
        let mut found: Vec<(Ref, Ref, Ref)> = Vec::new();
        for i in self.doc.dom.descendants_of(ws) {
            if i.class != "Humanoid" {
                continue;
            }
            let model = i.parent();
            if Some(model) == mine || !self.doc.class_of(model).is_some_and(|c| c == "Model") {
                continue;
            }
            let Some(root) = self.doc.find_named(model, "HumanoidRootPart") else { continue };
            if !self.doc.class_of(root).is_some_and(|c| doc::is_a(&c, "BasePart")) {
                continue;
            }
            let health = match self.doc.prop(i.referent(), "Health") {
                Some(Variant::Float32(h)) => h,
                _ => 100.0,
            };
            // (Health_XML is how files store it.)
            let anchored = matches!(self.doc.prop(root, "Anchored"), Some(Variant::Bool(true)));
            if health <= 0.0 || anchored {
                continue;
            }
            found.push((model, i.referent(), root));
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
                hum: Humanoid { state: State::Freefall, bal_p: crate::humanoid::BAL_P_FREEFALL, ..Default::default() },
                had_floor: false,
                floor_part: None,
                transforms: HashMap::new(),
                last_speed: -1.0,
                last_root: None,
                floor_material: mech::MATERIAL_AIR,
                jump: false,
                parts: parts.clone(),
            };
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                let name = |r: Ref| self.doc.dom.get_by_ref(r).map(|i| i.name.clone()).unwrap_or_default();
                eprintln!("HUMDBG npc added: {} {:?} at {:?} (player's character: {:?})", name(model), model, crate::scene::xform_of(&self.doc, root).map(|x| x.pos), mine.map(name));
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
        let models: Vec<Ref> = self.play.as_ref().map(|s| s.npcs.by_model.keys().copied().collect()).unwrap_or_default();
        let gravity = self.play.as_ref().map(|s| s.physics.gravity).unwrap_or(mech::GRAVITY);
        // The ownership boundary: characters a script kept on the server
        // (SetNetworkOwner(nil) on their root) against the player's.
        {
            let kept = crate::runtime::domains::world::part::server_owned_roots(self.doc.id);
            let s = self.play.as_mut().expect("playing");
            let server: Vec<Ref> = s.npcs.by_model.iter().filter(|(_, n)| kept.contains(&n.root)).map(|(m, _)| *m).collect();
            let client = s.character.as_ref().filter(|c| !c.dead).map(|c| c.model);
            s.physics.set_ownership_split(client, &server);
        }
        for model in models {
            let Some((humanoid, root)) = self.play.as_ref().and_then(|s| s.npcs.by_model.get(&model)).map(|n| (n.humanoid, n.root)) else {
                continue;
            };
            if !self.doc.exists(humanoid) || !self.doc.exists(root) || !self.doc.workspace().is_some_and(|ws| self.doc.is_ancestor(ws, model)) {
                self.npc_remove(model);
                continue;
            }
            let f = |name: &str, d: f32| match self.doc.prop(humanoid, name) {
                Some(Variant::Float32(v)) => v,
                Some(Variant::Float64(v)) => v as f32,
                _ => d,
            };
            let b = |name: &str, d: bool| match self.doc.prop(humanoid, name) {
                Some(Variant::Bool(v)) => v,
                _ => d,
            };
            let health = f("Health", 100.0);
            if health <= 0.0 || matches!(self.doc.prop(root, "Anchored"), Some(Variant::Bool(true))) {
                self.npc_remove(model);
                continue;
            }
            // The body, at the parts' poses.
            let parts: Vec<(Ref, Xform)> = self
                .doc
                .dom
                .descendants_of(model)
                .filter(|i| doc::is_a(&i.class, "BasePart"))
                .filter_map(|i| crate::scene::xform_of(&self.doc, i.referent()).map(|x| (i.referent(), x)))
                .collect();
            let doc_root = crate::scene::xform_of(&self.doc, root);
            {
                let s = self.play.as_mut().expect("playing");
                if !s.physics.has_humanoid(model) {
                    s.physics.add_humanoid(&self.doc, model, root, &parts);
                    if let Some(n) = s.npcs.by_model.get_mut(&model) {
                        n.last_root = doc_root;
                    }
                } else {
                    s.physics.refresh_humanoid(&self.doc, model, root, &parts);
                    // A script moved the root: the whole assembly goes there.
                    let last = s.npcs.by_model.get(&model).and_then(|n| n.last_root);
                    if let (Some(last), Some(xf)) = (last, doc_root) {
                        if (xf.pos - last.pos).length() > 1e-4 || !(xf.rot - last.rot).abs_diff_eq(Mat3::ZERO, 1e-4) {
                            s.physics.teleport_humanoid(model, &xf);
                            if let Some(n) = s.npcs.by_model.get_mut(&model) {
                                n.last_root = Some(xf);
                            }
                        }
                    }
                }
            }
            // The Humanoid's values (the server's).
            let speed = f("WalkSpeed", mech::WALK_SPEED);
            let jump_velocity = if b("UseJumpPower", true) {
                f("JumpPower", mech::JUMP_POWER)
            } else {
                ((2.0 * gravity * f("JumpHeight", mech::JUMP_HEIGHT)).max(0.0).sqrt() / mech::JUMP_TAKEOFF_FACTOR).clamp(0.0, mech::JUMP_SPEED_MAX)
            };
            let max_slope = f("MaxSlopeAngle", mech::MAX_SLOPE_DEG);
            let hip_height = f("HipHeight", 0.0);
            let auto_rotate = b("AutoRotate", true);
            let sit = b("Sit", false);
            let platform = b("PlatformStand", false);
            let jump_prop = b("Jump", false);
            let move_dir = match self.doc.prop(humanoid, "MoveDirection") {
                Some(Variant::Vector3(v)) => Vec3::new(v.x, v.y, v.z),
                _ => Vec3::ZERO,
            };
            let move_dir = if move_dir.is_finite() { move_dir.clamp_length_max(1.0) } else { Vec3::ZERO };
            let legs = |d: &Document, r6: bool| {
                if r6 {
                    ["Left Leg", "Right Leg"]
                        .iter()
                        .filter_map(|n| d.find_named(model, n))
                        .filter_map(|r| crate::scene::vec3_of(d.prop(r, "Size")))
                        .map(|s| s.y)
                        .fold(0.0f32, f32::max)
                } else {
                    0.0
                }
            };
            let own: HashSet<Ref> = parts.iter().map(|p| p.0).collect();
            let mut collide: HashMap<Ref, bool> = HashMap::new();
            for (r, _) in &parts {
                collide.insert(*r, !matches!(self.doc.prop(*r, "CanCollide"), Some(Variant::Bool(false))));
            }
            let head = self.doc.find_named(model, "Head");
            let s = self.play.as_mut().expect("playing");
            let doc = &self.doc;
            let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
            let Some((root_xf, root_v, root_w)) = s.physics.humanoid_root(model) else { continue };
            if let Some(c) = s.physics.humanoid_ctl(model) {
                n.hum = c.hum.clone();
            }
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
            let state = n.hum.state;
            let rot = glam::Quat::from_mat3(&root_xf.rot);
            let floor = if senses_floor(state) {
                s.physics.humanoid_floor(doc, model, root_xf.pos, rot, &n.shape, n.had_floor, &own)
            } else {
                None
            };
            n.had_floor = floor.is_some();
            n.floor_part = floor.and_then(|f| f.1);
            let full_hip = n.shape.root_size.y * 0.5 + legs(doc, n.r6) + hip_height;
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
            let air_parts: Vec<Ref> = [head, Some(root)].into_iter().flatten().collect();
            s.physics.humanoid_air_friction(model, &air_parts, freefall);
            s.physics.sync_humanoid_collide(model, &collide);
        }
    }

    /// After the Animator: the colliders follow each character's animated
    /// pose (the server's joint transforms).
    pub(super) fn npc_pose(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        for (model, n) in s.npcs.by_model.iter_mut() {
            let mut transforms = HashMap::new();
            for j in &n.rig.joints {
                if let Some(Variant::CFrame(cf)) = self.doc.dom.get_by_ref(j.motor).and_then(|i| i.properties.get(&ustr("Transform")).cloned()) {
                    transforms.insert(j.motor, Xform::from_cframe(&cf));
                }
            }
            let local: HashMap<Ref, Xform> = n.rig.solve(ident, &transforms).into_iter().collect();
            let rest: HashMap<Ref, Xform> = n.rig.solve(ident, &HashMap::new()).into_iter().collect();
            s.physics.set_humanoid_pose(*model, &local, &rest);
            n.transforms = transforms;
        }
    }

    /// After the world steps: each character's pose in both DataModels and
    /// its FloorMaterial.
    pub(super) fn npc_post(&mut self) {
        let models: Vec<Ref> = self.play.as_ref().map(|s| s.npcs.by_model.keys().copied().collect()).unwrap_or_default();
        for model in models {
            let Some(s) = self.play.as_mut() else { return };
            let Some(n) = s.npcs.by_model.get_mut(&model) else { continue };
            if let Some(c) = s.physics.humanoid_ctl(model) {
                n.hum = c.hum.clone();
            }
            let Some((root_xf, _, _)) = s.physics.humanoid_root(model) else { continue };
            let pose = n.rig.solve(root_xf, &n.transforms);
            n.last_root = Some(root_xf);
            let state = n.hum.state;
            let humanoid = n.humanoid;
            let floor = match n.floor_part.filter(|_| senses_floor(state) && n.had_floor) {
                None => mech::MATERIAL_AIR,
                Some(p) => crate::scene::enum_u32(self.doc.prop(p, "Material")).unwrap_or(256),
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
            if root_xf.pos.y < mech::FALLEN_PARTS_HEIGHT {
                self.npc_remove(model);
            }
        }
    }
}
