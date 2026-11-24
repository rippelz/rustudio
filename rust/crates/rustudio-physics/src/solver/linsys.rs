//! Two linear systems:
//! * `DualLdl` (legacy path, lane JOINTS): the direct part of "Solve Batch", DUAL §4 —
//!   per connected component of the bilateral ("LDL") rows, (J·M⁻¹·Jᵀ + ε)·Δ = r in both
//!   lanes, a block-sparse LDLᵀ over the components' connector graph.
//! * `LinSys` (next-gen path, not ported now): the primal island system below.
//!
//! The next-gen (primal) island system: per-body packed 6×6 blocks, off-diagonal
//! blocks per coupled body pair, and the linear solve.
//!
//! Studio (LINSYS.md): buildSystem Studio accumulates per-body packed
//! symmetric 6×6 blocks (21 floats, §2.4) through the one-body kernel Studio
//! and two-body items (§3.4); linearSolve Studio picks one of: block-diagonal
//! direct, two-body direct Studio, PCR (§4). The layout below is the
//! harness's; LINSYS decides the method, the float widths (f32 / f64 per field)
//! and may change the types (§8 "Port struct").

use glam::Vec3;
use rustc_hash::FxHashMap;

use super::body::{BodySet, NONE};
use super::rows::RowBuffer;

/// Packed upper triangle of a symmetric 6×6, row-major: (0,0) (0,1) … (0,5) (1,1) … (5,5).
pub type Sym6 = [f64; 21];
pub type Mat6 = [f64; 36];

#[inline]
pub fn sym6_index(r: usize, c: usize) -> usize {
    let (r, c) = if r <= c { (r, c) } else { (c, r) };
    r * 6 - r * (r + 1) / 2 + c
}

#[derive(Default)]
pub struct LinSys {
    /// Island-local body count.
    pub n: usize,
    /// Diagonal blocks (mass + JᵀWJ of rows touching the body alone).
    pub diag: Vec<Sym6>,
    /// Off-diagonal blocks: (i, j) with i < j, block = Σ J_iᵀ W J_j.
    pub off_ij: Vec<(u32, u32)>,
    pub off: Vec<Mat6>,
    /// Right-hand side and solution, 6 per body (linear, angular).
    pub rhs: Vec<f64>,
    pub x: Vec<f64>,
    /// Method state / factor storage (LINSYS §4, §6 what persists).
    pub order: Vec<u32>,
    pub factor_diag: Vec<Sym6>,
    pub factor_off: Vec<Mat6>,
}

impl LinSys {
    pub fn reset(&mut self, n: usize) {
        self.n = n;
        self.diag.clear();
        self.diag.resize(n, [0.0; 21]);
        self.off_ij.clear();
        self.off.clear();
        self.rhs.clear();
        self.rhs.resize(6 * n, 0.0);
        self.x.clear();
        self.x.resize(6 * n, 0.0);
        self.order.clear();
        self.factor_diag.clear();
        self.factor_off.clear();
    }

    /// Mass blocks of the island's bodies, then every row's JᵀWJ.
    pub fn assemble(&mut self, island: &[u32], bodies: &BodySet, rows: &RowBuffer, h: f32) {
        self.reset(island.len());
        let _ = (bodies, rows, h, NONE);
        if rows.len() == 0 {
            return;
        }
        crate::todo_spec!("LINSYS §3 buildSystem : inertia term (3.2), one-body kernel  (3.3), two-body items (3.4)");
    }

    /// Prepares the chosen method (LINSYS §4.1).
    pub fn factor(&mut self) {
        if self.off.is_empty() && self.diag.iter().all(|d| d.iter().all(|v| *v == 0.0)) {
            return;
        }
        crate::todo_spec!("LINSYS §4.1 method choice; §4.2 block-diagonal, §4.3 two-body direct , §4.4 PCR, §4.5 6x6 LDL");
    }

    /// Solves with the factor: x = A⁻¹ rhs.
    pub fn solve(&mut self) {
        if self.factor_diag.is_empty() {
            return;
        }
        crate::todo_spec!("LINSYS §4 linearSolve : the solve with the chosen method");
    }
}

// =====================================================================================
// Legacy path: the dual LDL (DUAL §4)
// =====================================================================================

/// Most bilateral rows one connector has (DUAL §4.1: blocks ≤ 6 rows).
pub const MAX_BLOCK: usize = 6;

/// One LDL row as the batch hands it over (DUAL §0.2 X / X): J and W = M⁻¹Jᵀ
/// per lane (v, p), each `[A.lin, A.ang, B.lin, B.ang]`; W unscaled (the body scale
/// X is the connector's `scale`).
#[derive(Debug, Clone, Copy, Default)]
pub struct LdlRow {
    pub j: [[Vec3; 4]; 2],
    pub w: [[Vec3; 4]; 2],
}

impl LdlRow {
    /// A_ii = J·W per lane, f32: the A half then the B half.
    pub fn diag(&self) -> [f32; 2] {
        let mut d = [0.0f32; 2];
        for l in 0..2 {
            let (j, w) = (&self.j[l], &self.w[l]);
            d[l] = (j[0].dot(w[0]) + j[1].dot(w[1])) + (j[2].dot(w[2]) + j[3].dot(w[3]));
        }
        d
    }
}

/// ε per connector (Studio, DUAL §4.1): ε_ℓ = mean_r(A_rr,ℓ)·max(cfg_ℓ, floor_ℓ)
/// (cfg_v = cfg = 1e-5, cfg_p = cfg = 1e-3; the §3.2 max rule).
pub fn ldl_eps(diag: &[[f32; 2]], floor: [f32; 2], cfg: [f32; 2]) -> [f32; 2] {
    let n = diag.len().max(1) as f32;
    let mut e = [0.0f32; 2];
    for l in 0..2 {
        let mean = diag.iter().map(|d| d[l]).sum::<f32>() / n;
        let c = if cfg[l] < floor[l] { floor[l] } else { cfg[l] };
        e[l] = mean * c;
    }
    e
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConnSig {
    key: u64,
    a: u32,
    b: u32,
    n: u8,
}

/// The cached symbolic structure of one batch's LDL set (DUAL §6.4: "LDL symbolic
/// structure per component key set", cfg): components, elimination order (the
/// sorted connector order, Open 1 leaves Studio's order open; any order is exact) and
/// the block pattern with fill-in.
#[derive(Default)]
struct Symbolic {
    sig: Vec<ConnSig>,
    /// Connector indices grouped by component; `comp_start[c]..comp_start[c+1]`.
    comp_conns: Vec<u32>,
    comp_start: Vec<u32>,
    /// Per connector: offset of its diagonal block (n×n, dense) in the value buffer,
    /// and its later neighbours (in elimination order) as (connector, block offset of
    /// L[i][k], n_i × n_k).
    diag_off: Vec<u32>,
    lower: Vec<(u32, u32)>,
    lower_start: Vec<u32>,
    /// Blocks (i, k) with i, k sharing a body, for the numeric assembly: (i, k, offset,
    /// shared bodies as bit 0 = A_i·A_k … see `assemble`).
    n_values: usize,
    last_used: u64,
}

/// The legacy batch's direct solve (DUAL §4): per component, the regularised bilateral
/// system K_ℓ = J_ℓ M_ℓ⁻¹ J_ℓᵀ + diag(ε_ℓ), both lanes in one factorisation (every entry
/// {f64 v, f64 p}), factored as a block LDLᵀ (one block per connector). The correction
/// (DUAL §4.3 step 2, "applyInverseLDL" Studio) is `solve`. No allocation per tick
/// once warm: the symbolic structure is rebuilt only when the connector graph changes.
#[derive(Default)]
pub struct DualLdl {
    // -- this batch --
    conns: Vec<ConnSig>,
    /// First LDL row of each connector (prefix), body scale per connector side.
    row_start: Vec<u32>,
    scale: Vec<[f32; 2]>,
    rows: Vec<LdlRow>,
    eps: Vec<[f32; 2]>,
    n_sim: u32,
    // -- factor --
    sym: Symbolic,
    cache: FxHashMap<u64, Symbolic>,
    vals: Vec<[f64; 2]>,
    /// Max / min pivot ratio per component (v lane), slot 4 of the arbiter.
    ratio: Vec<f32>,
    tick: u64,
    /// Symbolic rebuilds so far (tests, perf).
    pub rebuilds: u64,
    tmp: Vec<[f64; 2]>,
    /// LDL row of the caller's order for each internal row (`correct`).
    perm: Vec<u32>,
}

impl DualLdl {
    /// Starts a batch. Body indices `< n_sim` are sim bodies; anything else (NONE,
    /// the shared static index, other levels' bodies) couples nothing (scale 0 or
    /// infinite mass) and does not join components.
    pub fn begin(&mut self, n_sim: u32) {
        self.conns.clear();
        self.row_start.clear();
        self.row_start.push(0);
        self.scale.clear();
        self.rows.clear();
        self.eps.clear();
        self.n_sim = n_sim;
        self.tick += 1;
    }

    /// One LDL connector (DUAL §2.1: its `conn` bilateral rows, in sorted order):
    /// `scale` = the bodies' X scale (1 sim, 0 non-sim), `eps` its ε (`ldl_eps`).
    pub fn push_conn(&mut self, key: u64, a: u32, b: u32, rows: &[LdlRow], scale: [f32; 2], eps: [f32; 2]) {
        debug_assert!(rows.len() <= MAX_BLOCK);
        self.conns.push(ConnSig { key, a, b, n: rows.len() as u8 });
        self.rows.extend_from_slice(rows);
        self.row_start.push(self.rows.len() as u32);
        self.scale.push(scale);
        self.eps.push(eps);
    }

    pub fn n_rows(&self) -> usize {
        self.rows.len()
    }

    fn sim(&self, x: u32) -> bool {
        x != NONE && x < self.n_sim
    }

    /// Symbolic analysis (cached) and numeric factorisation (DUAL §4.1, Studio).
    pub fn factor(&mut self) {
        self.symbolic();
        self.numeric();
    }

    fn sig_hash(sig: &[ConnSig]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        sig.hash(&mut h);
        h.finish()
    }

    fn symbolic(&mut self) {
        if self.sym.sig == self.conns {
            self.sym.last_used = self.tick;
            return;
        }
        let hash = Self::sig_hash(&self.conns);
        let old = std::mem::take(&mut self.sym);
        if !old.sig.is_empty() {
            let oh = Self::sig_hash(&old.sig);
            self.cache.insert(oh, old);
        }
        if let Some(s) = self.cache.remove(&hash).filter(|s| s.sig == self.conns) {
            self.sym = s;
            self.sym.last_used = self.tick;
        } else {
            self.sym = self.analyse();
            self.rebuilds += 1;
        }
        // keep the cache small: entries unused for 240 ticks go
        let t = self.tick;
        if self.cache.len() > 64 {
            self.cache.retain(|_, s| t - s.last_used < 240);
        }
    }

    fn analyse(&self) -> Symbolic {
        let n = self.conns.len();
        let mut s = Symbolic { sig: self.conns.clone(), last_used: self.tick, ..Default::default() };
        // components: union-find over connectors through shared sim bodies
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                let up = p[p[x as usize] as usize];
                p[x as usize] = up;
                x = up;
            }
            x
        }
        let mut owner: FxHashMap<u32, u32> = FxHashMap::default();
        for (i, c) in self.conns.iter().enumerate() {
            for body in [c.a, c.b] {
                if !self.sim(body) {
                    continue;
                }
                match owner.get(&body) {
                    Some(&o) => {
                        let (ra, rb) = (find(&mut parent, o), find(&mut parent, i as u32));
                        if ra != rb {
                            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                            parent[hi as usize] = lo;
                        }
                    }
                    None => {
                        owner.insert(body, i as u32);
                    }
                }
            }
        }
        let mut comp_of = vec![u32::MAX; n];
        let mut ncomp = 0u32;
        let mut root_comp: FxHashMap<u32, u32> = FxHashMap::default();
        let mut counts: Vec<u32> = Vec::new();
        for i in 0..n {
            let r = find(&mut parent, i as u32);
            let c = *root_comp.entry(r).or_insert_with(|| {
                ncomp += 1;
                counts.push(0);
                ncomp - 1
            });
            comp_of[i] = c;
            counts[c as usize] += 1;
        }
        s.comp_start.push(0);
        let mut acc = 0;
        for c in &counts {
            acc += c;
            s.comp_start.push(acc);
        }
        s.comp_conns.resize(n, 0);
        let mut cur: Vec<u32> = s.comp_start[..ncomp as usize].to_vec();
        for i in 0..n {
            let c = comp_of[i] as usize;
            s.comp_conns[cur[c] as usize] = i as u32;
            cur[c] += 1;
        }
        // block pattern with fill-in, elimination in connector (sorted) order:
        // adjacency = shared sim body; eliminating k connects its later neighbours.
        let mut adj: Vec<std::collections::BTreeSet<u32>> = vec![Default::default(); n];
        let mut by_body: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for (i, c) in self.conns.iter().enumerate() {
            for body in [c.a, c.b] {
                if self.sim(body) {
                    by_body.entry(body).or_default().push(i as u32);
                }
            }
        }
        for list in by_body.values() {
            for &x in list {
                for &y in list {
                    if x != y {
                        adj[x as usize].insert(y);
                    }
                }
            }
        }
        for k in 0..n {
            let later: Vec<u32> = adj[k].iter().copied().filter(|&i| i as usize > k).collect();
            for &x in &later {
                for &y in &later {
                    if x != y {
                        adj[x as usize].insert(y);
                    }
                }
            }
        }
        let mut off = 0u32;
        for k in 0..n {
            s.diag_off.push(off);
            let nk = self.conns[k].n as u32;
            off += nk * nk;
        }
        s.lower_start.push(0);
        for k in 0..n {
            let nk = self.conns[k].n as u32;
            for &i in adj[k].iter().filter(|&&i| i as usize > k) {
                s.lower.push((i, off));
                off += self.conns[i as usize].n as u32 * nk;
            }
            s.lower_start.push(s.lower.len() as u32);
        }
        s.n_values = off as usize;
        s
    }

    /// K entry (row r of connector i, row q of connector k) per lane, f32 sums widened
    /// (DUAL §4.1 step 2: s_A·(J_r,A·W_q,A) + s_B·(J_r,B·W_q,B) over the shared bodies).
    fn entry(&self, i: usize, r: usize, k: usize, q: usize) -> [f64; 2] {
        let (ci, ck) = (&self.conns[i], &self.conns[k]);
        let ri = &self.rows[(self.row_start[i] as usize) + r];
        let rk = &self.rows[(self.row_start[k] as usize) + q];
        let (si, sk) = (self.scale[i], self.scale[k]);
        let mut out = [0.0f64; 2];
        for l in 0..2 {
            let mut acc = 0.0f32;
            // side x of i against side y of k when they are the same sim body
            for (xa, bi) in [(0usize, ci.a), (2, ci.b)] {
                for (ya, bk) in [(0usize, ck.a), (2, ck.b)] {
                    if bi != bk || !self.sim(bi) {
                        continue;
                    }
                    let sc = if ya == 0 { sk[0] } else { sk[1] };
                    let _ = si;
                    acc += sc * (ri.j[l][xa].dot(rk.w[l][ya]) + ri.j[l][xa + 1].dot(rk.w[l][ya + 1]));
                }
            }
            out[l] = acc as f64;
        }
        out
    }

    fn numeric(&mut self) {
        let nv = self.sym.n_values;
        self.vals.clear();
        self.vals.resize(nv, [0.0; 2]);
        let n = self.conns.len();
        // assemble: diagonal blocks (+ε on the diagonal) and the off-diagonal blocks of
        // coupled connectors (fill-in blocks stay 0)
        for k in 0..n {
            let nk = self.conns[k].n as usize;
            let d = self.sym.diag_off[k] as usize;
            for r in 0..nk {
                for q in 0..nk {
                    let mut e = self.entry(k, r, k, q);
                    if r == q {
                        e[0] += self.eps[k][0] as f64;
                        e[1] += self.eps[k][1] as f64;
                    }
                    self.vals[d + r * nk + q] = e;
                }
            }
            for li in self.sym.lower_start[k]..self.sym.lower_start[k + 1] {
                let (i, off) = self.sym.lower[li as usize];
                let i = i as usize;
                let ni = self.conns[i].n as usize;
                for r in 0..ni {
                    for q in 0..nk {
                        self.vals[off as usize + r * nk + q] = self.entry(i, r, k, q);
                    }
                }
            }
        }
        // right-looking block LDLᵀ in elimination (connector) order
        for k in 0..n {
            let nk = self.conns[k].n as usize;
            let d = self.sym.diag_off[k] as usize;
            dense_ldl(&mut self.vals[d..d + nk * nk], nk);
            let (ls, le) = (self.sym.lower_start[k] as usize, self.sym.lower_start[k + 1] as usize);
            // X_i = A_ik · L_k⁻ᵀ (kept in place), then the Schur updates A_ij −= X_i·D_k⁻¹·X_jᵀ,
            // finally L_ik = X_i·D_k⁻¹
            for li in ls..le {
                let (i, off) = self.sym.lower[li];
                let ni = self.conns[i as usize].n as usize;
                for r in 0..ni {
                    for q in 0..nk {
                        let mut x = self.vals[off as usize + r * nk + q];
                        for t in 0..q {
                            let l = self.vals[d + q * nk + t];
                            let xt = self.vals[off as usize + r * nk + t];
                            x[0] -= xt[0] * l[0];
                            x[1] -= xt[1] * l[1];
                        }
                        self.vals[off as usize + r * nk + q] = x;
                    }
                }
            }
            for a in ls..le {
                let (i, oi) = self.sym.lower[a];
                let ni = self.conns[i as usize].n as usize;
                for bq in ls..le {
                    let (j, oj) = self.sym.lower[bq];
                    if j > i {
                        continue;
                    }
                    let nj = self.conns[j as usize].n as usize;
                    // target block: (i, j) with i ≥ j: diagonal of i, or L[i][j] stored under j
                    let target = if i == j {
                        Some(self.sym.diag_off[i as usize] as usize)
                    } else {
                        let (s, e) = (self.sym.lower_start[j as usize] as usize, self.sym.lower_start[j as usize + 1] as usize);
                        self.sym.lower[s..e].iter().find(|(x, _)| *x == i).map(|(_, o)| *o as usize)
                    };
                    let Some(t) = target else { continue };
                    for r in 0..ni {
                        for c in 0..nj {
                            let mut acc = [0.0f64; 2];
                            for q in 0..nk {
                                let dq = self.vals[d + q * nk + q];
                                let xi = self.vals[oi as usize + r * nk + q];
                                let xj = self.vals[oj as usize + c * nk + q];
                                for l in 0..2 {
                                    if dq[l] != 0.0 {
                                        acc[l] += xi[l] * xj[l] / dq[l];
                                    }
                                }
                            }
                            let e = &mut self.vals[t + r * nj + c];
                            e[0] -= acc[0];
                            e[1] -= acc[1];
                        }
                    }
                }
            }
            for li in ls..le {
                let (i, off) = self.sym.lower[li];
                let ni = self.conns[i as usize].n as usize;
                for r in 0..ni {
                    for q in 0..nk {
                        let dq = self.vals[d + q * nk + q];
                        let x = &mut self.vals[off as usize + r * nk + q];
                        for l in 0..2 {
                            x[l] = if dq[l] != 0.0 { x[l] / dq[l] } else { 0.0 };
                        }
                    }
                }
            }
        }
        // pivot ratio per component (Studio, STEP §3.3a): max / min of the v-lane
        // pivots as f32, `<=` compares, 0 when empty
        self.ratio.clear();
        for c in 0..self.sym.comp_start.len().saturating_sub(1) {
            let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
            for &k in &self.sym.comp_conns[self.sym.comp_start[c] as usize..self.sym.comp_start[c + 1] as usize] {
                let nk = self.conns[k as usize].n as usize;
                let dd = self.sym.diag_off[k as usize] as usize;
                for q in 0..nk {
                    let p = self.vals[dd + q * nk + q][0] as f32;
                    if p <= lo {
                        lo = p;
                    }
                    if hi <= p {
                        hi = p;
                    }
                }
            }
            self.ratio.push(if lo.is_infinite() { 0.0 } else { hi / lo });
        }
    }

    /// r ← K⁻¹ r in place per lane (DUAL §4.3 step 2), `r` indexed by LDL row in push order.
    pub fn solve(&mut self, r: &mut [[f64; 2]]) {
        let n = self.conns.len();
        // forward: L z = b
        for k in 0..n {
            let nk = self.conns[k].n as usize;
            let d = self.sym.diag_off[k] as usize;
            let rk = self.row_start[k] as usize;
            for q in 0..nk {
                for t in 0..q {
                    let l = self.vals[d + q * nk + t];
                    let zt = r[rk + t];
                    r[rk + q][0] -= l[0] * zt[0];
                    r[rk + q][1] -= l[1] * zt[1];
                }
            }
            for li in self.sym.lower_start[k]..self.sym.lower_start[k + 1] {
                let (i, off) = self.sym.lower[li as usize];
                let ri = self.row_start[i as usize] as usize;
                let ni = self.conns[i as usize].n as usize;
                for a in 0..ni {
                    for q in 0..nk {
                        let l = self.vals[off as usize + a * nk + q];
                        let z = r[rk + q];
                        r[ri + a][0] -= l[0] * z[0];
                        r[ri + a][1] -= l[1] * z[1];
                    }
                }
            }
        }
        // diagonal
        for k in 0..n {
            let nk = self.conns[k].n as usize;
            let d = self.sym.diag_off[k] as usize;
            let rk = self.row_start[k] as usize;
            for q in 0..nk {
                let p = self.vals[d + q * nk + q];
                for l in 0..2 {
                    r[rk + q][l] = if p[l] != 0.0 { r[rk + q][l] / p[l] } else { 0.0 };
                }
            }
        }
        // backward: Lᵀ x = y
        for k in (0..n).rev() {
            let nk = self.conns[k].n as usize;
            let d = self.sym.diag_off[k] as usize;
            let rk = self.row_start[k] as usize;
            for li in self.sym.lower_start[k]..self.sym.lower_start[k + 1] {
                let (i, off) = self.sym.lower[li as usize];
                let ri = self.row_start[i as usize] as usize;
                let ni = self.conns[i as usize].n as usize;
                for q in 0..nk {
                    for a in 0..ni {
                        let l = self.vals[off as usize + a * nk + q];
                        let x = r[ri + a];
                        r[rk + q][0] -= l[0] * x[0];
                        r[rk + q][1] -= l[1] * x[1];
                    }
                }
            }
            for q in (0..nk).rev() {
                for t in q + 1..nk {
                    let l = self.vals[d + t * nk + q];
                    let x = r[rk + t];
                    r[rk + q][0] -= l[0] * x[0];
                    r[rk + q][1] -= l[1] * x[1];
                }
            }
        }
    }

    /// Components of the factor: count, each one's max / min pivot ratio (slot 4) and
    /// its connectors (indices in push order).
    pub fn n_components(&self) -> usize {
        self.ratio.len()
    }
    pub fn component_ratio(&self, c: usize) -> f32 {
        self.ratio[c]
    }
    pub fn component_conns(&self, c: usize) -> &[u32] {
        &self.sym.comp_conns[self.sym.comp_start[c] as usize..self.sym.comp_start[c + 1] as usize]
    }
    /// First LDL row of connector k (push order) and its row count.
    pub fn conn_rows(&self, k: usize) -> (usize, usize) {
        (self.row_start[k] as usize, self.conns[k].n as usize)
    }
}

/// Dense in-place LDLᵀ of an n×n block (row-major, both lanes): strictly lower part
/// becomes L (unit diagonal implied), the diagonal becomes D.
fn dense_ldl(a: &mut [[f64; 2]], n: usize) {
    for j in 0..n {
        for l in 0..2 {
            let mut dj = a[j * n + j][l];
            for t in 0..j {
                dj -= a[j * n + t][l] * a[j * n + t][l] * a[t * n + t][l];
            }
            a[j * n + j][l] = dj;
            for i in j + 1..n {
                let mut x = a[i * n + j][l];
                for t in 0..j {
                    x -= a[i * n + t][l] * a[j * n + t][l] * a[t * n + t][l];
                }
                // a zero pivot (a row with no sim body) contributes nothing
                a[i * n + j][l] = if dj != 0.0 { x / dj } else { 0.0 };
            }
        }
    }
}

impl DualLdl {
    /// The batch's LDL correction (DUAL §4.3 step 2 and §5.4 step 4) on CORE's LDL set
    /// (dual.rs `LdlSet`, DUAL §0.2 layout): rows per connector contiguous
    /// (`conn_first` / `conn_rows` / `conn_pair`, solve order), J and W lane-interleaved
    /// (§0.3, W already × body scale), ε per row (§4.1). `r` holds the residual on entry
    /// and K⁻¹r on return; each component's max / min pivot goes to `cond` of its
    /// connectors' sim bodies (STEP §3.3a: later components overwrite, non-sim skipped).
    #[allow(clippy::too_many_arguments)]
    pub fn correct(&mut self, j: &[[f32; 24]], w: &[[f32; 24]], eps: &[[f32; 2]], conn_first: &[u32], conn_rows: &[u8], conn_pair: &[[u32; 2]], n_sim: u32, r: &mut [[f64; 2]], cond: &mut [f32]) -> bool {
        if r.is_empty() {
            return false;
        }
        self.begin(n_sim);
        let mut buf = [LdlRow::default(); MAX_BLOCK];
        // Elimination order: the reverse of the solve (sort) order. DUAL Open 1 leaves
        // Studio's order open; it only shows in the pivots (slot 4), and two recordings
        // fix it: ballsocket_chain5 goes to level 2 at tick 7 (reverse order: ratio 69.9;
        // sorted order 14.5 → level 4) and hinge_free stays at level 1 (BallInSocket's
        // block before Align2Axes': 1566; the other way 97 → level 2). JOINTS-NOTES.
        self.perm.clear();
        for c in (0..conn_first.len()).rev() {
            let (f, n) = (conn_first[c] as usize, conn_rows[c] as usize);
            for k in 0..n {
                buf[k] = LdlRow { j: unpack(&j[f + k]), w: unpack(&w[f + k]) };
            }
            let p = conn_pair[c];
            // key: the solve order (the symbolic cache is keyed by the graph: pairs and sizes)
            self.push_conn(c as u64, p[0], p[1], &buf[..n], [1.0, 1.0], eps[f]);
            self.perm.extend(f as u32..(f + n) as u32);
        }
        self.factor();
        self.tmp.clear();
        self.tmp.extend(self.perm.iter().map(|&k| r[k as usize]));
        let mut tmp = std::mem::take(&mut self.tmp);
        self.solve(&mut tmp);
        for (i, &k) in self.perm.iter().enumerate() {
            r[k as usize] = tmp[i];
        }
        self.tmp = tmp;
        for c in 0..self.n_components() {
            let ratio = self.ratio[c];
            for &k in &self.sym.comp_conns[self.sym.comp_start[c] as usize..self.sym.comp_start[c + 1] as usize] {
                let ConnSig { a, b, .. } = self.conns[k as usize];
                for x in [a, b] {
                    if x != NONE && x < n_sim && (x as usize) < cond.len() {
                        cond[x as usize] = ratio;
                    }
                }
            }
        }
        true
    }
}

/// DUAL §0.3 lane-interleaved 24 f32 → per lane [A.lin, A.ang, B.lin, B.ang].
fn unpack(x: &[f32; 24]) -> [[Vec3; 4]; 2] {
    let mut out = [[Vec3::ZERO; 4]; 2];
    for l in 0..2 {
        for side in 0..2 {
            let o = side * 12;
            out[l][2 * side] = Vec3::new(x[o + l], x[o + 4 + l], x[o + 8 + l]);
            out[l][2 * side + 1] = Vec3::new(x[o + 2 + l], x[o + 6 + l], x[o + 10 + l]);
        }
    }
    out
}
