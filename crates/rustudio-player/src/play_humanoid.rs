//! Play's character as Studio's Humanoid (checks/humanoid-physics SPEC.md):
//! a dynamic assembly in the physics world (physics_humanoid.rs) pushed by
//! Studio's own controller (rustudio_physics::humanoid), in Studio's frame order
//! (§0.1, gameStep Studio):
//!
//!   PreAnimation → stepHumanoid (`hum_pre`) → the Animator → Stepped →
//!   PreSimulation → the pose (`hum_pose`) → 240 Hz world steps, each
//!   with the controller's force → the body's pose written (`hum_post`) →
//!   PostSimulation → Heartbeat.
//!
//! The client owns its character: its Humanoid's values and its movers
//! count, and the one simulation of it is this one (the server's copy only
//! receives the pose). (Round 26's kinematic capsule, which this replaced, is
//! in the history before the commit that made this the only path.)

use super::*;
use rustudio_physics::humanoid::{Floor, FrameIn, Humanoid, StepIn};
use rustudio_physics::physics::hum_body::HumCtl;

/// Studio 0.747 puts a Scriptable camera's Focus this many studs ahead of its
/// CFrame before each frame's RenderStepped (`scriptable_camera_focus`).
const SCRIPTABLE_FOCUS_AHEAD: f32 = 20.0;

/// Studio 0.747 puts a Scriptable CurrentCamera's Focus 20 studs ahead of its
/// CFrame between a frame's Heartbeat and the next RenderStepped, whatever a
/// script set it to (probe T24X_focus: Focus set 10 ahead at Heartbeat reads 20
/// ahead at the next frame's first render step; CameraRelative turns the root
/// toward it, `hum_face_camera`). Other camera types keep theirs (the camera
/// scripts set it every frame).
pub(super) fn scriptable_camera_focus(cd: &mut Document) {
    let Some(ws) = cd.workspace() else { return };
    let cam = match cd.prop(ws, "CurrentCamera") {
        Some(Variant::Ref(r)) if cd.exists(r) => r,
        _ => match cd.find_child(ws, "Camera") {
            Some(c) => c,
            None => return,
        },
    };
    if rustudio_render::scene::enum_u32(cd.prop(cam, "CameraType")) != Some(6) {
        return;
    }
    let Some(cf) = rustudio_render::scene::xform_of(cd, cam) else { return };
    let f = Xform { pos: cf.pos + cf.rot * Vec3::new(0.0, 0.0, -SCRIPTABLE_FOCUS_AHEAD), rot: cf.rot };
    if let Some(inst) = cd.dom.get_by_ref_mut(cam) {
        inst.properties.insert(ustr("Focus"), Variant::CFrame(f.to_cframe()));
    }
    cd.mark_prop(cam, ustr("Focus"));
}

/// The controller's state between frames (also kept in the physics world's
/// copy while the body exists; the world can be rebuilt under it).
pub(crate) struct HumDyn {
    pub hum: Humanoid,
    pub had_floor: bool,
    pub floor_part: Option<Ref>,
    /// Joint transforms used for this frame's pose.
    transforms: HashMap<Ref, Xform>,
    /// Swimming's vertical speed (the measured swim law).
    swim_vy: Option<f32>,
    /// A seat this character began to touch and the frames left until the
    /// server's SeatWeld is back on the client (`SEAT_TOUCH_FRAMES`).
    pub seat_wait: Option<(Ref, u8)>,
    /// The seat this character left last (it alone is refused during the
    /// re-sit cooldown).
    pub seat_left: Option<Ref>,
    /// The root's CFrame on the server and on the client as the world's step
    /// starts (`hum_roots_seen`): a side that differs at the publish (`hum_post`)
    /// had a script move it while the world stepped.
    roots_seen: Option<(Option<Xform>, Option<Xform>)>,
    /// Where a script put the root while the batch ran (`hum_script_moves`).
    script_root: Option<Xform>,
}

/// Frames from a character part entering a Seat to the Humanoid being
/// Seated: the client's pose goes to the server, the server's touch makes
/// the SeatWeld, the weld comes back. Studio 0.747 Play (local server), a
/// walking character: its leg's box enters a seat in the frame sampled at
/// 0.300 s and it is Seated at 0.350; an arm at 0.267 → 0.317 (3 frames);
/// T12: the hand passes the seat's face at 0.15, Seated at 0.2125–0.2333.
pub(crate) const SEAT_TOUCH_FRAMES: u8 = 3;

impl Default for HumDyn {
    /// A new character is in the air (it spawns above its SpawnLocation).
    fn default() -> Self {
        let hum = Humanoid { state: State::Freefall, bal_p: rustudio_physics::humanoid::BAL_P_FREEFALL, ..Default::default() };
        HumDyn { hum, had_floor: false, floor_part: None, transforms: HashMap::new(), swim_vy: None, seat_wait: None, seat_left: None, roots_seen: None, script_root: None }
    }
}

/// What a frame of the character produced for the events Lua sees.
pub(super) struct HumFrameOut {
    pub humanoid: Ref,
    pub char_part: Ref,
    pub touches: Vec<Ref>,
    pub state_change: Option<(State, State)>,
    pub seat_change: Option<SeatChange>,
    pub move_event: Option<(&'static str, f32)>,
    pub running_speed: Option<f32>,
    pub floor_changed: Option<u32>,
    /// A state change: the Humanoid sets its limbs' CanCollide false again.
    pub limbs_off: bool,
    /// This batch advances a local movement or jump request.
    pub active_input: bool,
}

impl Default for HumFrameOut {
    fn default() -> Self {
        HumFrameOut {
            humanoid: Ref::none(),
            char_part: Ref::none(),
            touches: Vec::new(),
            state_change: None,
            seat_change: None,
            move_event: None,
            running_speed: None,
            floor_changed: None,
            limbs_off: false,
            active_input: false,
        }
    }
}

/// States whose transition row tests the floor (events 11 / 12 / 13): the
/// floor sensor runs only in them.
fn senses_floor(s: State) -> bool {
    matches!(s, State::Jumping | State::Freefall | State::Landed | State::Running | State::Climbing)
}

/// States whose row has a ladder event (9 / 10): the ladder sensor runs in
/// them.
fn senses_ladder(s: State) -> bool {
    matches!(s, State::Running | State::Freefall | State::Climbing)
}

impl Game {
    /// The Lua side of the frame before the world steps (Studio's order):
    /// PreAnimation, then (after stepHumanoid, `hum_pre`) the Animator,
    /// Stepped and PreSimulation. `phase` 0 = PreAnimation (the client's
    /// RenderStepped / PreRender first), 1 = the Animator + Stepped +
    /// PreSimulation.
    pub(super) fn lua_pre_physics(&mut self, mode: RunMode, dt: f32, phase: u8) {
        let time = self.play.as_ref().map(|s| s.time).unwrap_or(0.0);
        let root = self.doc.dom.root_ref();
        let srs = self.doc.find_child(root, "RunService");
        let d = Value::Number(dt as f64);
        self.with_side(Side::Server, |rt, _, _| {
            let Some(rs) = srs else { return };
            if phase == 0 {
                rt.fire_bare(rs, "PreAnimation", vec![d.clone()]);
            } else {
                rt.queue_frame_hook(rs, "PreAnimation", dt as f64);
                rt.fire(rs, "Stepped", vec![Value::Number(time), d.clone()]);
                rt.fire(rs, "PreSimulation", vec![d.clone()]);
            }
            rt.dispatch();
        });
        if mode != RunMode::Play {
            return;
        }
        let crs = self.play.as_ref().and_then(|s| s.other_doc.as_ref()).and_then(|cd| {
            let r = cd.dom.root_ref();
            cd.find_child(r, "RunService")
        });
        self.with_side(Side::Client, |rt, _, _| {
            let Some(rs) = crs else { return };
            if phase == 0 {
                // (RenderStepped / PreRender fire at the end of the tick,
                // just before the frame is drawn: Studio draws what the
                // world was when RenderStepped ended, so the camera a script
                // placed there and the character it followed are one frame.
                // Fired before the world steps, the drawn character was one
                // frame of motion ahead of its camera: feel/README.md.)
                rt.fire_bare(rs, "PreAnimation", vec![d.clone()]);
            } else {
                rt.queue_frame_hook(rs, "PreAnimation", dt as f64);
                rt.fire(rs, "Stepped", vec![Value::Number(time), d.clone()]);
                rt.fire(rs, "PreSimulation", vec![d.clone()]);
            }
            rt.dispatch();
        });
    }

    /// The owning client's writes to its character that the world steps take:
    /// AssemblyLinearVelocity / AssemblyAngularVelocity a client script set (Place A's own
    /// jump), and the client's movers on the character (Place A's LinearVelocity and
    /// AlignOrientation) plus what else the player owns. Read in `hum_pre` and again
    /// after Stepped / PreSimulation (play.rs): Studio solves a value a script wrote
    /// there in that frame's world steps (probe T26_presim_echo: PostSimulation reads
    /// the PreSimulation write back the same frame). Read only before PreSimulation,
    /// Place A' Movement (pmove from AssemblyLinearVelocity each PreSimulation into a
    /// planar LinearVelocity) got every value one frame late: each speed showed on two
    /// frames and friction stopped the player at half rate (T26_presim_pmove: below 20
    /// u/s 0.55 s after release where Studio takes 0.27).
    pub(super) fn hum_client_writes(&mut self, model: Ref) {
        let s = self.play.as_mut().expect("playing");
        let model_c = s.map.s2c.get(&model).copied();
        let reqs: Vec<(Ref, Vec3)> = match s.client.as_mut().and_then(|c| c.world.as_mut()) {
            Some(w) => {
                let mine: Vec<(Ref, Vec3)> = w
                    .velocity_requests
                    .iter()
                    .filter(|(r, _)| s.map.c2s.get(r).is_some_and(|sr| s.physics.hums.of_part.get(sr) == Some(&model)))
                    .copied()
                    .collect();
                w.velocity_requests.retain(|(r, _)| !s.map.c2s.get(r).is_some_and(|sr| s.physics.hums.of_part.get(sr) == Some(&model)));
                mine
            }
            None => Vec::new(),
        };
        if let Some((_, v)) = reqs.last() {
            s.physics.set_humanoid_velocity(model, Some(*v), None);
        }
        // … and AssemblyAngularVelocity (T16: a Ragdoll given a spin).
        if let Some(cid) = s.other_doc.as_ref().map(|d| d.id) {
            let spins = rustudio_physics::physics::live::take_spin_sets_of(cid, |r| {
                s.map.c2s.get(&r).is_some_and(|sr| s.physics.hums.of_part.get(sr) == Some(&model))
            });
            if let Some((_, w)) = spins.last() {
                s.physics.set_humanoid_velocity(model, None, Some(*w));
            }
        }
        // The client's own movers on the character.
        if let (Some(cd), Some(mc)) = (s.other_doc.as_ref(), model_c) {
            s.physics.set_client_movers(cd, &s.map.c2s, mc);
            // What the player owns besides its character: parts a script
            // gave it, the seat it sits in.
            let mut owned = rustudio_engine::runtime::domains::world::part::player_owned_roots(self.doc.id);
            owned.extend(s.character.as_ref().and_then(|c| c.seat));
            s.physics.set_owned_movers(cd, &s.map.s2c, &s.map.c2s, &owned);
        }
    }

    /// RotationType CameraRelative (shift lock, first person): the root
    /// faces the camera's way, at once. Measured in Studio 0.747
    /// (checks/humanoid-physics/feel/traces/studio-yaw.log, studio-yaw2.log;
    /// probes T24X_*): once a frame, after RenderStepped and before PreAnimation,
    /// the root's yaw is set to the heading from Camera.CFrame's position to
    /// Camera.Focus as they are when RenderStepped has ended (a camera script that
    /// sets both, as the PlayerModule does, gets its look; a Scriptable camera's
    /// Focus is the engine's, 20 studs ahead of the CFrame the frame began with,
    /// `scriptable_camera_focus`: T24X_far / _at / _stand / _walk / _strafe lag
    /// 1.0 / 1.0 / 0.5 / 0.492 / 0.232 of the frame's camera turn, T24X_focus 0);
    /// its tilt and angular velocity are left alone; with AutoRotate off or Sit
    /// on it isn't turned; it is in Running, Freefall, PlatformStanding, Physics,
    /// with an anchored root and in first person alike. No servo is involved:
    /// the turn is a pose write. It comes before the frame is drawn, so the drawn
    /// character faces the drawn camera's way (done at the start of the next
    /// frame, every drawn frame showed the root one frame of camera turn behind,
    /// plus whatever the world steps turned it by, e.g. Freefall's air yaw toward
    /// a strafe: 0.13 rad a 60 Hz frame, probe T24_shiftlock_strafe_*).
    /// RUSTUDIO_SOLVER=studio: called right after the client's RenderStepped /
    /// PreRender (play.rs), with the Focus heading; the rapier path keeps the old
    /// call at the start of the next frame with the view camera's look.
    pub(super) fn hum_face_camera(&mut self) {
        let Some((humanoid, model)) = self.play.as_ref().and_then(|s| {
            let c = s.character.as_ref()?;
            (s.camera_relative && !c.dead && c.seat.is_none()).then_some((c.humanoid, c.model))
        }) else {
            return;
        };
        let (_, auto_rotate) = self.client_move_request(humanoid);
        let sit = matches!(self.doc.prop(humanoid, "Sit"), Some(Variant::Bool(true)))
            || matches!(self.owned_humanoid_prop(humanoid, "Sit"), Some(Variant::Bool(true)));
        if !auto_rotate || sit {
            return;
        }
        // (rapier path: the view camera's look, as before)
        let look = if rustudio_physics::solver::kind() != rustudio_physics::solver::SolverKind::Rapier {
            self.client_camera_heading().unwrap_or_else(|| self.cam.forward())
        } else {
            self.cam.forward()
        };
        if look.x.abs() + look.z.abs() < 1e-4 {
            return;
        }
        let cam_yaw = rustudio_physics::humanoid::yaw_of_look(look);
        let Some(s) = self.play.as_mut() else { return };
        let Some((root_xf, _, _)) = s.physics.humanoid_root(model) else { return };
        let facing = -root_xf.rot.z_axis;
        if facing.x.abs() + facing.z.abs() < 1e-4 {
            return;
        }
        let delta = rustudio_physics::humanoid::wrap(cam_yaw - rustudio_physics::humanoid::yaw_of_look(facing));
        if delta.abs() < 1e-7 {
            return;
        }
        let xf = Xform { pos: root_xf.pos, rot: Mat3::from_rotation_y(delta) * root_xf.rot };
        s.physics.turn_humanoid(model, delta);
        let Some(ch) = s.character.as_mut() else { return };
        ch.last_root = Some(xf);
        ch.yaw = cam_yaw;
        let mut pose = ch.rig.solve(xf, &ch.dynamic.transforms);
        pose.extend(self.tool_pose(&pose));
        let client_tool = self.client_tool_pose(&pose);
        if let Some(s) = self.play.as_mut() {
            if let Some(cdoc) = s.other_doc.as_mut() {
                rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
                rig::write_pose(cdoc, &client_tool, None);
            }
        }
        rig::write_pose(&mut self.doc, &pose, None);
    }

    /// The client's CurrentCamera: its CFrame and Focus.
    fn client_camera(&self) -> Option<(Xform, Option<Xform>)> {
        let cd = self.play.as_ref()?.other_doc.as_ref()?;
        let ws = cd.workspace()?;
        let cam = match cd.prop(ws, "CurrentCamera") {
            Some(Variant::Ref(r)) if cd.exists(r) => r,
            _ => cd.find_child(ws, "Camera")?,
        };
        let cf = rustudio_render::scene::xform_of(cd, cam)?;
        let focus = match cd.prop(cam, "Focus") {
            Some(Variant::CFrame(f)) => Some(Xform::from_cframe(&f)),
            _ => None,
        };
        Some((cf, focus))
    }

    /// The heading CameraRelative turns the root to (`hum_face_camera`): from the
    /// client camera's position to its Focus, else its look.
    fn client_camera_heading(&self) -> Option<Vec3> {
        let (cf, focus) = self.client_camera()?;
        let d = focus.map(|f| f.pos - cf.pos).filter(|d| d.x.abs() + d.z.abs() > 1e-4);
        Some(d.unwrap_or(-cf.rot.z_axis))
    }

    /// stepHumanoid: the body, the sensors, the
    /// timers, the transition table and the state's update.
    pub(super) fn hum_pre(&mut self, dt: f32) -> HumFrameOut {
        let mut out = HumFrameOut::default();
        let Some((humanoid, touch_part, model, root, rig_type)) = self
            .play
            .as_ref()
            .and_then(|s| s.character.as_ref())
            .map(|c| (c.humanoid, c.touch_part, c.model, c.rig.root, c.rig_type))
        else {
            return out;
        };
        out.humanoid = humanoid;
        out.char_part = touch_part;
        {
            let _scope = rustudio_core::bench::scope("hum_body_refresh");
            self.ensure_hum_body(model, root);
        }
        // A script moved the root (PivotTo, CFrame, either side): the body
        // goes there (Roblox moves the whole assembly).
        {
            let s = self.play.as_mut().expect("playing");
            let ch = s.character.as_mut().expect("character");
            if let Some(last) = ch.last_root {
                let client = s.map.s2c.get(&root).and_then(|c| s.other_doc.as_ref().and_then(|cd| rustudio_render::scene::xform_of(cd, *c)));
                let server = rustudio_render::scene::xform_of(&self.doc, root);
                // (Both moved: the server's write wins, as Studio's server drops the
                // owner's poses older than its CFrame until the client acks it.)
                if let Some(xf) = [server, client].into_iter().flatten().find(|xf| (xf.pos - last.pos).length() > 1e-4 || (xf.rot - last.rot).abs_diff_eq(Mat3::ZERO, 1e-4) == false) {
                    if rustudio_physics::physics::hum_debug() {
                        eprintln!("HUMDBG teleport from {:?} to {:?} (client {:?} server {:?})", last.pos, xf.pos, client.map(|c| c.pos), server.map(|c| c.pos));
                    }
                    s.physics.teleport_humanoid(model, &xf);
                    ch.last_root = Some(xf);
                }
            }
            // An Anchored root (either side) holds the body where it is.
            let anchored = |d: &doc::Document, r: Ref| matches!(d.prop(r, "Anchored"), Some(Variant::Bool(true)));
            let held = anchored(&self.doc, root)
                || s.map.s2c.get(&root).is_some_and(|c| s.other_doc.as_ref().is_some_and(|cd| anchored(cd, *c)));
            let at = if held { ch.last_root.or_else(|| rustudio_render::scene::xform_of(&self.doc, root)) } else { None };
            s.physics.anchor_humanoid(model, at);
        }
        // What the client's PreAnimation / RenderStepped wrote (again after PreSimulation:
        // `hum_client_writes`).
        self.hum_client_writes(model);
        // The client's Humanoid values (it owns the character).
        let speed = self.owned_humanoid_f32(humanoid, "WalkSpeed", mech::WALK_SPEED);
        let use_power = matches!(self.owned_humanoid_prop(humanoid, "UseJumpPower"), Some(Variant::Bool(true)));
        let gravity = self.play.as_ref().map(|s| s.physics.gravity).unwrap_or(mech::GRAVITY);
        // GetJumpVelocity.
        let jump_velocity = if use_power {
            self.owned_humanoid_f32(humanoid, "JumpPower", mech::JUMP_POWER)
        } else {
            ((2.0 * gravity * self.owned_humanoid_f32(humanoid, "JumpHeight", mech::JUMP_HEIGHT)).max(0.0).sqrt()
                / mech::JUMP_TAKEOFF_FACTOR)
                .clamp(0.0, mech::JUMP_SPEED_MAX)
        };
        let max_slope = self.owned_humanoid_f32(humanoid, "MaxSlopeAngle", mech::MAX_SLOPE_DEG);
        let (move_dir, auto_rotate) = self.client_move_request(humanoid);
        let (target, jump_req, at) = self
            .play
            .as_mut()
            .map(|s| {
                (
                    s.game.move_target,
                    std::mem::take(&mut s.game.jump),
                    s.character.as_ref().and_then(|c| c.last_root).map(|x| x.pos).unwrap_or(Vec3::ZERO),
                )
            })
            .unwrap_or((None, false, Vec3::ZERO));
        // MoveDirection (H), else Humanoid:MoveTo's (H).
        let mut dir = move_dir;
        let mut mag = move_dir.length().min(1.0);
        if Vec3::new(dir.x, 0.0, dir.z) != Vec3::ZERO {
            if let Some(s) = self.play.as_mut() {
                s.game.move_target = None;
            }
        } else if let Some(t) = target {
            let mut to = t - at;
            to.y = 0.0;
            if to.length() < mech::MOVE_TO_REACHED {
                if let Some(s) = self.play.as_mut() {
                    s.game.move_target = None;
                }
                self.with_side(Side::Server, |rt, _, _| rt.fire(humanoid, "MoveToFinished", vec![Value::Boolean(true)]));
                dir = Vec3::ZERO;
                mag = 0.0;
            } else {
                dir = to.normalize();
                mag = 1.0;
            }
        }
        // ChangeState from scripts.
        let (forced_req, states_off) = {
            let s = self.play.as_mut().expect("playing");
            (s.game.forced_state.take(), s.game.states_off.clone())
        };
        // Seats (SeatWeld made / removed by the server library, as before).
        let seated_state = self.play.as_ref().and_then(|s| s.character.as_ref()).is_some_and(|c| c.state == State::Seated);
        // The SeatPart as the frame starts (the server takes the SeatWeld
        // away 7–23 ms after a jump: measure/seat-jump.md; the jump predicate
        // still sees the seat).
        let had_seat = self.play.as_ref().and_then(|s| s.character.as_ref()).is_some_and(|c| c.seat.is_some());
        out.seat_change = self.update_seat(humanoid, jump_req && seated_state, dt);
        let seat_root = self.seat_root_xform();
        let sit = matches!(self.doc.prop(humanoid, "Sit"), Some(Variant::Bool(true)))
            || matches!(self.owned_humanoid_prop(humanoid, "Sit"), Some(Variant::Bool(true)));
        let platform = matches!(self.doc.prop(humanoid, "PlatformStand"), Some(Variant::Bool(true)))
            || matches!(self.owned_humanoid_prop(humanoid, "PlatformStand"), Some(Variant::Bool(true)));
        // The owner's Health (a LocalScript setting Health = 0 kills its
        // character, as in Studio's T14 probe).
        let health = self.owned_humanoid_f32(humanoid, "Health", 100.0).min(self.humanoid_f32(humanoid, "Health", 100.0));
        let camera_relative = {
            let s = self.play.as_ref().expect("playing");
            s.camera_relative.then(|| {
                let look = self.cam.forward();
                rustudio_physics::humanoid::yaw_of_look(look)
            })
        };
        // The character collides as its HumanoidRootPart's CollisionGroup.
        {
            let s = self.play.as_mut().expect("playing");
            let from = |d: &Document, r: Ref| match d.prop(r, "CollisionGroup") {
                Some(Variant::String(g)) if !g.is_empty() => Some(g),
                _ => None,
            };
            let g = s
                .map
                .s2c
                .get(&root)
                .and_then(|c| s.other_doc.as_ref().and_then(|cd| from(cd, *c)))
                .filter(|g| g != rustudio_core::collision_groups::DEFAULT)
                .or_else(|| from(&self.doc, root))
                .unwrap_or_else(|| rustudio_core::collision_groups::DEFAULT.to_string());
            s.physics.char_group = g;
        }
        let own: HashSet<Ref> = self
            .doc
            .dom
            .descendants_of(model)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .map(|i| i.referent())
            .collect();
        // The ladder reach scales with the character Model's scale
        // (Studio: Humanoid = its parent, a Model:), not
        // with the head. Studio 0.747, default R15 (Head.Size.Y 1.182): a
        // part's face is climbed from a root centre up to 1.1994 studs away
        // (1.2 × scale 1), a TrussPart from 1.8001 (the broad query).
        let head_y = rustudio_render::scene::f32_of(self.doc.prop(model, "Scale")).unwrap_or(1.0);
        let anchored = matches!(self.doc.prop(root, "Anchored"), Some(Variant::Bool(true)));
        let hip_height = self.owned_humanoid_f32(humanoid, "HipHeight", 0.0);
        let legs = if rig_type == RigType::R6 {
            ["Left Leg", "Right Leg"]
                .iter()
                .filter_map(|n| self.doc.find_named(model, n))
                .filter_map(|r| rustudio_render::scene::vec3_of(self.doc.prop(r, "Size")))
                .map(|s| s.y)
                .fold(0.0f32, f32::max)
        } else {
            0.0
        };
        let s = self.play.as_mut().expect("playing");
        s.physics.sync_gravity(&self.doc);
        let doc = &self.doc;
        let ch = s.character.as_mut().expect("character");
        let Some((root_xf, root_v, root_w)) = s.physics.humanoid_root(model) else { return out };
        let shape = ch.shape;
        let mut hd = std::mem::take(&mut ch.dynamic);
        if let Some(c) = s.physics.humanoid_ctl(model) {
            hd.hum = c.hum.clone();
        }
        // Sensors (Studio, per frame): the floor rays, the ladder.
        let state = hd.hum.state;
        let rot = glam::Quat::from_mat3(&root_xf.rot);
        let sensors = rustudio_core::bench::scope("hum_sensors");
        let floor: Option<(Floor, Option<Ref>)> = if senses_floor(state) {
            s.physics.humanoid_floor(doc, model, root_xf.pos, rot, &shape, hd.had_floor, &own)
        } else {
            None
        };
        hd.had_floor = floor.is_some();
        hd.floor_part = floor.and_then(|f| f.1);
        if rustudio_physics::physics::hum_debug() {
            eprintln!("HUMFRAME root={:.4},{:.4},{:.4} floor={:?}", root_xf.pos.x, root_xf.pos.y, root_xf.pos.z, floor.map(|f| (f.0.y, f.0.normal, f.0.point)));
        }
        let facing = -root_xf.rot.z_axis;
        let facing = Vec3::new(facing.x, 0.0, facing.z).normalize_or(Vec3::NEG_Z);
        if !senses_ladder(state) {
            ch.ladder_wait = 0;
        } else if ch.ladder_wait > 0 {
            ch.ladder_wait -= 1;
        } else {
            ch.ladder = s.physics.ladder_normal_scaled(
                doc,
                root_xf.pos,
                facing,
                shape.hip + shape.root_size.y * 0.5,
                (if shape.r6 { mech::CLIMB_REACH } else { mech::CLIMB_REACH_R15 }) * head_y,
                &own,
            );
            ch.ladder_wait = ladder_interval(state);
            if rustudio_physics::physics::hum_debug() && ch.ladder.is_some() {
                eprintln!("HUMLADDER root={:.3},{:.3},{:.3} facing={:?} height={:.3} reach={:.3} -> {:?}", root_xf.pos.x, root_xf.pos.y, root_xf.pos.z, facing, shape.hip + shape.root_size.y * 0.5, (if shape.r6 { mech::CLIMB_REACH } else { mech::CLIMB_REACH_R15 }) * head_y, ch.ladder);
            }
        }
        let ladder = senses_ladder(state) && ch.ladder.is_some() && !states_off.contains("Climbing");
        let in_water = !states_off.contains("Swimming") && s.physics.water_at(root_xf.pos) > mech::SWIM_ENTER_WATER;
        drop(sensors);
        // The full hip: ½ root.Size.Y, R6's longer leg, HipHeight.
        let full_hip = shape.root_size.y * 0.5 + legs + hip_height;
        let inp = FrameIn {
            dt,
            move_dir: dir,
            move_mag: mag,
            walk_speed: speed,
            auto_rotate,
            camera_relative,
            jump: jump_req,
            jump_velocity,
            sit,
            has_seat: had_seat || seat_root.is_some(),
            platform_stand: platform,
            dead: health <= 0.0,
            anchored,
            floor: floor.map(|f| f.0),
            full_hip,
            cos_max_slope: max_slope.to_radians().cos(),
            ladder,
            in_water,
            root_rot: root_xf.rot,
            root_pos: root_xf.pos,
            root_v,
            root_w,
            torso_up: Some(root_xf.rot.y_axis),
        };
        out.active_input = !anchored && health > 0.0 && (mag > 0.0 || jump_req);
        let controller = rustudio_core::bench::scope("hum_controller");
        // A forced state (ChangeState) replaces this frame's transition.
        let forced = forced_req.filter(|n| !states_off.contains(n)).and_then(|n| State::from_name(&n));
        let change = match forced {
            Some(State::Dead) => None,
            Some(st) => {
                // ChangeState swaps the state when it is called; the frame's
                // step then runs on the new state (its update, Studio).
                let forced = hd.hum.force_state(st, &inp);
                let o = hd.hum.frame(&inp);
                match (forced, o.changed) {
                    (Some((from, _)), Some((_, to))) => Some((from, to)),
                    (f, c) => f.or(c),
                }
            }
            None => {
                // A state a script disabled (SetStateEnabled false) is never entered.
                let before = hd.hum.clone();
                let o = hd.hum.frame(&inp);
                match o.changed {
                    Some((_, to)) if states_off.contains(to.enum_name()) => {
                        hd.hum = before;
                        hd.hum.timer -= dt;
                        hd.hum.air += dt;
                        None
                    }
                    c => c,
                }
            }
        };
        drop(controller);
        out.state_change = change;
        if change.is_some() {
            out.limbs_off = true;
        }
        // The timestep arbiter answers for the state this
        // frame ends up in and for this frame's targets (H..0x3c0,
        // written by Studio before the state step). Measured in Studio:
        // the frame PlatformStand begins falls 10·g·h² in 4 ticks (a level-2
        // pair: 12·g·h²), a take-off frame's ticks are single ones, and a
        // 180° turn (target 8π > 17 rad/s) is at level 1 from its first tick
        // (probe T3_turn180_cap10: yaw −1.5283 after 16 ticks; level 2 for
        // that frame gives −1.5675).
        let level = match s.physics.humanoid_body(model) {
            Some(b) => hd.hum.level(
                &b,
                s.physics.floor_static(hd.floor_part),
                s.physics.humanoid_touching_moving(model),
                camera_relative.is_some() && auto_rotate,
            ),
            None => 1,
        };
        if let Some((_, to)) = change {
            ch.state = to;
            ch.ladder_wait = 0;
            // A new state's first speed is always told (Studio fires
            // Running(0) in the frame Landed → Running: feel/traces/studio.log).
            ch.last_speed = -1.0;
        }
        // Humanoid.Running / Swimming / Climbing (Studio, called once a
        // frame from the Humanoid's step Studio after the state step,
        // with the velocity the frame started with): Running (state 8) tells
        // the horizontal speed, Swimming (4) the speed, Climbing (12) the
        // signed vertical velocity. A speed under 0.5 (.rdata Studio) is
        // 0; the signal fires when it left the last told value's ±5 % band
        // (< 0.95 × or > 1.05 ×, .rdata Studio) or changed
        // sign, and that value (H, one for all three) is then the last.
        {
            let sp = match hd.hum.state {
                State::Running => Some(Vec3::new(root_v.x, 0.0, root_v.z).length()),
                State::Swimming => Some(root_v.length()),
                State::Climbing => Some(root_v.y),
                _ => None,
            };
            if let Some(sp) = sp {
                let sp = if sp.abs() < 0.5 { 0.0 } else { sp };
                let last = ch.last_speed;
                let sign = (last < 0.0 && sp > 0.0) || (last > 0.0 && sp < 0.0);
                if sp.abs() < last.abs() * 0.95 || last.abs() * 1.05 < sp.abs() || sign {
                    ch.last_speed = sp;
                    if hd.hum.state == State::Running {
                        out.running_speed = Some(sp);
                    } else {
                        out.move_event = Some((hd.hum.state.enum_name(), sp));
                    }
                }
            }
        }
        let ceiling = hd.hum.state == State::Jumping && {
            let head = doc.find_named(model, "Head").and_then(|h| rustudio_render::scene::vec3_of(doc.prop(h, "Size"))).map(|v| v.y);
            s.physics.jump_ceiling(root_xf.pos, rot, shape.root_size, head, &own)
        };
        let step = StepIn {
            floor: floor.map(|f| f.0),
            full_hip,
            cos_max_slope: inp.cos_max_slope,
            ladder,
            jump_velocity,
            accel_scale: 1.0,
            ceiling,
        };
        // Swimming: round 26's measured swim law (SWIM_* in mech.rs, Studio
        // movement_probe water) on the body; rustudio's water has no
        // buoyancy for the swim force Studio to work against.
        let swim = (hd.hum.state == State::Swimming).then(|| {
            if hd.swim_vy.is_none() {
                hd.swim_vy = Some(root_v.y);
            }
            let flat = Vec3::new(move_dir.x, 0.0, move_dir.z);
            let swim_move = (move_dir.y.abs() * mech::SWIM_FLAT_RATIO >= flat.length() && move_dir.y.abs() > 1e-3)
                .then_some(move_dir * speed);
            let mut walk = match swim_move {
                Some(m) => Vec3::new(m.x, 0.0, m.z),
                None if flat.length() > 1e-4 => flat.normalize() * move_dir.length().max(flat.length()) * speed,
                None => Vec3::ZERO,
            };
            let float = s
                .physics
                .water_top(root_xf.pos)
                .map(|top| ((top - mech::SWIM_SURFACE_GAP - root_xf.pos.y) / mech::SWIM_SURFACE_FADE).clamp(0.0, 1.0))
                .unwrap_or(0.0);
            let drag = mech::WATER_DRAG_PER_PART * shape.parts as f32 / shape.mass.max(1.0);
            let drag_k = mech::SWIM_GAIN / (mech::SWIM_GAIN + drag);
            let jump_v = jump_velocity * mech::JUMP_TAKEOFF_FACTOR;
            let target = if jump_req {
                jump_v
            } else if let Some(m) = swim_move {
                m.y * drag_k
            } else if rig_type == RigType::R6 {
                mech::SWIM_FLOAT_SPEED_R6 * float
            } else {
                mech::SWIM_FLOAT_SPEED * float
            };
            let vy = hd.swim_vy.unwrap_or(0.0);
            let dv = (target - vy) * (1.0 - (-mech::SWIM_GAIN * dt).exp());
            let m = shape.mass.max(1.0);
            let vy = vy + if jump_req { dv } else { dv.clamp(-mech::SWIM_FORCE_MAX / m * dt, mech::SWIM_FORCE_UP_MAX / m * dt) };
            hd.swim_vy = Some(vy);
            walk *= drag_k;
            Vec3::new(walk.x, vy, walk.z)
        });
        if swim.is_none() {
            hd.swim_vy = None;
        }
        let ctl = HumCtl { hum: hd.hum.clone(), step, floor_part: hd.floor_part, hrp: root, last: Default::default(), level, swim };
        s.physics.set_humanoid_ctl(model, Some(ctl));
        // Freefall's contact friction rule: Head and RootPart.
        let air_parts: Vec<Ref> = [doc.find_named(model, "Head"), Some(root)].into_iter().flatten().collect();
        s.physics.humanoid_air_friction(model, &air_parts, hd.hum.state == State::Freefall);
        // Sitting is a weld into the seat's assembly.
        s.physics.seat_humanoid(model, ch.seat.zip(seat_root));
        let died = matches!(change, Some((_, State::Dead)));
        ch.dynamic = hd;
        if died {
            // Dead (event 0): the server's Health follows and the assembly
            // splits (BreakJointsOnDeath).
            // Set the way a script would on the server, so its HealthChanged and Died
            // fire there too (Studio: the server's Humanoid dies about two frames after
            // the owner's, probe T14srv_parts, lane DEATH).
            let set = self
                .with_side(Side::Server, |rt, _, _| {
                    rt.apply_prop(humanoid, "Health", Variant::Float32(0.0));
                    rt.dispatch();
                })
                .is_some();
            if !set {
                if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                    inst.properties.insert(ustr("Health"), Variant::Float32(0.0));
                }
                self.doc.mark_prop(humanoid, ustr("Health"));
            }
            self.record(RepOp::Prop(humanoid, "Health".into()));
            self.kill_character();
            return out;
        }
        if out.limbs_off {
            self.hum_limbs_off(model);
        }
        self.hum_sync_collide(model);
        out
    }

    /// On a state change the Humanoid sets its limbs CanCollide false (Studio
    /// probe T17: a limb a script made collide stays so until the next
    /// state change, then is false that frame; the HumanoidRootPart is never
    /// reset), on both copies.
    fn hum_limbs_off(&mut self, model: Ref) {
        let limbs: Vec<Ref> = self
            .doc
            .dom
            .descendants_of(model)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .filter(|i| !matches!(i.name.as_str(), "HumanoidRootPart" | "Head" | "Torso" | "UpperTorso" | "LowerTorso"))
            .filter(|i| rustudio_core::body::is_body_part_name(&i.name))
            .map(|i| i.referent())
            .collect();
        for r in limbs {
            if matches!(self.doc.prop(r, "CanCollide"), Some(Variant::Bool(true))) {
                if let Some(inst) = self.doc.dom.get_by_ref_mut(r) {
                    inst.properties.insert(ustr("CanCollide"), Variant::Bool(false));
                }
                self.doc.mark_prop(r, ustr("CanCollide"));
            }
            if let Some(s) = self.play.as_mut() {
                if let (Some(c), Some(cd)) = (s.map.s2c.get(&r).copied(), s.other_doc.as_mut()) {
                    if matches!(cd.prop(c, "CanCollide"), Some(Variant::Bool(true))) {
                        if let Some(inst) = cd.dom.get_by_ref_mut(c) {
                            inst.properties.insert(ustr("CanCollide"), Variant::Bool(false));
                        }
                        cd.mark_prop(c, ustr("CanCollide"));
                    }
                }
            }
        }
    }

    /// The colliders follow each part's CanCollide (the owning client's copy).
    fn hum_sync_collide(&mut self, model: Ref) {
        let Some(s) = self.play.as_mut() else { return };
        let mut collide: HashMap<Ref, bool> = HashMap::new();
        for i in self.doc.dom.descendants_of(model) {
            if !doc::is_a(&i.class, "BasePart") {
                continue;
            }
            let r = i.referent();
            let client = s.map.s2c.get(&r).and_then(|c| s.other_doc.as_ref().and_then(|cd| cd.prop(*c, "CanCollide")));
            let v = client.or_else(|| self.doc.prop(r, "CanCollide"));
            collide.insert(r, !matches!(v, Some(Variant::Bool(false))));
        }
        s.physics.sync_humanoid_collide(model, &collide);
    }

    /// Where the SeatWeld holds the root: seat · C0 · C1⁻¹.
    fn seat_root_xform(&self) -> Option<Xform> {
        let seat = self.play.as_ref()?.character.as_ref()?.seat?;
        let xf = rustudio_render::scene::xform_of(&self.doc, seat)?;
        let weld = self.doc.find_named(seat, "SeatWeld")?;
        let cf = |k: &str| match self.doc.prop(weld, k) {
            Some(Variant::CFrame(c)) => Xform::from_cframe(&c),
            _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
        };
        use rustudio_engine::ltypes::{cf_inv, cf_mul};
        Some(cf_mul(&cf_mul(&xf, &cf("C0")), &cf_inv(&cf("C1"))))
    }

    /// Builds the character's body when it has none (spawn, a rebuilt world).
    fn ensure_hum_body(&mut self, model: Ref, root: Ref) {
        let mut seen = 0usize;
        let mut parts: Vec<(Ref, Xform)> = self
            .doc
            .dom
            .descendants_of(model)
            .inspect(|_| seen += 1)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .filter_map(|i| rustudio_render::scene::xform_of(&self.doc, i.referent()).map(|x| (i.referent(), x)))
            .collect();
        // A tool equipped since the last frame still has its parts where they were
        // (Backpack, ReplicatedStorage…): built there, its handle would sit far off
        // the body and drag the centre of mass (and the turn) with it.
        let held: HashMap<Ref, Xform> = self.tool_pose(&parts).into_iter().collect();
        for (r, x) in parts.iter_mut() {
            if let Some(h) = held.get(r) {
                *x = *h;
            }
        }
        let Some(s) = self.play.as_mut() else { return };
        // Every part the body is built from is the body's alone. One that came
        // into the character after it was taken stays in the loose simulation
        // as well unless it leaves it here: a part added to the character
        // later (an AvatarSettings CollisionPart: Massless, colliding, welded
        // to the root) had the body's copy of it press on its own fixed copy
        // every tick, pushing the standing character back and forth along its
        // facing by 10+ studs.
        let loose: Vec<Ref> = parts.iter().map(|(r, _)| *r).filter(|r| !s.physics.skip.contains(r)).collect();
        if !loose.is_empty() {
            s.physics.skip.extend(loose.iter().copied());
            s.physics.refresh(loose);
        }
        // And the rig carries it from the body part its joint holds it to (an
        // accessory's AccessoryRigidConstraint, that CollisionPart's Weld), from
        // the frame it and its joint are in the model: the journal's redo
        // (play_npc.rs `npc_journal`) missed such a late CollisionPart, which
        // then stood where it arrived, a stud off the moving root.
        if let Some(ch) = s.character.as_mut().filter(|c| c.accessories_seen != seen) {
            ch.accessories_seen = seen;
            ch.rig.refresh_attached(&self.doc, model);
        }
        if s.physics.has_humanoid(model) {
            s.physics.refresh_humanoid(&self.doc, model, root, &parts);
            return;
        }
        s.physics.add_humanoid(&self.doc, model, root, &parts);
        if let Some(ch) = s.character.as_mut() {
            ch.last_root = rustudio_render::scene::xform_of(&self.doc, root);
        }
    }

    /// Just before the world steps (after Stepped / PreSimulation's writes reached
    /// it): each side's root as the step starts from. `hum_post` honours a script
    /// write made after this, while the batch is in flight.
    pub(super) fn hum_roots_seen(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(ch) = s.character.as_mut() else { return };
        let root = ch.rig.root;
        let client = s.map.s2c.get(&root).and_then(|c| s.other_doc.as_ref().and_then(|cd| rustudio_render::scene::xform_of(cd, *c)));
        let server = rustudio_render::scene::xform_of(&self.doc, root);
        ch.dynamic.roots_seen = Some((server, client));
    }

    /// A frame's batch ran on the solver's workers (scripts and the harness ran
    /// meanwhile) and is about to land: a side whose root differs from what the
    /// step started from (`hum_roots_seen`) had a script move it; `hum_post` puts
    /// the body there. (Before the batch's own poses are written: a character
    /// welded to a seat is moved by the world itself. A step on the calling thread
    /// leaves no such window and never gets here.)
    pub(super) fn hum_script_moves(&mut self) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(ch) = s.character.as_mut() else { return };
        let Some((server_was, client_was)) = ch.dynamic.roots_seen.take() else { return };
        let root = ch.rig.root;
        let client = s.map.s2c.get(&root).and_then(|c| s.other_doc.as_ref().and_then(|cd| rustudio_render::scene::xform_of(cd, *c)));
        let server = rustudio_render::scene::xform_of(&self.doc, root);
        let moved = |now: Option<Xform>, was: Option<Xform>| match (now, was) {
            (Some(n), Some(w)) => ((n.pos - w.pos).length() > 1e-4 || !(n.rot - w.rot).abs_diff_eq(Mat3::ZERO, 1e-4)).then_some(n),
            _ => None,
        };
        // (The body may have just moved to a server write the client copy hasn't
        // had yet: each side against its own value; the server's write first.)
        ch.dynamic.script_root = moved(server, server_was).or_else(|| moved(client, client_was));
    }

    /// After the Animator and Stepped: the colliders follow the animated
    /// pose (no sweep, no velocity: SPEC §1.4).
    pub(super) fn hum_pose(&mut self) {
        let transforms = self.joint_transforms();
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        // The equipped tool's parts ride the hand (the RightGrip weld): their
        // colliders follow it relative to the root like the rig's, at rest too,
        // so the assembly's mass and centre of mass count the tool where it is
        // held, not where its parts were before it was equipped.
        let (local, rest) = {
            let Some(ch) = self.play.as_ref().and_then(|s| s.character.as_ref()) else { return };
            let mut local = ch.rig.solve(ident, &transforms);
            let mut rest = ch.rig.solve(ident, &HashMap::new());
            local.extend(self.tool_pose(&local));
            rest.extend(self.tool_pose(&rest));
            (local.into_iter().collect::<HashMap<Ref, Xform>>(), rest.into_iter().collect::<HashMap<Ref, Xform>>())
        };
        let Some(s) = self.play.as_mut() else { return };
        let Some(ch) = s.character.as_mut() else { return };
        s.physics.set_humanoid_pose(ch.model, &local, &rest);
        ch.dynamic.transforms = transforms;
    }

    /// After the world steps: the body's pose written to both DataModels,
    /// FloorMaterial, the Running / Climbing / Swimming speed events.
    pub(super) fn hum_post(&mut self, out: &mut HumFrameOut) {
        let Some(s) = self.play.as_mut() else { return };
        let Some(ch) = s.character.as_mut() else { return };
        let model = ch.model;
        if let Some(c) = s.physics.humanoid_ctl(model) {
            ch.dynamic.hum = c.hum.clone();
        }
        let Some((mut root_xf, v, _w)) = s.physics.humanoid_root(model) else { return };
        // A script moved the root while the world stepped (a server Script or
        // LocalScript resumed between stepHumanoid and the publish, the
        // harness's --luau): its CFrame wins and the body goes there instead of
        // the stepped pose being written over it. Studio keeps a server CFrame /
        // PivotTo of the player's character on both sides: the server tracks the        // CFrame revision it sent, the client acks it and owner poses older than        // the ack are dropped before they reach the body. The server's write is checked first for the same reason.
        ch.dynamic.roots_seen = None;
        if let Some(xf) = ch.dynamic.script_root.take() {
            s.physics.teleport_humanoid(model, &xf);
            root_xf = xf;
        }
        let pose = ch.rig.solve(root_xf, &ch.dynamic.transforms);
        ch.last_root = Some(root_xf);
        ch.body.pos = root_xf.pos - Vec3::Y * ch.root_lift;
        ch.yaw = rustudio_physics::humanoid::yaw_of_look(-root_xf.rot.z_axis);
        ch.velocity = s.physics.humanoid_com_velocity(model).unwrap_or(v);
        ch.body.grounded = ch.dynamic.had_floor;
        let state = ch.state;
        // Humanoid.FloorMaterial: the floor's material in the floor states,
        // else Air (Seated, PlatformStanding, FallingDown…).
        let floor = match ch.dynamic.floor_part.filter(|_| senses_floor(state) && ch.dynamic.had_floor) {
            None => mech::MATERIAL_AIR,
            Some(p) if self.doc.class_of(p).is_some_and(|k| k == "Terrain") => s
                .physics
                .terrain_material_under(root_xf.pos - Vec3::Y * (ch.shape.hip + ch.shape.root_size.y * 0.5) + Vec3::Y * 0.1)
                .and_then(|m| rustudio_engine::ltypes::LEnumItem::from_name("Material", rustudio_core::terrain::material_name(m)))
                .map(|e| e.value)
                .unwrap_or(256),
            // (A part only the client has: its document's.)
            Some(p) => rustudio_render::scene::enum_u32(self.doc.prop(p, "Material"))
                .or_else(|| s.other_doc.as_ref().and_then(|cd| rustudio_render::scene::enum_u32(cd.prop(p, "Material"))))
                .unwrap_or(256),
        };
        if floor != ch.floor_material {
            ch.floor_material = floor;
            out.floor_changed = Some(floor);
        }
        let mut touches = s.physics.humanoid_touching(model);
        if let Some(f) = ch.dynamic.floor_part {
            if !touches.contains(&f) {
                touches.push(f);
            }
        }
        out.touches = touches;
        let fell = root_xf.pos.y < mech::FALLEN_PARTS_HEIGHT;
        let mut pose = pose;
        pose.extend(self.tool_pose(&pose));
        let client_tool = self.client_tool_pose(&pose);
        if let Some(s) = self.play.as_mut() {
            if let Some(cdoc) = s.other_doc.as_mut() {
                rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
                rig::write_pose(cdoc, &client_tool, None);
            }
        }
        rig::write_pose(&mut self.doc, &pose, None);
        let humanoid = out.humanoid;
        if let Some(m) = out.floor_changed {
            let v = Variant::Enum(rbx_dom_weak::types::Enum::from_u32(m));
            if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                inst.properties.insert(ustr("FloorMaterial"), v.clone());
            }
            self.doc.mark_prop(humanoid, ustr("FloorMaterial"));
            if let Some(s) = self.play.as_mut() {
                if let (Some(c), Some(cdoc)) = (s.map.s2c.get(&humanoid).copied(), s.other_doc.as_mut()) {
                    if let Some(inst) = cdoc.dom.get_by_ref_mut(c) {
                        inst.properties.insert(ustr("FloorMaterial"), v);
                    }
                    cdoc.mark_prop(c, ustr("FloorMaterial"));
                }
            }
        }
        if fell {
            if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                inst.properties.insert(ustr("Health"), Variant::Float32(0.0));
            }
            self.doc.mark_prop(humanoid, ustr("Health"));
            self.record(RepOp::Prop(humanoid, "Health".into()));
            self.kill_character();
        }
    }
}

#[cfg(test)]
mod anchored_root_tests {
    use crate::game::Game;
    use rustudio_engine::runtime::{RunMode, Side};

    /// Studio 0.747: a server PivotTo of the player's character with its
    /// HumanoidRootPart Anchored stays where it was put (5 studs up, in
    /// Freefall) while anchored; unanchored it falls back to the floor. The
    /// controller used to drop it (and walk it) regardless of Anchored.
    #[test]
    fn anchored_root_holds_the_character() {
        let mut game = Game::new(None);
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                assert(plr, "no player")
                if not plr.Character then plr:LoadCharacter() end
            "#),
            None
        );
        tick(&mut game, 120);
        let found = game.run_luau(Side::Server, r#"
            local plr = game:GetService("Players"):GetPlayers()[1]
            local ch = plr and plr.Character
            assert(ch and ch:FindFirstChild("HumanoidRootPart"), "no character")
            local root = ch.HumanoidRootPart
            _G.anch = { root = root, y0 = root.Position.Y }
            root.Anchored = true
            ch:PivotTo(root.CFrame + Vector3.new(30, 5, 0))
            _G.anch.y1 = root.Position.Y
        "#);
        assert_eq!(found, None);
        tick(&mut game, 90);
        assert_eq!(
            game.run_luau(Side::Server, "local a = _G.anch; assert(math.abs(a.root.Position.Y - a.y1) < 1e-3, 'moved to ' .. a.root.Position.Y)"),
            None
        );
        assert_eq!(game.run_luau(Side::Server, "_G.anch.root.Anchored = false"), None);
        tick(&mut game, 120);
        assert_eq!(
            game.run_luau(Side::Server, "local a = _G.anch; assert(a.root.Position.Y < a.y1 - 4, 'stayed at ' .. a.root.Position.Y)"),
            None
        );
    }
}

#[cfg(test)]
mod presim_mover_tests {
    use crate::game::Game;
    use rustudio_engine::runtime::{RunMode, Side};

    /// Studio (probe T26_presim_echo, Studio 2026-11-19): a client's LinearVelocity on
    /// its character, set in RunService.PreSimulation, is solved in that frame's world
    /// steps: PostSimulation reads the write back (6 / 12 on alternate frames, 12.2 /
    /// 6.15 with the Humanoid's floor forces). rustudio read the client's movers before
    /// PreSimulation fired, so it solved the frame before's write (12 → 6.35, 6 →
    /// 11.65), which made Place A' pmove friction act at half rate.
    #[test]
    fn presimulation_mover_write_is_solved_that_frame() {
        let mut game = Game::new(None);
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        let tick = |game: &mut Game, n: usize| {
            for _ in 0..n {
                game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
            }
        };
        tick(&mut game, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                assert(plr, "no player")
                if not plr.Character then plr:LoadCharacter() end
            "#),
            None
        );
        tick(&mut game, 120);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local RunService = game:GetService("RunService")
                local ch = game:GetService("Players").LocalPlayer.Character
                assert(ch and ch:FindFirstChild("HumanoidRootPart"), "no character")
                local root = ch.HumanoidRootPart
                ch:FindFirstChildOfClass("Humanoid").WalkSpeed = 0
                local att = Instance.new("Attachment")
                att.Parent = root
                local lv = Instance.new("LinearVelocity")
                lv.Attachment0 = att
                lv.RelativeTo = Enum.ActuatorRelativeTo.World
                lv.VelocityConstraintMode = Enum.VelocityConstraintMode.Plane
                lv.PrimaryTangentAxis = Vector3.xAxis
                lv.SecondaryTangentAxis = Vector3.zAxis
                lv.ForceLimitsEnabled = false
                lv.Parent = root
                local k, wrote = 0, 0
                _G.echo = {}
                RunService.PreSimulation:Connect(function()
                    k += 1
                    wrote = if k % 2 == 0 then 12 else 6
                    lv.PlaneVelocity = Vector2.new(0, -wrote)
                end)
                RunService.PostSimulation:Connect(function()
                    if k > 0 then
                        table.insert(_G.echo, { k, wrote, -root.AssemblyLinearVelocity.Z })
                    end
                end)
            "#),
            None
        );
        tick(&mut game, 40);
        assert_eq!(
            game.run_luau(Side::Client, r#"
                local rows = _G.echo
                assert(#rows >= 30, "only " .. #rows .. " frames")
                local near, bad = 0, {}
                for i = 2, #rows do
                    local w, prev, got = rows[i][2], rows[i - 1][2], rows[i][3]
                    if math.abs(got - w) < math.abs(got - prev) and math.abs(got - w) < 1 then
                        near += 1
                    elseif #bad < 4 then
                        table.insert(bad, string.format("frame %d wrote %g read %.3f", rows[i][1], w, got))
                    end
                end
                assert(near == #rows - 1, near .. " / " .. (#rows - 1) .. " frames read their own write; " .. table.concat(bad, ", "))
            "#),
            None
        );
    }
}

#[cfg(test)]
mod script_moves_player_character_tests {
    use crate::game::Game;
    use rustudio_engine::runtime::{RunMode, Side};

    fn playing() -> (Game, egui::Context) {
        let mut game = Game::new(None);
        game.start_play(RunMode::Play, &mut (), false, None);
        assert!(game.is_playing());
        let ctx = egui::Context::default();
        (game, ctx)
    }

    fn tick(game: &mut Game, ctx: &egui::Context, n: usize) {
        for _ in 0..n {
            game.play_tick(ctx, 1.0 / 60.0, &mut (), true, false);
        }
    }

    /// Studio 0.748 (a test place): a server `HumanoidRootPart.CFrame =`
    /// on the player's character stays where it was put on the server and the
    /// owning client. A write that landed
    /// while the world's batch was in flight was overwritten by the stepped pose
    /// (`hum_post`): the character snapped back to where it stood.
    #[test]
    fn server_cframe_of_the_player_character_sticks() {
        let (mut game, ctx) = playing();
        tick(&mut game, &ctx, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                assert(plr, "no player")
                if not plr.Character then plr:LoadCharacter() end
            "#),
            None
        );
        tick(&mut game, &ctx, 120);
        // Enough falling loose parts that the world steps on the solver's workers
        // (`Physics::step_pool`): the write below lands while that batch is in flight.
        assert_eq!(
            game.run_luau(Side::Server, r#"
                for i = 0, 99 do
                    local p = Instance.new("Part")
                    p.Size = Vector3.new(1, 1, 1)
                    p.Position = Vector3.new(-200 + (i % 10) * 8, 300, -200 + (i // 10) * 8)
                    p.Parent = workspace
                end
            "#),
            None
        );
        tick(&mut game, &ctx, 5);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local root = game:GetService("Players"):GetPlayers()[1].Character.HumanoidRootPart
                _G.tp = Vector3.new(40, root.Position.Y, 40)
                root.CFrame = CFrame.new(_G.tp)
            "#),
            None
        );
        // The server keeps it every frame; the client copy has it once the frame
        // in flight is published (an idle direct step skips a display while its
        // batch runs, so how many ticks that takes depends on the machine), and
        // keeps it from then on.
        // Ticks don't wait for the workers: on a slow, busy machine (CI's
        // macOS Intel runner) 120 back-to-back ticks can all run before the
        // batch is published, so wait in wall-clock time (up to ~10 s) and
        // then check 60 more frames.
        let mut client_since = None;
        let mut n = 0;
        while client_since.map_or(n < 2000, |s| n < s + 60) {
            n += 1;
            tick(&mut game, &ctx, 1);
            if client_since.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(
                game.run_luau(Side::Server, r#"
                    local p = game:GetService("Players"):GetPlayers()[1].Character.HumanoidRootPart.Position
                    local d = (p - _G.tp) * Vector3.new(1, 0, 1)
                    assert(d.Magnitude < 0.5, "server root at " .. tostring(p) .. ", put at " .. tostring(_G.tp))
                "#),
                None,
                "server after {n} frames"
            );
            let client = game.run_luau(Side::Client, r#"
                local p = game:GetService("Players").LocalPlayer.Character.HumanoidRootPart.Position
                local d = (p - Vector3.new(40, 0, 40)) * Vector3.new(1, 0, 1)
                assert(d.Magnitude < 0.5, "client root at " .. tostring(p))
            "#);
            match (client, client_since) {
                (None, None) => client_since = Some(n),
                (Some(e), Some(since)) => panic!("client lost it after {n} frames (had it from {since}): {e}"),
                _ => {}
            }
        }
        assert!(client_since.is_some(), "the client never had the server's CFrame");
    }

    /// Studio 0.748 (a test place's dungeon door: the server fires a RemoteEvent and
    /// the player's LocalScript sets the door's CanCollide false for 2 s): the
    /// player walks through the door, which the server still has solid. The owning
    /// client simulates its character against its own copy of the world.
    /// rustudio's character stopped against the server's door.
    #[test]
    fn client_cancollide_false_lets_the_player_through() {
        let (mut game, ctx) = playing();
        tick(&mut game, &ctx, 30);
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local plr = game:GetService("Players"):GetPlayers()[1]
                if not plr.Character then plr:LoadCharacter() end
            "#),
            None
        );
        tick(&mut game, &ctx, 120);
        // A wall 12 studs ahead (+X), solid on the server.
        assert_eq!(
            game.run_luau(Side::Server, r#"
                local root = game:GetService("Players"):GetPlayers()[1].Character.HumanoidRootPart
                local wall = Instance.new("Part")
                wall.Name = "Door"
                wall.Anchored = true
                wall.Size = Vector3.new(1, 20, 30)
                wall.CFrame = CFrame.new(root.Position + Vector3.new(12, 5, 0))
                wall.Parent = workspace
            "#),
            None
        );
        tick(&mut game, &ctx, 10);
        let walk = r#"
            local ch = game:GetService("Players").LocalPlayer.Character
            ch.Humanoid:MoveTo(workspace.Door.Position * Vector3.new(1, 0, 1) + Vector3.new(15, ch.HumanoidRootPart.Position.Y, 0))
        "#;
        let past = r#"
            local x = game:GetService("Players").LocalPlayer.Character.HumanoidRootPart.Position.X
            return x > workspace.Door.Position.X + 1
        "#;
        // Solid on both sides: it stops at the wall.
        assert_eq!(game.run_luau(Side::Client, walk), None);
        tick(&mut game, &ctx, 90);
        assert_eq!(
            game.run_luau(Side::Client, &format!("assert(not (function() {past} end)(), 'walked through a solid wall')")),
            None
        );
        // The client opens it: through.
        assert_eq!(game.run_luau(Side::Client, &format!("workspace.Door.CanCollide = false {walk}")), None);
        tick(&mut game, &ctx, 90);
        assert_eq!(
            game.run_luau(Side::Client, &format!("assert((function() {past} end)(), 'stopped at the door the client opened')")),
            None
        );
        // The server still has it solid.
        assert_eq!(game.run_luau(Side::Server, "assert(workspace.Door.CanCollide)"), None);
    }
}
