//! A Humanoid's character as one dynamic assembly (checks/humanoid-physics
//! SPEC.md §1, §4.1): in Studio every rig joint of a living character is a
//! kinematic AnimationConstraint / Motor6D, so all its parts are one rigid
//! body (AssemblyRootPart = HumanoidRootPart, AssemblyMass = Σ parts).
//! Here that is one dynamic rapier body with a collider per part: the
//! collidable ones (R15 HumanoidRootPart, UpperTorso, LowerTorso, Head;
//! R6 Torso, Head) collide, the limbs are sensors that still carry their
//! mass. Each frame the colliders are put where the animated pose has the
//! parts, relative to the root — no sweep and no velocity, as Studio
//! teleports them inside the assembly — and the contact solver separates
//! whatever the new pose overlaps (the dance-clip measurements). No rotation
//! lock: Studio's Balancing torque keeps the body upright.
//!
//! The Humanoid controller (crate::humanoid) pushes the body with a force
//! and torque before every 240 Hz world step (`humanoid_forces`).
//!
//! The parts stay out of `Physics::parts` (and in `skip`): the assembly
//! builder never sees them, so a pose written to the document each frame
//! never teleports the body. Their colliders are in `by_collider` (touches,
//! other characters' floor rays).

use super::*;
use crate::humanoid::{Body, Floor, Humanoid, StepIn};
use crate::runtime::domains::world::part::part_mass;

pub struct HumPart {
    pub part: Ref,
    pub collider: rp::ColliderHandle,
    /// The part's own friction (restored after Freefall).
    friction: f32,
    /// The shape's own offset inside the part (cylinders, off-centre unions).
    offset: rp::Pose,
    /// The part's mass, centre and inertia in the collider's frame. The
    /// collider itself is massless: see `humanoid_rest_mass`.
    mp: rp::MassProperties,
}

/// The controller's per-step half and what it needs between frames.
pub struct HumCtl {
    pub hum: Humanoid,
    pub step: StepIn,
    /// The floor part under the character (the reaction force's target).
    pub floor_part: Option<Ref>,
    /// The HumanoidRootPart (the Humanoid's RootPart, Studio).
    pub hrp: Ref,
    /// The last step's force and torque (diagnostics, tests).
    pub last: crate::humanoid::Force,
    /// The timestep arbiter's level: 1 = a solve every 240 Hz
    /// tick, 2 = every 2nd.
    pub level: u8,
    /// Swimming: the velocity the measured swim law holds (no water
    /// buoyancy in this physics; play_humanoid.rs).
    pub swim: Option<Vec3>,
}

pub struct HumBody {
    pub body: rp::RigidBodyHandle,
    /// The assembly root (the body's frame): the HumanoidRootPart.
    pub root: Ref,
    pub parts: Vec<HumPart>,
    pub ctl: Option<HumCtl>,
    /// Seated: the seat part and, on a moving seat, the weld's joint.
    pub seat: Option<(Ref, Option<rp::ImpulseJointHandle>)>,
    /// The root's pose and velocities after the last world tick (a level-2
    /// assembly moves on every 2nd tick only: `MidSolve`), kept so that a
    /// teleport or a velocity write shows at once. None = the body as it is.
    shown: Option<(Xform, Vec3, Vec3)>,
    /// The assembly's level (ticks per solve, assembly byte) and the
    /// ticks left before it may rise (`ARB_DEBOUNCE`).
    lvl: u8,
    debounce: i32,
    /// This tick the level rose: see `humanoid_forces`.
    rose: bool,
    /// Between two level-2 solves (`MidSolve`): the pose and velocities to
    /// keep through the tick.
    hold: Option<(rp::Pose, Vec3, Vec3)>,
    /// The 240 Hz ticks the solve of this tick covers (0 between solves).
    span: u8,
    /// Linear and angular velocity the solve in progress began with (the
    /// restitution's reference) and the contacts that pushed during it.
    ref_v: Option<(Vec3, Vec3)>,
    hits: Vec<Hit>,
    /// Each touching pair's normal impulse in the last solve that had it
    /// (`CONTACT_KEEP`), by (own collider, other collider).
    lambda: HashMap<(rp::ColliderHandle, rp::ColliderHandle), f32>,
    /// What the body was built from (`humanoid_sig`): a part resized, given
    /// its mesh or another density since means it is built again.
    sig: u64,
    /// The centre of mass and orientation the solve in progress began with
    /// (the solver body's /, read after the solve by Studio).
    solve0: Option<(Vec3, glam::Quat)>,
    /// The body's own timestep requests from its last solve (`body_arbiter`):
    /// slot 0, the bounding box's, and slot 6, the position correction's
    /// (0 = none yet).
    arb: [u8; 2],
}

/// The timestep arbiter's position-correction request (Studio, slot 6,
/// from the factors Studio writes after each solve, Studio):
/// a solve whose motion was more than DFIntSimArbiterPositionCorrection
/// FactorThresholdThou / 1000 (live 300: StudioAppSettings) correction asks
/// for level 1.
pub const ARB_POSITION_CORRECTION: f32 = 0.3;
/// SimPositionalCorrectionScaleThou / 1000 (50): the length (and angle)
/// added to the solve's motion in the factor's denominators.
pub const ARB_CORRECTION_SCALE: f32 = 0.05;
/// SimArbiterCFLLengthScaleThou / 1000 (5000).
pub const ARB_CFL_LENGTH: f32 = 5.0;
/// TimestepArbiterBoundingBoxIntersectionThresholdThou1 / 2 (500, 250).
pub const ARB_BOX_FOUR: f32 = 0.5;
pub const ARB_BOX_TWO: f32 = 0.25;

/// Whose simulation a collider belongs to, where the two meet (SetNetworkOwner).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OwnRole {
    /// The player's character: its client simulates it.
    Client,
    /// A character a script kept on the server (SetNetworkOwner(nil)).
    Server,
    /// The stand-in the server's bodies touch for the player's character.
    GhostOfClient,
    /// The stand-in the player's character touches for a server body.
    GhostOfServer,
}

/// A body's stand-in on the other side of the ownership boundary: a
/// kinematic copy of its colliding shapes that follows it.
pub struct Ghost {
    body: rp::RigidBodyHandle,
    /// (the original's collider, the copy).
    colliders: Vec<(rp::ColliderHandle, rp::ColliderHandle)>,
    role: OwnRole,
}

/// Studio leaves a penetration of up to 0.05 studs alone and takes what is
/// beyond it away within a frame, without giving the body speed. Measured in
/// Studio 0.747: a free block put 0.5 (0.1, 0.4) studs into an anchored floor
/// is at −0.0628 (−0.0500, −0.0501) after the first frame and stays at
/// −0.0500. Inside the band a contact only stops the approach: a block put
/// 0.02 (0.04) in with 0.5 studs/s downward stays there, sinking at 0.0033
/// studs/s (the solver's compliance, CollisionVelStageMinComplianceE7 =
/// 10010: 0.001 of the 3.27 studs/s a 60 Hz step of gravity asks the contact
/// to take; not ported). A character whose animated head is 0.4845
/// (0.7845) into a wall is out by 0.4048 (0.6593) after the frame's two
/// solves, i.e. each solve takes about 0.7 of the excess (probe T8). (Tick
/// by tick, ticks/README.md, one solve takes 0.0315 of a 0.0353 excess:
/// the position stage's row keeps PrimalCollisionPenetrationResolution
/// Damping / 100 = 0.1 of slop + separation, Studio. With 0.9 here
/// the wall lanes of tools/fidelity lose 5 points: open, PROGRESS.md.)
pub const ALLOWED_PENETRATION: f32 = 0.05;
pub const DEPENETRATION_PER_SOLVE: f32 = 0.7;
/// The same numbers in Studio's contact rows (CollisionConnector vtable
/// Studio slot 10, Studio, the LDL-PGS solver's): the position
/// term is −cfg[] × (slop + separation), cfg[] = 0.7 and the slop
/// = clamp((|g| + 0.1) × cfg[] / |ω|², cfg[],
/// cfg[]) = clamp((g + 0.1) × 0.05 / ω², 1e-4, 0.05), ω the faster
/// body's spin; the solver configuration's defaults are set in Studio.
pub const SLOP_MIN: f32 = 1.0e-4;
pub const SLOP_SPIN: f32 = 0.05;
pub const SLOP_GRAVITY_ADD: f32 = 0.1;
/// Restitution in that row: its velocity term is e × vₙ(reference) + vₙ, the
/// reference being the bodies' velocity the step began with, counted only
/// when it closes at cfg[] = 1 stud/s or more — on every step, not
/// only when the contact is new. e = Σ elasticity·weight / Σ weight
/// (Studio, written to the connector by Studio). The two
/// velocities are the body state's two sets (loader Studio: =
/// v + dt·F/m, = v). Measured on a free block (Studio 0.747): falling
/// into the floor at 9.81 it leaves at 4.89, at 4.92 at 2.45; arriving at
/// 0.82 it stops. A character pressed into a wall by a level-2 solve's
/// push arrives at more than 1 stud/s after every flight, so its bouncing
/// goes on in bursts (T19; Studio's own runs differ from each other).
pub const RESTITUTION_MIN_SPEED: f32 = 1.0;
/// How far apart two shapes may come before a contact between them forgets
/// its impulse and counts as new again. Bracketed by measurement, the
/// connector's own rule not read: a character 0.02 studs off the block it
/// bounced from did not bounce on return (T10), a block 0.057 above the
/// floor it bounced from did (in at 9.81, out at 4.89, in at 4.92, out at
/// 2.45). 0.05 is the solver configuration's cfg[].
pub const CONTACT_KEEP: f32 = 0.05;

/// A collider's friction and elasticity weights, kept in its user data
/// (0 = the default 1).
pub(super) fn weights_data(friction_weight: f32, elasticity_weight: f32) -> u128 {
    (friction_weight.to_bits() as u128) << 32 | elasticity_weight.to_bits() as u128
}

fn weights_of(c: &rp::Collider) -> (f32, f32) {
    let d = c.user_data;
    if d == 0 {
        (1.0, 1.0)
    } else {
        (f32::from_bits((d >> 32) as u32), f32::from_bits(d as u32))
    }
}

/// Studio's contact material of two parts: friction and
/// elasticity, each the weighted mean of the two parts' values.
fn contact_material(a: &rp::Collider, b: &rp::Collider) -> (f32, f32) {
    let ((fa, ea), (fb, eb)) = (weights_of(a), weights_of(b));
    let mix = |x: f32, wx: f32, y: f32, wy: f32| if wx + wy > 1.0e-6 { (x * wx + y * wy) / (wx + wy) } else { 0.0 };
    (mix(a.friction(), fa, b.friction(), fb), mix(a.restitution(), ea, b.restitution(), eb))
}

/// A contact that pushed on a character during its current solve.
#[derive(Clone, Copy)]
struct Hit {
    /// The character's collider and the other one.
    own: rp::ColliderHandle,
    other: rp::ColliderHandle,
    /// Unit normal toward the character, and the point (world).
    n: Vec3,
    p: Vec3,
}

/// Contact filtering across the ownership boundary (see `set_ownership_split`),
/// and the allowed penetration of a character's contacts.
pub(super) struct OwnerHooks<'a>(pub &'a HashMap<rp::ColliderHandle, OwnRole>, pub f32, pub &'a MidSolve);

/// Characters between two solves of the timestep arbiter's level 2. Studio
/// solves such an assembly on every 2nd 240 Hz tick, over 2h (Studio
/// with dt = level × h; the body states are loaded once per solve by
/// Studio: velocity + dt·F/m, and the velocity the solve began with).
/// On the tick in between the assembly is not in the solver: it neither
/// moves nor feels a contact. What it touches is solved with it, at its
/// rate. Measured (T10, a character pushing a loose 151-mass block): the
/// pair gains 0.3250 studs/s a frame while the push is at its limit, which
/// is two solves' pushes of m × 839 × 2h against one frame of the block's
/// friction, shared by both masses; a block solved on the ticks in between
/// as well, against a character it cannot move, would get its friction
/// loss back for nothing (rustudio did: 0.395 a frame). Here the world
/// steps every tick, so on the tick in between the character's contacts
/// are simply not solved: the other body goes on alone for that tick.
#[derive(Default)]
pub struct MidSolve {
    /// The colliders of those characters.
    colliders: HashSet<rp::ColliderHandle>,
}

impl rp::PhysicsHooks for OwnerHooks<'_> {
    fn filter_contact_pair(&self, c: &rp::PairFilterContext) -> Option<rp::SolverFlags> {
        use OwnRole::*;
        let (a, b) = (self.0.get(&c.collider1).copied(), self.0.get(&c.collider2).copied());
        match (a, b) {
            // Each side meets the other's stand-in, never the body itself.
            (Some(Client), Some(Server)) | (Some(Server), Some(Client)) => None,
            (Some(Client), Some(GhostOfServer)) | (Some(GhostOfServer), Some(Client)) => Some(rp::SolverFlags::COMPUTE_RIGID_IMPULSES),
            (Some(Server), Some(GhostOfClient)) | (Some(GhostOfClient), Some(Server)) => Some(rp::SolverFlags::COMPUTE_RIGID_IMPULSES),
            (Some(GhostOfClient | GhostOfServer), _) | (_, Some(GhostOfClient | GhostOfServer)) => None,
            _ => Some(rp::SolverFlags::COMPUTE_RIGID_IMPULSES),
        }
    }

    /// (character colliders only: ActiveHooks::MODIFY_SOLVER_CONTACTS) The
    /// solver's position correction sees only the penetration beyond the
    /// allowed band; inside it the contact just stops approach.
    fn modify_solver_contacts(&self, c: &mut rp::ContactModificationContext) {
        let material = match (c.colliders.get(c.collider1), c.colliders.get(c.collider2)) {
            (Some(a), Some(b)) => Some(contact_material(a, b)),
            _ => None,
        };
        // The slop of this pair: smaller when one of the bodies spins fast.
        let spin2 = [c.rigid_body1, c.rigid_body2]
            .into_iter()
            .flatten()
            .filter_map(|h| c.bodies.get(h))
            .map(|b| gv(b.angvel()).length_squared())
            .fold(0.0, f32::max);
        let g = self.1; // (Workspace.Gravity)
        let slop = if spin2 > 0.0 {
            ((g + SLOP_GRAVITY_ADD) * SLOP_SPIN / spin2).clamp(SLOP_MIN, ALLOWED_PENETRATION)
        } else {
            ALLOWED_PENETRATION
        };
        // (`MidSolve`: between its solves a character has no contacts)
        let mid = &self.2;
        let idle = mid.colliders.contains(&c.collider1) || mid.colliders.contains(&c.collider2);
        if let Some(m) = c.rigid_mut() {
            if idle {
                m.solver_contacts.clear();
                return;
            }
            // Studio's rows have no separation term in their velocity part:
            // a contact acts once the shapes touch, not while they still
            // close on each other (a block at rest sits g·h² = 0.0034 inside
            // the floor: one tick of fall, then stopped). rapier's
            // speculative points (separation > 0) are left out.
            m.solver_contacts.retain(|sc| sc.dist <= 0.0);
            for sc in m.solver_contacts.iter_mut() {
                if sc.dist < 0.0 {
                    sc.dist = (sc.dist + slop).min(0.0);
                }
            }
            if let Some((friction, _)) = material {
                *m.friction = friction;
            }
            // (the bounce is `humanoid_restitution`'s: Studio's rule, not
            // rapier's new-contacts-only one)
            *m.restitution = 0.0;
        }
    }
}

#[derive(Default)]
pub struct Hums {
    pub by_model: HashMap<Ref, HumBody>,
    /// Part → its character model.
    pub of_part: HashMap<Ref, Ref>,
    /// Force / torque this step from force-type movers (VectorForce,
    /// BodyForce, BodyThrust, Torque) by body: Studio's accumulator reads them.
    pub fext: HashMap<rp::RigidBodyHandle, (Vec3, Vec3)>,
    /// See `MidSolve`.
    pub(super) mid: MidSolve,
    /// Movers the owning client made on its character (only the client
    /// document has them; Place A's LinearVelocity / AlignOrientation), by client
    /// instance, their part mapped to the server's.
    pub(super) foreign: HashMap<Ref, Mover>,
    /// Their client-only attachments: (server part, CFrame in it).
    pub(super) foreign_att: HashMap<Ref, (Ref, Xform)>,
    /// The ownership boundary (`set_ownership_split`): each collider's side,
    /// and the stand-ins by the body they copy.
    pub(super) roles: HashMap<rp::ColliderHandle, OwnRole>,
    pub(super) ghosts: HashMap<rp::RigidBodyHandle, Ghost>,
}

impl Physics {
    /// Builds (or rebuilds) a character's body at its parts' current poses.
    /// `parts`: every BasePart of the character with its world pose.
    pub fn add_humanoid(&mut self, doc: &Document, model: Ref, root: Ref, parts: &[(Ref, Xform)]) {
        self.remove_humanoid(model);
        let Some(root_xf) = parts.iter().find(|(r, _)| *r == root).map(|(_, x)| *x) else { return };
        let root_pose = pose(&root_xf);
        let rb = rp::RigidBodyBuilder::dynamic().pose(root_pose).ccd_enabled(true).can_sleep(false);
        let body = self.world.insert_body(rb.build());
        self.dyn_version += 1;
        let inv = root_pose.inverse();
        let mut list = Vec::new();
        let sig_hash = Self::humanoid_sig(doc, root, parts);
        for (r, xf) in parts {
            let Some((_, mut sig)) = part_sig(doc, *r) else { continue };
            // A part's volume comes from its physics geometry
            // (primitive) when that is a mesh, else from its Size box.
            // Studio's default R15 body: torso and limbs weigh exactly their
            // box (UpperTorso 2.3190, UpperArm 0.8717 for boxes 2.3187 /
            // 0.8720), the Head its mesh (0.7729 for a 0.7735 hull, box
            // 1.1133): the body parts' geometry is their box, the Head's its
            // mesh. They collide as that geometry too.
            let name = doc.dom.get_by_ref(*r).map(|i| i.name.clone()).unwrap_or_default();
            let body_part = sig.mesh.is_some() && name != "Head" && crate::rig::is_body_part_name(&name);
            if body_part {
                sig.mesh = None;
                sig.fidelity = crate::collision::FIDELITY_BOX;
            }
            let (b, offset, _) = collider(&sig);
            let massless = matches!(doc.prop(*r, "Massless"), Some(Variant::Bool(true))) && *r != root;
            let pm = part_mass(doc, *r);
            // The part's mass as Lua reads it (part_mass: a Head's from its
            // mesh geometry, Studio), whatever shape it collides as right
            // now: the collision shape of a mesh can arrive later or not at
            // all, and the assembly must still weigh Studio's 14.2551 (a
            // Head weighed from a stand-in box made it 14.599: the walk's
            // acceleration cap m × 196.2 × μ showed it, feel/README.md).
            let mp = if massless {
                rp::MassProperties::default()
            } else {
                let com = offset.inverse() * rv(pm.com);
                rp::MassProperties::new(com, pm.mass, rv(pm.inertia))
            };
            // Contacts use the part's own friction () and elasticity.
            let props = crate::runtime::domains::world::part::phys_props(doc, *r);
            let b = b
                .mass_properties(rp::MassProperties::default())
                .friction(props.friction)
                .restitution(props.elasticity)
                .active_hooks(rp::ActiveHooks::MODIFY_SOLVER_CONTACTS)
                .user_data(weights_data(props.friction_weight, props.elasticity_weight))
                .collision_groups(self.part_groups(doc, *r));
            let local = inv * pose(xf);
            let c = self.world.insert_collider(b.position(local * offset), Some(body));
            self.by_collider.insert(c, *r);
            self.hums.of_part.insert(*r, model);
            list.push(HumPart { part: *r, collider: c, offset, friction: props.friction, mp });
        }
        let total = list
            .iter()
            .filter_map(|p| Some(p.mp.transform_by(self.world.colliders.get(p.collider)?.position_wrt_parent()?)))
            .fold(rp::MassProperties::default(), |a, b| a + b);
        if let Some(b) = self.world.bodies.get_mut(body) {
            b.set_additional_mass_properties(total, true);
            b.recompute_mass_properties_from_colliders(&self.world.colliders);
        }
        if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
            let m = self.world.bodies.get(body).map(|b| b.mass());
            let head = list.iter().find(|p| doc.dom.get_by_ref(p.part).is_some_and(|i| i.name == "Head")).and_then(|p| self.world.colliders.get(p.collider));
            eprintln!("HUMDBG body built: mass {m:?}, head {:?} mass {:?}", head.map(|c| c.shape().shape_type()), head.map(|c| c.mass()));
            for p in &list {
                let name = doc.dom.get_by_ref(p.part).map(|i| i.name.clone()).unwrap_or_default();
                eprintln!("HUMDBG   {name} {:?} doc {:.4}", p.mp.mass(), part_mass(doc, p.part).mass);
            }
        }
        self.hums.by_model.insert(model, HumBody { body, root, parts: list, ctl: None, seat: None, shown: None, lvl: 1, debounce: 0, rose: false, hold: None, span: 1, ref_v: None, hits: Vec::new(), lambda: HashMap::new(), sig: sig_hash, solve0: None, arb: [0, 0] });
    }

    /// What a character's body depends on besides its pose: each part's
    /// size, shape, mesh (and whether it has loaded), mass and Massless.
    fn humanoid_sig(doc: &Document, root: Ref, parts: &[(Ref, Xform)]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        root.hash(&mut h);
        for (r, _) in parts {
            let Some((_, mut sig)) = part_sig(doc, *r) else { continue };
            // (As add_humanoid builds it: body parts as their box.)
            let name = doc.dom.get_by_ref(*r).map(|i| i.name.clone()).unwrap_or_default();
            if sig.mesh.is_some() && name != "Head" && crate::rig::is_body_part_name(&name) {
                sig.mesh = None;
                sig.fidelity = crate::collision::FIDELITY_BOX;
            }
            r.hash(&mut h);
            for v in [sig.size.x, sig.size.y, sig.size.z] {
                v.to_bits().hash(&mut h);
            }
            sig.kind.hash(&mut h);
            sig.fidelity.hash(&mut h);
            sig.mesh.as_deref().hash(&mut h);
            // A mesh that hasn't loaded yet stands in as its box (or an
            // interim hull): the shape's kind and "more to come" tell.
            let (b, _, pending) = collider(&sig);
            std::mem::discriminant(&b.shape.shape_type()).hash(&mut h);
            pending.hash(&mut h);
            part_mass(doc, *r).mass.to_bits().hash(&mut h);
            matches!(doc.prop(*r, "Massless"), Some(Variant::Bool(true))).hash(&mut h);
        }
        h.finish()
    }

    /// The character's parts changed (an avatar's scale or meshes arrived, a
    /// script resized a limb): the body is built again where it is, keeping
    /// its velocity, its controller and its seat. Studio's assembly takes
    /// its mass from the parts as they are (AssemblyMass follows a resize).
    pub fn refresh_humanoid(&mut self, doc: &Document, model: Ref, root: Ref, parts: &[(Ref, Xform)]) {
        let Some(h) = self.hums.by_model.get(&model) else { return };
        if h.sig == Self::humanoid_sig(doc, root, parts) {
            return;
        }
        let Some((xf, _, _)) = self.humanoid_root_now(model) else { return };
        let vel = self.world.bodies.get(h.body).map(|b| (b.linvel(), b.angvel()));
        let seat = h.seat.map(|(s, _)| s);
        let ctl = self.hums.by_model.get_mut(&model).and_then(|h| h.ctl.take());
        // The parts' poses relative to the root as the document has them,
        // at the body's own pose.
        let Some(doc_root) = parts.iter().find(|(r, _)| *r == root).map(|(_, x)| *x) else { return };
        use crate::ltypes::{cf_inv, cf_mul};
        let inv = cf_inv(&doc_root);
        let placed: Vec<(Ref, Xform)> = parts.iter().map(|(r, x)| (*r, cf_mul(&xf, &cf_mul(&inv, x)))).collect();
        self.add_humanoid(doc, model, root, &placed);
        if let Some(h) = self.hums.by_model.get_mut(&model) {
            h.ctl = ctl;
            if let (Some((v, w)), Some(b)) = (vel, self.world.bodies.get_mut(h.body)) {
                b.set_linvel(v, true);
                b.set_angvel(w, true);
            }
        }
        if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
            eprintln!("HUMDBG body rebuilt: mass {:?}", self.humanoid_mass(model));
        }
        if let Some(seat) = seat {
            let at = self.humanoid_root(model).map(|r| r.0);
            if let Some(at) = at {
                self.seat_humanoid(model, Some((seat, at)));
            }
        }
    }

    /// The network-ownership boundary between the player's character (its
    /// client simulates it) and the characters a script kept on the server
    /// with SetNetworkOwner(nil). In Studio each side simulates only its own
    /// assemblies and meets the other's as bodies it cannot move, at the
    /// poses it last received: a player walking into a server-owned NPC is
    /// stopped by it and barely moves it (probe T11: 0.5–1.1 studs/s; with
    /// both in one solver they shared momentum and went on at 8), and two
    /// walking into each other hold each other up. Here each such body has
    /// a kinematic stand-in that follows it; the player's character collides
    /// with the server bodies' stand-ins and they with its stand-in
    /// (`OwnerHooks`), never with each other directly.
    pub fn set_ownership_split(&mut self, client: Option<Ref>, server: &[Ref]) {
        let mut want: HashMap<rp::RigidBodyHandle, OwnRole> = HashMap::new();
        if !server.is_empty() {
            if let Some(h) = client.and_then(|m| self.hums.by_model.get(&m)) {
                want.insert(h.body, OwnRole::GhostOfClient);
                for m in server {
                    if let Some(h) = self.hums.by_model.get(m) {
                        want.insert(h.body, OwnRole::GhostOfServer);
                    }
                }
            }
        }
        if want.len() < 2 {
            want.clear();
        }
        // Stand-ins no longer needed (the body went, the split ended).
        let gone: Vec<rp::RigidBodyHandle> = self
            .hums
            .ghosts
            .iter()
            .filter(|(b, g)| want.get(*b) != Some(&g.role) || self.world.bodies.get(**b).is_none())
            .map(|(b, _)| *b)
            .collect();
        for b in gone {
            if let Some(g) = self.hums.ghosts.remove(&b) {
                for (_, gc) in &g.colliders {
                    self.by_collider.remove(gc);
                }
                self.world.remove_body(g.body);
            }
        }
        for (b, role) in &want {
            if self.hums.ghosts.contains_key(b) {
                continue;
            }
            let Some(orig) = self.world.bodies.get(*b) else { continue };
            let list: Vec<rp::ColliderHandle> = orig.colliders().to_vec();
            let ghost = self.world.insert_body(rp::RigidBodyBuilder::kinematic_position_based().pose(*orig.position()).build());
            let mut colliders = Vec::new();
            for ch in list {
                let Some(c) = self.world.colliders.get(ch) else { continue };
                if c.is_sensor() {
                    continue;
                }
                let copy = rp::ColliderBuilder::new(c.shared_shape().clone())
                    .position(c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY))
                    .friction(c.friction())
                    .restitution(c.restitution())
                    .collision_groups(c.collision_groups())
                    .density(0.0)
                    .active_hooks(rp::ActiveHooks::FILTER_CONTACT_PAIRS);
                let gc = self.world.insert_collider(copy, Some(ghost));
                // Touching the stand-in is touching the part (Touched).
                if let Some(part) = self.by_collider.get(&ch).copied() {
                    self.by_collider.insert(gc, part);
                }
                colliders.push((ch, gc));
            }
            self.hums.ghosts.insert(*b, Ghost { body: ghost, colliders, role: *role });
        }
        // Every collider's side.
        self.hums.roles.clear();
        for (b, g) in &self.hums.ghosts {
            let side = if g.role == OwnRole::GhostOfClient { OwnRole::Client } else { OwnRole::Server };
            if let Some(orig) = self.world.bodies.get(*b) {
                for ch in orig.colliders().to_vec() {
                    self.hums.roles.insert(ch, side);
                    if let Some(c) = self.world.colliders.get_mut(ch) {
                        if !c.active_hooks().contains(rp::ActiveHooks::FILTER_CONTACT_PAIRS) {
                            c.set_active_hooks(c.active_hooks() | rp::ActiveHooks::FILTER_CONTACT_PAIRS);
                        }
                    }
                }
            }
            for (_, gc) in &g.colliders {
                self.hums.roles.insert(*gc, g.role);
            }
        }
    }

    /// Each stand-in goes where its body is (before a world step), its
    /// shapes where the body's animated pose has them.
    pub(super) fn ghosts_follow(&mut self) {
        if self.hums.ghosts.is_empty() {
            return;
        }
        let list: Vec<(rp::RigidBodyHandle, rp::RigidBodyHandle)> = self.hums.ghosts.iter().map(|(b, g)| (*b, g.body)).collect();
        if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() && self.steps % 60 == 0 {
            for (b, g) in &self.hums.ghosts {
                let touching: usize = g
                    .colliders
                    .iter()
                    .map(|(_, gc)| self.world.narrow_phase.contact_pairs_with(*gc).filter(|p| p.has_any_active_contact()).count())
                    .sum();
                let pairs: usize = g.colliders.iter().map(|(_, gc)| self.world.narrow_phase.contact_pairs_with(*gc).count()).sum();
                let info: Vec<String> = g.colliders.iter().filter_map(|(_, gc)| self.world.colliders.get(*gc)).map(|c| format!("{:?} groups {:?} hooks {:?} sensor {} enabled {}", c.shape().shape_type(), c.collision_groups(), c.active_hooks(), c.is_sensor(), c.is_enabled())).collect();
                eprintln!("HUMGHOST   pairs {pairs} {:?} body type {:?}", info.first(), self.world.bodies.get(g.body).map(|b| b.body_type()));
                let at = |h: rp::RigidBodyHandle| self.world.bodies.get(h).map(|b| b.position().translation);
                eprintln!("HUMGHOST step={} of {:?} colliders {} touching {} body at {:?} ghost at {:?}", self.steps, g.role as u8, g.colliders.len(), touching, at(*b), at(g.body));
            }
        }
        for (orig, ghost) in list {
            let Some(p) = self.world.bodies.get(orig).map(|b| *b.position()) else { continue };
            // Put there, not moved there: a stand-in has no velocity of its
            // own to hand over (the other side only knows where the body is;
            // moved kinematically it rammed the server's character away at
            // the player's walking speed).
            if let Some(g) = self.world.bodies.get_mut(ghost) {
                if *g.position() != p {
                    g.set_position(p, true);
                }
            }
            let pairs = self.hums.ghosts.get(&orig).map(|g| g.colliders.clone()).unwrap_or_default();
            for (oc, gc) in pairs {
                let Some(local) = self.world.colliders.get(oc).and_then(|c| c.position_wrt_parent().copied()) else { continue };
                if let Some(c) = self.world.colliders.get_mut(gc) {
                    let have = c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY);
                    if (local.translation - have.translation).length() > 1e-6 || local.rotation.angle_between(have.rotation) > 1e-6 {
                        c.set_position_wrt_parent(local);
                    }
                }
            }
        }
    }

    /// Freefall (enter Studio) zeroes the friction (primitive,
    /// setter Studio) of the Head and the RootPart and restores it
    /// on leaving: `zero` = in Freefall.
    pub fn humanoid_air_friction(&mut self, model: Ref, parts: &[Ref], zero: bool) {
        let Some(h) = self.hums.by_model.get(&model) else { return };
        for p in &h.parts {
            if !parts.contains(&p.part) {
                continue;
            }
            if let Some(c) = self.world.colliders.get_mut(p.collider) {
                let f = if zero { 0.0 } else { p.friction };
                if c.friction() != f {
                    c.set_friction(f);
                }
            }
        }
    }

    /// Each part's CanCollide as the document has it now: a collider
    /// collides or only senses accordingly.
    pub fn sync_humanoid_collide(&mut self, model: Ref, collide: &HashMap<Ref, bool>) {
        let Some(h) = self.hums.by_model.get(&model) else { return };
        for p in &h.parts {
            let Some(on) = collide.get(&p.part) else { continue };
            if let Some(c) = self.world.colliders.get_mut(p.collider) {
                if c.is_sensor() == *on {
                    c.set_sensor(!*on);
                }
            }
        }
    }

    /// Bodies whose character left Workspace (destroyed, respawned,
    /// unparented) go: a LoadCharacter while the old character was alive
    /// left its assembly standing at the spawn, unseen, and the new
    /// character was thrown off it.
    pub fn prune_humanoids(&mut self, doc: &Document) {
        let Some(ws) = doc.workspace() else { return };
        let gone: Vec<Ref> = self.hums.by_model.keys().copied().filter(|m| !doc.exists(*m) || !doc.is_ancestor(ws, *m)).collect();
        for m in gone {
            self.remove_humanoid(m);
        }
    }

    pub fn has_humanoid(&self, model: Ref) -> bool {
        self.hums.by_model.contains_key(&model)
    }

    /// Takes a character's body out of the world (death splits it; Play stops).
    pub fn remove_humanoid(&mut self, model: Ref) {
        let Some(h) = self.hums.by_model.remove(&model) else { return };
        for p in &h.parts {
            self.by_collider.remove(&p.collider);
            self.hums.of_part.remove(&p.part);
        }
        self.world.remove_body(h.body);
        self.dyn_version += 1;
    }

    /// The assembly's mass, centre of mass and inertia are those of its
    /// joints' own pose (C0·C1⁻¹ chained, `rest`), whatever the Animator has
    /// in Motor6D.Transform: measured in Studio 0.747, a default R15
    /// character's AssemblyCenterOfMass stays at (0, −0.469, 0.000) in its
    /// root's frame while it idles, walks, runs and presses against a wall
    /// (hands up to 1.6 studs ahead, UpperTorso 0.26). A part outside the
    /// rig counts where its collider is.
    fn humanoid_rest_mass(&mut self, model: Ref, rest: &HashMap<Ref, Xform>) {
        let Some(h) = self.hums.by_model.get(&model) else { return };
        let total = h
            .parts
            .iter()
            .filter_map(|p| {
                let at = match rest.get(&p.part) {
                    Some(xf) => pose(xf) * p.offset,
                    None => *self.world.colliders.get(p.collider)?.position_wrt_parent()?,
                };
                Some(p.mp.transform_by(&at))
            })
            .fold(rp::MassProperties::default(), |a, b| a + b);
        let body = h.body;
        let Some(b) = self.world.bodies.get_mut(body) else { return };
        let have = b.mass_properties().local_mprops;
        let same = (have.local_com - total.local_com).length() < 1.0e-5
            && (have.mass() - total.mass()).abs() < 1.0e-5
            && (have.principal_inertia() - total.principal_inertia()).length() < 1.0e-4;
        if !same {
            b.set_additional_mass_properties(total, true);
            b.recompute_mass_properties_from_colliders(&self.world.colliders);
        }
    }

    /// Puts each part's collider where the animated pose has it: `local` =
    /// each part's pose relative to the root (C0·Transform·C1⁻¹ chained).
    pub fn set_humanoid_pose(&mut self, model: Ref, local: &HashMap<Ref, Xform>, rest: &HashMap<Ref, Xform>) {
        self.humanoid_rest_mass(model, rest);
        let Some(h) = self.hums.by_model.get(&model) else { return };
        for p in &h.parts {
            let Some(xf) = local.get(&p.part) else { continue };
            if let Some(c) = self.world.colliders.get_mut(p.collider) {
                let want = pose(xf) * p.offset;
                let have = c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY);
                let moved = (want.translation - have.translation).length() > 1e-6
                    || want.rotation.angle_between(have.rotation) > 1e-6;
                if moved {
                    c.set_position_wrt_parent(want);
                }
            }
        }
    }

    /// The root's world pose, its velocity and the assembly's angular
    /// velocity, as of the assembly's last solve (`HumBody::shown`).
    pub fn humanoid_root(&self, model: Ref) -> Option<(Xform, Vec3, Vec3)> {
        match self.hums.by_model.get(&model)?.shown {
            Some(s) => Some(s),
            None => self.humanoid_root_now(model),
        }
    }

    /// After a world tick: the contact points that pushed on each character
    /// and may bounce. Studio's live solver (the primal one: its contact rows
    /// are Studio, vtable slot 11) gives a row its restitution term only
    /// while the impulse it kept from its last solve is zero (connector
    ///), i.e. on a contact that is new or that last ended up carrying
    /// nothing. Measured (T10): a character running into a loose block at 16
    /// studs/s bounces off it (back at +6.4 against the block's −1.6: e × 16)
    /// and, coming back at 6.3 two frames later, does not: the two then move
    /// as one.
    fn humanoid_hits(&mut self) {
        for h in self.hums.by_model.values_mut() {
            if h.ref_v.is_none() {
                continue;
            }
            let own: HashSet<rp::ColliderHandle> = h.parts.iter().map(|p| p.collider).collect();
            let mut seen: HashSet<(rp::ColliderHandle, rp::ColliderHandle)> = HashSet::new();
            for p in &h.parts {
                let Some(c) = self.world.colliders.get(p.collider) else { continue };
                if c.is_sensor() {
                    continue;
                }
                for pair in self.world.narrow_phase.contact_pairs_with(p.collider) {
                    let first = pair.collider1 == p.collider;
                    let other = if first { pair.collider2 } else { pair.collider1 };
                    if own.contains(&other) {
                        continue;
                    }
                    // (the pair as one contact: which of a box's features
                    // touch changes with every animated pose)
                    let key = (p.collider, other);
                    let points = || pair.manifolds().iter().flat_map(|m| m.points.iter().map(move |pt| (m, pt)));
                    if points().next().is_some() {
                        seen.insert(key);
                    }
                    if !points().any(|(_, pt)| pt.dist <= 0.0) {
                        continue; // (not in this solve: its impulse stays)
                    }
                    let now = points().filter(|(_, pt)| pt.dist <= 0.0).map(|(_, pt)| pt.data.impulse).fold(0.0, f32::max);
                    let kept = h.lambda.insert(key, now).unwrap_or(0.0);
                    if kept != 0.0 {
                        continue;
                    }
                    for (m, pt) in points() {
                        if pt.dist <= 0.0 && pt.data.impulse > 0.0 {
                            let n = gv(m.data.normal);
                            let at = gv(*c.position() * if first { pt.local_p1 } else { pt.local_p2 });
                            h.hits.push(Hit { own: p.collider, other, n: if first { -n } else { n }, p: at });
                        }
                    }
                }
            }
            // A pair the narrow phase no longer lists keeps its impulse
            // while the two shapes are within `CONTACT_KEEP` of each other.
            let colliders = &self.world.colliders;
            h.lambda.retain(|k, _| {
                seen.contains(k)
                    || match (colliders.get(k.0), colliders.get(k.1)) {
                        (Some(a), Some(b)) => rapier3d::parry::query::distance(a.position(), a.shape(), b.position(), b.shape()).is_ok_and(|d| d.distance <= CONTACT_KEEP),
                        _ => false,
                    }
            });
        }
    }

    /// A solve is over: Studio's restitution (`RESTITUTION_MIN_SPEED`). Each
    /// contact point that pushed and that the bodies closed on at 1 stud/s
    /// or more when the solve began leaves at e times that speed or faster
    /// (a row of its own per point, as in Studio's solver: a head and a
    /// hip against the same wall each bounce, which is what rocks a pressed
    /// character); this tick's move follows the new velocity.
    fn humanoid_restitution(&mut self) {
        let h = 1.0 / mech::PHYSICS_HZ;
        let models: Vec<Ref> = self.hums.by_model.keys().copied().collect();
        for model in models {
            let Some(hb) = self.hums.by_model.get_mut(&model) else { continue };
            if hb.hold.is_some() {
                continue;
            }
            let (Some((v0, w0)), hits) = (hb.ref_v.take(), std::mem::take(&mut hb.hits)) else { continue };
            let body = hb.body;
            let Some(b) = self.world.bodies.get(body) else { continue };
            if !b.is_dynamic() || hits.is_empty() {
                continue;
            }
            let com = gv(b.center_of_mass());
            let (lv0, av0) = (gv(b.linvel()), gv(b.angvel()));
            // (point, normal toward the character, the other body, the speed
            // to leave at, the pair's friction)
            let mut rows: Vec<(Vec3, Vec3, Option<rp::RigidBodyHandle>, f32, f32)> = Vec::new();
            for hit in &hits {
                let (Some(c), Some(oc)) = (self.world.colliders.get(hit.own), self.world.colliders.get(hit.other)) else { continue };
                let (mu, e) = contact_material(c, oc);
                if e <= 0.0 {
                    continue;
                }
                let ob = oc.parent();
                let other_at = ob.and_then(|ob| self.world.bodies.get(ob)).map_or(Vec3::ZERO, |o| gv(o.velocity_at_point(rv(hit.p))));
                // (closing speed with the velocity the solve began with)
                let closing = -hit.n.dot(v0 + w0.cross(hit.p - com) - other_at);
                if closing >= RESTITUTION_MIN_SPEED {
                    rows.push((hit.p, hit.n, ob, e * closing, mu));
                }
            }
            if rows.is_empty() {
                continue;
            }
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                eprintln!("HUMBOUNCE {} rows {:?}", self.steps, rows.iter().map(|r| (r.0, r.3)).collect::<Vec<_>>());
            }
            for _ in 0..8 {
                let mut done = true;
                for (p, n, ob, want, mu) in &rows {
                    let Some(b) = self.world.bodies.get(body) else { continue };
                    let r = *p - com;
                    let other = ob.and_then(|ob| self.world.bodies.get(ob)).filter(|o| o.is_dynamic());
                    let other_at = ob.and_then(|ob| self.world.bodies.get(ob)).map_or(Vec3::ZERO, |o| gv(o.velocity_at_point(rv(*p))));
                    let now = n.dot(gv(b.linvel()) + gv(b.angvel()).cross(r) - other_at);
                    if now >= *want - 1.0e-4 {
                        continue;
                    }
                    let mut k = 1.0 / b.mass() + n.dot((world_inv_inertia(b) * r.cross(*n)).cross(r));
                    if let Some(o) = other {
                        let ro = *p - gv(o.center_of_mass());
                        k += 1.0 / o.mass() + n.dot((world_inv_inertia(o) * ro.cross(*n)).cross(ro));
                    }
                    if k <= 0.0 {
                        continue;
                    }
                    done = false;
                    let jn = (*want - now) / k;
                    // The bounce is part of the row's normal impulse, so the
                    // friction rows' bound μ·λₙ (row = friction ×
                    // cfg[]) grows with it: friction against the slide.
                    let slide = gv(b.linvel()) + gv(b.angvel()).cross(r) - other_at;
                    let slide = slide - *n * slide.dot(*n);
                    let speed = slide.length();
                    let mut j = *n * jn;
                    if speed > 1.0e-4 && *mu > 0.0 {
                        let t = slide / speed;
                        let mut kt = 1.0 / b.mass() + t.dot((world_inv_inertia(b) * r.cross(t)).cross(r));
                        if let Some(o) = other {
                            let ro = *p - gv(o.center_of_mass());
                            kt += 1.0 / o.mass() + t.dot((world_inv_inertia(o) * ro.cross(t)).cross(ro));
                        }
                        if kt > 0.0 {
                            j -= t * (*mu * jn).min(speed / kt);
                        }
                    }
                    let moves_other = other.is_some();
                    if let Some(b) = self.world.bodies.get_mut(body) {
                        b.apply_impulse_at_point(rv(j), rv(*p), true);
                    }
                    if moves_other {
                        if let Some(o) = ob.and_then(|ob| self.world.bodies.get_mut(ob)) {
                            o.apply_impulse_at_point(rv(-j), rv(*p), true);
                        }
                    }
                }
                if done {
                    break;
                }
            }
            if let Some(b) = self.world.bodies.get_mut(body) {
                let (dv, dw) = (gv(b.linvel()) - lv0, gv(b.angvel()) - av0);
                let mut p = *b.position();
                let turn = glam::Quat::from_scaled_axis(dw * h);
                let origin = gv(p.translation);
                p.translation = rv(com + turn * (origin - com) + dv * h);
                p.rotation = rp::Rotation::from_xyzw(turn.x, turn.y, turn.z, turn.w) * p.rotation;
                b.set_position(p, true);
            }
        }
    }

    /// After a solve: a character deeper than the allowed band in something
    /// that doesn't move is carried out along the contact normals by
    /// `DEPENETRATION_PER_SOLVE` of the excess, its velocity untouched.
    fn humanoid_depenetrate(&mut self) {
        let mut moves: Vec<(rp::RigidBodyHandle, Vec3)> = Vec::new();
        for h in self.hums.by_model.values() {
            if h.seat.is_some() || h.hold.is_some() {
                continue;
            }
            if !self.world.bodies.get(h.body).is_some_and(|b| b.is_dynamic()) {
                continue;
            }
            let own: HashSet<rp::ColliderHandle> = h.parts.iter().map(|p| p.collider).collect();
            let mut deep: Vec<(f32, Vec3)> = Vec::new();
            for p in &h.parts {
                for pair in self.world.narrow_phase.contact_pairs_with(p.collider) {
                    let other = if pair.collider1 == p.collider { pair.collider2 } else { pair.collider1 };
                    if own.contains(&other) {
                        continue;
                    }
                    let Some(oc) = self.world.colliders.get(other) else { continue };
                    if oc.is_sensor() || oc.parent().and_then(|b| self.world.bodies.get(b)).is_some_and(|b| b.is_dynamic()) {
                        continue;
                    }
                    for m in pair.manifolds() {
                        if !m.data.solver_flags.contains(rp::SolverFlags::COMPUTE_RIGID_IMPULSES) {
                            continue;
                        }
                        let depth = m.points.iter().map(|pt| -pt.dist).fold(0.0, f32::max);
                        let excess = depth - ALLOWED_PENETRATION;
                        if excess > 0.0 {
                            let n = gv(m.data.normal);
                            deep.push((excess, if pair.collider1 == p.collider { -n } else { n }));
                        }
                    }
                }
            }
            if deep.is_empty() {
                continue;
            }
            deep.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut total = Vec3::ZERO;
            for (excess, n) in deep {
                let need = excess * DEPENETRATION_PER_SOLVE - total.dot(n);
                if need > 0.0 {
                    total += n * need;
                }
            }
            moves.push((h.body, total));
        }
        for (body, d) in moves {
            if let Some(b) = self.world.bodies.get_mut(body) {
                let mut p = *b.position();
                p.translation += rv(d);
                b.set_position(p, true);
            }
        }
    }

    /// After a world step: a character whose assembly has just finished a
    /// solve (every tick at level 1, every 2nd at level 2) shows that state.
    pub(super) fn humanoid_solved(&mut self) {
        // (`MidSolve`: the tick between two level-2 solves changed nothing)
        for hb in self.hums.by_model.values() {
            if let (Some((p, v, w)), Some(b)) = (hb.hold, self.world.bodies.get_mut(hb.body)) {
                b.set_position(p, true);
                b.set_linvel(rv(v), true);
                b.set_angvel(rv(w), true);
            }
        }
        self.humanoid_hits();
        self.humanoid_depenetrate();
        self.humanoid_restitution();
        let h = 1.0 / mech::PHYSICS_HZ;
        // A level-2 solve covers 2h: its second h of motion, with the
        // velocity the solve ended with.
        for hb in self.hums.by_model.values_mut() {
            hb.rose = false;
            if hb.hold.take().is_none() && hb.span == 2 {
                if let Some(b) = self.world.bodies.get_mut(hb.body) {
                    if b.is_dynamic() {
                        let p = b.predict_position_using_velocity(h);
                        b.set_position(p, true);
                    }
                }
            }
        }
        self.body_arbiter();
        let all: Vec<Ref> = self.hums.by_model.keys().copied().collect();
        for m in all {
            let now = self.humanoid_root_now(m);
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                if let (Some((xf, _, w)), Some(v)) = (now, self.humanoid_com_velocity(m)) {
                    // (after the tick, as Studio's StepPhysics shows it:
                    // checks/humanoid-physics/ticks)
                    eprintln!(
                        "HUMPOST {} {:.4} {:.4} {:.4} | {:.4} {:.4} {:.4} | {:.4} {:.4} {:.4} | {:.5} {:.5}",
                        self.steps, xf.pos.x, xf.pos.y, xf.pos.z, v.x, v.y, v.z, w.x, w.y, w.z, xf.rot.y_axis.z, xf.rot.y_axis.x
                    );
                }
            }
            if let Some(h) = self.hums.by_model.get_mut(&m) {
                h.shown = now;
            }
        }
    }

    /// A character's solve is over: the timestep arbiter's requests from it
    /// (live: SimAdaptiveEnable60HzParts on, SimAdaptiveRemoveVelocity
    /// CriteriaHistoryDependence on, so Studio takes slot 0 from
    /// Studio). A lower level than the assembly's takes effect at once
    /// (`humanoid_forces`).
    ///
    /// * Slot 6: the solve's correction factor (Studio,
    ///   body) = ½ (|Δx − dt·v| / (s + |Δx|) + |Δθ − dt·ω| / (s + |Δθ|)),
    ///   Δx and Δθ the solve's motion of the centre of mass and the turn, v
    ///   and ω the velocities it ends with, dt its span, s
    ///   `ARB_CORRECTION_SCALE`: above `ARB_POSITION_CORRECTION`, while the
    ///   body moved at all (CFL = (|Δθ|·r + |Δx|) / `ARB_CFL_LENGTH`
    ///   above 1e-6), level 1, else 4.
    /// * Slot 0: the body's box (about its centre
    ///   of mass: the same in Studio at the world's origin and 4243 studs
    ///   from it) moved by v and turned by ω over 4 ticks keeps more than
    ///   `ARB_BOX_FOUR` of its volume: 4; else over 2 ticks more than
    ///   `ARB_BOX_TWO`: 2; else 1.
    /// Slot 4 (body against the arbiter context's, reset to 1
    /// each time) is not ported: no request.
    fn body_arbiter(&mut self) {
        let h = 1.0 / mech::PHYSICS_HZ;
        let models: Vec<Ref> = self.hums.by_model.keys().copied().collect();
        for model in models {
            let Some(hb) = self.hums.by_model.get(&model) else { continue };
            if hb.span == 0 || hb.seat.is_some() {
                continue;
            }
            let Some((x0, q0)) = hb.solve0 else { continue };
            let Some(b) = self.world.bodies.get(hb.body) else { continue };
            if !b.is_dynamic() {
                continue;
            }
            let dt = h * hb.span.max(1) as f32;
            let (x1, q1) = (gv(b.center_of_mass()), quat_of(b.rotation()));
            let (v, w) = (gv(b.linvel()), gv(b.angvel()));
            let dx = x1 - x0;
            let mut dq = q1 * q0.inverse();
            if dq.w < 0.0 {
                dq = -dq;
            }
            let dth = dq.to_scaled_axis();
            let s = ARB_CORRECTION_SCALE;
            let factor = 0.5 * ((dx - v * dt).length() / (s + dx.length()) + (dth - w * dt).length() / (s + dth.length()));
            // The assembly's box about its centre of mass (..).
            let mut lo = Vec3::splat(f32::MAX);
            let mut hi = Vec3::splat(f32::MIN);
            for p in &hb.parts {
                if let Some(c) = self.world.colliders.get(p.collider) {
                    let bb = c.compute_aabb();
                    lo = lo.min(gv(bb.mins) - x1);
                    hi = hi.max(gv(bb.maxs) - x1);
                }
            }
            let radius = (hi - lo).length() * 0.5;
            let cfl = (dth.length() * radius + dx.length()) / ARB_CFL_LENGTH;
            let slot6 = if factor > ARB_POSITION_CORRECTION && cfl > 1.0e-6 { 1 } else { 4 };
            let slot0 = if lo.cmple(hi).all() {
                let keep = |k: f32| box_kept(lo, hi, v, w, k * h);
                if keep(4.0) > ARB_BOX_FOUR { 4 } else if keep(2.0) > ARB_BOX_TWO { 2 } else { 1 }
            } else {
                1
            };
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                eprintln!("HUMARB {} span={} factor={factor:.4} cfl={cfl:.6} slot0={slot0} slot6={slot6}", self.steps, hb.span);
            }
            if let Some(hb) = self.hums.by_model.get_mut(&model) {
                hb.arb = [slot0, slot6];
            }
        }
    }

    /// RotationType CameraRelative's turn (play_humanoid.rs `hum_face_camera`):
    /// the assembly turned about the vertical through its root by `delta`,
    /// its velocities untouched.
    pub fn turn_humanoid(&mut self, model: Ref, delta: f32) {
        let Some(h) = self.hums.by_model.get_mut(&model) else { return };
        let turn = Mat3::from_rotation_y(delta);
        if let Some((xf, _, _)) = h.shown.as_mut() {
            xf.rot = turn * xf.rot;
        }
        if let Some(b) = self.world.bodies.get_mut(h.body) {
            let mut xf = xform(b.position());
            xf.rot = turn * xf.rot;
            b.set_position(pose(&xf), true);
        }
    }

    /// The root's world pose and velocities as the body is this tick.
    pub fn humanoid_root_now(&self, model: Ref) -> Option<(Xform, Vec3, Vec3)> {
        let h = self.hums.by_model.get(&model)?;
        let b = self.world.bodies.get(h.body)?;
        let p = *b.position();
        let xf = xform(&p);
        Some((xf, gv(b.velocity_at_point(rv(xf.pos))), gv(b.angvel())))
    }

    /// The assembly's linear velocity: its centre of mass's (what
    /// AssemblyLinearVelocity reads; measured in Studio on a spinning
    /// character: 53.0114 where the root part's own point moves at 52.9998).
    pub fn humanoid_com_velocity(&self, model: Ref) -> Option<Vec3> {
        let h = self.hums.by_model.get(&model)?;
        Some(gv(self.world.bodies.get(h.body)?.linvel()))
    }

    /// The assembly's mass (AssemblyMass).
    pub fn humanoid_mass(&self, model: Ref) -> Option<f32> {
        let h = self.hums.by_model.get(&model)?;
        Some(self.world.bodies.get(h.body)?.mass())
    }

    /// A script moved the root (PivotTo, CFrame): the body goes there,
    /// keeping its velocity (Roblox moves the assembly).
    pub fn teleport_humanoid(&mut self, model: Ref, root: &Xform) {
        let Some(h) = self.hums.by_model.get_mut(&model) else { return };
        h.shown = None;
        if let Some(b) = self.world.bodies.get_mut(h.body) {
            b.set_position(pose(root), true);
        }
    }

    /// AssemblyLinearVelocity / AssemblyAngularVelocity written by a script.
    pub fn set_humanoid_velocity(&mut self, model: Ref, v: Option<Vec3>, w: Option<Vec3>) {
        let Some(h) = self.hums.by_model.get_mut(&model) else { return };
        if let Some(b) = self.world.bodies.get_mut(h.body) {
            if let Some(v) = v {
                b.set_linvel(rv(v), true);
            }
            if let Some(w) = w {
                b.set_angvel(rv(w), true);
            }
        }
        // (what scripts and this frame's Humanoid step read is the write)
        if let Some(s) = h.shown.as_mut() {
            if let Some(v) = v {
                s.1 = v;
            }
            if let Some(w) = w {
                s.2 = w;
            }
        }
    }

    /// The character body of a part, if it has one.
    pub(super) fn humanoid_body_of(&self, part: Ref) -> Option<rp::RigidBodyHandle> {
        let m = self.hums.of_part.get(&part)?;
        self.hums.by_model.get(m).map(|h| h.body)
    }

    /// The pose of a character part (its collider without the shape offset).
    pub(super) fn humanoid_part_xform(&self, part: Ref) -> Option<Xform> {
        let m = self.hums.of_part.get(&part)?;
        let h = self.hums.by_model.get(m)?;
        let p = h.parts.iter().find(|p| p.part == part)?;
        let c = self.world.colliders.get(p.collider)?;
        // From the body's pose: a collider's own follows a teleport only in
        // the next world step, and the controller's first force after one
        // (the hip PD's error) must already see it.
        match (self.world.bodies.get(h.body), c.position_wrt_parent()) {
            (Some(b), Some(local)) => Some(xform(&(*b.position() * *local * p.offset.inverse()))),
            _ => Some(xform(&(*c.position() * p.offset.inverse()))),
        }
    }

    /// The owning client's movers on its character (read each frame from
    /// the client document; replicated ones are the server's already).
    pub fn set_client_movers(&mut self, cd: &Document, c2s: &HashMap<Ref, Ref>, client_model: Ref) {
        self.hums.foreign.clear();
        self.hums.foreign_att.clear();
        let Some(cws) = cd.workspace() else { return };
        let found: Vec<Ref> = cd
            .dom
            .descendants_of(client_model)
            .filter(|i| is_mover_class(&i.class))
            .map(|i| i.referent())
            .filter(|r| !c2s.contains_key(r))
            .collect();
        for r in found {
            let Some(mut m) = read_mover(cd, cws, r) else { continue };
            let Some(sp) = c2s.get(&m.part).copied() else { continue };
            m.part = sp;
            for a in [&mut m.att0, &mut m.att1] {
                let Some(ar) = *a else { continue };
                if let Some(s) = c2s.get(&ar) {
                    *a = Some(*s);
                    continue;
                }
                let parent = cd.dom.get_by_ref(ar).map(|i| i.parent()).unwrap_or_else(Ref::none);
                let Some(spart) = c2s.get(&parent).copied() else {
                    *a = None;
                    continue;
                };
                let local = match cd.prop(ar, "CFrame") {
                    Some(Variant::CFrame(cf)) => Xform::from_cframe(&cf).orthonormalized(),
                    _ => Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY },
                };
                self.hums.foreign_att.insert(ar, (spart, local));
            }
            self.hums.foreign.insert(r, m);
        }
    }

    /// Assemblies the player's client simulates (SetNetworkOwner(player),
    /// the vehicle it sits in): their movers act with the values the client
    /// has, as in Studio, where the owner's own DataModel drives them (probe
    /// T13: the LocalScript sets the vehicle's VectorForce.Force; the server
    /// never sees it, and the vehicle accelerates at F / m all the same).
    pub fn set_owned_movers(&mut self, cd: &Document, s2c: &HashMap<Ref, Ref>, c2s: &HashMap<Ref, Ref>, owned: &[Ref]) {
        if owned.is_empty() {
            return;
        }
        let Some(cws) = cd.workspace() else { return };
        let asms: HashSet<_> = owned.iter().filter_map(|r| self.parts.get(r).and_then(|p| p.asm)).collect();
        if asms.is_empty() {
            return;
        }
        let list: Vec<Ref> = self
            .movers
            .iter()
            .filter(|(_, m)| self.parts.get(&m.part).and_then(|p| p.asm).is_some_and(|a| asms.contains(&a)))
            .map(|(r, _)| *r)
            .collect();
        for r in list {
            let Some(cr) = s2c.get(&r) else { continue };
            let Some(mut m) = read_mover(cd, cws, *cr) else { continue };
            let Some(sp) = c2s.get(&m.part).copied() else { continue };
            m.part = sp;
            let mut ok = true;
            for a in [&mut m.att0, &mut m.att1] {
                if let Some(ar) = *a {
                    match c2s.get(&ar) {
                        Some(s) => *a = Some(*s),
                        None => ok = false,
                    }
                }
            }
            if ok {
                self.movers.insert(r, m);
            }
        }
    }

    /// Sitting is a weld into the seat's assembly (SPEC §3): on an anchored
    /// seat the character is held where the SeatWeld puts its root; on a
    /// loose one a fixed joint makes the two move as one.
    pub fn seat_humanoid(&mut self, model: Ref, seat: Option<(Ref, Xform)>) {
        let Some(hb) = self.hums.by_model.get(&model) else { return };
        let body = hb.body;
        let had = hb.seat;
        match seat {
            None => {
                if let Some((_, j)) = had {
                    if let Some(j) = j {
                        self.world.impulse_joints.remove(j, true);
                    }
                    if let Some(b) = self.world.bodies.get_mut(body) {
                        b.set_body_type(rp::RigidBodyType::Dynamic, true);
                    }
                }
                if let Some(h) = self.hums.by_model.get_mut(&model) {
                    h.seat = None;
                }
            }
            Some((part, root_at)) => {
                let seat_body = self.body_of(part).filter(|b| self.world.bodies.get(*b).is_some_and(|b| b.is_dynamic()));
                match seat_body {
                    None => {
                        if let Some(b) = self.world.bodies.get_mut(body) {
                            if !b.is_kinematic() {
                                b.set_body_type(rp::RigidBodyType::KinematicPositionBased, true);
                            }
                            b.set_next_kinematic_position(pose(&root_at));
                            b.set_linvel(rp::Vector::ZERO, true);
                            b.set_angvel(rp::Vector::ZERO, true);
                        }
                        if let Some(h) = self.hums.by_model.get_mut(&model) {
                            h.seat = Some((part, None));
                        }
                    }
                    Some(sb) => {
                        if matches!(had, Some((p, Some(_))) if p == part) {
                            return;
                        }
                        if let Some((_, Some(j))) = had {
                            self.world.impulse_joints.remove(j, true);
                        }
                        let (Some(a), Some(b)) = (self.world.bodies.get(sb), self.world.bodies.get(body)) else { return };
                        let f1 = a.position().inverse() * pose(&root_at);
                        let _ = b;
                        if let Some(bm) = self.world.bodies.get_mut(body) {
                            bm.set_body_type(rp::RigidBodyType::Dynamic, true);
                            bm.set_position(pose(&root_at), true);
                        }
                        let joint = rp::FixedJointBuilder::new().local_frame1(f1).local_frame2(rp::Pose::IDENTITY);
                        let j = self.world.impulse_joints.insert(sb, body, joint, true);
                        if let Some(h) = self.hums.by_model.get_mut(&model) {
                            h.seat = Some((part, Some(j)));
                        }
                    }
                }
            }
        }
    }

    /// What a character's parts overlap (Touched, seats): every part's own
    /// shape where it is, grown by 0.1 studs (round 22's touch reach).
    pub fn humanoid_touching(&self, model: Ref) -> Vec<Ref> {
        let Some(h) = self.hums.by_model.get(&model) else { return Vec::new() };
        let own: HashSet<rp::ColliderHandle> = h.parts.iter().map(|p| p.collider).collect();
        let qp = self.world.query_pipeline();
        let mut out: HashSet<Ref> = HashSet::new();
        for p in &h.parts {
            let Some(c) = self.world.colliders.get(p.collider) else { continue };
            let aabb = rapier3d::parry::bounding_volume::BoundingVolume::loosened(&c.compute_aabb(), 0.1);
            let half = (aabb.maxs - aabb.mins) * 0.5;
            let centre = (aabb.maxs + aabb.mins) * 0.5;
            let probe = rp::Cuboid::new(half);
            for (hd, _) in qp.intersect_shape(rp::Pose::from_translation(centre), &probe) {
                if own.contains(&hd) {
                    continue;
                }
                // The AABB is coarse: keep what the part's own shape is
                // within 0.1 of (a resting contact keeps a small gap). A
                // part that doesn't collide (a limb the animation swings)
                // touches only what it overlaps: Studio's walking character
                // is seated by its leg entering a 2-stud seat, not by the
                // hand that swings past 0.12 beside it two frames earlier
                // (probe T12; the limbs' reach is the same in both, frame by
                // frame).
                let Some(other) = self.world.colliders.get(hd) else { continue };
                let slack = if c.is_sensor() { 0.0 } else { 0.1 };
                let close = rapier3d::parry::query::distance(c.position(), c.shape(), other.position(), other.shape())
                    .map(|d| d.distance <= slack)
                    .unwrap_or(true);
                if close {
                    if let Some(r) = self.by_collider.get(&hd) {
                        out.insert(*r);
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// The character's active contacts with other assemblies: (world
    /// point, normal toward the character).
    fn humanoid_contacts(&self, model: Ref) -> Vec<(Vec3, Vec3)> {
        let mut out = Vec::new();
        let Some(h) = self.hums.by_model.get(&model) else { return out };
        for p in &h.parts {
            for pair in self.world.narrow_phase.contact_pairs_with(p.collider) {
                if !pair.has_any_active_contact() {
                    continue;
                }
                let other = if pair.collider1 == p.collider { pair.collider2 } else { pair.collider1 };
                if self.world.colliders.get(other).and_then(|c| c.parent()) == Some(h.body) {
                    continue;
                }
                let sign = if pair.collider1 == p.collider { -1.0 } else { 1.0 };
                let Some(c1) = self.world.colliders.get(pair.collider1) else { continue };
                let pose1 = *c1.position();
                for m in pair.manifolds() {
                    let n = gv(m.data.normal) * sign;
                    for c in m.points.iter().filter(|c| c.dist <= 0.0) {
                        out.push((gv(pose1 * c.local_p1), n));
                    }
                }
            }
        }
        out
    }

    /// Whether something that moves touches the character (the arbiter's
    /// sensor list is empty only when nothing does).
    pub fn humanoid_touching_moving(&self, model: Ref) -> bool {
        let Some(h) = self.hums.by_model.get(&model) else { return false };
        for p in &h.parts {
            for pair in self.world.narrow_phase.contact_pairs_with(p.collider) {
                if !pair.has_any_active_contact() {
                    continue;
                }
                let other = if pair.collider1 == p.collider { pair.collider2 } else { pair.collider1 };
                let Some(c) = self.world.colliders.get(other) else { continue };
                if c.parent().and_then(|b| self.world.bodies.get(b)).is_some_and(|b| b.is_dynamic()) && c.parent() != Some(h.body) {
                    return true;
                }
            }
        }
        false
    }

    /// Whether a floor part is static (anchored, terrain): the arbiter's floor test.
    pub fn floor_static(&self, part: Option<Ref>) -> bool {
        match part.and_then(|p| self.body_of(p)) {
            None => true,
            Some(b) => !self.world.bodies.get(b).is_some_and(|b| b.is_dynamic()),
        }
    }

    pub fn humanoid_ctl(&mut self, model: Ref) -> Option<&mut HumCtl> {
        self.hums.by_model.get_mut(&model)?.ctl.as_mut()
    }

    pub fn set_humanoid_ctl(&mut self, model: Ref, ctl: Option<HumCtl>) {
        if let Some(h) = self.hums.by_model.get_mut(&model) {
            h.ctl = ctl;
        }
    }

    /// What Studio's force functions read off the root assembly.
    pub fn humanoid_body(&self, model: Ref) -> Option<Body> {
        let h = self.hums.by_model.get(&model)?;
        let b = self.world.bodies.get(h.body)?;
        let p = *b.position();
        let xf = xform(&p);
        let mp = b.mass_properties().local_mprops;
        let li = mp.reconstruct_inertia_matrix();
        let li = Mat3::from_cols(
            Vec3::new(li.x_axis.x, li.x_axis.y, li.x_axis.z),
            Vec3::new(li.y_axis.x, li.y_axis.y, li.y_axis.z),
            Vec3::new(li.z_axis.x, li.z_axis.y, li.z_axis.z),
        );
        let inertia = xf.rot * li * xf.rot.transpose();
        let mass = b.mass();
        let (fx, tx) = self.hums.fext.get(&h.body).copied().unwrap_or((Vec3::ZERO, Vec3::ZERO));
        let hrp = h.ctl.as_ref().map(|c| c.hrp).unwrap_or(h.root);
        let hrp_xf = self.humanoid_part_xform(hrp).unwrap_or(xf);
        Some(Body {
            mass,
            rot: xf.rot,
            pos: xf.pos,
            v: gv(b.velocity_at_point(rv(xf.pos))),
            w: gv(b.angvel()),
            inertia,
            fext: fx + Vec3::new(0.0, -mass * self.gravity, 0.0),
            text: tx,
            hrp_up: hrp_xf.rot.y_axis,
            hrp_y: hrp_xf.pos.y,
            gravity: self.gravity,
            contacts: self.humanoid_contacts(model),
        })
    }

    /// Before each world step: every controlled character's force and
    /// torque, and the opposite force on a movable floor.
    pub(super) fn humanoid_forces(&mut self, h: f32) {
        self.ghosts_follow();
        self.hums.mid.colliders.clear();
        let models: Vec<Ref> = self
            .hums
            .by_model
            .iter()
            .filter(|(_, b)| b.ctl.is_some())
            .map(|(m, _)| *m)
            .collect();
        for model in models {
            let Some(body) = self.humanoid_body(model) else { continue };
            let Some(hb) = self.hums.by_model.get_mut(&model) else { continue };
            let handle = hb.body;
            // The level the frame's arbiter asked for takes effect at once
            // when it is lower (Studio, which arms the debounce) and,
            // when higher, on a tick the new level divides
            // once the debounce has run out.
            // The requests (Studio: the lowest nonzero of the seven):
            // the Humanoid's (slot 3, per frame) and the body's own from its
            // last solve (`body_arbiter`).
            let want = hb.arb.iter().copied().filter(|l| *l != 0).fold(hb.ctl.as_ref().map_or(1, |c| c.level.max(1)), u8::min);
            if want < hb.lvl {
                hb.lvl = want;
                hb.debounce = crate::humanoid::ARB_DEBOUNCE;
            } else if want > hb.lvl && hb.debounce < 1 && (self.steps + 1) % want as u64 == 0 {
                // Studio: (level + sub-tick) % new level == 0, i.e. on
                // the tick a solve of the new level is due, and that solve
                // spans the new level's ticks although the last level-1
                // solve was one tick ago: the body gains a tick of motion.
                // (Studio, T3_turn180_cap10: the frame the turn returns to
                // level 2, 60 ticks after it began, turns 0.1748 rad where
                // level 1 alone turns 0.166 and level 2 less.)
                hb.lvl = want;
                hb.rose = true;
            }
            let lvl = hb.lvl;
            let rose = hb.rose;
            // Level 2: the assembly solves on every 2nd tick, over 2h with
            // N = 2 substeps of h, and is out of the solver on
            // the tick in between (`MidSolve`). The world here steps at
            // 240 Hz: the solve's tick carries the whole impulse (force and
            // gravity twice) and, afterwards, the second h of motion
            // (`humanoid_solved`). The tick a level rises is such a solve too.
            let two = lvl == 2 && hb.seat.is_none() && !hb.ctl.as_ref().is_some_and(|c| c.swim.is_some());
            let between = two && !rose && self.steps % 2 != 0;
            hb.span = if between { 0 } else if two { 2 } else { 1 };
            if rose || self.steps % lvl as u64 == 0 {
                hb.debounce = (hb.debounce - lvl as i32).max(0);
            }
            if between {
                if let Some(b) = self.world.bodies.get_mut(handle) {
                    hb.hold = Some((*b.position(), gv(b.linvel()), gv(b.angvel())));
                    if b.gravity_scale() != 0.0 {
                        b.set_gravity_scale(0.0, true);
                    }
                }
                self.hums.mid.colliders.extend(hb.parts.iter().map(|p| p.collider));
                continue;
            }
            // A solve begins: the velocity it begins with.
            hb.ref_v = self.world.bodies.get(handle).map(|b| (gv(b.linvel()), gv(b.angvel())));
            hb.solve0 = self.world.bodies.get(handle).map(|b| (gv(b.center_of_mass()), quat_of(b.rotation())));
            hb.hits.clear();
            let Some(ctl) = hb.ctl.as_mut() else { continue };
            let step = ctl.step;
            if let Some(v) = ctl.swim {
                if let Some(b) = self.world.bodies.get_mut(handle) {
                    b.set_linvel(rv(v), true);
                    if b.gravity_scale() != 0.0 {
                        b.set_gravity_scale(0.0, true);
                    }
                }
                continue;
            }
            let (f, scale) = if two { (ctl.hum.force(&body, &step, 2, h), 2.0) } else { (ctl.hum.force(&body, &step, 1, h), 1.0) };
            ctl.last = f;
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() {
                eprintln!(
                    "HUMSTEP {} st={:?} lvl={} pos={:.3},{:.3},{:.3} v={:.4},{:.4},{:.4} F={:.1},{:.1},{:.1} s={} w={:.4},{:.4},{:.4} up={:.4},{:.4},{:.4}",
                    self.steps, ctl.hum.state, lvl, body.pos.x, body.pos.y, body.pos.z, body.v.x, body.v.y, body.v.z,
                    f.force.x, f.force.y, f.force.z, scale, body.w.x, body.w.y, body.w.z, body.rot.y_axis.x, body.rot.y_axis.y, body.rot.y_axis.z
                );
            }
            let floor_part = ctl.floor_part;
            if let Some(b) = self.world.bodies.get_mut(handle) {
                if f.dv != Vec3::ZERO {
                    let v = gv(b.linvel()) + f.dv;
                    b.set_linvel(rv(v), true);
                }
                b.add_force(rv(f.force * scale), true);
                b.add_torque(rv(f.torque * scale), true);
                if b.gravity_scale() != scale {
                    b.set_gravity_scale(scale, true);
                }
                self.pushed.insert(handle);
            }
            let f = crate::humanoid::Force { floor: f.floor.map(|(fl, at)| (fl * scale, at)), ..f };
            if std::env::var_os("RUSTUDIO_HUM_DEBUG").is_some() && self.steps % 120 == 0 {
                let fb = floor_part.and_then(|fp| self.body_of(fp));
                eprintln!("HUMFLOOR step={} floor={:?} part={:?} body={:?} dyn={:?}", self.steps, f.floor, floor_part, fb,
                    fb.and_then(|b| self.world.bodies.get(b)).map(|b| (b.is_dynamic(), b.mass())));
            }
            if let (Some((force, at)), Some(fp)) = (f.floor, floor_part) {
                if let Some(fb) = self.body_of(fp) {
                    if fb != handle {
                        if let Some(b) = self.world.bodies.get_mut(fb) {
                            if b.is_dynamic() {
                                b.add_force_at_point(rv(force), rv(at), true);
                                self.pushed.insert(fb);
                            }
                        }
                    }
                }
            }
        }
        self.hums.fext.clear();
    }

    /// The floor under a character for the controller (Studio via
    /// `find_floor`) with the floor's velocity, friction mix and solidity.
    pub fn humanoid_floor(
        &self,
        doc: &Document,
        model: Ref,
        root: Vec3,
        rot: Quat,
        sh: &HumShape,
        had_floor: bool,
        own: &HashSet<Ref>,
    ) -> Option<(Floor, Option<Ref>)> {
        let f = self.find_floor(root, rot, sh, had_floor, own)?;
        let (v, w) = match f.part {
            Some(p) => self.floor_velocity(doc, p, f.point),
            None => (Vec3::ZERO, Vec3::ZERO),
        };
        let mu = match f.part {
            Some(p) => self.floor_mix(doc, p, f.point),
            None => mech::PART_FRICTION,
        };
        let my_mass = self.humanoid_mass(model).unwrap_or(0.0);
        let solid = match f.part.and_then(|p| self.body_of(p)) {
            None => true,
            Some(fb) => match self.world.bodies.get(fb) {
                Some(b) if b.is_dynamic() => b.mass() >= SOLID_FLOOR_MASS_MULT * my_mass,
                _ => true,
            },
        };
        Some((Floor { y: f.y, point: f.point, normal: f.normal, v, w, mu, solid }, f.part))
    }

    /// Studio's friction mix of a floor with the character's Plastic
    ///: (f·w + 0.3·1) / (w + 1),
    /// the floor's material (terrain: the voxel's) or its custom properties.
    fn floor_mix(&self, doc: &Document, part: Ref, at: Vec3) -> f32 {
        let props = if doc.class_of(part).is_some_and(|c| c == "Terrain") {
            let name = self
                .terrain_material_under(at + Vec3::Y * 0.1)
                .map(crate::terrain::material_name)
                .unwrap_or("Grass");
            crate::ltypes::LPhysicalProperties::for_material(name)
        } else {
            crate::runtime::domains::world::part::phys_props(doc, part)
        };
        let plastic = crate::ltypes::LPhysicalProperties::for_material("Plastic");
        let wsum = props.friction_weight + plastic.friction_weight;
        if wsum <= crate::humanoid::EPS {
            return 0.0;
        }
        (props.friction * props.friction_weight + plastic.friction * plastic.friction_weight) / wsum
    }
}

/// FInt SolidFloorMassMultTenth 50 / 10.
const SOLID_FLOOR_MASS_MULT: f32 = crate::humanoid::SOLID_FLOOR_MASS_MULT;

/// A body's inverse inertia tensor in world axes.
fn world_inv_inertia(b: &rp::RigidBody) -> Mat3 {
    let li = b.mass_properties().local_mprops.reconstruct_inertia_matrix();
    let li = Mat3::from_cols(
        Vec3::new(li.x_axis.x, li.x_axis.y, li.x_axis.z),
        Vec3::new(li.y_axis.x, li.y_axis.y, li.y_axis.z),
        Vec3::new(li.z_axis.x, li.z_axis.y, li.z_axis.z),
    );
    let rot = xform(b.position()).rot;
    let w = rot * li * rot.transpose();
    if w.determinant().abs() > 1.0e-12 { w.inverse() } else { Mat3::ZERO }
}

/// A rapier rotation as glam's.
fn quat_of(r: &rp::Rotation) -> glam::Quat {
    glam::Quat::from_xyzw(r.x, r.y, r.z, r.w)
}

/// Studio: how much of a box [lo, hi] (about the centre of mass) is
/// still covered by the same box moved by v and turned by w over t: the
/// moved box's centre is R·c + v·t, its half extents |R|·e (R the turn
/// w·t, Studio), the overlap's volume over the box's.
fn box_kept(lo: Vec3, hi: Vec3, v: Vec3, w: Vec3, t: f32) -> f32 {
    let r = glam::Mat3::from_quat(glam::Quat::from_scaled_axis(w * t));
    let c = (hi + lo) * 0.5;
    let e = (hi - lo) * 0.5;
    let ra = glam::Mat3::from_cols(r.x_axis.abs(), r.y_axis.abs(), r.z_axis.abs());
    let c2 = r * c + v * t;
    let e2 = ra * e;
    let (lo2, hi2) = (c2 - e2, c2 + e2);
    let o = (hi.min(hi2) - lo.max(lo2)).max(Vec3::ZERO);
    let vol = (hi - lo).x * (hi - lo).y * (hi - lo).z;
    if vol <= 0.0 {
        return 0.0;
    }
    o.x * o.y * o.z / vol
}
