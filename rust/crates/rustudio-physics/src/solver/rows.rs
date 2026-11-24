//! Rows of the system: one per scalar constraint (a contact's normal, its friction
//! directions, a joint's locked axis, a limit, a motor …), struct-of-arrays.
//!
//! A row couples at most two bodies of an island (island-local indices, NONE for a
//! fixed / out-of-island side) through its Jacobian (linear and angular parts for
//! each side), and carries what the primal solve needs: bounds, a velocity target,
//! a weight (penalty / compliance), and where its result is written back.
//!
//! Adding a row type: add a `RowKind`, push rows from a builder below, and make
//! the write-back (contact.rs `write_back`, joints.rs) read them by `src`.

use glam::Vec3;

use super::body::{BodySet, NONE};
use super::contact::ContactStore;
use super::island::Islands;
use super::dual::{BodyRec, DualBatch, LevelCfg};
use super::joints::JointList;
use super::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RowKind {
    ContactNormal,
    ContactFriction,
    /// Rolling / twisting friction rows, if the spec has them.
    ContactTwist,
    JointLinear,
    JointAngular,
    JointLimit,
    JointMotor,
    /// Movers (AlignPosition, LinearVelocity …) as rows, if the spec puts them in the system.
    Mover,
}

#[derive(Default)]
pub struct RowBuffer {
    pub kind: Vec<RowKind>,
    /// Island-local body index of each side (NONE = not in the solve).
    pub a: Vec<u32>,
    pub b: Vec<u32>,
    pub ja_lin: Vec<Vec3>,
    pub ja_ang: Vec<Vec3>,
    pub jb_lin: Vec<Vec3>,
    pub jb_ang: Vec<Vec3>,
    pub lo: Vec<f32>,
    pub hi: Vec<f32>,
    /// Velocity target of the row (restitution, position correction, motor speed …).
    pub target: Vec<f32>,
    /// Penalty weight / compliance of the row.
    pub weight: Vec<f32>,
    /// Back-reference: (contact list index or joint index, axis).
    pub src: Vec<(u32, u8)>,
    /// The solve's result per row (impulse), filled by the iteration.
    pub lambda: Vec<f32>,
}

impl RowBuffer {
    pub fn clear(&mut self) {
        self.kind.clear();
        self.a.clear();
        self.b.clear();
        self.ja_lin.clear();
        self.ja_ang.clear();
        self.jb_lin.clear();
        self.jb_ang.clear();
        self.lo.clear();
        self.hi.clear();
        self.target.clear();
        self.weight.clear();
        self.src.clear();
        self.lambda.clear();
    }

    pub fn len(&self) -> usize {
        self.kind.len()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(&mut self, kind: RowKind, a: u32, b: u32, ja: (Vec3, Vec3), jb: (Vec3, Vec3), bounds: (f32, f32), target: f32, weight: f32, src: (u32, u8)) {
        self.kind.push(kind);
        self.a.push(a);
        self.b.push(b);
        self.ja_lin.push(ja.0);
        self.ja_ang.push(ja.1);
        self.jb_lin.push(jb.0);
        self.jb_ang.push(jb.1);
        self.lo.push(bounds.0);
        self.hi.push(bounds.1);
        self.target.push(target);
        self.weight.push(weight);
        self.src.push(src);
        self.lambda.push(0.0);
    }
}

/// Island-local numbering of the island's bodies: `local[slot]`.
#[derive(Default)]
pub struct LocalIndex {
    pub local: Vec<u32>,
}

impl LocalIndex {
    pub fn set(&mut self, bodies: &BodySet, island: &[u32]) {
        if self.local.len() < bodies.len() {
            self.local.resize(bodies.len(), NONE);
        }
        for (i, &s) in island.iter().enumerate() {
            self.local[s as usize] = i as u32;
        }
    }

    /// One slot's local index (sizes the map on first use).
    #[inline]
    pub fn set_one(&mut self, bodies: &BodySet, slot: u32, local: u32) {
        if self.local.len() < bodies.len() {
            self.local.resize(bodies.len(), NONE);
        }
        self.local[slot as usize] = local;
    }

    pub fn unset(&mut self, island: &[u32]) {
        for &s in island {
            self.local[s as usize] = NONE;
        }
    }

    #[inline]
    pub fn of(&self, slot: u32) -> u32 {
        if slot == NONE { NONE } else { self.local[slot as usize] }
    }
}

/// The scratch row record of the legacy path (0xb0 bytes, DUAL §2.3 / DUALROWS §1.2),
/// what connector vtbl[10] fills; `dual::DualBatch::begin` pre-fills it.
#[derive(Debug, Clone, Copy, Default)]
pub struct DualRow {
    /// kept impulse × dt (v, p), the warm start (DUAL §5.2).
    pub lambda: [f64; 2],
    /// rhs (v, p): velocity bias b and position-pass target e.
    pub rhs: [f32; 2],
    /// lo (v, p), hi (v, p). Contacts: row 0's lo = the block's
    /// compliance (c_v, c_p); rows 1, 2: friction coefficients.
    pub lo: [f32; 2],
    pub hi: [f32; 2],
    /// SOR factor ω (v, p) from the kept record.
    pub omega: [f32; 2],
    /// velocity-lane Jacobian: A.lin, A.ang, B.lin, B.ang.
    pub jv: [Vec3; 4],
    /// position-lane Jacobian, same layout.
    pub jp: [Vec3; 4],
}

/// Studio: the sticking basis of a contact normal (DUALROWS §2 step 9).
#[inline]
pub fn tangent_basis(n: Vec3) -> (Vec3, Vec3) {
    let t = if n.x.abs() < n.y.abs() || n.x.abs() < n.z.abs() { Vec3::new(0.0, n.z, -n.y) } else { Vec3::new(-n.y, n.x, 0.0) };
    let t1 = t * (1.0 / t.length());
    (t1, n.cross(t1))
}

/// u(r, v, ω) = v + ω × r: a body's velocity at a point.
#[inline]
fn pt(v: Vec3, w: Vec3, r: Vec3) -> Vec3 {
    v + w.cross(r)
}

#[inline]
fn set_j(row: &mut DualRow, d: Vec3, ra: Vec3, rb: Vec3) {
    // J_A = (−d, −(r_a × d)), J_B = (d, r_b × d); both copies equal (DUALROWS §1.2).
    let j = [-d, -(ra.cross(d)), d, rb.cross(d)];
    row.jv = j;
    row.jp = j;
}

/// Slot 10 of a contact connector, Studio (DUALROWS §2), into the three
/// scratch rows `rows` (row 0 normal, rows 1, 2 friction). `p` = the point on A,
/// `n` from A into B, `sep` < 0 penetrating, μ / e the connector's.
#[allow(clippy::too_many_arguments)]
pub fn contact_rows_dual(rows: &mut [DualRow], p: Vec3, n: Vec3, sep: f32, mu: f32, e: f32, ra: &BodyRec, rb: &BodyRec, cfg: &LevelCfg) {
    // Step 1: |n|² ≤ 0.8 = inactive connector: placeholders, every b/e/lo/hi 0. Only
    // active connectors reach here (contact.rs).
    // Step 2: conn == 0 for FeaturePoly / Bullet → P = p + n·sep·0.5.
    let pp = p + n * (sep * 0.5);
    // Step 3: lever arms from the centres of mass (rec [0xf..0x11]).
    let r_a = pp - ra.x;
    let r_b = pp - rb.x;
    // Step 4: row 0.
    set_j(&mut rows[0], n, r_a, r_b);
    // Step 5: relative velocity of A over B, now (ṽ) and at the step start.
    let u_cur = pt(ra.vf, ra.wf, r_a) - pt(rb.vf, rb.wf, r_b);
    let vn = n.dot(u_cur);
    let u0 = pt(ra.v0, ra.w0, r_a) - pt(rb.v0, rb.w0, r_b);
    let vn0 = n.dot(u0);
    // Step 6: restitution on every closing contact ≥ cfg (1 stud/s).
    let rest = if cfg.rest_threshold <= vn0 { vn0 } else { 0.0 };
    rows[0].rhs[0] = rest * e + vn;
    rows[0].lo[0] = cfg.c_v;
    // Step 7.
    let t_cur = u_cur - n * vn;
    let t0 = u0 - n * vn0;
    let s2 = t0.dot(t0);
    let slop;
    if s2 >= cfg.slip_switch * cfg.slip_switch {
        // Step 8, sliding: one row along the step-start slip, row 2 dead.
        let d = t0 * (1.0 / s2.sqrt());
        let w = d.cross(n);
        set_j(&mut rows[1], d, r_a, r_b);
        rows[1].rhs = [d.dot(t_cur), 0.0];
        rows[1].lo = [0.0, 0.0];
        rows[1].hi = [mu * cfg.mu_kinetic, 0.0];
        set_j(&mut rows[2], w, r_a, r_b);
        rows[2].rhs = [0.0, 0.0];
        rows[2].lo = [0.0, 0.0];
        rows[2].hi = [0.0, 0.0];
        let q = (s2.sqrt() - cfg.slip_switch).min(cfg.slip_top) / (cfg.slip_top - cfg.slip_switch);
        slop = (1.0 - q) * cfg.slop_max + q * cfg.slop_min;
    } else {
        // Step 9, sticking: two rows on the fixed basis, box ±μ·cfg.
        let (t1, t2) = tangent_basis(n);
        let m = mu * cfg.mu_static;
        for (k, t) in [(1usize, t1), (2, t2)] {
            set_j(&mut rows[k], t, r_a, r_b);
            rows[k].rhs = [t.dot(t_cur), 0.0];
            rows[k].lo = [-m, -m];
            rows[k].hi = [m, m];
        }
        // Studio: x = (|cfg| + 0.1)·cfg; slop = clamp(min(x/|ω0_A|²,
        // x/|ω0_B|²), cfg, cfg) (|ω0| = 0 → +∞ → 0.05).
        let x = (cfg.gravity.abs() + 0.1) * cfg.slop_spin;
        let s = (x / ra.w0.length_squared()).min(x / rb.w0.length_squared());
        slop = s.max(cfg.slop_min).min(cfg.slop_max);
    }
    // Step 10: position-pass target and compliance; hi stays +∞.
    rows[0].rhs[1] = (slop + sep) * -cfg.depen;
    rows[0].lo[1] = cfg.c_p;
}

/// Legacy path: joint / constraint rows of slot 10 (lane JOINTS): per joint,
/// `batch.begin(kept, cfg)` → fill the rows (JOINT D1) → `batch.commit(ctype::JOINT,
/// key, pair, rows, bilateral mask conn, floors, ConnSrc::Joint(j))`.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
pub fn build_joint_rows_dual(i: usize, islands: &Islands, joints: &JointList, bodies: &BodySet, local: &LocalIndex, nonsim: &mut [u32], cfg: &LevelCfg, batch: &mut DualBatch, state: &mut Vec<(u32, super::joints::Kind)>, gs: &mut Vec<glam::Vec3>) {
    joints.build_dual(i, islands, bodies, local, nonsim, cfg, batch, state, gs);
}

/// Next-gen path: the rows of island `i`'s contacts (the primal rows, connector
/// slot 11 = Studio, built per connector by Studio).
#[allow(clippy::too_many_arguments)]
pub fn build_contact_rows_primal(i: usize, islands: &Islands, store: &mut ContactStore, bodies: &BodySet, local: &LocalIndex, cfg: &Config, h: f32, rows: &mut RowBuffer) {
    let _ = (store, bodies, local, cfg, h, &rows);
    if islands.manifolds(i).is_empty() {
        return;
    }
    crate::todo_spec!("CONTACT §4 slot 11  primal rows (normal, 2 friction), §4.1 slot 14 penalty floor / compliance, §3 material");
}

/// Next-gen path: joint / constraint rows of slot 11 (JOINT lane).
#[allow(clippy::too_many_arguments)]
pub fn build_joint_rows_primal(i: usize, islands: &Islands, joints: &JointList, bodies: &BodySet, local: &LocalIndex, cfg: &Config, h: f32, rows: &mut RowBuffer) {
    let _ = (joints, bodies, local, cfg, h, &rows);
    if islands.joint_ids(i).is_empty() {
        return;
    }
    crate::todo_spec!("JOINT §4 primal rows per class (slot 11, next-gen) and I (instance side)");
}
