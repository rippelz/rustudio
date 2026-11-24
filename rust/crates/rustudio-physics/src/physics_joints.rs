//! Joint constraints between assemblies: HingeConstraint, BallSocketConstraint,
//! PrismaticConstraint, CylindricalConstraint, UniversalConstraint,
//! PlaneConstraint and NoCollisionConstraint become rapier generic impulse
//! joints between the two attachments' assembly bodies (an anchored end hangs
//! on one fixed `ground` body, its frame in world space). SpringConstraint and
//! TorsionSpringConstraint push with impulses every substep (`apply_springs`);
//! RodConstraint is the rope solver run two-sided (physics_rope.rs).
//!
//! The joint frame is each attachment's frame in its body: Roblox's axes
//! are the attachments' (X primary: the hinge / slide axis; Y secondary:
//! the angle's zero), which is also rapier's generic-joint X axis. A
//! constraint doesn't merge assemblies, and its parts still collide (as in
//! Studio), except NoCollisionConstraint's.
//!
//! Before this, only ropes and rigid joints existed: a Hinge / BallSocket
//! bob fell freely (a pendulum fixture).
use super::*;
use rp::{GenericJointBuilder, JointAxesMask as Mask, JointAxis as Axis, MotorModel};

const JOINT_CLASSES: [&str; 10] = [
    "HingeConstraint",
    "BallSocketConstraint",
    "PrismaticConstraint",
    "CylindricalConstraint",
    "UniversalConstraint",
    "PlaneConstraint",
    "NoCollisionConstraint",
    "SpringConstraint",
    "TorsionSpringConstraint",
    // (no rapier joint; tracked for the Studio solver's force items, JOINT I.14)
    "LineForce",
];

pub(super) fn is_joint_constraint(class: &str) -> bool {
    JOINT_CLASSES.contains(&class)
}

/// A joint made for a constraint: the bodies it joins and what it was built
/// from (rebuilt when either changes).
pub(super) struct LiveJoint {
    handle: rp::ImpulseJointHandle,
    bodies: (rp::RigidBodyHandle, rp::RigidBodyHandle),
    sig: Vec<f32>,
}

impl LiveJoint {
    pub(super) fn handle(&self) -> rp::ImpulseJointHandle {
        self.handle
    }
}

/// One end of a constraint: its body (None = fixed: the ground body) and
/// the attachment's frame in that body (world frame for a fixed end).
struct End {
    body: Option<rp::RigidBodyHandle>,
    frame: rp::Pose,
    world: rp::Pose,
}

fn deg(v: f32) -> f32 {
    v.to_radians()
}

/// Roblox's math.huge limits; rapier wants finite numbers.
fn fin(v: f32) -> f32 {
    if v.is_finite() { v.abs() } else { f32::MAX }
}

fn pose_sig(p: &rp::Pose, out: &mut Vec<f32>) {
    let t = p.translation;
    let r = p.rotation;
    out.extend([t.x, t.y, t.z, r.x, r.y, r.z, r.w]);
}

impl Physics {
    fn ground(&mut self) -> rp::RigidBodyHandle {
        if let Some(g) = self.ground.filter(|g| self.world.bodies.get(*g).is_some()) {
            return g;
        }
        let g = self.world.insert_body(rp::RigidBodyBuilder::fixed().build());
        self.ground = Some(g);
        g
    }

    /// An attachment's end: its part's body and its frame there.
    fn joint_end(&self, doc: &Document, att: Ref) -> Option<End> {
        let part_d = attachment_part(doc, att)?;
        let att_world = rustudio_core::attachment::world(doc, att, false)?;
        let part_doc = xform_of(doc, part_d)?;
        // (a client-owned mover read from the client document: its part's server copy)
        let part = self.hums.desc_remap.get(&part_d).copied().unwrap_or(part_d);
        // The attachment in its part's space, from the document (both there).
        let local = pose(&part_doc).inverse() * pose(&att_world);
        let world = pose(&self.part_xform(doc, part).unwrap_or(part_doc)) * local;
        match self.parts.get(&part) {
            Some(pb) if pb.asm.is_some() => Some(End {
                body: self.body_of(part),
                frame: pb.local * local,
                world,
            }),
            // A Humanoid's part (its own body, physics_humanoid.rs): the attachment in
            // that body's frame. (Studio: the character is one assembly like any other;
            // a mover on it acts on that assembly's body, JOINT I.13 / I.14.)
            _ if self.studio.is_some() && self.humanoid_body_of(part).is_some() => {
                let hb = self.humanoid_body_of(part)?;
                let bp = *self.world.bodies.get(hb)?.position();
                Some(End { body: Some(hb), frame: bp.inverse() * world, world })
            }
            // Anchored / not simulated: the ground body, in world space.
            _ => Some(End { body: None, frame: world, world }),
        }
    }

    /// A part's collider: its own, or its collider on a Humanoid's body.
    fn collider_of_part(&self, r: Ref) -> Option<rp::ColliderHandle> {
        if let Some(pb) = self.parts.get(&r) {
            return Some(pb.collider);
        }
        let m = self.hums.of_part.get(&r)?;
        self.hums.by_model.get(m)?.parts.iter().find(|p| p.part == r).map(|p| p.collider)
    }

    /// Creates, updates or drops the joints for the joint constraints.
    pub(super) fn apply_joints(&mut self, doc: &Document) {
        if self.joint_cons.is_empty() && self.live_joints.is_empty() {
            return;
        }
        let Some(ws) = doc.workspace() else { return };
        let cons: Vec<Ref> = self.joint_cons.iter().copied().collect();
        let mut keep: HashSet<Ref> = HashSet::default();
        let mut no_collide: rustc_hash::FxHashSet<(u32, u32)> = Default::default();
        for r in cons {
            let Some(class) = doc.class_of(r) else {
                self.joint_cons.remove(&r);
                continue;
            };
            // RUSTUDIO_SOLVER=studio: only NoCollisionConstraints are read here; the other
            // classes are the solver's connectors (studio_constraints, which checks them
            // itself), so they skip the ancestor walk every frame (CHAR-3, ph_joints).
            if self.studio.is_some() && class != "NoCollisionConstraint" && is_joint_constraint(&class) {
                continue;
            }
            if !is_joint_constraint(&class) || !doc.is_ancestor(ws, r) {
                self.joint_cons.remove(&r);
                continue;
            }
            if matches!(class.as_str(), "SpringConstraint" | "TorsionSpringConstraint" | "LineForce") {
                continue; // apply_springs
            }
            // RUSTUDIO_SOLVER=studio: the constraints are the solver's connectors
            // (studio_constraints); rapier keeps only NoCollisionConstraint's joint, as
            // the narrow phase's contact filter.
            if self.studio.is_some() && class != "NoCollisionConstraint" {
                continue;
            }
            // Studio path: a NoCollisionConstraint removes the contact of its two parts
            // (ContactStore::no_collide); it is no rapier joint (the collision pipeline
            // ignores joints).
            if self.studio.is_some() {
                if bool_prop(doc, r, "Enabled", true) {
                    if let Some((p0, p1)) = joint_parts(doc, r) {
                        if let (Some(c0), Some(c1)) = (self.collider_of_part(p0), self.collider_of_part(p1)) {
                            let (a, b) = (c0.into_raw_parts().0, c1.into_raw_parts().0);
                            no_collide.insert((a.min(b), a.max(b)));
                        }
                    }
                }
                continue;
            }
            if !bool_prop(doc, r, "Enabled", true) {
                continue;
            }
            let Some((data, b0, b1, sig)) = self.joint_data(doc, r, &class) else { continue };
            keep.insert(r);
            let live = self.live_joints.get(&r).filter(|l| {
                self.world.impulse_joints.get(l.handle).is_some()
            });
            match live {
                Some(l) if l.bodies == (b0, b1) => {
                    if l.sig.len() != sig.len()
                        || l.sig.iter().zip(&sig).any(|(a, b)| (a - b).abs() > 1e-4)
                    {
                        let h = l.handle;
                        if let Some(j) = self.world.impulse_joints.get_mut(h, true) {
                            j.data = data;
                        }
                        if let Some(l) = self.live_joints.get_mut(&r) {
                            l.sig = sig;
                        }
                    }
                }
                _ => {
                    if let Some(old) = self.live_joints.remove(&r) {
                        self.world.impulse_joints.remove(old.handle, true);
                    }
                    let handle = self.world.impulse_joints.insert(b0, b1, data, true);
                    self.live_joints.insert(r, LiveJoint { handle, bodies: (b0, b1), sig });
                }
            }
        }
        if self.studio.is_some() {
            // A seated character and its vehicle form a single assembly in Studio
            // and never collide with each other.
            for hb in self.hums.by_model.values() {
                if let Some((seat_part, Some(_))) = hb.seat {
                    if let Some(body) = self.body_of(seat_part).and_then(|sb| self.world.bodies.get(sb)) {
                        for hp in &hb.parts {
                            let a = hp.collider.into_raw_parts().0;
                            for &sc in body.colliders() {
                                let b = sc.into_raw_parts().0;
                                no_collide.insert((a.min(b), a.max(b)));
                            }
                        }
                    }
                }
            }
        }
        if let Some(sv) = self.studio.as_mut() {
            sv.contacts.no_collide = no_collide;
        }
        let gone: Vec<Ref> = self.live_joints.keys().filter(|r| !keep.contains(r)).copied().collect();
        for r in gone {
            if let Some(old) = self.live_joints.remove(&r) {
                self.world.impulse_joints.remove(old.handle, true);
            }
        }
    }

    /// The rapier joint for a constraint, its bodies and its signature.
    fn joint_data(
        &mut self,
        doc: &Document,
        r: Ref,
        class: &str,
    ) -> Option<(rp::GenericJoint, rp::RigidBodyHandle, rp::RigidBodyHandle, Vec<f32>)> {
        let (e0, e1) = if class == "NoCollisionConstraint" {
            // Part0 / Part1: two bodies that don't touch each other.
            let p0 = joint_parts(doc, r).map(|p| p.0)?;
            let p1 = joint_parts(doc, r).map(|p| p.1)?;
            let b0 = self.body_of(p0);
            let b1 = self.body_of(p1);
            (
                End { body: b0, frame: rp::Pose::IDENTITY, world: rp::Pose::IDENTITY },
                End { body: b1, frame: rp::Pose::IDENTITY, world: rp::Pose::IDENTITY },
            )
        } else {
            let a0 = ref_prop(doc, r, "Attachment0")?;
            let a1 = ref_prop(doc, r, "Attachment1")?;
            (self.joint_end(doc, a0)?, self.joint_end(doc, a1)?)
        };
        // Nothing to hold: both fixed, or one assembly.
        if e0.body.is_none() && e1.body.is_none() || e0.body == e1.body {
            return None;
        }
        let e = |name: &str| enum_u32(doc.prop(r, name));
        let n = |name: &str, d: f32| num(doc, r, name, d);
        let on = |name: &str| bool_prop(doc, r, name, false);
        let mut sig = Vec::new();
        pose_sig(&e0.frame, &mut sig);
        pose_sig(&e1.frame, &mut sig);
        let mut j = match class {
            "HingeConstraint" => GenericJointBuilder::new(Mask::LOCKED_REVOLUTE_AXES),
            "BallSocketConstraint" => GenericJointBuilder::new(Mask::LOCKED_SPHERICAL_AXES),
            "PrismaticConstraint" => GenericJointBuilder::new(Mask::LOCKED_PRISMATIC_AXES),
            "CylindricalConstraint" => GenericJointBuilder::new(
                Mask::LIN_Y | Mask::LIN_Z | Mask::ANG_Y | Mask::ANG_Z,
            ),
            "UniversalConstraint" => GenericJointBuilder::new(Mask::LIN_AXES | Mask::ANG_X),
            "PlaneConstraint" => GenericJointBuilder::new(Mask::LIN_X),
            "NoCollisionConstraint" => GenericJointBuilder::new(Mask::empty()).contacts_enabled(false),
            _ => return None,
        }
        .local_frame1(e0.frame)
        .local_frame2(e1.frame);
        // ActuatorType: None 0, Motor 1, Servo 2.
        let mut actuate = |j: GenericJointBuilder,
                           axis: Axis,
                           kind: Option<u32>,
                           vel: f32,
                           motor_max: f32,
                           target: f32,
                           resp: f32,
                           servo_max: f32,
                           sig: &mut Vec<f32>| {
            sig.extend([kind.unwrap_or(0) as f32, vel, motor_max, target, resp, servo_max]);
            match kind {
                Some(1) => j
                    .motor_model(axis, MotorModel::ForceBased)
                    .motor_velocity(axis, vel, 1e5)
                    .motor_max_force(axis, fin(motor_max)),
                Some(2) => j
                    .motor_model(axis, MotorModel::AccelerationBased)
                    // Critically damped at the responsiveness.
                    .motor_position(axis, target, resp * resp * 0.25, resp)
                    .motor_max_force(axis, fin(servo_max)),
                _ => j,
            }
        };
        match class {
            "HingeConstraint" => {
                if on("LimitsEnabled") {
                    let (lo, hi) = (deg(n("LowerAngle", -45.0)), deg(n("UpperAngle", 45.0)));
                    sig.extend([lo, hi]);
                    j = j.limits(Axis::AngX, [lo.min(hi), hi.max(lo)]);
                }
                j = actuate(
                    j,
                    Axis::AngX,
                    e("ActuatorType"),
                    n("AngularVelocity", 0.0),
                    n("MotorMaxTorque", 0.0),
                    deg(n("TargetAngle", 0.0)),
                    n("AngularResponsiveness", 45.0),
                    n("ServoMaxTorque", 0.0),
                    &mut sig,
                );
            }
            "BallSocketConstraint" => {
                if on("LimitsEnabled") {
                    let a = deg(n("UpperAngle", 45.0)).abs();
                    sig.push(a);
                    j = j.limits(Axis::AngY, [-a, a]).limits(Axis::AngZ, [-a, a]);
                }
                if on("TwistLimitsEnabled") {
                    let (lo, hi) = (deg(n("TwistLowerAngle", -45.0)), deg(n("TwistUpperAngle", 45.0)));
                    sig.extend([lo, hi]);
                    j = j.limits(Axis::AngX, [lo.min(hi), hi.max(lo)]);
                }
                let friction = n("MaxFrictionTorque", 0.0);
                if friction > 0.0 {
                    sig.push(friction);
                    for axis in [Axis::AngX, Axis::AngY, Axis::AngZ] {
                        j = j
                            .motor_model(axis, MotorModel::ForceBased)
                            .motor_velocity(axis, 0.0, 1e5)
                            .motor_max_force(axis, fin(friction));
                    }
                }
            }
            "PrismaticConstraint" | "CylindricalConstraint" => {
                if on("LimitsEnabled") {
                    let (lo, hi) = (n("LowerLimit", 0.0), n("UpperLimit", 5.0));
                    sig.extend([lo, hi]);
                    j = j.limits(Axis::LinX, [lo.min(hi), hi.max(lo)]);
                }
                j = actuate(
                    j,
                    Axis::LinX,
                    e("ActuatorType"),
                    n("Velocity", 0.0),
                    n("MotorMaxForce", 0.0),
                    n("TargetPosition", 0.0),
                    n("LinearResponsiveness", 45.0),
                    n("ServoMaxForce", 0.0),
                    &mut sig,
                );
                if class == "CylindricalConstraint" {
                    if on("AngularLimitsEnabled") {
                        let (lo, hi) = (deg(n("LowerAngle", -45.0)), deg(n("UpperAngle", 45.0)));
                        sig.extend([lo, hi]);
                        j = j.limits(Axis::AngX, [lo.min(hi), hi.max(lo)]);
                    }
                    j = actuate(
                        j,
                        Axis::AngX,
                        e("AngularActuatorType"),
                        n("AngularVelocity", 0.0),
                        n("MotorMaxTorque", 0.0),
                        deg(n("TargetAngle", 0.0)),
                        n("AngularResponsiveness", 45.0),
                        n("ServoMaxTorque", 0.0),
                        &mut sig,
                    );
                }
            }
            "UniversalConstraint" => {
                if on("LimitsEnabled") {
                    let a = deg(n("MaxAngle", 45.0)).abs();
                    sig.push(a);
                    j = j.limits(Axis::AngY, [-a, a]).limits(Axis::AngZ, [-a, a]);
                }
            }
            _ => {}
        }
        let g = if e0.body.is_none() || e1.body.is_none() { Some(self.ground()) } else { None };
        let b0 = e0.body.or(g)?;
        let b1 = e1.body.or(g)?;
        Some((j.build(), b0, b1, sig))
    }

    /// SpringConstraint and TorsionSpringConstraint: forces for one substep.
    pub(super) fn apply_springs(&mut self, doc: &Document, _h: f32) {
        if self.joint_cons.is_empty() || self.studio.is_some() {
            return;
        }
        let springs: Vec<Ref> = self
            .joint_cons
            .iter()
            .copied()
            .filter(|r| {
                doc.class_of(*r)
                    .is_some_and(|c| c == "SpringConstraint" || c == "TorsionSpringConstraint")
            })
            .collect();
        for r in springs {
            if !bool_prop(doc, r, "Enabled", true) {
                continue;
            }
            let (Some(a0), Some(a1)) = (ref_prop(doc, r, "Attachment0"), ref_prop(doc, r, "Attachment1")) else {
                continue;
            };
            let (Some(e0), Some(e1)) = (self.joint_end(doc, a0), self.joint_end(doc, a1)) else {
                continue;
            };
            if e0.body == e1.body {
                continue;
            }
            let p0 = e0.world.translation;
            let p1 = e1.world.translation;
            let class = doc.class_of(r).unwrap_or_default();
            let n = |name: &str, d: f32| num(doc, r, name, d);
            let vel = |b: Option<rp::RigidBodyHandle>, p: rp::Vector| {
                b.and_then(|b| self.world.bodies.get(b))
                    .map(|b| b.velocity_at_point(p))
                    .unwrap_or(rp::Vector::ZERO)
            };
            let angvel = |b: Option<rp::RigidBodyHandle>| {
                b.and_then(|b| self.world.bodies.get(b))
                    .map(|b| b.angvel())
                    .unwrap_or(rp::Vector::ZERO)
            };
            if class == "SpringConstraint" {
                let d = p1 - p0;
                let len = d.length();
                if len < 1e-6 {
                    continue;
                }
                let dir = d / len;
                // Stretch pulls the ends together; damping opposes separation.
                let sep = (vel(e1.body, p1) - vel(e0.body, p0)).dot(dir);
                let mut f = n("Stiffness", 0.0) * (len - n("FreeLength", 1.0)) + n("Damping", 0.0) * sep;
                let max = fin(n("MaxForce", f32::INFINITY));
                f = f.clamp(-max, max);
                // A force over the step (cleared by the next apply_movers), so
                // gravity and the spring integrate together.
                let force = dir * f;
                if std::env::var_os("RUSTUDIO_SPRING_DEBUG").is_some() {
                    eprintln!("SPRING step={} len={len:.4} sep={sep:.4} f={f:.1} bodies {:?} {:?} m1={:?} v1={:?}", self.steps, e0.body.is_some(), e1.body.is_some(),
                        e1.body.and_then(|b| self.world.bodies.get(b)).map(|b| b.mass()), e1.body.and_then(|b| self.world.bodies.get(b)).map(|b| b.linvel().y));
                }
                for (b, p, s) in [(e0.body, p0, 1.0), (e1.body, p1, -1.0)] {
                    if let Some(bh) = b {
                        if let Some(b) = self.world.bodies.get_mut(bh) {
                            b.add_force_at_point(force * s, p, true);
                            self.pushed.insert(bh);
                        }
                    }
                }
            } else {
                // TorsionSpring: turns Attachment1's Y back to Attachment0's
                // about their X axes.
                let x0 = e0.world.rotation * rp::Vector::X;
                let y0 = e0.world.rotation * rp::Vector::Y;
                let y1 = e1.world.rotation * rp::Vector::Y;
                let y1p = (y1 - x0 * y1.dot(x0)).normalize_or_zero();
                if y1p == rp::Vector::ZERO {
                    continue;
                }
                let angle = y0.cross(y1p).dot(x0).atan2(y0.dot(y1p));
                let w = (angvel(e1.body) - angvel(e0.body)).dot(x0);
                let mut t = -n("Stiffness", 0.0) * angle - n("Damping", 0.0) * w;
                let max = fin(n("MaxTorque", f32::INFINITY));
                t = t.clamp(-max, max);
                let torque = x0 * t;
                for (b, s) in [(e0.body, -1.0), (e1.body, 1.0)] {
                    if let Some(bh) = b {
                        if let Some(b) = self.world.bodies.get_mut(bh) {
                            b.add_torque(torque * s, true);
                            self.pushed.insert(bh);
                        }
                    }
                }
            }
        }
    }
}

/// Studio solver path (RUSTUDIO_SOLVER=studio): the constraint Instances as
/// Studio's connectors (crates/rustudio-physics/src/solver/joints, JOINT I). Every frame each tracked
/// constraint's properties and attachments are read into a `Desc`; the solver keeps
/// one connector set per Instance (kept impulses survive property changes).
fn studio_end(e: &End) -> solver_joints::End {
    let t = e.frame.translation;
    let r = e.frame.rotation;
    solver_joints::End {
        body: e.body,
        pos: Vec3::new(t.x, t.y, t.z),
        rot: Mat3::from_quat(Quat::from_xyzw(r.x, r.y, r.z, r.w)),
        mass: 0.0,
    }
}

use crate::solver::joints as solver_joints;
use solver_joints::instance::Class as SClass;

/// The last frame's connector descriptions (`studio_constraints`' change tracking).
pub(super) struct SjCache {
    sig: (u64, usize, usize),
    refs: Vec<Ref>,
    known: HashSet<Ref>,
    descs: Vec<Option<solver_joints::Desc>>,
    /// Indices read every frame (ends on a Humanoid's body, client-owned movers).
    volatile: Vec<usize>,
    /// Constraint / attachment / part → the indices whose description reads it.
    deps: HashMap<Ref, Vec<u32>>,
}

impl Physics {
    pub(super) fn studio_constraints(&mut self, doc: &Document, marks: doc::Marks) {
        if self.studio.is_none() {
            return;
        }
        let Some(ws) = doc.workspace() else { return };
        let mut seen: HashSet<Ref> = HashSet::default();
        let mut refs: Vec<Ref> = self.joint_cons.iter().chain(self.ropes.iter()).chain(self.movers.keys()).copied().filter(|r| seen.insert(*r)).collect();
        // Connectors are allocated (and keyed: the batch's sort key, DUAL §1 step 5) in
        // document order, standing in for Studio's creation order: new Instances are
        // upserted in the order the Workspace lists them.
        // (A Ref is new when last frame's list did not have it: one with no description
        // never gets a solver Instance, and testing that made every frame walk the whole
        // Workspace — the 3.2 M instructions a step of og.rbxl's ph_joints.)
        let new = match &self.sj_cache {
            Some(c) => refs.len() > c.known.len() || refs.iter().any(|r| !c.known.contains(r)),
            None => true,
        };
        if new {
            refs = doc.dom.descendants_of(ws).map(|i| i.referent()).filter(|r| seen.contains(r)).collect();
            // Constraints outside Workspace (a character's ragdoll sockets kept in a
            // Model, a destroyed original) are no connectors: forgotten here, or every
            // frame finds them "new" and walks the Workspace again (round HANG: the
            // client's world in a test place, 330k instances, every frame it stepped). A tree
            // mark brings one back if it is ever parented in (`note_other`).
            if refs.len() < seen.len() {
                let inside: HashSet<Ref> = refs.iter().copied().collect();
                self.joint_cons.retain(|r| inside.contains(r));
                self.ropes.retain(|r| inside.contains(r));
            }
        } else if let Some(c) = &self.sj_cache {
            refs = c.refs.iter().copied().filter(|r| seen.contains(r)).collect();
        }
        // Change tracking: a description is read again only when a journal mark touches
        // the constraint, one of its attachments or their parts; all of them when the set
        // of constraints, the bodies (dyn_version) or the collider / body counts change,
        // or the journal was lost. Ends on a Humanoid's body and client-owned movers read
        // live state: read every frame.
        let sig = (self.dyn_version, self.world.bodies.len(), self.world.colliders.len());
        let mut wake_joints = HashSet::default();
        let full = match (&self.sj_cache, &marks) {
            (Some(c), doc::Marks::Some(ms)) => {
                let mut rebound = false;
                for mark in ms {
                    let doc::Mark::Prop(r, key) = mark else { continue };
                    if matches!(key.as_str(), "Attachment0" | "Attachment1") && c.known.contains(r) {
                        // Rebinding changes reverse dependencies as well as the frame.
                        rebound = true;
                        wake_joints.insert(*r);
                    }
                    if *key != "CFrame" {
                        continue;
                    }
                    let Some(indices) = c.deps.get(r) else { continue };
                    // Moving bodies also publish CFrame marks. Their local attachment
                    // frames can pick up rounding noise when rebuilt from world poses.
                    // Only an attachment edit or a fixed support's move wakes the joint.
                    if doc.class_of(*r).is_some_and(|class| {
                        doc::is_a(&class, "Attachment")
                            || doc::is_a(&class, "BasePart") && self.body_of(*r).is_none()
                    }) {
                        wake_joints.extend(indices.iter().map(|&i| c.refs[i as usize]));
                    }
                }
                c.sig != sig || new || c.refs != refs || rebound
            }
            _ => true,
        };
        let mut cache = if full { None } else { self.sj_cache.take() };
        let dirty: Vec<usize> = match (&cache, &marks) {
            (Some(c), doc::Marks::Some(ms)) => {
                let mut d: Vec<usize> = c.volatile.clone();
                for m in ms {
                    let r = match m {
                        doc::Mark::Prop(r, _) | doc::Mark::Tree(r) => *r,
                    };
                    if let Some(v) = c.deps.get(&r) {
                        d.extend(v.iter().map(|&i| i as usize));
                    }
                }
                d.sort_unstable();
                d.dedup();
                d
            }
            _ => Vec::new(),
        };
        let mut c = match cache.take() {
            Some(mut c) => {
                for &i in &dirty {
                    let r = c.refs[i];
                    c.descs[i] = self.studio_desc_of(doc, ws, r);
                }
                c
            }
            None => {
                let mut c = SjCache { sig, known: refs.iter().copied().collect(), refs: refs.clone(), descs: Vec::with_capacity(refs.len()), volatile: Vec::new(), deps: HashMap::default() };
                for (i, &r) in refs.iter().enumerate() {
                    c.descs.push(self.studio_desc_of(doc, ws, r));
                    let mut dep = vec![r];
                    let mut live = self.hums.owned_movers.contains(&r);
                    let parent = doc.dom.get_by_ref(r).map(|x| x.parent());
                    for a in [ref_prop(doc, r, "Attachment0"), ref_prop(doc, r, "Attachment1")].into_iter().flatten() {
                        dep.push(a);
                        if let Some(p) = attachment_part(doc, a) {
                            dep.push(p);
                            live |= self.hums.desc_remap.contains_key(&p) || self.humanoid_body_of(p).is_some();
                        }
                    }
                    if let Some(p) = parent {
                        dep.push(p);
                        live |= self.hums.desc_remap.contains_key(&p) || self.humanoid_body_of(p).is_some();
                    }
                    if live {
                        c.volatile.push(i);
                    }
                    for d in dep {
                        c.deps.entry(d).or_default().push(i as u32);
                    }
                }
                c
            }
        };
        // Seated characters on dynamic seats: a rigid 6-DOF weld between the seat
        // assembly and the character body (matching Studio's merged assembly).
        let seat_welds: Vec<(Ref, solver_joints::Desc)> = self
            .hums
            .by_model
            .iter()
            .filter_map(|(&model, hb)| {
                let Some((seat_part, Some(_))) = hb.seat else { return None };
                let sb = self.body_of(seat_part)?;
                if !self.world.bodies.get(sb).is_some_and(|b| b.is_dynamic()) {
                    return None;
                }
                let t = hb.seat_local.translation;
                let q = hb.seat_local.rotation;
                let a = solver_joints::End {
                    body: Some(sb),
                    pos: Vec3::new(t.x, t.y, t.z),
                    rot: Mat3::from_quat(Quat::from_xyzw(q.x, q.y, q.z, q.w)),
                    mass: 0.0,
                };
                let b = solver_joints::End {
                    body: Some(hb.body),
                    pos: Vec3::ZERO,
                    rot: Mat3::IDENTITY,
                    mass: 0.0,
                };
                Some((model, solver_joints::Desc { class: SClass::Fixed, a, b, in_world: true }))
            })
            .collect();
        let Some(sv) = self.studio.as_mut() else { return };
        sv.joints.begin_sync();
        let changed: HashSet<usize> = if full { (0..c.refs.len()).collect() } else { dirty.iter().copied().collect() };
        for (i, (r, d)) in c.refs.iter().zip(&c.descs).enumerate() {
            let Some(d) = d else { continue };
            if changed.contains(&i) {
                for body in sv.joints.upsert(*r, d, wake_joints.contains(r)).into_iter().flatten() {
                    sv.property_wake(body);
                }
            } else {
                sv.joints.keep(*r);
            }
        }
        for (r, d) in &self.hums.foreign_descs {
            for body in sv.joints.upsert(*r, d, true).into_iter().flatten() {
                sv.property_wake(body);
            }
        }
        for (r, d) in &seat_welds {
            for body in sv.joints.upsert(*r, d, true).into_iter().flatten() {
                sv.property_wake(body);
            }
        }
        sv.joints.end_sync();
        c.sig = sig;
        self.sj_cache = Some(c);
    }

    /// One constraint's description (a client-owned mover: as the client's DataModel has
    /// it, physics_humanoid.rs `set_owned_movers`, `owned_descs`).
    fn studio_desc_of(&mut self, doc: &Document, ws: Ref, r: Ref) -> Option<solver_joints::Desc> {
        if self.hums.owned_movers.contains(&r) {
            return self.hums.owned_descs.get(&r).cloned();
        }
        self.studio_desc(doc, ws, r)
    }

    /// The part's assembly mass (Studio, the spring arbiter's; JOINT §5):
    /// the body's for a simulated part, the collider's own for an anchored one.
    fn studio_mass(&self, doc: &Document, att: Ref) -> f32 {
        let Some(part) = attachment_part(doc, att) else { return 0.0 };
        if let Some(b) = self.body_of(part).and_then(|b| self.world.bodies.get(b)) {
            return b.mass();
        }
        self.parts.get(&part).and_then(|pb| self.world.colliders.get(pb.collider)).map_or(0.0, |c| c.mass())
    }

    pub(super) fn studio_desc(&mut self, doc: &Document, ws: Ref, r: Ref) -> Option<solver_joints::Desc> {
        let class = doc.class_of(r)?;
        if !doc.is_ancestor(ws, r) || class == "NoCollisionConstraint" {
            return None;
        }
        let n = |name: &str, d: f32| num(doc, r, name, d);
        let on = |name: &str| bool_prop(doc, r, name, false);
        let e = |name: &str, d: u32| enum_u32(doc.prop(r, name)).unwrap_or(d);
        let v = |name: &str| vec3_of(doc.prop(r, name)).filter(|v| !v.is_nan()).unwrap_or(Vec3::ZERO);
        let inf = f32::INFINITY;
        let legacy = class.starts_with("Body") || class == "RocketPropulsion";
        let mut enabled = legacy || bool_prop(doc, r, "Enabled", true);
        let a0 = ref_prop(doc, r, "Attachment0");
        let a1 = ref_prop(doc, r, "Attachment1");
        // One-attachment classes / modes: B = the world (I.10, I.11, I.12, I.14)
        let one = match class.as_str() {
            "AlignPosition" | "AlignOrientation" => e("Mode", 1) == 0,
            "VectorForce" | "Torque" | "LinearVelocity" => true,
            "AngularVelocity" => a1.is_none(),
            _ => false,
        };
        let end = |me: &Self, a: Option<Ref>| a.and_then(|a| me.joint_end(doc, a)).map(|x| {
            let mut s = studio_end(&x);
            s.mass = a.map_or(0.0, |a| me.studio_mass(doc, a));
            // [J3] the attachment writers copy the Attachment's own f32 CFrame:
            // when the attachment sits directly in a part that is its body's frame, take
            // that matrix as is rather than through the quaternion of `joint_end`
            if let Some(a) = a {
                if let Some(f) = me.attachment_frame_f32(doc, a) {
                    if s.body.is_some() {
                        s.rot = f.0;
                        s.pos = f.1;
                    } else if let Some((pr, pp)) = me.part_frame_f32(doc, a) {
                        // An anchored part's end is in world space: its f32 CFrame times the
                        // attachment's (lane STACK: through the quaternion, a −90° attachment's
                        // cos(π/2) = 6.1e-17 became −1.19e-7 and hinge_limits' Align2Axes row
                        // got a 2⁻²³ position target at rest; Studio's stays at 1e-17).
                        s.rot = pr * f.0;
                        s.pos = pr * f.1 + pp;
                    }
                }
            }
            s
        });
        // A legacy mover acts on its parent part's body (I.15: Inst = the part)
        let mover_end = || -> Option<solver_joints::End> {
            let part = doc.dom.get_by_ref(r)?.parent();
            let part = self.hums.desc_remap.get(&part).copied().unwrap_or(part);
            let body = self.body_of(part)?;
            // [J3] the part's frame in its body (slot 78 reads the part's pose, Studio)
            let (pos, rot) = match self.parts.get(&part) {
                Some(pb) => {
                    let t = pb.local.translation;
                    let q = pb.local.rotation;
                    (Vec3::new(t.x, t.y, t.z), Mat3::from_quat(Quat::from_xyzw(q.x, q.y, q.z, q.w)))
                }
                None => (Vec3::ZERO, Mat3::IDENTITY),
            };
            Some(solver_joints::End { body: Some(body), pos, rot, mass: 0.0 })
        };
        let ea = if legacy { Some(mover_end().unwrap_or(solver_joints::End::WORLD)) } else { end(self, a0) };
        let eb = if legacy || one { Some(solver_joints::End::WORLD) } else { end(self, a1) };
        if ea.is_none() || eb.is_none() {
            enabled = false;
        }
        let (a, b) = (ea.unwrap_or(solver_joints::End::WORLD), eb.unwrap_or(solver_joints::End::WORLD));
        // both fixed, or one assembly: not in the world (I.16)
        if !legacy && (a.body.is_none() && b.body.is_none() || a.body.is_some() && a.body == b.body) {
            enabled = false;
        }
        let class = match class.as_str() {
            "HingeConstraint" => SClass::Hinge {
                actuator: e("ActuatorType", 0),
                ang_resp: n("AngularResponsiveness", 45.0),
                ang_speed: n("AngularSpeed", 0.0),
                ang_vel: n("AngularVelocity", 0.0),
                lower: n("LowerAngle", -45.0),
                upper: n("UpperAngle", 45.0),
                motor_max_acc: n("MotorMaxAcceleration", inf),
                motor_max_torque: n("MotorMaxTorque", 0.0),
                restitution: n("Restitution", 0.0),
                servo_max_torque: n("ServoMaxTorque", 0.0),
                target: n("TargetAngle", 0.0),
                limits: on("LimitsEnabled"),
            },
            "BallSocketConstraint" => SClass::BallSocket {
                max_friction: n("MaxFrictionTorque", 0.0),
                restitution: n("Restitution", 0.0),
                twist_lower: n("TwistLowerAngle", -45.0),
                twist_upper: n("TwistUpperAngle", 45.0),
                upper: n("UpperAngle", 45.0),
                limits: on("LimitsEnabled"),
                twist_limits: on("TwistLimitsEnabled"),
            },
            "PrismaticConstraint" | "CylindricalConstraint" | "SlidingBallConstraint" => SClass::Sliding {
                variant: match class.as_str() {
                    "PrismaticConstraint" => 0,
                    "CylindricalConstraint" => 1,
                    _ => 2,
                },
                actuator: e("ActuatorType", 0),
                lin_resp: n("LinearResponsiveness", 45.0),
                lower: n("LowerLimit", 0.0),
                upper: n("UpperLimit", 5.0),
                motor_max_acc: n("MotorMaxAcceleration", inf),
                motor_max_force: n("MotorMaxForce", 0.0),
                restitution: n("Restitution", 0.0),
                servo_max_force: n("ServoMaxForce", 0.0),
                speed: n("Speed", 0.0),
                target: n("TargetPosition", 0.0),
                velocity: n("Velocity", 0.0),
                limits: on("LimitsEnabled"),
                ang: (class == "CylindricalConstraint").then(|| solver_joints::instance::CylAngular {
                    actuator: e("AngularActuatorType", 0),
                    resp: n("AngularResponsiveness", 45.0),
                    restitution: n("AngularRestitution", 0.0),
                    speed: n("AngularSpeed", 0.0),
                    velocity: n("AngularVelocity", 0.0),
                    lower: n("LowerAngle", -45.0),
                    upper: n("UpperAngle", 45.0),
                    motor_max_acc: n("MotorMaxAngularAcceleration", inf),
                    motor_max_torque: n("MotorMaxTorque", 0.0),
                    servo_max_torque: n("ServoMaxTorque", 0.0),
                    target: n("TargetAngle", 0.0),
                    inclination: n("InclinationAngle", 0.0),
                    limits: on("AngularLimitsEnabled"),
                }),
            },
            "UniversalConstraint" => SClass::Universal { max_angle: n("MaxAngle", 45.0), restitution: n("Restitution", 0.0), limits: on("LimitsEnabled") },
            "PlaneConstraint" => SClass::Plane,
            "RodConstraint" => SClass::Rod { length: n("Length", 5.0), limit0: n("LimitAngle0", 90.0), limit1: n("LimitAngle1", 90.0), limits: on("LimitsEnabled") },
            "RopeConstraint" => SClass::Rope {
                length: n("Length", 5.0),
                restitution: n("Restitution", 0.0),
                winch: on("WinchEnabled"),
                winch_force: n("WinchForce", 1000.0),
                winch_resp: n("WinchResponsiveness", 45.0),
                winch_speed: n("WinchSpeed", 2.0),
                winch_target: n("WinchTarget", 5.0),
            },
            "SpringConstraint" => SClass::Spring {
                damping: n("Damping", 0.0),
                free_length: n("FreeLength", 1.0),
                max_force: n("MaxForce", inf),
                max_length: n("MaxLength", 5.0),
                min_length: n("MinLength", 0.0),
                stiffness: n("Stiffness", 0.0),
                limits: on("LimitsEnabled"),
            },
            "TorsionSpringConstraint" => SClass::Torsion {
                damping: n("Damping", 0.01),
                max_angle: n("MaxAngle", 45.0),
                max_torque: n("MaxTorque", inf),
                restitution: n("Restitution", 0.0),
                stiffness: n("Stiffness", 100.0),
                limits: on("LimitsEnabled"),
            },
            "AlignPosition" => SClass::AlignPosition {
                one_attachment: one,
                position: v("Position"),
                magnitude_mode: e("ForceLimitMode", 0) == 0,
                relative_to: e("ForceRelativeTo", 2) as u8,
                max_force: n("MaxForce", 10000.0),
                max_axes_force: v("MaxAxesForce"),
                max_velocity: n("MaxVelocity", inf),
                responsiveness: n("Responsiveness", 10.0),
                at_com: on("ApplyAtCenterOfMass"),
                reaction: on("ReactionForceEnabled"),
                rigid: on("RigidityEnabled"),
            },
            "AlignOrientation" => {
                // OneAttachment's goal frame: PrimaryAxis / SecondaryAxis when set, else CFrame
                let target = match (vec3_of(doc.prop(r, "PrimaryAxis")), vec3_of(doc.prop(r, "SecondaryAxis"))) {
                    (Some(x), Some(y)) if x.length_squared() > 0.0 && y.length_squared() > 0.0 => {
                        let x = x.normalize();
                        let z = x.cross(y).normalize_or_zero();
                        Mat3::from_cols(x, z.cross(x), z)
                    }
                    _ => Mat3::from_quat(rot_prop(doc, r, "CFrame")),
                };
                SClass::AlignOrientation {
                    one_attachment: one,
                    target,
                    align_type: e("AlignType", 5),
                    primary_only: on("PrimaryAxisOnly"),
                    max_ang_vel: n("MaxAngularVelocity", inf),
                    max_torque: n("MaxTorque", 10000.0),
                    responsiveness: n("Responsiveness", 10.0),
                    reaction: on("ReactionTorqueEnabled"),
                    rigid: on("RigidityEnabled"),
                }
            }
            "AngularVelocity" => SClass::AngularVelocity { ang_vel: v("AngularVelocity"), max_torque: n("MaxTorque", 0.0), relative_to: e("RelativeTo", 2) as u8, reaction: on("ReactionTorqueEnabled") },
            "LinearVelocity" => SClass::LinearVelocity {
                mode: e("VelocityConstraintMode", 2),
                relative_to: e("RelativeTo", 2),
                force_limit_mode: e("ForceLimitMode", 0),
                limits: bool_prop(doc, r, "ForceLimitsEnabled", true),
                max_force: n("MaxForce", 1000.0),
                max_axes_force: vec3_of(doc.prop(r, "MaxAxesForce")).unwrap_or(Vec3::splat(1000.0)),
                vector_velocity: v("VectorVelocity"),
                line_direction: vec3_of(doc.prop(r, "LineDirection")).unwrap_or(Vec3::X),
                line_velocity: n("LineVelocity", 0.0),
                reaction: on("ReactionForceEnabled"),
                // [J3] Plane mode (wrapper): PlaneVelocity, MaxPlanarAxesForce (Vector2),
                // Primary / SecondaryTangentAxis
                plane_velocity: vec2_prop(doc, r, "PlaneVelocity").unwrap_or([0.0, 0.0]),
                max_planar_axes_force: vec2_prop(doc, r, "MaxPlanarAxesForce").unwrap_or([1000.0, 1000.0]),
                primary_tangent: vec3_of(doc.prop(r, "PrimaryTangentAxis")).unwrap_or(Vec3::X),
                secondary_tangent: vec3_of(doc.prop(r, "SecondaryTangentAxis")).unwrap_or(Vec3::Y),
            },
            "VectorForce" => SClass::VectorForce { force: v("Force"), relative_to: e("RelativeTo", 0) as u8, at_com: on("ApplyAtCenterOfMass") },
            "Torque" => SClass::Torque { torque: v("Torque"), relative_to: e("RelativeTo", 0) as u8 },
            "LineForce" => SClass::LineForce {
                magnitude: n("Magnitude", 0.0),
                max_force: n("MaxForce", 0.0),
                inverse_square: on("InverseSquareLaw"),
                at_com: on("ApplyAtCenterOfMass"),
                reaction: on("ReactionForceEnabled"),
            },
            // I.15 [J2]: the per-step update (vt slot 78) — BodyVelocity Studio (connector =
            // Velocity, = clampF(MaxForce)), BodyForce Studio (F = Force, world)
            "BodyVelocity" => SClass::BodyVelocity { velocity: v("Velocity"), max_force: vec3_of(doc.prop(r, "MaxForce")).unwrap_or(Vec3::splat(4000.0)), p: n("P", 1250.0) },
            "BodyForce" => SClass::BodyForce { force: v("Force") },
            // [J3] slot 78: BodyThrust Studio, BodyAngularVelocity Studio, BodyPosition Studio
            "BodyGyro" => SClass::BodyGyro {
                cframe: Mat3::from_quat(rot_prop(doc, r, "CFrame")),
                max_torque: vec3_of(doc.prop(r, "MaxTorque")).unwrap_or(Vec3::new(400000.0, 400000.0, 400000.0)),
                p: n("P", 3000.0),
                d: n("D", 500.0),
            },
            "BodyThrust" => SClass::BodyThrust { force: v("Force"), location: v("Location") },
            "BodyAngularVelocity" => SClass::BodyAngularVelocity { ang_vel: vec3_of(doc.prop(r, "AngularVelocity")).unwrap_or(Vec3::new(0.0, 2.0, 0.0)), max_torque: vec3_of(doc.prop(r, "MaxTorque")).unwrap_or(Vec3::new(4000.0, 4000.0, 4000.0)), p: n("P", 1250.0) },
            "BodyPosition" => SClass::BodyPosition { position: vec3_of(doc.prop(r, "Position")).unwrap_or(Vec3::new(0.0, 50.0, 0.0)), max_force: vec3_of(doc.prop(r, "MaxForce")).unwrap_or(Vec3::new(4000.0, 4000.0, 4000.0)), p: n("P", 10000.0), d: n("D", 1250.0) },
            c if c.starts_with("Body") || c == "RocketPropulsion" => {
                if c == "RocketPropulsion" {
                    uncovered_once(&UNCOVERED_ROCKET, "RocketPropulsion");
                }
                SClass::LegacyMover
            }
            _ => return None,
        };
        if let SClass::AlignOrientation { align_type: 4, .. } = &class {
            uncovered_once(&UNCOVERED_LOOKAT, "AlignOrientation with AlignType PrimaryAxisLookAt");
        }
        // VectorForce / Torque with RelativeTo Attachment1 read B's frame
        let b = match &class {
            SClass::VectorForce { relative_to: 1, .. } | SClass::Torque { relative_to: 1, .. } => end(self, a1).unwrap_or(b),
            SClass::LineForce { .. } => end(self, a1).unwrap_or(b),
            SClass::LinearVelocity { relative_to: 1, .. } => end(self, a1).unwrap_or(b),
            _ => b,
        };
        Some(solver_joints::Desc { class, a, b, in_world: enabled })
    }
}

static UNCOVERED_ROCKET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static UNCOVERED_LOOKAT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the Studio solver (the default) does not simulate yet but rapier does: said
/// once per process, so a place that relies on it can be run with the old physics.
fn uncovered_once(flag: &std::sync::atomic::AtomicBool, what: &str) {
    if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rustudio: {what} is not simulated by the Studio solver yet (it exerts nothing); \
             run with RUSTUDIO_SOLVER=rapier for the old physics, which does"
        );
    }
}

impl Physics {
    /// The Attachment's CFrame (part space, f32 rows) when its parent is a part whose
    /// pose in its body is the identity; `None` otherwise.
    /// The f32 CFrame of an attachment's part, from the document (row-major orientation).
    fn part_frame_f32(&self, doc: &Document, att: Ref) -> Option<(Mat3, Vec3)> {
        let part = doc.dom.get_by_ref(att)?.parent();
        let Variant::CFrame(cf) = doc.prop(part, "CFrame")? else { return None };
        let m = cf.orientation;
        let rot = Mat3::from_cols(Vec3::new(m.x.x, m.y.x, m.z.x), Vec3::new(m.x.y, m.y.y, m.z.y), Vec3::new(m.x.z, m.y.z, m.z.z));
        Some((rot, Vec3::new(cf.position.x, cf.position.y, cf.position.z)))
    }

    fn attachment_frame_f32(&self, doc: &Document, att: Ref) -> Option<(Mat3, Vec3)> {
        let part = doc.dom.get_by_ref(att)?.parent();
        if !doc::is_a(&doc.dom.get_by_ref(part)?.class, "BasePart") {
            return None;
        }
        let pb = self.parts.get(&part)?;
        if pb.local.translation != rp::Pose::IDENTITY.translation || pb.local.rotation != rp::Pose::IDENTITY.rotation {
            return None;
        }
        let Variant::CFrame(cf) = doc.prop(att, "CFrame")? else { return None };
        let m = cf.orientation;
        let rot = Mat3::from_cols(Vec3::new(m.x.x, m.y.x, m.z.x), Vec3::new(m.x.y, m.y.y, m.z.y), Vec3::new(m.x.z, m.y.z, m.z.z));
        Some((rot, Vec3::new(cf.position.x, cf.position.y, cf.position.z)))
    }
}

fn vec2_prop(doc: &Document, r: Ref, name: &str) -> Option<[f32; 2]> {
    match doc.prop(r, name)? {
        Variant::Vector2(v) => Some([v.x, v.y]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::types::{CFrame, Vector3};
    use rbx_dom_weak::InstanceBuilder;

    fn part(name: &str, pos: Vec3, anchored: bool) -> InstanceBuilder {
        InstanceBuilder::new("Part")
            .with_name(name)
            .with_property("Anchored", anchored)
            .with_property("Size", Vector3::new(1.0, 1.0, 1.0))
            .with_property("CFrame", CFrame::new(Vector3::new(pos.x, pos.y, pos.z), CFrame::identity().orientation))
    }

    /// The pendulum fixture: an anchored pivot at (0,300,0), a
    /// 1-stud bob at (0,300,5) whose attachment sits on the pivot.
    fn pendulum(class: &str, props: &[(&str, Variant)]) -> (Document, Ref) {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let top = doc.dom.insert(ws, part("Top", Vec3::new(0.0, 300.0, 0.0), true));
        let bob = doc.dom.insert(ws, part("Bob", Vec3::new(0.0, 300.0, 5.0), false));
        let a0 = doc.dom.insert(top, InstanceBuilder::new("Attachment"));
        let a1 = doc.dom.insert(bob, InstanceBuilder::new("Attachment").with_property(
            "CFrame",
            CFrame::new(Vector3::new(0.0, 0.0, -5.0), CFrame::identity().orientation),
        ));
        let mut b = InstanceBuilder::new(class)
            .with_property("Attachment0", a0)
            .with_property("Attachment1", a1);
        for (k, v) in props {
            b = b.with_property(*k, v.clone());
        }
        doc.dom.insert(top, b);
        (doc, bob)
    }

    fn run(doc: &mut Document, steps: usize) -> Vec<Vec3> {
        let mut p = Physics::new(doc);
        let mut out = Vec::new();
        for _ in 0..steps {
            p.sync(doc);
            p.step(doc, 1.0 / 60.0);
            out.push(Vec3::ZERO);
        }
        out
    }

    fn pos(doc: &Document, r: Ref) -> Vec3 {
        xform_of(doc, r).unwrap().pos
    }

    /// Studio (an offline Studio's client, 2026-11-09): the hinge bob swings in the YZ
    /// plane about the pivot and never falls away (y ≥ 295, |z| ≤ 5).
    #[test]
    fn hinge_and_ball_socket_pendulums_swing_about_the_pivot() {
        for class in ["HingeConstraint", "BallSocketConstraint"] {
            let (mut doc, bob) = pendulum(class, &[]);
            let mut p = Physics::new(&mut doc);
            let mut low: f32 = 300.0;
            let mut bottom_at = None;
            for i in 0..120 {
                p.sync(&mut doc);
                p.step(&mut doc, 1.0 / 60.0);
                let b = pos(&doc, bob);
                if bottom_at.is_none() && b.z <= 0.0 {
                    bottom_at = Some((i + 1) as f32 / 60.0);
                }
                low = low.min(b.y);
                let r = (b - Vec3::new(0.0, 300.0, 0.0)).length();
                assert!((r - 5.0).abs() < 0.3, "{class}: bob left the pivot: {b:?}");
                if class == "HingeConstraint" {
                    assert!(b.x.abs() < 0.05, "hinge swings in its plane: {b:?}");
                }
            }
            assert!(low < 295.5, "{class} swung down: {low}");
            // A quarter of a 90° swing: T/4 = 1.18·2π√(L/g)/4 ≈ 0.30 s.
            let t = bottom_at.expect("reached the bottom");
            assert!((0.25..=0.36).contains(&t), "{class}: bottom at {t} s");
        }
    }

    #[test]
    fn prismatic_slides_only_along_its_axis_within_limits() {
        // Axis X along world Y (attachments turned): the bob drops to the limit.
        let rot = CFrame::new(Vector3::new(0.0, 0.0, 0.0), rbx_dom_weak::types::Matrix3::new(
            Vector3::new(0.0, 1.0, 0.0), Vector3::new(-1.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0)));
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let top = doc.dom.insert(ws, part("Top", Vec3::new(0.0, 300.0, 0.0), true));
        let bob = doc.dom.insert(ws, part("Bob", Vec3::new(0.0, 300.0, 0.0), false).with_property("CanCollide", false));
        let a0 = doc.dom.insert(top, InstanceBuilder::new("Attachment").with_property("CFrame", rot));
        let a1 = doc.dom.insert(bob, InstanceBuilder::new("Attachment").with_property("CFrame", rot));
        doc.dom.insert(top, InstanceBuilder::new("PrismaticConstraint")
            .with_property("Attachment0", a0).with_property("Attachment1", a1)
            .with_property("LimitsEnabled", true)
            .with_property("LowerLimit", 0.0f32).with_property("UpperLimit", 4.0f32));
        let _ = run(&mut doc, 120);
        let b = pos(&doc, bob);
        assert!(b.x.abs() < 0.05 && b.z.abs() < 0.05, "stays on the axis: {b:?}");
        assert!((b.y - 296.0).abs() < 0.2, "rests on the 4-stud limit: {b:?}");
    }

    #[test]
    fn moving_replacement_attachment_updates_the_hinge_frame() {
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let start = Vec3::new(0.0, 300.0, 0.0);
        let top = doc.dom.insert(ws, part("Top", start, true));
        let bob = doc.dom.insert(ws, part("Bob", start, false).with_property("CanCollide", false));
        let initial = doc.dom.insert(top, InstanceBuilder::new("Attachment"));
        let replacement = doc.dom.insert(top, InstanceBuilder::new("Attachment"));
        let driven = doc.dom.insert(bob, InstanceBuilder::new("Attachment"));
        let hinge = doc.dom.insert(top, InstanceBuilder::new("HingeConstraint")
            .with_property("Attachment0", initial)
            .with_property("Attachment1", driven));
        let mut physics = Physics::new(&mut doc);
        let mut step = |doc: &mut Document| {
            physics.sync(doc);
            physics.step(doc, 1.0 / 60.0);
        };
        for _ in 0..30 {
            step(&mut doc);
        }
        doc.set_prop_live(hinge, "Attachment0".into(), replacement.into());
        for _ in 0..30 {
            step(&mut doc);
        }
        let moved = CFrame::new(Vector3::new(2.0, 0.0, 0.0), CFrame::identity().orientation);
        doc.set_prop_live(replacement, "CFrame".into(), moved.into());
        for _ in 0..60 {
            step(&mut doc);
        }
        let actual = pos(&doc, bob).x;
        assert!((actual - 2.0).abs() < 0.05, "replacement hinge endpoint moved to x=2, bob x={actual}");
    }

    #[test]
    fn rod_holds_its_length_and_spring_settles_at_its_load() {
        // A rod from the pivot to the bob's centre swings at its length.
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let top = doc.dom.insert(ws, part("Top", Vec3::new(0.0, 300.0, 0.0), true));
        let bob = doc.dom.insert(ws, part("Bob", Vec3::new(0.0, 300.0, 5.0), false));
        let a0 = doc.dom.insert(top, InstanceBuilder::new("Attachment"));
        let a1 = doc.dom.insert(bob, InstanceBuilder::new("Attachment"));
        doc.dom.insert(top, InstanceBuilder::new("RodConstraint")
            .with_property("Attachment0", a0).with_property("Attachment1", a1)
            .with_property("Length", 5.0f32));
        let mut p = Physics::new(&mut doc);
        for i in 0..90 {
            p.sync(&mut doc);
            p.step(&mut doc, 1.0 / 60.0);
            let b = pos(&doc, bob);
            assert!(((b - Vec3::new(0.0, 300.0, 0.0)).length() - 5.0).abs() < 0.25, "rod at {i}: {b:?}");
        }
        // A spring straight down: stiffness k, free length L → rests at L + m g / k.
        let mut doc = Document::new_place();
        let ws = doc.workspace().unwrap();
        let top = doc.dom.insert(ws, part("Top", Vec3::new(0.0, 300.0, 0.0), true));
        let bob = doc.dom.insert(ws, part("Bob", Vec3::new(0.0, 296.0, 0.0), false));
        let a0 = doc.dom.insert(top, InstanceBuilder::new("Attachment"));
        let a1 = doc.dom.insert(bob, InstanceBuilder::new("Attachment"));
        doc.dom.insert(top, InstanceBuilder::new("SpringConstraint")
            .with_property("Attachment0", a0).with_property("Attachment1", a1)
            .with_property("Stiffness", 200.0f32).with_property("Damping", 20.0f32)
            .with_property("FreeLength", 4.0f32));
        let mut p = Physics::new(&mut doc);
        for _ in 0..600 {
            p.sync(&mut doc);
            p.step(&mut doc, 1.0 / 60.0);
        }
        let mass = p.parts.get(&bob).and_then(|pb| pb.asm).and_then(|id| p.asms.get(&id))
            .and_then(|a| p.world.bodies.get(a.body)).map(|b| b.mass()).unwrap();
        let want = 300.0 - 4.0 - mass * p.gravity / 200.0;
        let b = pos(&doc, bob);
        assert!((b.y - want).abs() < 0.05, "spring rests at {want}: {b:?}");
    }
}
