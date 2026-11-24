//! RUSTUDIO_SOLVER=studio: the player's character and the client's own loose parts
//! (round HANG, Studio observations).
//!
//! A LocalScript that clones a part, destroys the original and unanchors the clone
//! makes a part only the client has (client-side object scripts: a rope
//! platforms, the spawn lanterns). Studio's client simulates it in the same world
//! as its character: the character stands on it, pushes it and is pushed back, and
//! the original it destroyed is gone from that world.
//!
//! rustudio steps the character in the server's world and those parts in the
//! client's (`client::ClientSim`). Before, the two never met: the character stood on
//! the server's original (still there, anchored), the clone hung beside it untouched
//! (a board on a rope shorter than its gap rose 1.01 stud and the character's feet
//! were inside it; a lantern walked into did not move).
//!
//! Here, once a frame:
//! * every client body near the character gets a stand-in in the server's world: a
//!   dynamic body with its shapes and its mass, at its pose and velocity, without
//!   gravity, touching nothing but the player's character (`Link::filter`);
//! * the world steps: the character stands on the stand-in (floor rays, the floor
//!   reaction), walks into it and is held up by it, as by the part itself;
//! * what the frame did to the stand-in's velocity (the character's floor reaction
//!   and contacts; it has no other force) goes to the client body before the
//!   client's world steps (`ClientSim::kick`), which adds its ropes and gravity;
//! * the character's own parts stay out of the client's world (they would push the
//!   body a second time);
//! * a replicated part the client took out of its Workspace (destroyed, unparented)
//!   no longer holds up or stops the player's character (`hidden`): floor rays and
//!   contacts skip it. The server's own bodies still meet it.
//!
//! * the client's own anchored parts near the character (an island a LocalScript
//!   cloned into Workspace: a game's island loader puts an island there on the
//!   client only) get a fixed copy here that only the player's character meets
//!   (`link_client_statics`): its floor, walls and touches. Studio's client owns
//!   its character and simulates it in the client's world, where those parts are;
//!   the server never has them, so its bodies and NPCs pass through the copies and
//!   their queries skip them (`hides_from`).
//!
//! The rapier path keeps its old behaviour (none of this runs there).
use super::*;
use rapier3d::geometry::BoundingVolume;

/// Studs around the character within which a client body gets a stand-in, plus this
/// many seconds of the two speeds.
const NEAR_MARGIN: f32 = 4.0;

fn debug() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("RUSTUDIO_LINK_DEBUG").is_some())
}

/// RUSTUDIO_NO_CLIENT_LINK=1 or the rustudio option: off (A/B and perf
/// checks; the studio path as before).
pub fn off() -> bool {
    !rustudio_core::rustudio_settings::client_link()
}
const NEAR_LEAD_S: f32 = 0.25;
/// Studs around the character within which a client part's static copy is kept,
/// plus this many seconds of its speed; rescanned every `STATIC_EVERY` frames and
/// kept `STATIC_KEEP` frames after leaving (a fall or a run outpaces neither).
const STATIC_MARGIN: f32 = 24.0;
const STATIC_LEAD_S: f32 = 0.5;
const STATIC_EVERY: u64 = 6;
const STATIC_KEEP: u64 = 30;

/// The fixed copy of one of the client's own anchored parts.
struct StaticCopy {
    /// Its collider in the client's world (a rebuild there makes a new one).
    client: rp::ColliderHandle,
    mirror: rp::ColliderHandle,
    seen: u64,
}

struct Proxy {
    body: rp::RigidBodyHandle,
    colliders: Vec<rp::ColliderHandle>,
    parts: Vec<Ref>,
    /// The velocities it was given at the frame's start.
    v0: Vec3,
    w0: Vec3,
    /// What the client's world did to the body in its last step (gravity, ropes,
    /// movers) as a force and torque, put on the stand-in every tick so the character
    /// meets a body held as the real one is; and the velocity change they made this
    /// frame (kept out of the kick).
    force: Vec3,
    torque: Vec3,
    pred_v: Vec3,
    pred_w: Vec3,
}

#[derive(Default)]
pub struct Link {
    /// Client body (its handle in the client's world) → its stand-in here.
    proxies: HashMap<rp::RigidBodyHandle, Proxy>,
    /// Client part → stand-in body (`Physics::body_of`).
    part_body: HashMap<Ref, rp::RigidBodyHandle>,
    proxy_colliders: HashSet<rp::ColliderHandle>,
    /// Client part → (friction, friction weight) of its shape (the floor's friction).
    friction: HashMap<Ref, (f32, f32)>,
    /// Server parts the client took out of its Workspace → the client instance taken out
    /// (the part's own copy, or an ancestor's).
    hidden: HashMap<Ref, Ref>,
    // Only the local character skips server colliders absent on its client.
    unstreamed: rustc_hash::FxHashSet<Ref>,
    streaming_revision: Option<u64>,
    /// Server parts that collide on the server but that the client made
    /// CanCollide=false (a LocalScript opening a door for its own player):
    /// the client simulates its character against its own copy, so the
    /// player's character passes through them; everything else still meets them.
    pub(super) nocollide: HashSet<Ref>,
    /// The player's character body (the only thing a stand-in touches).
    pub(super) player: Option<rp::RigidBodyHandle>,
    /// Its HumanoidRootPart (a query by the character has it in `own`).
    player_root: Option<Ref>,
    /// Client part → the fixed copy of it here (`link_client_statics`).
    statics: HashMap<Ref, StaticCopy>,
    frames: u64,
    /// The character's box centre at the last scan for copies.
    statics_at: Option<Vec3>,
}

impl Link {
    /// The stand-in bodies.
    pub(super) fn proxy_bodies(&self) -> impl Iterator<Item = rp::RigidBodyHandle> + '_ {
        self.proxies.values().map(|p| p.body)
    }

    /// A new world (`Physics::reconcile`): the stand-ins and fixed copies were
    /// the old world's bodies and colliders, whose handles the new one hands
    /// out again (dropping a stale stand-in would remove a new body). What the
    /// client did to replicated parts (`hidden`, `nocollide`) stays.
    pub(super) fn new_world(&mut self) {
        self.proxies.clear();
        self.part_body.clear();
        self.proxy_colliders.clear();
        self.friction.clear();
        self.player = None;
        self.player_root = None;
        self.statics.clear();
        self.statics_at = None;
    }

    pub fn is_empty(&self) -> bool {
        self.proxies.is_empty() && self.statics.is_empty() && self.all_server_parts_visible()
    }

    fn all_server_parts_visible(&self) -> bool {
        self.hidden.is_empty() && self.nocollide.is_empty() && self.unstreamed.is_empty()
    }

    fn hides_part(&self, r: &Ref) -> bool {
        self.hidden.contains_key(r) || self.nocollide.contains(r) || self.unstreamed.contains(r)
    }

    pub(super) fn body_of(&self, r: Ref) -> Option<rp::RigidBodyHandle> {
        if self.part_body.is_empty() {
            return None;
        }
        self.part_body.get(&r).copied()
    }

    pub(super) fn friction_of(&self, r: Ref) -> Option<(f32, f32)> {
        self.friction.get(&r).copied()
    }

    /// Whether a query by a character (its parts `own`) should skip this collider:
    /// the player's character skips what its client took out of Workspace, every
    /// other one (NPCs, the server's) skips what only the client has.
    pub(super) fn hides_from<S: std::hash::BuildHasher>(&self, h: rp::ColliderHandle, by_collider: &rustc_hash::FxHashMap<rp::ColliderHandle, Ref>, own: &std::collections::HashSet<Ref, S>) -> bool {
        if self.is_empty() {
            return false;
        }
        let player = self.player_root.is_some_and(|r| own.contains(&r));
        if player {
            by_collider.get(&h).is_some_and(|r| self.hides_part(r))
        } else {
            self.proxy_colliders.contains(&h)
        }
    }

    /// The pair rule (`OwnerHooks`): Some(answer) for the pairs this decides.
    pub(super) fn filter(&self, c: &rp::PairFilterContext, by_collider: &rustc_hash::FxHashMap<rp::ColliderHandle, Ref>) -> Option<Option<rp::SolverFlags>> {
        if self.is_empty() {
            return None;
        }
        let me = |b: Option<rp::RigidBodyHandle>| b.is_some() && b == self.player;
        let (p1, p2) = (self.proxy_colliders.contains(&c.collider1), self.proxy_colliders.contains(&c.collider2));
        if p1 || p2 {
            let ok = !(p1 && p2) && if p1 { me(c.rigid_body2) } else { me(c.rigid_body1) };
            return Some(ok.then_some(rp::SolverFlags::COMPUTE_RIGID_IMPULSES));
        }
        if !self.all_server_parts_visible() && (me(c.rigid_body1) || me(c.rigid_body2)) {
            let other = if me(c.rigid_body1) { c.collider2 } else { c.collider1 };
            if by_collider.get(&other).is_some_and(|r| self.hides_part(r)) {
                return Some(None);
            }
        }
        None
    }
}

impl Physics {
    pub fn set_streaming_visibility(
        &mut self,
        revision: Option<u64>,
        missing: impl Iterator<Item = Ref>,
    ) {
        if self.link.streaming_revision == revision {
            return;
        }
        self.link.streaming_revision = revision;
        self.link.unstreamed.clear();
        if revision.is_some() {
            self.link.unstreamed.extend(missing);
        }
    }

    /// A replicated part whose CanCollide the client changed: off on the client while
    /// the server's collides → the player's character passes through it (`nocollide`);
    /// the same on both sides again (the client turned it back on, or the server's
    /// value replicated over it) → it meets it again. (Studio path only.)
    pub(super) fn link_note_collide(&mut self, server: &Document, client: &Document, c: Ref, s: Ref) {
        let off = |d: &Document, r: Ref| matches!(d.prop(r, "CanCollide"), Some(Variant::Bool(false)));
        if self.parts.contains_key(&s) && off(client, c) && !off(server, s) {
            self.link.nocollide.insert(s);
        } else {
            self.link.nocollide.remove(&s);
        }
    }

    /// The replicated parts under `trees` (client refs whose parent changed, or every
    /// replicated part) that the client has out of its Workspace now: hidden from the
    /// player's character; back in, shown again. (Studio path only.)
    pub(super) fn link_note_trees(&mut self, server: &Document, client: &Document, c2s: &rustudio_core::RefMap, trees: &[Ref], all: bool) {
        let ws = client.workspace();
        let inside = |c: Ref| ws.is_some_and(|w| client.dom.get_by_ref(c).is_some() && client.is_ancestor(w, c));
        if all {
            // (The client's start-up flood: every replicated part once.)
            let mut hidden = HashMap::default();
            for (c, s) in c2s {
                if self.parts.contains_key(s) && !inside(*c) {
                    hidden.insert(*s, *c);
                }
            }
            self.link.hidden = hidden;
            return;
        }
        // Back in (or the server dropped it): shown again. (Few parts.)
        if !self.link.hidden.is_empty() {
            let parts = &self.parts;
            self.link.hidden.retain(|s, c| parts.contains_key(s) && !inside(*c));
        }
        // Taken out: everything under it. (Most tree marks are arrivals, which are in.)
        for c in trees {
            let Some(&s) = c2s.get(c) else { continue };
            if inside(*c) {
                continue;
            }
            for d in server.subtree(s) {
                if self.parts.contains_key(&d) {
                    self.link.hidden.insert(d, *c);
                }
            }
        }
    }

    /// Once a frame before the character's step (studio path): the stand-ins of the
    /// client's own bodies near the player's character, where and how fast those are.
    pub fn link_client_bodies(&mut self, cs: &client::ClientSim, c2s: &rustudio_core::RefMap, player_model: Option<Ref>) {
        if self.studio.is_none() || off() {
            return;
        }
        let player = player_model.and_then(|m| self.hums.by_model.get(&m)).map(|h| h.body);
        self.link.player = player;
        self.link.player_root = player_model.and_then(|m| self.hums.by_model.get(&m)).map(|h| h.root);
        let mut want: Vec<rp::RigidBodyHandle> = Vec::new();
        let region = player.and_then(|b| self.world.bodies.get(b)).and_then(|b| {
            let mut bb: Option<rp::Aabb> = None;
            for ch in b.colliders() {
                let Some(c) = self.world.colliders.get(*ch) else { continue };
                let x = c.compute_aabb();
                bb = Some(match bb {
                    Some(mut m) => {
                        m.merge(&x);
                        m
                    }
                    None => x,
                });
            }
            bb.map(|bb| (bb, gv(b.linvel()).length()))
        });
        if let (Some((pbb, pspeed)), Some(cp)) = (region, cs.physics()) {
            let mut asms: Vec<&Assembly> = cp.asms.values().collect();
            asms.sort_unstable_by_key(|a| a.body.into_raw_parts());
            for a in asms {
                let Some(b) = cp.world.bodies.get(a.body) else { continue };
                if !b.is_dynamic() || a.parts.iter().any(|r| cp.external.contains(r) || c2s.contains_key(r)) {
                    continue;
                }
                let mut bb: Option<rp::Aabb> = None;
                for r in &a.parts {
                    let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) else { continue };
                    if c.is_sensor() {
                        continue;
                    }
                    let x = c.compute_aabb();
                    bb = Some(match bb {
                        Some(mut m) => {
                            m.merge(&x);
                            m
                        }
                        None => x,
                    });
                }
                let Some(bb) = bb else { continue };
                let lead = (pspeed + gv(b.linvel()).length()) * NEAR_LEAD_S;
                if pbb.loosened(NEAR_MARGIN + lead).intersects(&bb) {
                    want.push(a.body);
                }
            }
        }
        if debug() && self.steps % 240 == 0 {
            eprintln!("LINKSTATE step={} player={:?} proxies={} hidden={} want={} client_asms={}", self.steps, player, self.link.proxies.len(), self.link.hidden.len(), want.len(), cs.physics().map_or(0, |p| p.asms.len()));
        }
        // Stand-ins no longer wanted.
        let gone: Vec<rp::RigidBodyHandle> = self.link.proxies.keys().filter(|k| !want.contains(k)).copied().collect();
        for k in gone {
            self.drop_proxy(k);
        }
        if let Some(cp) = cs.physics() {
            self.link_client_statics(cp, c2s, region);
        } else if !self.link.statics.is_empty() {
            self.drop_statics(|_| true);
        }
        let Some(cp) = cs.physics() else { return };
        for cb in want {
            let Some(b) = cp.world.bodies.get(cb) else { continue };
            let (pose, v, w) = (*b.position(), gv(b.linvel()), gv(b.angvel()));
            if !self.link.proxies.contains_key(&cb) {
                self.make_proxy(cp, cb);
            }
            let Some(p) = self.link.proxies.get_mut(&cb) else { continue };
            p.v0 = v;
            p.w0 = w;
            p.pred_v = Vec3::ZERO;
            p.pred_w = Vec3::ZERO;
            let body = p.body;
            let cols = p.colliders.clone();
            // (Solved on every tick, as the character it touches.)
            if let Some(sv) = self.studio.as_mut() {
                sv.set_level_request(body, 3, 1);
            }
            if let Some(pb) = self.world.bodies.get_mut(body) {
                pb.set_position(pose, true);
                pb.set_linvel(rv(v), true);
                pb.set_angvel(rv(w), true);
            }
            // (Queries before the next tick see the shapes where the body is.)
            for ch in cols {
                if let Some(c) = self.world.colliders.get_mut(ch) {
                    let local = c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY);
                    c.set_position(pose * local);
                }
            }
        }
        // The player's colliders go through the pair rule while there is something
        // for it to decide (rapier asks only for flagged colliders).
        if !self.link.is_empty() {
            if let Some(b) = player.and_then(|b| self.world.bodies.get(b)) {
                let list: Vec<rp::ColliderHandle> = b.colliders().to_vec();
                for ch in list {
                    if let Some(c) = self.world.colliders.get_mut(ch) {
                        if !c.active_hooks().contains(rp::ActiveHooks::FILTER_CONTACT_PAIRS) {
                            c.set_active_hooks(c.active_hooks() | rp::ActiveHooks::FILTER_CONTACT_PAIRS);
                        }
                    }
                }
            }
        }
    }

    /// The client's own anchored parts near the player's character, as fixed
    /// copies only it meets (see the module notes). `region`: the character's box
    /// and speed (None: no character, every copy goes).
    fn link_client_statics(&mut self, cp: &Physics, c2s: &rustudio_core::RefMap, region: Option<(rp::Aabb, f32)>) {
        self.link.frames += 1;
        let frame = self.link.frames;
        let Some((pbb, pspeed)) = region else {
            if !self.link.statics.is_empty() {
                self.drop_statics(|_| true);
            }
            return;
        };
        // The client's own part, anchored, in its Workspace (cp builds only those).
        let own_static = |r: &Ref, pb: &PartBody| pb.asm.is_none() && !cp.external.contains(r) && !c2s.contains_key(r);
        // (Rescanned at once after a jump of half the margin: a teleport into an island
        // the client cloned, a test place's graveyard island, otherwise dropped the character through
        // its ground for the frames until the next scan; with the realm's scripts
        // loading, 6 frames were ~0.3 s, a 40-stud fall.)
        let centre = gv(pbb.center().into());
        let jumped = self.link.statics_at.is_none_or(|c| (c - centre).length() > STATIC_MARGIN * 0.5);
        if frame % STATIC_EVERY == 1 || self.link.statics.is_empty() || jumped {
            let _s = rustudio_core::bench::scope("pl_static_scan");
            self.link.statics_at = Some(centre);
            let area = pbb.loosened(STATIC_MARGIN + pspeed * STATIC_LEAD_S);
            // (The client world's broad phase: a walk over every part was ~5 ms a
            // scan with a test place's graveyard island, 20k parts, on the client.)
            let mut found: Vec<(Ref, rp::ColliderHandle)> = Vec::new();
            for (ch, _) in cp.world.query_pipeline().intersect_aabb_conservative(area) {
                let Some(r) = cp.by_collider.get(&ch) else { continue };
                let Some(pb) = cp.parts.get(r) else { continue };
                if pb.collider == ch && own_static(r, pb) {
                    found.push((*r, ch));
                }
            }
            found.sort_unstable_by_key(|(r, _)| rbx_dom_weak::ref_order(*r));
            for (r, ch) in found {
                match self.link.statics.get_mut(&r) {
                    Some(sc) if sc.client == ch => sc.seen = frame,
                    _ => self.make_static(cp, r, ch, frame),
                }
            }
            self.drop_statics(|sc| frame - sc.seen > STATIC_KEEP);
        }
        // Copies whose part went (or changed in the client's world): gone or rebuilt;
        // the rest where the part is now (a tweened anchored part).
        let _t = rustudio_core::bench::scope("pl_static_track");
        rustudio_core::bench::add("pl_statics", 0.0, self.link.statics.len() as u64);
        let mut stale: Vec<Ref> = Vec::new();
        for (r, sc) in &self.link.statics {
            match cp.parts.get(r) {
                Some(pb) if pb.collider == sc.client && own_static(r, pb) => {
                    let (Some(c), Some(m)) = (cp.world.colliders.get(pb.collider), self.world.colliders.get(sc.mirror)) else {
                        stale.push(*r);
                        continue;
                    };
                    if c.position() != m.position() {
                        let at = *c.position();
                        if let Some(m) = self.world.colliders.get_mut(sc.mirror) {
                            m.set_position(at);
                        }
                    }
                }
                _ => stale.push(*r),
            }
        }
        if !stale.is_empty() {
            let set: HashSet<Ref> = stale.iter().copied().collect();
            self.drop_statics_of(&set);
            for r in stale {
                if let Some(pb) = cp.parts.get(&r).filter(|pb| own_static(&r, pb)) {
                    self.make_static(cp, r, pb.collider, frame);
                }
            }
        }
    }

    fn make_static(&mut self, cp: &Physics, r: Ref, ch: rp::ColliderHandle, frame: u64) {
        if let Some(old) = self.link.statics.remove(&r) {
            self.remove_static_copy(&old);
        }
        let Some(c) = cp.world.colliders.get(ch) else { return };
        let sensor = c.is_sensor();
        let mut copy = rp::ColliderBuilder::new(c.shared_shape().clone())
            .position(*c.position())
            .friction(c.friction())
            .restitution(c.restitution())
            .friction_combine_rule(c.friction_combine_rule())
            .restitution_combine_rule(c.restitution_combine_rule())
            .collision_groups(c.collision_groups())
            .user_data(c.user_data)
            .sensor(sensor);
        // (A CanCollide=false part only reports touches: the query that finds the
        // character's touches sees it, nothing collides with it.)
        if !sensor {
            copy = copy.active_hooks(rp::ActiveHooks::FILTER_CONTACT_PAIRS);
        }
        let gc = self.world.insert_collider(copy, None);
        if let Some(sv) = self.studio.as_mut() {
            sv.note_collider(gc, r, c.shape());
        }
        self.register_collider(gc, r);
        self.link.proxy_colliders.insert(gc);
        self.link.friction.insert(r, (c.friction(), hum_body::weights_of(c).0));
        self.link.statics.insert(r, StaticCopy { client: ch, mirror: gc, seen: frame });
    }

    fn remove_static_copy(&mut self, sc: &StaticCopy) {
        self.by_collider.remove(&sc.mirror);
        self.link.proxy_colliders.remove(&sc.mirror);
        self.world.remove_collider(sc.mirror);
    }

    fn drop_statics(&mut self, f: impl Fn(&StaticCopy) -> bool) {
        let gone: HashSet<Ref> = self.link.statics.iter().filter(|(_, sc)| f(sc)).map(|(r, _)| *r).collect();
        self.drop_statics_of(&gone);
    }

    fn drop_statics_of(&mut self, gone: &HashSet<Ref>) {
        for r in gone {
            if let Some(sc) = self.link.statics.remove(r) {
                self.remove_static_copy(&sc);
                self.link.friction.remove(r);
            }
        }
    }

    fn make_proxy(&mut self, cp: &Physics, cb: rp::RigidBodyHandle) {
        let Some(b) = cp.world.bodies.get(cb) else { return };
        let Some(a) = cp.asms.values().find(|a| a.body == cb) else { return };
        let mprops = b.mass_properties().local_mprops;
        let rb = rp::RigidBodyBuilder::dynamic()
            .pose(*b.position())
            .gravity_scale(0.0)
            .can_sleep(false)
            .additional_mass_properties(mprops);
        let body = self.world.insert_body(rb.build());
        let mut colliders = Vec::new();
        let mut parts = Vec::new();
        for r in &a.parts {
            let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) else { continue };
            parts.push(*r);
            if c.is_sensor() {
                continue;
            }
            let copy = rp::ColliderBuilder::new(c.shared_shape().clone())
                .position(c.position_wrt_parent().copied().unwrap_or(rp::Pose::IDENTITY))
                .friction(c.friction())
                .restitution(c.restitution())
                .friction_combine_rule(c.friction_combine_rule())
                .restitution_combine_rule(c.restitution_combine_rule())
                .collision_groups(c.collision_groups())
                .user_data(c.user_data)
                .density(0.0)
                .active_hooks(rp::ActiveHooks::FILTER_CONTACT_PAIRS);
            let gc = self.world.insert_collider(copy, Some(body));
            if let Some(sv) = self.studio.as_mut() {
                sv.note_collider(gc, *r, c.shape());
            }
            self.register_collider(gc, *r);
            self.link.proxy_colliders.insert(gc);
            colliders.push(gc);
        }
        // The client world's records of the body (its mass record, its box).
        if let (Some(sv), Some(csv)) = (self.studio.as_mut(), cp.studio.as_ref()) {
            let (m, bx, len) = csv.body_notes(cb);
            sv.note_mass(body, m);
            if let Some((lo, hi)) = bx {
                sv.note_box(body, lo, hi);
            }
            if let Some(l) = len {
                sv.note_char_len(body, l);
            }
        }
        for r in &parts {
            self.link.part_body.insert(*r, body);
            if let Some(c) = cp.parts.get(r).and_then(|pb| cp.world.colliders.get(pb.collider)) {
                self.link.friction.insert(*r, (c.friction(), hum_body::weights_of(c).0));
            }
        }
        self.dyn_version += 1;
        self.link.proxies.insert(cb, Proxy { body, colliders, parts, v0: Vec3::ZERO, w0: Vec3::ZERO, force: Vec3::ZERO, torque: Vec3::ZERO, pred_v: Vec3::ZERO, pred_w: Vec3::ZERO });
    }

    fn drop_proxy(&mut self, cb: rp::RigidBodyHandle) {
        let Some(p) = self.link.proxies.remove(&cb) else { return };
        for c in &p.colliders {
            self.by_collider.remove(c);
            self.link.proxy_colliders.remove(c);
        }
        for r in &p.parts {
            self.link.part_body.remove(r);
            self.link.friction.remove(r);
        }
        if let Some(sv) = self.studio.as_mut() {
            sv.note_mass(p.body, None);
        }
        self.world.remove_body(p.body);
        self.dyn_version += 1;
    }

    /// After the frame's world steps: what they did to each stand-in's velocities
    /// (client body, Δv, Δω), for `ClientSim::kick`.
    pub fn link_kicks(&self) -> Vec<(rp::RigidBodyHandle, Vec3, Vec3)> {
        let mut out = Vec::new();
        for (cb, p) in &self.link.proxies {
            let Some(b) = self.world.bodies.get(p.body) else { continue };
            let dv = gv(b.linvel()) - p.v0 - p.pred_v;
            let dw = gv(b.angvel()) - p.w0 - p.pred_w;
            if debug() {
                let lvl = self.studio.as_ref().map(|sv| sv.next_solve(p.body));
                eprintln!("LINKKICK step={} lvl={lvl:?} v0={:?} dv={dv:?} dw={dw:?} pos={:?}", self.steps, p.v0, b.translation());
            }
            if dv.length_squared() > 1e-12 || dw.length_squared() > 1e-12 {
                out.push((*cb, dv, dw));
            }
        }
        out.sort_unstable_by_key(|(b, _, _)| b.into_raw_parts());
        out
    }

    /// Before each world tick (after the movers cleared last tick's forces): the
    /// client world's own push on each stand-in.
    pub(super) fn link_forces(&mut self, h: f32) {
        if self.link.proxies.is_empty() {
            return;
        }
        let studio = self.studio.as_deref();
        for p in self.link.proxies.values_mut() {
            if p.force == Vec3::ZERO && p.torque == Vec3::ZERO {
                continue;
            }
            let Some(b) = self.world.bodies.get_mut(p.body) else { continue };
            b.add_force(rv(p.force), true);
            b.add_torque(rv(p.torque), true);
            let (m, inv, _) = solver_mass(studio, p.body, b);
            if m > 0.0 {
                p.pred_v += p.force / m * h;
            }
            p.pred_w += inv * p.torque * h;
            self.pushed.insert(p.body);
        }
    }

    /// After the client's world stepped `dt`: what its step did to each linked body
    /// (`ClientSim::link_motion`), as the force and torque for the next frame.
    pub fn link_note_motion(&mut self, motion: &[(rp::RigidBodyHandle, Vec3, Vec3)], dt: f32) {
        if dt <= 0.0 {
            return;
        }
        let studio = self.studio.as_deref();
        for (cb, dv, dw) in motion {
            let Some(p) = self.link.proxies.get_mut(cb) else { continue };
            let Some(b) = self.world.bodies.get(p.body) else { continue };
            let (m, _, _) = solver_mass(studio, p.body, b);
            p.force = *dv * m / dt;
            // Only the linear part goes over: the solver turns a torque into spin through
            // its geometric-stiffness inertia and angular damping (step.rs
            // `free_velocity`), which no prediction here matches, and the miss came back
            // as next frame's kick: a thin light part (small nails, 0.85×0.05×0.05,
            // 0.02 mass) spun up without bound beside the character (10¹⁸ rad/s, flung
            // 10⁵ studs). The stand-in takes the body's spin anew every frame; between, it
            // keeps it. (Linear motion has neither term: its prediction is exact.)
            let _ = dw;
            p.torque = Vec3::ZERO;
        }
    }

    /// Whether this world has stand-ins of client bodies.
    pub fn link_active(&self) -> bool {
        !self.link.proxies.is_empty()
    }

    /// The client bodies with stand-ins.
    pub fn link_bodies(&self) -> Vec<rp::RigidBodyHandle> {
        let mut v: Vec<_> = self.link.proxies.keys().copied().collect();
        v.sort_unstable_by_key(|b| b.into_raw_parts());
        v
    }
}

/// The mass, world inverse inertia and world inertia a stand-in is integrated with: the
/// solver's record (note_mass, the client body's Studio mass, solver/body.rs `gather`:
/// Iw⁻¹ = R·I⁻¹·Rᵀ) where it has one, else rapier's. The force and torque that carry the
/// client world's motion over, and the motion they are predicted to cause, must use the
/// one the solver integrates with: with rapier's, a thin light part (small nails,
/// 0.85×0.05×0.05, 0.02 mass) got back a kick of (I_studio⁻¹·I_rapier − 1)·Δω every frame
/// and spun up without bound beside the character (10¹⁸ rad/s, flung 10⁵ studs).
fn solver_mass(studio: Option<&crate::solver::StudioSolver>, h: rp::RigidBodyHandle, b: &rp::RigidBody) -> (f32, Mat3, Mat3) {
    if let Some(sm) = studio.and_then(|sv| sv.body_notes(h).0) {
        let i = sm.inertia();
        if sm.mass > 0.0 && i.determinant().is_finite() && i.determinant().abs() > 1e-20 {
            let r = Mat3::from_quat(glam::Quat::from_xyzw(b.rotation().x, b.rotation().y, b.rotation().z, b.rotation().w));
            return (sm.mass, r * i.inverse() * r.transpose(), r * i * r.transpose());
        }
    }
    let inv = hum_body::world_inv_inertia(b);
    let i = if inv.determinant().abs() > 1e-12 { inv.inverse() } else { Mat3::ZERO };
    (b.mass(), inv, i)
}

impl client::ClientSim {
    /// The character's push on the client's own bodies (`Physics::link_kicks`);
    /// `linked`: every body with a stand-in (their velocities after the kick are
    /// the start of `link_motion`).
    pub fn kick(&mut self, kicks: &[(rp::RigidBodyHandle, Vec3, Vec3)], linked: &[rp::RigidBodyHandle]) {
        self.link_start.clear();
        let Some(p) = self.physics.as_mut() else { return };
        for (cb, dv, dw) in kicks {
            if let Some(b) = p.world.bodies.get_mut(*cb) {
                if b.is_dynamic() {
                    let v = gv(b.linvel()) + *dv;
                    let w = gv(b.angvel()) + *dw;
                    b.set_linvel(rv(v), true);
                    b.set_angvel(rv(w), true);
                }
            }
        }
        for cb in linked {
            if let Some(b) = p.world.bodies.get(*cb) {
                self.link_start.insert(*cb, (gv(b.linvel()), gv(b.angvel())));
            }
        }
    }

    /// After its step: what the step did to each linked body's velocities.
    pub fn link_motion(&self) -> Vec<(rp::RigidBodyHandle, Vec3, Vec3)> {
        let Some(p) = self.physics.as_ref() else { return Vec::new() };
        let mut out: Vec<_> = self
            .link_start
            .iter()
            .filter_map(|(cb, (v, w))| p.world.bodies.get(*cb).map(|b| (*cb, gv(b.linvel()) - *v, gv(b.angvel()) - *w)))
            .collect();
        out.sort_unstable_by_key(|(b, _, _)| b.into_raw_parts());
        out
    }
}
