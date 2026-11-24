//! The legacy solver (`RUSTUDIO_SOLVER=studio`): what Studio 0.747 runs while
//! `workspace.NextGenerationPhysicsSolver` = Default. Every island goes to
//! "Solve Batch" Studio (0.744 addresses): rows from connector vtbl[10]
//! (contacts Studio: rows.rs `contact_rows_dual`; joints: JOINT, slot 10),
//! two lanes per row solved side by side in f64 (velocity v, position p),
//! projected Gauss–Seidel over the PGS rows and one sparse-LDL correction of the
//! bilateral rows. Spec: solver/DUAL.md (cited §n below), the contact rows
//! solver/DUALROWS.md.
//!
//! Layout as Studio's working set X (DUAL §0.2): bodies by island-local index
//! (sim bodies first, then non-sim), the body record (§0.4), the inverse-mass
//! record with both lanes (§0.5), u = the solve's correction (§0.6, 12 f32
//! interleaved lanes); PGS rows as SoA (state 8 f64, ω pair, J / Ĵ / W 24 f32
//! lane-interleaved (§0.3), diagonal pair, damping pair); LDL rows likewise.
//! One `DualBatch` per solver, cleared per island, never dropped: no allocation
//! per tick once warm.

use glam::{Mat3, Vec3, Vec4};

use super::body::NONE;
use super::rows::DualRow;

/// Position-lane mass exponent (DUAL §0.5, cfg).
pub(super) const POSITION_MASS_EXPONENT: f32 = 0.3;

/// The per-batch config (cfg = batch; ctor Studio, rescaled for the
/// batch's level by Studio). Fields used by the legacy path, with source.
#[derive(Debug, Clone, Copy)]
pub struct LevelCfg {
    /// Level L and dt = (f32)L · h (DUAL §0.1 T / Studio; DUALROWS §1.3).
    pub level: u8,
    pub dt: f32,
    /// cfg+0 = 20 + round(L−1)·min(SimAdaptiveExtraIterations (live 6), 20)
    /// (DUAL §3.6, Studio; base = .data int Studio = 20).
    pub sweeps: u32,
    /// cfg+4 = cfg+0 − 2.
    pub sweeps_pre: u32,
    /// cfg restitution threshold 1.0 stud/s (DUALROWS §1.3).
    pub rest_threshold: f32,
    /// cfg slop max 0.05, slop min 1e-4, spin scale 0.05.
    pub slop_max: f32,
    pub slop_min: f32,
    pub slop_spin: f32,
    /// cfg = 1 − powf(0.3, 240·dt) (Studio; .rdata Studio).
    pub depen: f32,
    /// cfg slip range top 20, slide/stick switch 1.0.
    pub slip_top: f32,
    pub slip_switch: f32,
    /// cfg static, kinetic friction scale (1.0, 1.0).
    pub mu_static: f32,
    pub mu_kinetic: f32,
    /// cfg c_v = 0.999 (0x3f7fbe77), cfg c_p = 0.95 (0x3f733333): the
    /// contact block's compliance (DebugSimAdaptiveIncreaseCollisionStiffness live false).
    pub c_v: f32,
    pub c_p: f32,
    /// cfg p-lane angular factor 0.05 (DUAL §0.5).
    pub ang_factor: f32,
    /// cfg warm start (ctor 1), cfg store λ (ctor 1).
    pub warm: bool,
    pub store: bool,
    /// cfg angular damping rate −logf(0.95).
    pub ang_damp: f32,
    /// cfg SOR on, cfg SOR on for the p lane (ctor 1 / 1; DUAL §2.2 step 3).
    pub sor: bool,
    pub sor_p: bool,
    /// cfg / compliance 1e-5 / 1e-3 (DUAL §3.2).
    pub comp_v: f32,
    pub comp_p: f32,
    /// cfg = Workspace.Gravity (STEP §0.2 [STEP2]; DUALROWS §2 step 9 reads it
    /// in the sticking slop — STEP Open 10 / DUALROWS Open 1).
    pub gravity: f32,
}

impl LevelCfg {
    pub fn new(level: u8, h: f32, gravity: f32) -> Self {
        let dt = level as f32 * h;
        // Studio: x = dt × 240 (.rdata Studio), cfg = 1 − powf(0.3, x).
        let x = dt * 240.0;
        let extra = 6u32.min(20); // FInt SimAdaptiveExtraIterations, live 6 (DUAL §7)
        let sweeps = 20 + ((x - 1.0).round().max(0.0) as u32) * extra;
        LevelCfg {
            level,
            dt,
            sweeps,
            sweeps_pre: sweeps - 2,
            rest_threshold: 1.0,
            slop_max: 0.05,
            slop_min: 1e-4,
            slop_spin: 0.05,
            depen: 1.0 - 0.3f32.powf(x),
            slip_top: 20.0,
            slip_switch: 1.0,
            mu_static: 1.0,
            mu_kinetic: 1.0,
            c_v: f32::from_bits(0x3f7fbe77),
            c_p: f32::from_bits(0x3f733333),
            ang_factor: 0.05,
            warm: true,
            store: true,
            ang_damp: -(0.95f32.ln()),
            sor: true,
            sor_p: true,
            comp_v: 1e-5,
            comp_p: 1e-3,
            gravity,
        }
    }
}

/// The body record (0x78 bytes, DUAL §0.4 / STEP §4.1a step 7): ṽ (the free
/// velocity the solve corrects), the start pose, the step-start velocity.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyRec {
    /// vf, ωf.
    pub vf: Vec3,
    pub wf: Vec3,
    /// R, x (world centre of mass).
    pub r: Mat3,
    pub x: Vec3,
    /// v0, ω0.
    pub v0: Vec3,
    pub w0: Vec3,
}

/// The inverse-mass record (0x90 bytes, DUAL §0.5): both lanes.
#[derive(Debug, Clone, Copy, Default)]
pub struct MassRec {
    /// [0] m⁻¹, [1] m⁻¹p = powf(m⁻¹, 0.3).
    pub im: f32,
    pub im_p: f32,
    /// World inverse inertia (v lane) and k·I⁻¹ (p lane), k = (m⁻¹p/m⁻¹)·0.05.
    pub iw: Mat3,
    pub iw_p: Mat3,
}

/// Connector types (DUAL §2.1: conn.vtbl[1]): 0 ordinary joints, 1 / 2
/// double / triple-axis velocity constraints, 3 Levitation, 4 contacts.
pub mod ctype {
    pub const JOINT: u8 = 0;
    /// Double / Triple-axis linear velocity in magnitude mode (JOINT D3.22 / D3.23).
    pub const DOUBLE_VEL: u8 = 1;
    pub const TRIPLE_VEL: u8 = 2;
    pub const CONTACT: u8 = 4;
}

/// Where a connector's result goes after the solve.
#[derive(Debug, Clone, Copy)]
pub enum ConnSrc {
    /// ContactList index (contact.rs).
    Contact(u32),
    /// JointList index (joints.rs, lane JOINTS).
    Joint(u32),
}

/// The LDL set (DUAL §0.2 "LDL set", X..), public for the LDL
/// correction (JOINTS' linsys.rs) — see `ldl_correction`.
#[derive(Default)]
pub struct LdlSet {
    /// X pair per row (island-local; NONE = static side).
    pub pair: Vec<[u32; 2]>,
    /// X J, X W (24 f32, §0.3 layout).
    pub j: Vec<[f32; 24]>,
    pub w: Vec<[f32; 24]>,
    /// X A_ii pair, X ε pair (§4.1), X rhs pair.
    pub diag: Vec<[f32; 2]>,
    pub eps: Vec<[f32; 2]>,
    pub rhs: Vec<[f32; 2]>,
    /// X Ĵ = s·J, X s·rhs, X s·ε (§4.2).
    pub jh: Vec<[f32; 24]>,
    pub srhs: Vec<[f64; 2]>,
    pub seps: Vec<[f64; 2]>,
    /// X λ pair, X residual / correction Δλ (§4.3).
    pub lambda: Vec<[f64; 2]>,
    pub r: Vec<[f64; 2]>,
    /// Per LDL connector (in solve order): first LDL row, row count, body pair.
    pub conn_first: Vec<u32>,
    pub conn_rows: Vec<u8>,
    pub conn_pair: Vec<[u32; 2]>,
}

impl LdlSet {
    fn clear(&mut self) {
        self.pair.clear();
        self.j.clear();
        self.w.clear();
        self.diag.clear();
        self.eps.clear();
        self.rhs.clear();
        self.jh.clear();
        self.srhs.clear();
        self.seps.clear();
        self.lambda.clear();
        self.r.clear();
        self.conn_first.clear();
        self.conn_rows.clear();
        self.conn_pair.clear();
    }

    pub fn len(&self) -> usize {
        self.pair.len()
    }
}

/// One batch's working set (DUAL §0.2).
#[derive(Default)]
pub struct DualBatch {
    // -- bodies, island-local --
    pub rec: Vec<BodyRec>,
    pub mass: Vec<MassRec>,
    /// X: 1.0 sim body, 0.0 non-sim.
    pub scale: Vec<f32>,
    /// X u: [v.x, p.x, ω.x, θ.x, v.y, …] (§0.6).
    pub u: Vec<[f32; 12]>,
    /// Condition-number output per body (STEP §3.3a, written by the LDL
    /// correction; NaN = not written this solve).
    pub cond: Vec<f32>,
    pub n_sim: usize,
    // -- connectors as built (unsorted staging) --
    c_type: Vec<u8>,
    c_key: Vec<u64>,
    c_pair: Vec<[u32; 2]>,
    c_rows: Vec<u8>,
    c_mask: Vec<u8>,
    c_floor: Vec<[f32; 2]>,
    c_src: Vec<ConnSrc>,
    c_stage: Vec<u32>,
    stage: Vec<DualRow>,
    /// Connector indices in solve order: type, then key (DUAL §1 step 5).
    order: Vec<u32>,
    /// Per connector (staging index): first PGS row (X), first LDL row (X).
    c_pgs: Vec<u32>,
    c_ldl: Vec<u32>,
    // -- PGS set (X..) --
    /// Row ranges of the PGS connectors in solve order, per group 0..4.
    pgs_conn: Vec<(u8, u32, u8, [u32; 2])>,
    /// X: [lo_v, lo_p, hi_v, hi_p, rhs_v, rhs_p, λ_v, λ_p] (f64).
    pub st: Vec<[f64; 8]>,
    /// X ω pair, X J, X Ĵ, X W, X diag, X d.
    pub omega: Vec<[f32; 2]>,
    j: Vec<[f32; 24]>,
    jh: Vec<[f32; 24]>,
    w: Vec<[f32; 24]>,
    diag: Vec<[f32; 2]>,
    d: Vec<[f64; 2]>,
    /// X pair per PGS row.
    pair: Vec<[u32; 2]>,
    // -- LDL set --
    pub ldl: LdlSet,
    /// The direct solve of the LDL set (linsys.rs, lane JOINTS); kept between ticks
    /// for its cached symbolic structure (DUAL §6.4).
    pub ldl_solver: crate::solver::linsys::DualLdl,
}

/// J (24, lane-interleaved, §0.3) from a scratch row's two Jacobians.
#[inline]
fn pack_j(r: &DualRow) -> [f32; 24] {
    let mut j = [0.0f32; 24];
    for side in 0..2 {
        for a in 0..3 {
            let o = side * 12 + 4 * a;
            j[o] = r.jv[2 * side][a];
            j[o + 1] = r.jp[2 * side][a];
            j[o + 2] = r.jv[2 * side + 1][a];
            j[o + 3] = r.jp[2 * side + 1][a];
        }
    }
    j
}

/// W = M⁻¹Jᵀ per lane for one side, × the body's scale (§3 step 3).
#[inline]
fn side_w(j: &[f32; 24], o: usize, m: &MassRec, s: f32, w: &mut [f32; 24]) {
    let lin_v = Vec3::new(j[o], j[o + 4], j[o + 8]);
    let lin_p = Vec3::new(j[o + 1], j[o + 5], j[o + 9]);
    let ang_v = m.iw * Vec3::new(j[o + 2], j[o + 6], j[o + 10]);
    let ang_p = m.iw_p * Vec3::new(j[o + 3], j[o + 7], j[o + 11]);
    for a in 0..3 {
        w[o + 4 * a] = m.im * lin_v[a] * s;
        w[o + 4 * a + 1] = m.im_p * lin_p[a] * s;
        w[o + 4 * a + 2] = ang_v[a] * s;
        w[o + 4 * a + 3] = ang_p[a] * s;
    }
}

/// The refined reciprocal of the scaling kernels:
/// r = rcpps(a), r = (2 − r·a)·r; ±∞ input → ±0. (rcpps's table is the CPU's:
/// an exact 1/a stands in, one Newton step later the difference is ≤ 1 ulp.)
#[inline]
fn rcp(a: f32) -> f32 {
    if a.is_infinite() {
        return 0.0f32.copysign(a);
    }
    let r = 1.0 / a;
    let r2 = (2.0 - r * a) * r;
    if r2.is_finite() { r2 } else { r }
}

/// Ĵ·u of one side, four-wide (lin v, lin p, ang v, ang p) over x, y, z (DUAL §6.3:
/// two f32 lanes per row; the order of the 24-term sum is not specified).
#[inline(always)]
fn dot_side(j: &[f32], u: &[f32; 12], mut acc: Vec4) -> Vec4 {
    for (j, u) in j.as_chunks::<4>().0.iter().zip(u.as_chunks::<4>().0) {
        acc += Vec4::from_array(*j) * Vec4::from_array(*u);
    }
    acc
}

#[inline(always)]
fn dot_lane(jh: &[f32; 24], u: &[[f32; 12]], pair: [u32; 2]) -> [f32; 2] {
    let mut s = Vec4::ZERO;
    if pair[0] != NONE {
        s = dot_side(&jh[..12], &u[pair[0] as usize], s);
    }
    if pair[1] != NONE {
        s = dot_side(&jh[12..], &u[pair[1] as usize], s);
    }
    [s.x + s.z, s.y + s.w]
}

#[inline(always)]
fn add_lane(u: &mut [[f32; 12]], pair: [u32; 2], d: [f32; 2], w: &[f32; 24]) {
    let d = Vec4::new(d[0], d[1], d[0], d[1]);
    for side in 0..2 {
        if pair[side] == NONE {
            continue;
        }
        let us = &mut u[pair[side] as usize];
        let (out, _) = us.as_chunks_mut::<4>();
        let w = w.as_chunks::<4>().0;
        for a in 0..3 {
            out[a] = (Vec4::from_array(out[a]) + d * Vec4::from_array(w[3 * side + a])).to_array();
        }
    }
}

/// The contact block's update (DUAL §3.3 step 5): u += Δn·W_0 + Δ1·W_1 + Δ2·W_2.
#[inline(always)]
fn add_block(u: &mut [[f32; 12]], pair: [u32; 2], d: &[[f32; 2]; 3], w: &[[f32; 24]]) {
    let d4 = |r: usize| Vec4::new(d[r][0], d[r][1], d[r][0], d[r][1]);
    let (d0, d1, d2) = (d4(0), d4(1), d4(2));
    let (w0, w1, w2) = (w[0].as_chunks::<4>().0, w[1].as_chunks::<4>().0, w[2].as_chunks::<4>().0);
    for side in 0..2 {
        if pair[side] == NONE {
            continue;
        }
        let us = &mut u[pair[side] as usize];
        let (out, _) = us.as_chunks_mut::<4>();
        for a in 0..3 {
            let i = 3 * side + a;
            let delta = d0 * Vec4::from_array(w0[i])
                + d1 * Vec4::from_array(w1[i])
                + d2 * Vec4::from_array(w2[i]);
            out[a] = (Vec4::from_array(out[a]) + delta).to_array();
        }
    }
}

impl DualBatch {
    pub fn clear(&mut self) {
        self.rec.clear();
        self.mass.clear();
        self.scale.clear();
        self.u.clear();
        self.cond.clear();
        self.n_sim = 0;
        self.c_type.clear();
        self.c_key.clear();
        self.c_pair.clear();
        self.c_rows.clear();
        self.c_mask.clear();
        self.c_floor.clear();
        self.c_src.clear();
        self.c_stage.clear();
        self.stage.clear();
        self.order.clear();
        self.c_pgs.clear();
        self.c_ldl.clear();
        self.pgs_conn.clear();
        self.st.clear();
        self.omega.clear();
        self.j.clear();
        self.jh.clear();
        self.w.clear();
        self.diag.clear();
        self.d.clear();
        self.pair.clear();
        self.ldl.clear();
    }

    /// Adds a body (Studio: sim bodies first, then non-sim); returns its
    /// island-local index. u starts at 0.
    pub fn add_body(&mut self, rec: BodyRec, mass: MassRec, sim: bool) -> u32 {
        if sim {
            self.n_sim += 1;
        }
        self.rec.push(rec);
        self.mass.push(mass);
        self.scale.push(if sim { 1.0 } else { 0.0 });
        self.u.push([0.0; 12]);
        self.cond.push(f32::NAN);
        (self.rec.len() - 1) as u32
    }

    #[inline]
    pub fn body(&self, i: u32) -> BodyRec {
        if i == NONE { BodyRec { r: Mat3::IDENTITY, ..Default::default() } } else { self.rec[i as usize] }
    }

    /// Scratch rows for one connector, pre-filled as Studio step 1 does
    /// (lo = −∞, hi = +∞, λ / ω from `kept` = (λ_v, λ_p, s_v, s_p) per row when
    /// warm starting, else 0 / 1.0). The builder fills them (slot 10), then calls
    /// `commit`.
    pub fn begin(&mut self, kept: &[(f64, f64, f32, f32)], cfg: &LevelCfg) -> &mut [DualRow] {
        let at = self.stage.len();
        for k in kept {
            let mut r = DualRow::default();
            r.lo = [f32::NEG_INFINITY; 2];
            r.hi = [f32::INFINITY; 2];
            if cfg.warm {
                // R.λ = (dt·K.λ_v, dt·K.λ_p) (f64), R.ω = (K.s_v, K.s_p).
                r.lambda = [cfg.dt as f64 * k.0, cfg.dt as f64 * k.1];
                r.omega = [k.2, k.3];
            } else {
                r.lambda = [0.0; 2];
                r.omega = [1.0; 2];
            }
            self.stage.push(r);
        }
        &mut self.stage[at..]
    }

    /// Commits the connector whose rows `begin` returned: type, sort key, body
    /// pair (island-local, NONE = static), bilateral mask (conn, bit r = row r
    /// goes to the LDL set), compliance floors (conn,), result target.
    #[allow(clippy::too_many_arguments)]
    pub fn commit(&mut self, ty: u8, key: u64, pair: [u32; 2], rows: u8, mask: u8, floor: [f32; 2], src: ConnSrc) {
        self.c_type.push(ty);
        self.c_key.push(key);
        self.c_pair.push(pair);
        self.c_rows.push(rows);
        self.c_mask.push(mask);
        self.c_floor.push(floor);
        self.c_src.push(src);
        self.c_stage.push((self.stage.len() - rows as usize) as u32);
        if crate::solver::step::row_trace() {
            for r in &self.stage[self.stage.len() - rows as usize..] {
                eprintln!("row ty {ty} key {key} mask {mask} rhs {:?} lo {:?} hi {:?} jv {:?} jp {:?}", r.rhs, r.lo, r.hi, r.jv, r.jp);
            }
        }
    }

    pub fn conn_count(&self) -> usize {
        self.c_type.len()
    }

    /// Rows this batch holds (PGS + LDL), after `prepare`.
    pub fn row_count(&self) -> usize {
        self.st.len() + self.ldl.len()
    }

    /// "Init Batch Solve" Studio steps 4–10 (DUAL §2): sort the connectors
    /// (§1 step 5), split rows into PGS / LDL sets (§2.1), scatter them (§2.2
    /// step 4), W and A_ii (step 9), LDL ε (step 10).
    pub fn prepare(&mut self, cfg: &LevelCfg) {
        let n = self.c_type.len();
        self.order.extend(0..n as u32);
        // Partition by type (groups 0..4, stable), each group by key ascending
        //.
        let (ty, key) = (&self.c_type, &self.c_key);
        self.order.sort_unstable_by_key(|&c| (ty[c as usize], key[c as usize]));
        self.c_pgs.resize(n, 0);
        self.c_ldl.resize(n, 0);
        for oi in 0..n {
            let c = self.order[oi] as usize;
            let (ty, pair, rows, mask) = (self.c_type[c], self.c_pair[c], self.c_rows[c] as usize, self.c_mask[c]);
            // §2.1: type 0 with cfg+8 = 0 (stepWorld passes 0): bilateral rows (bit r
            // of conn) → LDL, the rest → PGS; types 1–4: all rows → PGS.
            let mask = if ty == ctype::JOINT { mask } else { 0 };
            self.c_pgs[c] = self.st.len() as u32;
            self.c_ldl[c] = self.ldl.len() as u32;
            let n_pgs = (0..rows).filter(|r| mask & (1 << r) == 0).count();
            if n_pgs > 0 {
                self.pgs_conn.push((ty, self.st.len() as u32, n_pgs as u8, pair));
            }
            let n_ldl = rows - n_pgs;
            if n_ldl > 0 {
                self.ldl.conn_first.push(self.ldl.len() as u32);
                self.ldl.conn_rows.push(n_ldl as u8);
                self.ldl.conn_pair.push(pair);
            }
            // §3.2 damping per connector (groups 0..3): ε = max(cfg, floor) per lane,
            // d = (f64)(1/(ε + 1)) (f32 divide, then widen).
            let fl = self.c_floor[c];
            let ev = if cfg.comp_v < fl[0] { fl[0] } else { cfg.comp_v };
            let ep = if cfg.comp_p < fl[1] { fl[1] } else { cfg.comp_p };
            let dd = [(1.0 / (ev + 1.0)) as f64, (1.0 / (ep + 1.0)) as f64];
            let first = self.c_stage[c] as usize;
            for r in 0..rows {
                let row = self.stage[first + r];
                let j = pack_j(&row);
                if mask & (1 << r) != 0 {
                    // Studio step 4, bilateral: X ← λ, X ← rhs, X ← J.
                    self.ldl.lambda.push(row.lambda);
                    self.ldl.rhs.push(row.rhs);
                    self.ldl.j.push(j);
                    self.ldl.pair.push(pair);
                } else {
                    // PGS: X ← (f64)[lo_v, lo_p, hi_v, hi_p, rhs_v, rhs_p] + λ, X ← ω, X ← J.
                    self.st.push([
                        row.lo[0] as f64,
                        row.lo[1] as f64,
                        row.hi[0] as f64,
                        row.hi[1] as f64,
                        row.rhs[0] as f64,
                        row.rhs[1] as f64,
                        row.lambda[0],
                        row.lambda[1],
                    ]);
                    self.omega.push(row.omega);
                    self.j.push(j);
                    self.pair.push(pair);
                    self.d.push(dd);
                }
            }
        }
        // Studio step 3: ω (X): SOR off → both lanes 1.0; SOR on but not
        // for p → p lane 1.0; else both keep the kept values.
        if !cfg.sor {
            for o in &mut self.omega {
                *o = [1.0, 1.0];
            }
        } else if !cfg.sor_p {
            for o in &mut self.omega {
                o[1] = 1.0;
            }
        }
        // Step 9: W = M⁻¹Jᵀ per row per lane, A_ii = J·W, W × scale.
        self.w.resize(self.j.len(), [0.0; 24]);
        self.diag.resize(self.j.len(), [0.0; 2]);
        for r in 0..self.j.len() {
            let (w, dg) = Self::w_of(&self.j[r], self.pair[r], &self.mass, &self.scale);
            self.w[r] = w;
            self.diag[r] = dg;
        }
        let nl = self.ldl.len();
        self.ldl.w.resize(nl, [0.0; 24]);
        self.ldl.diag.resize(nl, [0.0; 2]);
        for r in 0..nl {
            let (w, dg) = Self::w_of(&self.ldl.j[r], self.ldl.pair[r], &self.mass, &self.scale);
            self.ldl.w[r] = w;
            self.ldl.diag[r] = dg;
        }
        // Step 10, Studio: LDL ε per connector = mean A_ii over its rows ×
        // max(cfg, floor) per lane (§4.1).
        self.ldl.eps.resize(nl, [0.0; 2]);
        for oi in 0..n {
            let c = self.order[oi] as usize;
            if self.c_type[c] != ctype::JOINT || self.c_mask[c] == 0 {
                continue;
            }
            let first = self.c_ldl[c] as usize;
            let k = self.c_mask[c].count_ones() as usize;
            let mut mean = [0.0f32; 2];
            for r in first..first + k {
                mean[0] += self.ldl.diag[r][0];
                mean[1] += self.ldl.diag[r][1];
            }
            let fl = self.c_floor[c];
            let fv = if cfg.comp_v < fl[0] { fl[0] } else { cfg.comp_v };
            let fp = if cfg.comp_p < fl[1] { fl[1] } else { cfg.comp_p };
            let e = [mean[0] / k as f32 * fv, mean[1] / k as f32 * fp];
            for r in first..first + k {
                self.ldl.eps[r] = e;
            }
        }
    }

    fn w_of(j: &[f32; 24], pair: [u32; 2], mass: &[MassRec], scale: &[f32]) -> ([f32; 24], [f32; 2]) {
        let mut w = [0.0f32; 24];
        if pair[0] != NONE {
            side_w(j, 0, &mass[pair[0] as usize], scale[pair[0] as usize], &mut w);
        }
        if pair[1] != NONE {
            side_w(j, 12, &mass[pair[1] as usize], scale[pair[1] as usize], &mut w);
        }
        // A_ii = J·W per lane (the unscaled W: equal, scale is 1 for sim bodies and
        // the non-sim mass record is zero).
        let mut dg = [0.0f32; 2];
        for k in 0..24 {
            dg[k & 1] += j[k] * w[k];
        }
        (w, dg)
    }

    /// "PGS Solve" Studio (DUAL §3) and the finish of "Finalize" Studio
    /// (§4.3): scaling, warm start, cfg+4+1 sweeps, the LDL correction, the last
    /// sweep. Leaves λ in the row state and the correction in `u`.
    pub fn solve(&mut self, cfg: &LevelCfg) {
        if self.st.is_empty() && self.ldl.len() == 0 {
            return;
        }
        // 1: Studio applyDiagonalPreconditioner, per PGS row per lane:
        // s = rcp(A_ii)·ω; Ĵ = s·J (f32); rhs = (f64)s·rhs (λ not rescaled).
        self.jh.resize(self.j.len(), [0.0; 24]);
        for r in 0..self.j.len() {
            let s = [rcp(self.diag[r][0]) * self.omega[r][0], rcp(self.diag[r][1]) * self.omega[r][1]];
            for k in 0..24 {
                self.jh[r][k] = s[k & 1] * self.j[r][k];
            }
            self.st[r][4] *= s[0] as f64;
            self.st[r][5] *= s[1] as f64;
        }
        // 4: Studio LDL row scaling: s = rcp(A_ii + ε); s·rhs, s·J, s·ε (§4.2).
        let nl = self.ldl.len();
        self.ldl.jh.resize(nl, [0.0; 24]);
        self.ldl.srhs.resize(nl, [0.0; 2]);
        self.ldl.seps.resize(nl, [0.0; 2]);
        for r in 0..nl {
            let s = [rcp(self.ldl.diag[r][0] + self.ldl.eps[r][0]), rcp(self.ldl.diag[r][1] + self.ldl.eps[r][1])];
            for k in 0..24 {
                self.ldl.jh[r][k] = s[k & 1] * self.ldl.j[r][k];
            }
            for l in 0..2 {
                self.ldl.srhs[r][l] = s[l] as f64 * self.ldl.rhs[r][l] as f64;
                self.ldl.seps[r][l] = s[l] as f64 * self.ldl.eps[r][l] as f64;
            }
        }
        // 5: warm start: u += λ·W (PGS rows Studio, LDL rows Studio), both lanes.
        for r in 0..self.st.len() {
            let l = [self.st[r][6] as f32, self.st[r][7] as f32];
            add_lane(&mut self.u, self.pair[r], l, &self.w[r]);
        }
        for r in 0..nl {
            let l = [self.ldl.lambda[r][0] as f32, self.ldl.lambda[r][1] as f32];
            add_lane(&mut self.u, self.ldl.pair[r], l, &self.ldl.w[r]);
        }
        if crate::solver::step::pose_trace() {
            for (b, u) in self.u.iter().enumerate() {
                eprintln!("u warm body {b} dth [{:e} {:e} {:e}] dw [{:e} {:e} {:e}]", u[3], u[7], u[11], u[2], u[6], u[10]);
            }
        }
        // 6: cfg+4 + 1 sweeps (bilateral GS in all but the last), no early exit (§3.6).
        let pre = cfg.sweeps_pre;
        for it in 0..=pre {
            self.sweep(it < pre);
        }
        if crate::solver::step::pose_trace() {
            for (b, u) in self.u.iter().enumerate() {
                eprintln!("u sweeps body {b} dth [{:e} {:e} {:e}] dw [{:e} {:e} {:e}]", u[3], u[7], u[11], u[2], u[6], u[10]);
            }
        }
        // 7 / §4.3: residual r = rhs − J·u − ε·λ per LDL row, r ← K⁻¹r
        // (applyInverseLDL Studio, JOINTS), λ += r, u += r·W.
        if nl > 0 {
            self.ldl.r.resize(nl, [0.0; 2]);
            for r in 0..nl {
                let p = self.ldl.pair[r];
                let ju = dot_lane(&self.ldl.j[r], &self.u, p);
                for l in 0..2 {
                    self.ldl.r[r][l] = self.ldl.rhs[r][l] as f64 - ju[l] as f64 - self.ldl.eps[r][l] as f64 * self.ldl.lambda[r][l];
                }
            }
            if ldl_correction(&mut self.ldl_solver, &mut self.ldl, self.n_sim as u32, &mut self.cond) {
                for r in 0..nl {
                    self.ldl.lambda[r][0] += self.ldl.r[r][0];
                    self.ldl.lambda[r][1] += self.ldl.r[r][1];
                    let d = [self.ldl.r[r][0] as f32, self.ldl.r[r][1] as f32];
                    add_lane(&mut self.u, self.ldl.pair[r], d, &self.ldl.w[r]);
                }
            }
        }
        if crate::solver::step::pose_trace() {
            for (b, u) in self.u.iter().enumerate() {
                eprintln!("u ldl body {b} dth [{:e} {:e} {:e}] dw [{:e} {:e} {:e}]", u[3], u[7], u[11], u[2], u[6], u[10]);
            }
        }
        // §4.3 step 5: it = cfg+4+1 .. cfg+0 − 1, full sweeps incl. Studio.
        for _ in pre + 1..cfg.sweeps {
            self.sweep(true);
        }
    }

    /// One sweep (Studio step 6): groups in type order, connectors in key
    /// order, every update applied to u at once (Gauss–Seidel).
    fn sweep(&mut self, bilateral: bool) {
        for ci in 0..self.pgs_conn.len() {
            let (ty, first, rows, pair) = self.pgs_conn[ci];
            let first = first as usize;
            match ty {
                ctype::CONTACT => self.contact_block(first, pair),
                ctype::JOINT => {
                    for r in first..first + rows as usize {
                        self.box_row(r);
                    }
                }
                ctype::DOUBLE_VEL | ctype::TRIPLE_VEL => self.ball_block(first, rows as usize, pair),
                _ => {
                    // Group 3 (Levitation Studio): not decoded; no such connector is built.
                    crate::todo_spec!("DUAL §3.5 / Open 2: the type-3 kernel (Levitation)");
                }
            }
        }
        if bilateral {
            self.bilateral_sweep();
        }
    }

    /// Studio (2 rows) / Studio (3 rows) [J2] (DUAL §3.5): a Jacobi block whose
    /// new impulses x_k = d·(rhs − Ĵ·u + λ) (all from the same u) are projected per lane
    /// onto the ball of radius row 0's hi: s = min(1, hi / |x|) (rsqrtps; |x|² ≤ 0 → 1),
    /// λ_k = (f64)(x_k·s), Δ_k = x_k·s − (f32)λ_k(old), then u += Δ_k·W_k row by row.
    #[inline]
    fn ball_block(&mut self, r0: usize, n: usize, p: [u32; 2]) {
        let mut x = [[0.0f32; 2]; 3];
        for k in 0..n {
            let ju = dot_lane(&self.jh[r0 + k], &self.u, p);
            let st = &self.st[r0 + k];
            for l in 0..2 {
                x[k][l] = (self.d[r0 + k][l] * (st[4 + l] - ju[l] as f64 + st[6 + l])) as f32;
            }
        }
        let mut s = [1.0f32; 2];
        for l in 0..2 {
            let n2: f32 = (0..n).map(|k| x[k][l] * x[k][l]).sum();
            let hi = self.st[r0][2 + l] as f32;
            let v = if n2 > 0.0 { hi / n2.sqrt() } else { 1.0 };
            s[l] = if v < 1.0 { v } else { 1.0 };
        }
        for k in 0..n {
            let st = &mut self.st[r0 + k];
            let mut dl = [0.0f32; 2];
            for l in 0..2 {
                let nl = x[k][l] * s[l];
                dl[l] = nl - st[6 + l] as f32;
                st[6 + l] = nl as f64;
            }
            add_lane(&mut self.u, p, dl, &self.w[r0 + k]);
        }
    }

    /// Studio (DUAL §3.4), one box row, both lanes:
    /// λ' = clamp(d·(rhs − Ĵ·u + λ), lo, hi); Δ = (f32)(λ' − λ); u += Δ·W.
    #[inline]
    fn box_row(&mut self, r: usize) {
        let p = self.pair[r];
        let ju = dot_lane(&self.jh[r], &self.u, p);
        let st = &mut self.st[r];
        let mut dl = [0.0f32; 2];
        for l in 0..2 {
            let x = self.d[r][l] * (st[4 + l] - ju[l] as f64 + st[6 + l]);
            let nl = x.max(st[l]).min(st[2 + l]);
            dl[l] = (nl - st[6 + l]) as f32;
            st[6 + l] = nl;
        }
        add_lane(&mut self.u, p, dl, &self.w[r]);
    }

    /// Studio (DUAL §3.3, DUALROWS §3): one contact connector, rows n, t1, t2
    /// as a Jacobi block; d = row 0's lo pair; normal floor 0, friction box
    /// λn'·[lo, hi] from this sweep's λn'.
    #[inline]
    fn contact_block(&mut self, r0: usize, p: [u32; 2]) {
        let j0 = dot_lane(&self.jh[r0], &self.u, p);
        let j1 = dot_lane(&self.jh[r0 + 1], &self.u, p);
        let j2 = dot_lane(&self.jh[r0 + 2], &self.u, p);
        let mut dl = [[0.0f32; 2]; 3];
        for l in 0..2 {
            let d = self.st[r0][l];
            let s0 = &mut self.st[r0];
            let n = (d * (s0[4 + l] - j0[l] as f64 + s0[6 + l])).max(0.0);
            dl[0][l] = (n - s0[6 + l]) as f32;
            s0[6 + l] = n;
            for (k, jk) in [(1usize, j1), (2, j2)] {
                let s = &mut self.st[r0 + k];
                let x = d * (s[4 + l] - jk[l] as f64 + s[6 + l]);
                let nl = x.max(n * s[l]).min(n * s[2 + l]);
                dl[k][l] = (nl - s[6 + l]) as f32;
                s[6 + l] = nl;
            }
        }
        add_block(&mut self.u, p, &dl, &self.w[r0..r0 + 3]);
    }

    /// Studio (DUAL §4.2): bilateral rows, unclamped GS on (JM⁻¹Jᵀ + ε)λ = rhs:
    /// δ = s·rhs − (s·ε)·λ − Ĵ·u; λ += δ; u += (f32)δ·W.
    fn bilateral_sweep(&mut self) {
        for r in 0..self.ldl.len() {
            let p = self.ldl.pair[r];
            let ju = dot_lane(&self.ldl.jh[r], &self.u, p);
            let mut dl = [0.0f32; 2];
            for l in 0..2 {
                let d = self.ldl.srhs[r][l] - self.ldl.seps[r][l] * self.ldl.lambda[r][l] - ju[l] as f64;
                self.ldl.lambda[r][l] += d;
                dl[l] = d as f32;
            }
            add_lane(&mut self.u, p, dl, &self.ldl.w[r]);
        }
    }

    /// The connectors in solve order with their results, for the write-back
    /// (DUAL §5.4 step 5): (src, PGS λ pairs and ω pairs of its PGS rows, LDL λ
    /// pairs of its LDL rows).
    pub fn results(&self) -> impl Iterator<Item = (ConnSrc, &[[f64; 8]], &[[f32; 2]], &[[f64; 2]])> + '_ {
        self.order.iter().map(move |&c| {
            let c = c as usize;
            let rows = self.c_rows[c] as usize;
            let mask = if self.c_type[c] == ctype::JOINT { self.c_mask[c] } else { 0 };
            let nl = mask.count_ones() as usize;
            let np = rows - nl;
            let (p, l) = (self.c_pgs[c] as usize, self.c_ldl[c] as usize);
            (self.c_src[c], &self.st[p..p + np], &self.omega[p..p + np], &self.ldl.lambda[l..l + nl])
        })
    }
}

/// The LDL correction of DUAL §4.3 steps 2: per component, r ← K⁻¹ r in place
/// (applyInverseLDL Studio with the factor of Studio, §4.1), and the
/// condition estimate max pivot / min pivot of each component into `cond` of its
/// connectors' bodies (STEP §3.3a, Studio). Lane JOINTS (linsys.rs) fills this
/// in; returns whether `ldl.r` now holds the correction. Without it the batch
/// still runs every sweep (the bilateral rows are then plain GS), and no
/// correction is applied.
pub fn ldl_correction(solver: &mut crate::solver::linsys::DualLdl, ldl: &mut LdlSet, n_sim: u32, cond: &mut [f32]) -> bool {
    if ldl.len() == 0 {
        return false;
    }
    let LdlSet { j, w, eps, conn_first, conn_rows, conn_pair, r, .. } = ldl;
    solver.correct(j, w, eps, conn_first, conn_rows, conn_pair, n_sim, r, cond)
}
