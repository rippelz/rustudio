//! Play's character as Studio's Humanoid (checks/humanoid-physics SPEC.md):
//! a dynamic assembly in the physics world (physics_humanoid.rs) pushed by
//! Studio's own controller (crate::humanoid), in Studio's frame order
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
use crate::humanoid::{Floor, FrameIn, Humanoid, StepIn};
use crate::physics::hum_body::HumCtl;

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
        let hum = Humanoid { state: State::Freefall, bal_p: crate::humanoid::BAL_P_FREEFALL, ..Default::default() };
        HumDyn { hum, had_floor: false, floor_part: None, transforms: HashMap::new(), swim_vy: None, seat_wait: None, seat_left: None }
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

impl Editor {
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

    /// RotationType CameraRelative (shift lock, first person): the root
    /// faces where the camera looks, at once. Measured in Studio 0.747
    /// (checks/humanoid-physics/feel/traces/studio-yaw.log, studio-yaw2.log):
    /// each frame, after RenderStepped and before PreAnimation, the root's
    /// yaw is exactly the camera's (the yaw the camera had when RenderStepped
    /// ended), its tilt and its angular velocity (0 while standing) are left
    /// alone; with AutoRotate off or Sit on it isn't turned; it is in
    /// Running, PlatformStanding, Physics, with an anchored root and in
    /// first person alike. No servo is involved: the turn is a pose write.
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
        let look = self.cam.forward();
        if look.x.abs() + look.z.abs() < 1e-4 {
            return;
        }
        let cam_yaw = crate::humanoid::yaw_of_look(look);
        let Some(s) = self.play.as_mut() else { return };
        let Some((root_xf, _, _)) = s.physics.humanoid_root(model) else { return };
        let facing = -root_xf.rot.z_axis;
        if facing.x.abs() + facing.z.abs() < 1e-4 {
            return;
        }
        let delta = crate::humanoid::wrap(cam_yaw - crate::humanoid::yaw_of_look(facing));
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
        if let Some(s) = self.play.as_mut() {
            if let Some(cdoc) = s.other_doc.as_mut() {
                rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
            }
        }
        rig::write_pose(&mut self.doc, &pose, None);
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
        self.ensure_hum_body(model, root);
        // A script moved the root (PivotTo, CFrame, either side): the body
        // goes there (Roblox moves the whole assembly).
        {
            let s = self.play.as_mut().expect("playing");
            let ch = s.character.as_mut().expect("character");
            if let Some(last) = ch.last_root {
                let client = s.map.s2c.get(&root).and_then(|c| s.other_doc.as_ref().and_then(|cd| crate::scene::xform_of(cd, *c)));
                let server = crate::scene::xform_of(&self.doc, root);
                if let Some(xf) = [client, server].into_iter().flatten().find(|xf| (xf.pos - last.pos).length() > 1e-4 || (xf.rot - last.rot).abs_diff_eq(Mat3::ZERO, 1e-4) == false) {
                    if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                        eprintln!("HUMDBG teleport from {:?} to {:?} (client {:?} server {:?})", last.pos, xf.pos, client.map(|c| c.pos), server.map(|c| c.pos));
                    }
                    s.physics.teleport_humanoid(model, &xf);
                    ch.last_root = Some(xf);
                }
            }
        }
        // AssemblyLinearVelocity a client script gave its character (Place A's own jump).
        {
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
                let spins = crate::physics::live::take_spin_sets_of(cid, |r| {
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
                let mut owned = crate::runtime::domains::world::part::player_owned_roots(self.doc.id);
                owned.extend(s.character.as_ref().and_then(|c| c.seat));
                s.physics.set_owned_movers(cd, &s.map.s2c, &s.map.c2s, &owned);
            }
        }
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
                crate::humanoid::yaw_of_look(look)
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
                .filter(|g| g != crate::collision_groups::DEFAULT)
                .or_else(|| from(&self.doc, root))
                .unwrap_or_else(|| crate::collision_groups::DEFAULT.to_string());
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
        let head_y = crate::scene::f32_of(self.doc.prop(model, "Scale")).unwrap_or(1.0);
        let anchored = matches!(self.doc.prop(root, "Anchored"), Some(Variant::Bool(true)));
        let hip_height = self.owned_humanoid_f32(humanoid, "HipHeight", 0.0);
        let legs = if rig_type == RigType::R6 {
            ["Left Leg", "Right Leg"]
                .iter()
                .filter_map(|n| self.doc.find_named(model, n))
                .filter_map(|r| crate::scene::vec3_of(self.doc.prop(r, "Size")))
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
        let floor: Option<(Floor, Option<Ref>)> = if senses_floor(state) {
            s.physics.humanoid_floor(doc, model, root_xf.pos, rot, &shape, hd.had_floor, &own)
        } else {
            None
        };
        hd.had_floor = floor.is_some();
        hd.floor_part = floor.and_then(|f| f.1);
        if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
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
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() && ch.ladder.is_some() {
                eprintln!("HUMLADDER root={:.3},{:.3},{:.3} facing={:?} height={:.3} reach={:.3} -> {:?}", root_xf.pos.x, root_xf.pos.y, root_xf.pos.z, facing, shape.hip + shape.root_size.y * 0.5, (if shape.r6 { mech::CLIMB_REACH } else { mech::CLIMB_REACH_R15 }) * head_y, ch.ladder);
            }
        }
        let ladder = senses_ladder(state) && ch.ladder.is_some() && !states_off.contains("Climbing");
        let in_water = !states_off.contains("Swimming") && s.physics.water_at(root_xf.pos) > mech::SWIM_ENTER_WATER;
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
            let head = doc.find_named(model, "Head").and_then(|h| crate::scene::vec3_of(doc.prop(h, "Size"))).map(|v| v.y);
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
            if let Some(inst) = self.doc.dom.get_by_ref_mut(humanoid) {
                inst.properties.insert(ustr("Health"), Variant::Float32(0.0));
            }
            self.doc.mark_prop(humanoid, ustr("Health"));
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
            .filter(|i| crate::rig::is_body_part_name(&i.name))
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
        let xf = crate::scene::xform_of(&self.doc, seat)?;
        let weld = self.doc.find_named(seat, "SeatWeld")?;
        let cf = |k: &str| match self.doc.prop(weld, k) {
            Some(Variant::CFrame(c)) => Xform::from_cframe(&c),
            _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
        };
        use crate::ltypes::{cf_inv, cf_mul};
        Some(cf_mul(&cf_mul(&xf, &cf("C0")), &cf_inv(&cf("C1"))))
    }

    /// Builds the character's body when it has none (spawn, a rebuilt world).
    fn ensure_hum_body(&mut self, model: Ref, root: Ref) {
        let Some(s) = self.play.as_mut() else { return };
        if s.physics.has_humanoid(model) {
            let parts: Vec<(Ref, Xform)> = self
                .doc
                .dom
                .descendants_of(model)
                .filter(|i| doc::is_a(&i.class, "BasePart"))
                .filter_map(|i| crate::scene::xform_of(&self.doc, i.referent()).map(|x| (i.referent(), x)))
                .collect();
            s.physics.refresh_humanoid(&self.doc, model, root, &parts);
            return;
        }
        let parts: Vec<(Ref, Xform)> = self
            .doc
            .dom
            .descendants_of(model)
            .filter(|i| doc::is_a(&i.class, "BasePart"))
            .filter_map(|i| crate::scene::xform_of(&self.doc, i.referent()).map(|x| (i.referent(), x)))
            .collect();
        s.physics.add_humanoid(&self.doc, model, root, &parts);
        if let Some(ch) = s.character.as_mut() {
            ch.last_root = crate::scene::xform_of(&self.doc, root);
        }
    }

    /// After the Animator and Stepped: the colliders follow the animated
    /// pose (no sweep, no velocity: SPEC §1.4).
    pub(super) fn hum_pose(&mut self) {
        let transforms = self.joint_transforms();
        let Some(s) = self.play.as_mut() else { return };
        let Some(ch) = s.character.as_mut() else { return };
        let ident = Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY };
        let local: HashMap<Ref, Xform> = ch.rig.solve(ident, &transforms).into_iter().collect();
        let rest: HashMap<Ref, Xform> = ch.rig.solve(ident, &HashMap::new()).into_iter().collect();
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
        let Some((root_xf, v, _w)) = s.physics.humanoid_root(model) else { return };
        let pose = ch.rig.solve(root_xf, &ch.dynamic.transforms);
        ch.last_root = Some(root_xf);
        ch.body.pos = root_xf.pos - Vec3::Y * ch.root_lift;
        ch.yaw = crate::humanoid::yaw_of_look(-root_xf.rot.z_axis);
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
                .and_then(|m| crate::ltypes::LEnumItem::from_name("Material", crate::terrain::material_name(m)))
                .map(|e| e.value)
                .unwrap_or(256),
            Some(p) => crate::scene::enum_u32(self.doc.prop(p, "Material")).unwrap_or(256),
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
        if let Some(s) = self.play.as_mut() {
            if let Some(cdoc) = s.other_doc.as_mut() {
                rig::write_pose(cdoc, &pose, Some(&s.map.s2c));
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
