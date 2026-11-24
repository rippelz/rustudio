//! Unit tests of the joint rows and the dual LDL against Studio lab recordings
//! (checks/humanoid-physics/ticks/solver/labs/JOINT-L1.txt) and against a dense solve.

use glam::{Mat3, Vec3};

use super::instance::{Class, Desc, End, Inst};
use super::*;
use super::RowExt;
use crate::solver::linsys::{ldl_eps, DualLdl, LdlRow};

/// A sim body for the tests: inverse mass and world inverse inertia.
struct Body {
    inv_m: f32,
    inv_i: Mat3,
}

fn w_of(j: &[Vec3; 4], a: Option<&Body>, b: Option<&Body>) -> [Vec3; 4] {
    let side = |l: Vec3, g: Vec3, x: Option<&Body>| match x {
        Some(x) => (l * x.inv_m, x.inv_i * g),
        None => (Vec3::ZERO, Vec3::ZERO),
    };
    let (al, aa) = side(j[0], j[1], a);
    let (bl, ba) = side(j[2], j[3], b);
    [al, aa, bl, ba]
}

/// The hinge of lab_j1a..d: anchored 1×1×1 at y 10, free 1×1×1 (Plastic, m = 0.7)
/// hanging from its bottom face, axis X. A = the anchored part (world frame), B = Free.
fn hinge_desc(limits: bool) -> Desc {
    let a = End { body: None, pos: Vec3::new(0.0, 9.5, 0.0), rot: Mat3::IDENTITY, mass: 0.7 };
    let b = End { body: None, pos: Vec3::new(0.0, 0.5, 0.0), rot: Mat3::IDENTITY, mass: 0.7 };
    Desc {
        class: Class::Hinge {
            actuator: 0,
            ang_resp: 45.0,
            ang_speed: 0.0,
            ang_vel: 0.0,
            lower: -30.0,
            upper: 30.0,
            motor_max_acc: f32::INFINITY,
            motor_max_torque: 0.0,
            restitution: 0.0,
            servo_max_torque: 0.0,
            target: 0.0,
            limits,
        },
        a,
        b,
        in_world: true,
    }
}

/// One solve with only the LDL rows acting (PGS rows' bounds are 0 on a first solve:
/// the hinge friction row is bounded by the alignment rows' warm start, which is 0):
/// λ = K⁻¹·rhs, u_B = Σ W_B·λ. Returns B's (Δv, Δω) per lane.
fn ldl_only(inst: &mut Inst, ra: &Rec, rb: &Rec, pa: &Pose, pb: &Pose, body_b: &Body, h: f32) -> [(Vec3, Vec3); 2] {
    let cfg = JointCfg::for_level(1);
    let cx = Ctx { ra, rb, pa, pb, cfg: &cfg, h, gs: None, lam0: [0.0; super::MAX_ROWS] };
    let mut ldl = DualLdl::default();
    ldl.begin(1);
    let mut rhs: Vec<[f64; 2]> = Vec::new();
    let mut ws: Vec<[[Vec3; 4]; 2]> = Vec::new();
    let mut rows = [DualRow::default(); MAX_ROWS];
    let active = inst.active;
    for (ci, c) in inst.conns.iter_mut().enumerate() {
        if active & (1 << ci) == 0 {
            continue;
        }
        c.prepare_rows(&mut rows, h, &cfg);
        c.build(&mut rows, &cx);
        let mut lrows = Vec::new();
        for r in 0..c.n_rows as usize {
            if !c.is_ldl_row(r, &cfg) {
                assert_eq!(rows[r].hi[0], 0.0, "a first solve's PGS row here is bounded to 0");
                continue;
            }
            let w = w_of(&rows[r].jv, None, Some(body_b));
            let wp = w_of(&rows[r].jp, None, Some(body_b));
            lrows.push(LdlRow { j: [rows[r].jv, rows[r].jp], w: [w, wp] });
            rhs.push([rows[r].rhs[0] as f64, rows[r].rhs[1] as f64]);
            ws.push([w, wp]);
        }
        let diag: Vec<[f32; 2]> = lrows.iter().map(|r| r.diag()).collect();
        let eps = ldl_eps(&diag, c.floor, [cfg.compliance_v, cfg.compliance_p]);
        ldl.push_conn(c.key, NONE, 0, &lrows, [0.0, 1.0], eps);
    }
    ldl.factor();
    ldl.solve(&mut rhs);
    let mut out = [(Vec3::ZERO, Vec3::ZERO); 2];
    for (lam, w) in rhs.iter().zip(&ws) {
        for l in 0..2 {
            out[l].0 += w[l][2] * lam[l] as f32;
            out[l].1 += w[l][3] * lam[l] as f32;
        }
    }
    out
}

/// lab_j1d_limits_e0, tick 5 (the first solve, level 1): the free block hangs
/// straight down with ω = (5, 0, 0) about its centre; the hinge keeps the pivot, so
/// ω' = I_c/I_p · ω̃ = 0.4 · 5·0.95^h and the pivot's velocity goes. Studio:
/// vy −1.63912773e-05 (gravity less the ε regularisation), vz −0.99977839, wx 1.9995966.
#[test]
fn hinge_first_solve_matches_lab_j1d() {
    let h = 1.0 / 240.0;
    let mut next = 0;
    let mut inst = Inst::new(&hinge_desc(true), &mut next);
    let m = 0.7f32;
    let body = Body { inv_m: 1.0 / m, inv_i: Mat3::from_diagonal(Vec3::splat(6.0 / m)) };
    // ṽ (DUAL §5.1): v + h·g, ω·0.95^h (the damping's exact form is DUAL Open 5; a
    // scalar factor about one axis is exact here)
    let w0 = 5.0 * (-(-(0.95f32).ln()) * h).exp();
    let rb = Rec { v: Vec3::new(0.0, -196.2 * h, 0.0), w: Vec3::new(w0, 0.0, 0.0), com: Vec3::new(0.0, 9.0, 0.0), v_ref: Vec3::ZERO, w_ref: Vec3::new(5.0, 0.0, 0.0) };
    let ra = Rec::default();
    let pb = Pose { r: Mat3::IDENTITY, p: Vec3::new(0.0, 9.0, 0.0) };
    let du = ldl_only(&mut inst, &ra, &rb, &Pose::IDENTITY, &pb, &body, h);
    let v = rb.v + du[0].0;
    let w = rb.w + du[0].1;
    eprintln!("v {v:?} w {w:?}");
    assert!((v.y - -1.63912773e-05).abs() < 2e-7, "vy {}", v.y);
    assert!((v.z - -0.99977839).abs() < 2e-6, "vz {}", v.z);
    assert!((w.x - 1.9995966).abs() < 2e-6, "wx {}", w.x);
    assert!(v.x.abs() < 1e-6 && w.y.abs() < 1e-6 && w.z.abs() < 1e-6);
}

/// lab_j1b_motor, tick 5: hanging straight down at rest, so only gravity loads the
/// hinge: vy = −0.8175·(ε share) = −1.63912773e-05 again (the motor is a PGS row).
#[test]
fn hinge_hanging_regularisation_matches_lab_j1b() {
    let h = 1.0 / 240.0;
    let mut next = 0;
    let mut inst = Inst::new(&hinge_desc(false), &mut next);
    let m = 0.7f32;
    let body = Body { inv_m: 1.0 / m, inv_i: Mat3::from_diagonal(Vec3::splat(6.0 / m)) };
    let rb = Rec { v: Vec3::new(0.0, -196.2 * h, 0.0), w: Vec3::ZERO, com: Vec3::new(0.0, 9.0, 0.0), v_ref: Vec3::ZERO, w_ref: Vec3::ZERO };
    let pb = Pose { r: Mat3::IDENTITY, p: Vec3::new(0.0, 9.0, 0.0) };
    let du = ldl_only(&mut inst, &Rec::default(), &rb, &Pose::IDENTITY, &pb, &body, h);
    let vy = rb.v.y + du[0].0.y;
    assert!((vy - -1.63912773e-05).abs() < 2e-7, "vy {vy}");
}

fn rng(seed: &mut u64) -> f32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) as f32 / (1u64 << 31) as f32) * 2.0 - 1.0
}

fn rv(seed: &mut u64) -> Vec3 {
    Vec3::new(rng(seed), rng(seed), rng(seed))
}

/// The block LDLᵀ against a dense f64 solve of the same K + ε, on a graph with
/// fill-in (a star of connectors around one body, then a chain, a cycle, and a
/// connector to a static body), both lanes.
#[test]
fn dual_ldl_matches_dense_solve() {
    let mut seed = 7u64;
    let nb = 5u32;
    let bodies: Vec<Body> = (0..nb).map(|i| Body { inv_m: 0.5 + i as f32 * 0.3, inv_i: Mat3::from_diagonal(Vec3::new(1.0 + i as f32, 2.0, 0.7)) }).collect();
    // (a, b, rows); NONE = static
    let conns: [(u32, u32, usize); 7] = [(0, 1, 3), (1, 2, 2), (2, 3, 1), (3, 0, 3), (1, 3, 2), (4, NONE, 3), (NONE, 2, 1)];
    let mut ldl = DualLdl::default();
    for round in 0..2 {
        ldl.begin(nb);
        let mut all: Vec<(u32, u32, LdlRow, [f32; 2])> = Vec::new();
        for (k, &(a, b, n)) in conns.iter().enumerate() {
            let mut rows = Vec::new();
            for _ in 0..n {
                let j = [rv(&mut seed), rv(&mut seed), rv(&mut seed), rv(&mut seed)];
                let ba = (a != NONE).then(|| &bodies[a as usize]);
                let bb = (b != NONE).then(|| &bodies[b as usize]);
                let w = w_of(&j, ba, bb);
                rows.push(LdlRow { j: [j, j], w: [w, w] });
            }
            let diag: Vec<[f32; 2]> = rows.iter().map(|r| r.diag()).collect();
            let eps = ldl_eps(&diag, [0.0; 2], [1e-5, 1e-3]);
            for r in &rows {
                all.push((a, b, *r, eps));
            }
            ldl.push_conn(k as u64, a, b, &rows, [1.0, 1.0], eps);
        }
        ldl.factor();
        let n = all.len();
        let mut rhs: Vec<[f64; 2]> = (0..n).map(|_| [rng(&mut seed) as f64, rng(&mut seed) as f64]).collect();
        let want = rhs.clone();
        ldl.solve(&mut rhs);
        // dense check: K·x = b per lane
        for l in 0..2 {
            let mut worst = 0.0f64;
            for i in 0..n {
                let mut acc = 0.0f64;
                for k in 0..n {
                    // the factor reads the lower triangle (J_row·W_col with row ≥ col): the
                    // f32 sums of J_i·W_k and J_k·W_i differ in the last bit
                    let (p, q) = if i >= k { (i, k) } else { (k, i) };
                    let (ai, bi, ri, _) = &all[p];
                    let (ak, bk, rk, _) = &all[q];
                    let ei = &all[i].3;
                    let mut e = 0.0f32;
                    for (xa, x) in [(0, *ai), (2, *bi)] {
                        for (ya, y) in [(0, *ak), (2, *bk)] {
                            if x == y && x != NONE {
                                e += ri.j[l][xa].dot(rk.w[l][ya]) + ri.j[l][xa + 1].dot(rk.w[l][ya + 1]);
                            }
                        }
                    }
                    let mut e = e as f64;
                    if i == k {
                        e += ei[l] as f64;
                    }
                    acc += e * rhs[k][l];
                }
                worst = worst.max((acc - want[i][l]).abs());
            }
            // relative to the solution's size (K is near-singular with ε = 1e-5·A_ii)
            let xmax = rhs.iter().map(|x| x[l].abs()).fold(0.0f64, f64::max);
            assert!(worst < 1e-12 * (1.0 + xmax), "round {round} lane {l}: residual {worst}, |x| {xmax}");
        }
        // components: {0,1,2,3,4,6} share bodies 0..3; {5} alone (body 4 + static)
        assert_eq!(ldl.n_components(), 2);
        assert!((0..2).all(|c| ldl.component_ratio(c) >= 1.0));
    }
    // the second round had the same graph: the symbolic structure was reused
    assert_eq!(ldl.rebuilds, 1);
}

/// Studio: block rows store λ/h with no adaptation; PGS rows adapt ω toward
/// the 0x74 block's target 1.5 by w = 0.2 per solve when the prediction holds.
#[test]
fn kept_write_back_rules() {
    let cfg = JointCfg::for_level(1);
    let h = 1.0 / 240.0;
    let mut k = Kept::default();
    k.store([0.5, 0.25], [1.0, 1.0], h, false, false, &cfg);
    let inv = 1.0 / h as f64;
    assert_eq!(k.lam, [inv * 0.5, inv * 0.25]);
    assert_eq!(k.c, [1.9, 1.9]);
    let mut k = Kept::default();
    k.store([0.0, 0.0], [1.9, 1.9], h, false, true, &cfg);
    // rel = 0 → t = P[2] = 1.5 ≤ c → w = P[5] = 0.2: c = 0.8·1.9 + 0.2·1.5
    assert!((k.c[0] - (0.8 * 1.9 + 0.2 * 1.5)).abs() < 1e-6);
    assert!((k.a[0] - 0.1).abs() < 1e-7);
}

/// JOINT §5 with the spring scene's numbers (k 200, d 5, two 0.7-mass parts): T = 2.919,
/// dtmax = 0.0465 ≥ 4h → L = 4 → conn = min(L, 2) = 2.
#[test]
fn spring_arbiter_caps_at_two() {
    assert_eq!(spring_level(200.0, 5.0, 0.35), 2);
    // a very stiff spring on light parts asks for 240 Hz
    assert_eq!(spring_level(1e6, 1e3, 0.35), 1);
}

/// JOINT D1.5 / D3.1 step 6 [J2]: the ball joint's GS block is r·fᵀ − (r·f)·I.
#[test]
fn gs_block_is_outer_minus_dot() {
    let r = glam::Vec3::new(0.3, -0.7, 1.1);
    let f = glam::Vec3::new(-2.0, 0.5, 4.0);
    let g = super::dual_rows::gs_block(r, f);
    for row in 0..3 {
        for col in 0..3 {
            let want = r[row] * f[col] - if row == col { r.dot(f) } else { 0.0 };
            assert!((g.col(col)[row] - want).abs() < 1e-6, "({row},{col}) {} vs {want}", g.col(col)[row]);
        }
    }
}

/// [J3] Studio (BodyPosition / BodyGyro K, D clamp) on lab_mover_bodyposition's P 1e4,
/// D 1250 at the world step 1/240: overdamped, 0.5·D = 625 > 0.8·240 → f = 0.8/(625/240).
#[test]
fn mover_kd_clamp() {
    let (k, d) = dual_rows::mover_kd(10000.0, 1250.0, 1.0 / 240.0);
    let f = 0.8 / (625.0 * (1.0 / 240.0));
    assert!((d - 1250.0 * f).abs() < 1e-3 && (k - 10000.0 * f * f).abs() < 1e-2, "{k} {d}");
    // underdamped and slow enough: untouched
    assert_eq!(dual_rows::mover_kd(100.0, 1.0, 1.0 / 240.0), (100.0, 1.0));
}

/// [J3] Studio: an axis-aligned X gives (Y, Z); the pair is orthonormal with s2 = n × s1.
#[test]
fn perp_pair_b_reading() {
    let (s1, s2) = dual_rows::perp_pair_b(Vec3::X);
    assert_eq!((s1, s2), (Vec3::Y, Vec3::Z));
    let n = Vec3::new(0.3, -0.8, 0.52).normalize();
    let (s1, s2) = dual_rows::perp_pair_b(n);
    assert!(s1.dot(n).abs() < 1e-6 && s2.dot(n).abs() < 1e-6 && (s2 - n.cross(s1)).length() < 1e-6);
    assert_eq!(s1.x, 0.0); // |n.y| is the largest → s1 = (0, n.z, −n.y) normalised
}

/// [J3] the Plane wrapper's world tangents: lab_lv_plane sets
/// Primary (1,0,0), Secondary (0,0,1); a secondary parallel to the primary keeps the old one.
#[test]
fn plane_tangents() {
    let mut c = dual_rows::DoubleLinVel::new();
    c.set_world_tangents(Vec3::X, Vec3::Z);
    assert_eq!((c.world_p, c.world_s), (Vec3::X, Vec3::Z));
    c.set_world_tangents(Vec3::X, Vec3::X * 2.0);
    assert_eq!((c.world_p, c.world_s), (Vec3::X, Vec3::Z));
}

/// [J3] BodyGyro's first slot-78 call (filters at zero: e = 1) on a body turned about y
/// only: the y torque −I·(P·v0 + D·ω_y) with v0 = −(Rᵀ Z_goal).x, and no x / z torque.
#[test]
fn gyro_first_step() {
    let mut c = dual_rows::BodyAngVel { target: Vec3::ZERO, hi: Vec3::ZERO, lo: Vec3::ZERO, basis: Mat3::IDENTITY, frame: 2, use_current: false, legacy: true, gyro: Some(Box::new(dual_rows::Gyro { target: Mat3::IDENTITY, max_torque: Vec3::splat(4e5), p: 3000.0, d: 500.0, part_rot: Mat3::IDENTITY, filt: [[0.0; 4]; 3] })) };
    let r = Mat3::from_rotation_y(0.3);
    let i = Vec3::splat(2.0);
    dual_rows::gyro_step(&mut c, r, Vec3::ZERO, i, 1.0 / 240.0);
    let (p, _) = dual_rows::mover_kd(3000.0, 500.0, 1.0 / 240.0);
    let v0 = -(r.transpose() * Vec3::Z).x;
    let ty = -i.y * (v0 * p);
    assert!((c.target.y - ty / i.y / 240.0).abs() < 1e-6, "{:?}", c.target);
    assert!(c.target.x.abs() < 1e-6 && c.target.z.abs() < 1e-6);
    assert_eq!(c.hi, Vec3::splat(8e5));
    // the filters advanced: g = 0.1, h0 = the error
    assert!((c.gyro.as_ref().unwrap().filt[1][0] - 0.1).abs() < 1e-7);
}

/// Class::Fixed produces 6 rigid equality rows (3 positional + 3 orientational)
/// locking two dynamic bodies together (used for dynamic seat welds).
#[test]
fn fixed_joint_builds_six_rigid_rows() {
    let desc = Desc {
        class: Class::Fixed,
        a: End { body: None, pos: Vec3::new(0.0, 1.0, 0.0), rot: Mat3::IDENTITY, mass: 10.0 },
        b: End { body: None, pos: Vec3::new(0.0, -1.0, 0.0), rot: Mat3::IDENTITY, mass: 2.0 },
        in_world: true,
    };
    let mut next = 0;
    let mut inst = Inst::new(&desc, &mut next);
    assert_eq!(inst.active.count_ones(), 2, "Fixed weld builds BallInSocket + Orient connectors");
    let h = 1.0 / 240.0;
    let ra = Rec::default();
    let rb = Rec {
        v: Vec3::ZERO,
        w: Vec3::ZERO,
        com: Vec3::new(0.1, 2.0, 0.0),
        v_ref: Vec3::ZERO,
        w_ref: Vec3::ZERO,
    };
    let pb = Pose { r: Mat3::from_rotation_y(0.1), p: Vec3::new(0.1, 2.0, 0.0) };
    let body_b = Body { inv_m: 0.5, inv_i: Mat3::from_diagonal(Vec3::splat(1.0)) };
    let [(dv, dw), (dp, dq)] = ldl_only(&mut inst, &ra, &rb, &Pose::IDENTITY, &pb, &body_b, h);
    let total_rows: usize = inst.conns.iter().enumerate()
        .filter(|(ci, _)| inst.active & (1 << ci) != 0)
        .map(|(_, c)| c.n_rows as usize)
        .sum();
    assert_eq!(total_rows, 6, "Fixed weld must lock all 6 DOF");
    assert!(dv.length() < 1e-4 && dw.length() < 1e-4, "zero initial vel needs no vel impulse: {dv:?} {dw:?}");
    assert!(dp.x < -0.05, "position lane corrects +X offset: {dp:?}");
    assert!(dq.y < -0.05, "position lane corrects +Y yaw offset: {dq:?}");
}
