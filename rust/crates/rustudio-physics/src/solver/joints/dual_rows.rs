//! Slot-10 row builders of the legacy path, one per connector class (JOINT D3,
//! §4.2–§4.4, §4.9, §4.11). Every builder writes the rows DUAL §2.3 describes
//! (b = rhs[0], e = rhs[1], bounds per lane, both Jacobian copies) for the rows
//! D1.1 step 1 pre-set (`DualRow::prepare`). Notation as JOINT D3.9: `cA`/`cB` =
//! world centre of mass (rec), `v`/`ω` = rec[0..5] (the batch's start-of-solve
//! iterate = ṽ), `v_ref`/`ω_ref` = rec/ (step start), `R·a` the pose
//! rotation times a vector in the body frame, `pX = R_X·a + p_X`. All f32.
//! Addresses are 0.744's (Studio's exe).

use glam::{Mat3, Vec3};

use super::{Ctx, DualRow, RowExt};

pub const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];
/// π, 180 as JOINT D3.9 reads them (.rdata Studio).
const PI: f32 = 3.1415927;
const TAU: f32 = 6.2831855; // .rdata Studio

#[inline]
pub fn rad(x: f32) -> f32 {
    x * PI / 180.0
}

#[inline]
pub fn velpt(v: Vec3, w: Vec3, r: Vec3) -> Vec3 {
    v + w.cross(r)
}

/// Clamp as Studio does it: `min` then `max` (NaN-tolerant like maxss/minss).
#[inline]
fn clamp(x: f32, lo: f32, hi: f32) -> f32 {
    let x = if hi <= x { hi } else { x };
    if x <= lo { lo } else { x }
}

/// The substep integrator Studio(h, K, D, &x, &v, m) (JOINT D3.9, transcribed
/// from ../dumps/dc/Studio.c): n = (int)(h/(1/240) + 0.5)·m explicit steps
/// `a = −(K·x + D·v)·dt; x += v·dt; v += a`, returns Σa. At level 1 (n = 1) it is
/// exactly `−(K·x + D·v)·h`. The callers' argument mapping is only visible at level 1
/// in the spec (the stack arguments are not listed); each call site below says
/// which mapping it uses.
pub fn substep(h: f32, k: f32, d: f32, x0: f32, v0: f32, m: i32) -> f32 {
    let n = ((h / 0.0041666669 + 0.5) as i32) * m;
    if n <= 0 {
        return 0.0;
    }
    let dt = h / n as f32;
    let (mut x, mut v, mut sum) = (x0, v0, 0.0f32);
    // (Studio unrolls by 8 and adds the 8 increments to the sum as a group;
    // n ≤ 4 at the live levels, so the plain loop is the same order.)
    for _ in 0..n {
        let a = -(x * k + v * d) * dt;
        x += v * dt;
        v += a;
        sum += a;
    }
    sum
}

/// Studio, the 3-vector twin (AchievePosition, LinearSpring): per component.
pub fn substep3(h: f32, k: f32, d: f32, x: Vec3, v: Vec3, m: i32) -> Vec3 {
    Vec3::new(substep(h, k, d, x.x, v.x, m), substep(h, k, d, x.y, v.y, m), substep(h, k, d, x.z, v.z, m))
}

/// Two unit perpendiculars of a unit direction: Studio not known (JOINT
/// D3.14 step 4, D3.15 step 4); the rows built on them are basis-invariant except for
/// per-row box bounds, so any orthonormal pair is used (JOINT-NOTES "perpendiculars").
pub fn perps(n: Vec3) -> (Vec3, Vec3) {
    let (a, b) = n.any_orthonormal_pair();
    (a, b)
}

/// Studio(n, &s1, &s2, 0) (asm, [J3]): Align2Axes' B setter Studio calls it on
/// its axis: n normalised; s1 = (−n.y, n.x, 0) when |n.x| ≥ |n.y| and
/// |n.x| ≥ |n.z|, else (0, n.z, −n.y); s1 normalised; s2 = n × s1.
pub fn perp_pair_b(n: Vec3) -> (Vec3, Vec3) {
    let n = n.normalize_or_zero();
    let s1 = if n.x.abs() >= n.y.abs() && n.x.abs() >= n.z.abs() { Vec3::new(-n.y, n.x, 0.0) } else { Vec3::new(0.0, n.z, -n.y) };
    let s1 = s1.normalize_or_zero();
    let s2 = Vec3::new(n.y * s1.z - n.z * s1.y, s1.x * n.z - n.x * s1.z, n.x * s1.y - s1.x * n.y);
    (s1, s2)
}

// ---------------------------------------------------------------------------------
// D3.1 BallInSocket — Studio (3 rows, mask 7)
pub fn ball_in_socket(rows: &mut [DualRow], cx: &Ctx, p_a: Vec3, p_b: Vec3, use_com_a: bool) {
    // 1. pA = conn ? cA : R_A·() + p_A;  pB = R_B·() + p_B
    let pa = if use_com_a { cx.ra.com } else { cx.pa.point(p_a) };
    let pb = cx.pb.point(p_b);
    // 2. lever point at the midpoint when cfg (live 1)
    let (qa, qb) = if cx.cfg.midpoint_lever {
        let m = (pa + pb) * 0.5;
        (m, m)
    } else {
        (pa, pb)
    };
    let ra = qa - cx.ra.com;
    let rb = qb - cx.rb.com;
    // 4. b_i = (velpt_B − velpt_A)_i (current v, ω)
    let dv = velpt(cx.rb.v, cx.rb.w, rb) - velpt(cx.ra.v, cx.ra.w, ra);
    // 5. e = L > 0 ? d·(min(L·cfg, cfg)/L) : 0
    let d = pb - pa;
    let l = d.length();
    let e = if l > 0.0 { d * ((l * cx.cfg.lin_frac).min(cx.cfg.max_lin) / l) } else { Vec3::ZERO };
    for i in 0..3 {
        let ei = AXES[i];
        // 3. Ja = (e_i, rA × e_i); Jb = (−e_i, −(rB × e_i))
        rows[i].set_j(ei, ra.cross(ei), -ei, -(rb.cross(ei)));
        rows[i].rhs = [dv[i], e[i]];
        // 6. bounds stay ±∞
    }
    // 6. GS [J2] (Studio l.207–240; gate !SimSkipGeometricStiffnessWithoutCache (live
    // True) || cfg (warm start, live 1)): f = h²·(K[0..3].λ0) (f32 of the f64), per
    // side G = r·fᵀ − (r·f)·I (the 9 floats as Studio forms them); p9[0] −= G(rA)
    //, p9[1] += G(rB).
    if cx.cfg.warm_start {
        let h2 = cx.h * cx.h;
        let f = Vec3::new(cx.lam0[0] as f32 * h2, cx.lam0[1] as f32 * h2, cx.lam0[2] as f32 * h2);
        cx.gs_add(0, gs_block(ra, f), false);
        cx.gs_add(1, gs_block(rb, f), true);
    }
}

/// The ball joint's geometric-stiffness block (Studio l.217–226), row-major
/// [−(ry·fy) − rz·fz, rx·fy, rx·fz; fx·ry, −(fx·rx) − rz·fz, ry·fz; fx·rz, rz·fy, −(fx·rx) − ry·fy].
pub fn gs_block(r: Vec3, f: Vec3) -> Mat3 {
    Mat3::from_cols(
        Vec3::new(-(r.y * f.y) - r.z * f.z, f.x * r.y, f.x * r.z),
        Vec3::new(r.x * f.y, -(f.x * r.x) - r.z * f.z, r.z * f.y),
        Vec3::new(r.x * f.z, r.y * f.z, -(f.x * r.x) - r.y * f.y),
    )
}

// ---------------------------------------------------------------------------------
// D3.2 Align2Axes — Studio (3 rows, mask 3)
#[derive(Clone)]
pub struct Align2Axes {
    pub axis_a: Vec3,  //
    pub axis_b: Vec3,  //
    pub s1_b: Vec3,    //
    pub s2_b: Vec3,    //
    pub s1_w: Vec3,    // (world, last build)
    pub s2_w: Vec3,    //
    pub servo: bool,   //
    pub max_torque: f32, //
    pub max_speed: f32,  //
    pub k: f32,        //
    pub d: f32,        //
}

impl Align2Axes {
    pub fn new(axis_a: Vec3, axis_b: Vec3, s1_b: Vec3, s2_b: Vec3) -> Self {
        Align2Axes { axis_a, axis_b, s1_b, s2_b, s1_w: Vec3::ZERO, s2_w: Vec3::ZERO, servo: false, max_torque: 0.0, max_speed: 0.0, k: 0.0, d: 0.0 }
    }
}

fn reproject(rows: &mut [DualRow], s1_old: Vec3, s2_old: Vec3, s1: Vec3, s2: Vec3) {
    // Λ = s1_old·λ[0] + s2_old·λ[1] (f64, per lane), then row k.λ = s_k·Λ
    for lane in 0..2 {
        let l0 = rows[0].lambda[lane];
        let l1 = rows[1].lambda[lane];
        let big = [
            s1_old.x as f64 * l0 + s2_old.x as f64 * l1,
            s1_old.y as f64 * l0 + s2_old.y as f64 * l1,
            s1_old.z as f64 * l0 + s2_old.z as f64 * l1,
        ];
        let dot = |s: Vec3| s.x as f64 * big[0] + s.y as f64 * big[1] + s.z as f64 * big[2];
        rows[0].lambda[lane] = dot(s1);
        rows[1].lambda[lane] = dot(s2);
    }
}

fn hinge_friction(rows: &mut [DualRow], cx: &Ctx) {
    // step 5: friction bounds of row 2 from this solve's (re-projected) warm starts
    let hv = ((rows[0].lambda[0] * rows[0].lambda[0] + rows[1].lambda[0] * rows[1].lambda[0]).sqrt() as f32) * cx.cfg.fric_v;
    let hp = ((rows[0].lambda[1] * rows[0].lambda[1] + rows[1].lambda[1] * rows[1].lambda[1]).sqrt() as f32) * cx.cfg.fric_p;
    rows[2].hi = [hv, hp];
    rows[2].lo = [-hv, -hp];
}

/// Position error of the alignment rows (step 6 mode 0): c = wA × wB clipped at
/// sin(rad(cfg)) when wA·wB < cos(rad(cfg)).
fn align_error(wa: Vec3, wb: Vec3, s1: Vec3, cx: &Ctx) -> Vec3 {
    let mut c = wa.cross(wb);
    let a = rad(cx.cfg.max_ang);
    if wa.dot(wb) < a.cos() {
        let n = c.length();
        c = if n > 1e-5 { c * (a.sin() / n) } else { s1 * a.sin() };
    }
    c
}

pub fn align2axes(rows: &mut [DualRow], cx: &Ctx, c: &mut Align2Axes) {
    let wa = cx.pa.rot(c.axis_a);
    let wb = cx.pb.rot(c.axis_b);
    let dw = cx.rb.w - cx.ra.w;
    if !c.servo {
        // 1–2. warm starts re-projected from the old basis onto s1 = R_B·(), s2 = R_B·()
        let s1 = cx.pb.rot(c.s1_b);
        let s2 = cx.pb.rot(c.s2_b);
        reproject(rows, c.s1_w, c.s2_w, s1, s2);
        c.s1_w = s1;
        c.s2_w = s2;
        // 3. rows 0, 1: (0, s_k | 0, −s_k);  row 2: (0, a | 0, −a), a = R_B·() (B's axis)
        rows[0].set_j(Vec3::ZERO, s1, Vec3::ZERO, -s1);
        rows[1].set_j(Vec3::ZERO, s2, Vec3::ZERO, -s2);
        rows[2].set_j(Vec3::ZERO, wb, Vec3::ZERO, -wb);
        // 6. position error c = wA × wB (JOINT2 correction), clipped; e_k = cfg·(c·s_k)
        let ce = align_error(wa, wb, s1, cx);
        // 4. b_k = (ω_B − ω_A)·s_k;  b_2 = (ω_B − ω_A)·a
        rows[0].rhs = [dw.dot(s1), cx.cfg.ang_frac * ce.dot(s1)];
        rows[1].rhs = [dw.dot(s2), cx.cfg.ang_frac * ce.dot(s2)];
        rows[2].rhs = [dw.dot(wb), 0.0];
        hinge_friction(rows, cx);
        return;
    }
    // mode ≠ 0 (align servo, JOINT2 — the else-branch of Studio)
    let n = wa.cross(wb);
    let s = n.length();
    let cdot = wa.dot(wb);
    let theta = s.atan2(cdot);
    let active = if cdot < 0.0017453f32.cos() { 1.0 } else { 0.0 };
    let (s1, s2) = if s <= 0.0017453 {
        (cx.pb.rot(c.s1_b), cx.pb.rot(c.s2_b))
    } else {
        let s1 = n / s;
        (s1, s1.cross(wb))
    };
    reproject(rows, c.s1_w, c.s2_w, s1, s2);
    c.s1_w = s1;
    c.s2_w = s2;
    rows[0].set_j(Vec3::ZERO, s1, Vec3::ZERO, -s1);
    rows[1].set_j(Vec3::ZERO, s2, Vec3::ZERO, -s2);
    rows[2].set_j(Vec3::ZERO, wb, Vec3::ZERO, -wb);
    // 4. ωr = (ω_ref,A − ω_ref,B)·s1; t = (K·θ − D·ωr)·h + ωr, substep when cfg && active
    let h = cx.h;
    let wr = (cx.ra.w_ref - cx.rb.w_ref).dot(s1);
    let mut t = if cx.cfg.adaptive && active > 0.0 {
        // level-1 identity: −(K'x + D'v)h = (Kθ − Dωr)h with K' = −K, x = θ, D' = D, v = ωr
        substep(h, -c.k, c.d, theta, wr, 1) + wr
    } else {
        (c.k * theta - c.d * wr) * h + wr
    };
    t = clamp(t, -c.max_speed, c.max_speed);
    rows[0].rhs[0] = t * active + dw.dot(s1);
    rows[1].rhs[0] = dw.dot(s2);
    rows[2].rhs[0] = dw.dot(wb);
    rows[0].lo[0] = -h * c.max_torque;
    rows[0].hi[0] = h * c.max_torque;
    rows[1].lo[0] = -h * c.max_torque;
    rows[1].hi[0] = h * c.max_torque;
    // 5. row 2 friction as mode 0
    hinge_friction(rows, cx);
    // 6. position pass only when nearly aligned and nearly still
    let wv = Vec3::new(dw.dot(s1), dw.dot(s2), 0.0).length();
    if theta.abs() < 0.0017453 && wv < 0.0017453 / h {
        let ce = align_error(wa, wb, s1, cx);
        rows[0].rhs[1] = cx.cfg.ang_frac * ce.dot(s1);
        rows[1].rhs[1] = cx.cfg.ang_frac * ce.dot(s2);
        rows[0].lo[1] = -h * c.max_torque;
        rows[0].hi[1] = h * c.max_torque;
        rows[1].lo[1] = -h * c.max_torque;
        rows[1].hi[1] = h * c.max_torque;
    } else {
        for r in &mut rows[..2] {
            r.rhs[1] = 0.0;
            r.lo[1] = 0.0;
            r.hi[1] = 0.0;
        }
    }
    rows[2].rhs[1] = 0.0;
}

// ---------------------------------------------------------------------------------
// D3.3 / §4.2 AngularVelocity (motor) — Studio (1 row)
#[derive(Clone)]
pub struct Motor {
    pub axis_a: Vec3,    // (init (1,0,0))
    pub max_torque: f32, //
    pub target: f32,     //
    pub max_acc: f32,    // (init +∞)
    pub acc_cap: f32,    // = 500000
}

pub fn motor(rows: &mut [DualRow], cx: &Ctx, m: &Motor) {
    let h = cx.h;
    // 1. w = R_A·(); Ja = (0, −w); Jb = (0, w)
    let w = cx.pa.rot(m.axis_a);
    rows[0].set_j(Vec3::ZERO, -w, Vec3::ZERO, w);
    // 2–3. t = ωref + clamp(target − ωref, ±h·maxAcc)
    let wref = (cx.rb.w_ref - cx.ra.w_ref).dot(w);
    let t = wref + clamp(m.target - wref, -h * m.max_acc, h * m.max_acc);
    // 4–5.
    rows[0].rhs = [t - (cx.rb.w - cx.ra.w).dot(w), 0.0];
    rows[0].lo = [-h * m.max_torque, 0.0];
    rows[0].hi = [h * m.max_torque, 0.0];
}

// ---------------------------------------------------------------------------------
// §4.3 ServoMotor — Studio (1 row), transcribed from ../dumps/dc/Studio.c
#[derive(Clone)]
pub struct Servo {
    pub axis_a: Vec3,      //
    pub ref_b: Vec3,       //
    pub ref_target_a: Vec3, //
    pub ref_zero_a: Vec3,  //
    pub max_torque: f32,   //
    pub max_speed: f32,    //
    pub limits: bool,      //
    pub upper: f32,        //
    pub lower: f32,        //
    pub target: f32,       //
    pub k: f32,            //
    pub d: f32,            //
}

/// `x − floorf((x + π)/2π)·2π`, absolute value (Studio's wrapped distance).
fn wrapped_dist(x: f32) -> f32 {
    (x - ((x + PI) / TAU).floor() * TAU).abs()
}

pub fn servo(rows: &mut [DualRow], cx: &Ctx, s: &Servo) {
    let h = cx.h;
    let w = cx.pa.rot(s.axis_a);
    rows[0].set_j(Vec3::ZERO, -w, Vec3::ZERO, w);
    let u = cx.pa.rot(s.ref_target_a);
    let vb = cx.pb.rot(s.ref_b);
    let vp = vb - w * vb.dot(w);
    // err = atan2((v' × u)·w, u·v'): Studio computes v'×u (target minus current),
    // not the u × v' JOINT §4.3 step 2 writes (JOINT-NOTES "servo sign").
    let mut err = vp.cross(u).dot(w).atan2(u.dot(vp));
    // 3. unwrap against the limits (|err| > cfg), branch order of Studio
    if s.limits && err.abs() > cx.cfg.servo_unwrap {
        let z = cx.pa.rot(s.ref_zero_a);
        let zp = z - w * z.dot(w);
        let a = zp.cross(vb).dot(w).atan2(zp.dot(vb));
        let (up, lo, tg) = (s.upper, s.lower, s.target);
        let inside = !(up < a || a < lo);
        let dl = wrapped_dist(lo - tg);
        let du = wrapped_dist(up - tg);
        if inside {
            let tin = !(up < tg || tg < lo);
            if (a < tg && tin) || (!tin && du <= dl) {
                if err < 0.0 {
                    err += TAU;
                }
            }
            if (tg < a && tin) || (!tin && dl <= du) {
                if err > 0.0 {
                    err -= TAU;
                }
            }
        } else {
            let dla = wrapped_dist(lo - a);
            let dua = wrapped_dist(up - a);
            if dl <= du || dua <= dla {
                if dl < du && dua < dla && err > 0.0 {
                    err -= TAU;
                }
            } else if err < 0.0 {
                err += TAU;
            }
        }
    }
    // 4. ωref = (ω_ref,B − ω_ref,A)·w; t = h·(K·err − D·ωref) + ωref (cfg: substep with −K)
    let wref = (cx.rb.w_ref - cx.ra.w_ref).dot(w);
    let t = if cx.cfg.adaptive {
        // Studio(h, −K, …): level-1 identity gives x = err, D' = D, v = ωref
        substep(h, -s.k, s.d, err, wref, 1) + wref
    } else {
        (err * s.k - wref * s.d) * h + wref
    };
    // 5. clamp to ±max speed (min then max, as Studio)
    let t = clamp(t, -s.max_speed, s.max_speed);
    rows[0].rhs = [t - (cx.rb.w - cx.ra.w).dot(w), 0.0];
    rows[0].lo = [-h * s.max_torque, -0.0];
    rows[0].hi = [h * s.max_torque, 0.0];
}

// ---------------------------------------------------------------------------------
// §4.4 Cone — Studio (1 row, unilateral)
#[derive(Clone)]
pub struct Cone {
    pub axis_a: Vec3,      //
    pub axis_b: Vec3,      //
    pub alpha: f32,        // (init π)
    pub restitution: f32,  //
}

pub fn cone(rows: &mut [DualRow], cx: &Ctx, c: &Cone) {
    let wa = cx.pa.rot(c.axis_a);
    let wb = cx.pb.rot(c.axis_b);
    let n = wa.cross(wb);
    let s = n.length();
    let cs = wa.dot(wb);
    // 2. inactive outside the limit, near-antiparallel (−0.99, .rdata Studio) or s < 0.01
    if cs > c.alpha.cos() || cs < -0.99 || s < 0.01 {
        rows[0].inactive();
        return;
    }
    let u = n / s;
    let theta = s.atan2(cs);
    rows[0].set_j(Vec3::ZERO, u, Vec3::ZERO, -u);
    let b = c.restitution * (cx.rb.w_ref - cx.ra.w_ref).dot(u) + (cx.rb.w - cx.ra.w).dot(u);
    let e = clamp(theta - c.alpha - rad(cx.cfg.cone_slop), 0.0, rad(cx.cfg.cone_max)).sin();
    rows[0].rhs = [b, e];
    rows[0].lo = [0.0, 0.0];
}

// ---------------------------------------------------------------------------------
// D3.5 SingleAxisSpring (SpringConstraint) — Studio, explicit force row
#[derive(Clone)]
pub struct Spring {
    pub p_a: Vec3,       //
    pub p_b: Vec3,       //
    pub max_force: f32,  //
    pub free_len: f32,   //
    pub k: f32,          //
    pub d: f32,          //
    pub max_len: f32,    //
    pub min_len: f32,    //
}

pub fn spring(rows: &mut [DualRow], cx: &Ctx, s: &Spring) {
    let pa = cx.pa.point(s.p_a);
    let pb = cx.pb.point(s.p_b);
    let d = pb - pa;
    let l = d.length();
    // 1. L ≤ 0.1: inactive row, warm start 0
    if l <= 0.1 {
        rows[0].inactive();
        return;
    }
    let n = d / l;
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    // 2. Ja = (n, rA × n); Jb = (−n, n × rB)   (J·v = −dL/dt)
    rows[0].set_j(n, ra.cross(n), -n, n.cross(rb));
    // 3. u = dL/dt at the step start; F = clamp(D·u + K·(Lc − L0), ±MaxForce)
    let u = (velpt(cx.rb.v_ref, cx.rb.w_ref, rb) - velpt(cx.ra.v_ref, cx.ra.w_ref, ra)).dot(n);
    let lc = clamp(l, s.min_len, s.max_len);
    let f = clamp(s.d * u + s.k * (lc - s.free_len), -s.max_force, s.max_force);
    // 4. fixed impulse F·h, also the warm start; nothing in the position lane
    let fh = f * cx.h;
    rows[0].rhs = [0.0, 0.0];
    rows[0].lo = [fh, 0.0];
    rows[0].hi = [fh, 0.0];
    rows[0].lambda = [fh as f64, 0.0];
}

// ---------------------------------------------------------------------------------
// D3.6 TorsionSpring — Studio, explicit torque row
#[derive(Clone)]
pub struct Torsion {
    pub axis_a: Vec3,    //
    pub axis_b: Vec3,    //
    pub max_torque: f32, //
    pub k: f32,          //
    pub d: f32,          //
    pub max_angle: f32,  //
}

/// eps of Studio (.rdata Studio): its value is not in JOINT; f32::EPSILON
/// stands in (JOINT-NOTES "torsion eps").
const TORSION_EPS: f32 = f32::EPSILON;

pub fn torsion(rows: &mut [DualRow], cx: &Ctx, t: &Torsion) {
    let wa = cx.pa.rot(t.axis_a);
    let wb = cx.pb.rot(t.axis_b);
    let n = wa.cross(wb);
    let nl = n.length();
    if nl <= TORSION_EPS {
        // inactive-like: J = (0,(1,0,0)) / (0,(−1,−0,−0)), τ = 0
        rows[0].set_j(Vec3::ZERO, Vec3::X, Vec3::ZERO, -Vec3::X);
        rows[0].rhs = [0.0, 0.0];
        rows[0].lo = [0.0, 0.0];
        rows[0].hi = [0.0, 0.0];
        rows[0].lambda = [0.0, 0.0];
        return;
    }
    let u = n / nl;
    // θ = angle(wA, wB) (Studio, not known: atan2(|wA×wB|, wA·wB))
    let theta = nl.atan2(wa.dot(wb));
    rows[0].set_j(Vec3::ZERO, u, Vec3::ZERO, -u);
    let tau = clamp(t.d * (cx.rb.w_ref - cx.ra.w_ref).dot(u) + t.k * theta.min(t.max_angle), -t.max_torque, t.max_torque);
    let th = tau * cx.h;
    rows[0].rhs = [0.0, 0.0];
    rows[0].lo = [th, 0.0];
    rows[0].hi = [th, 0.0];
    rows[0].lambda = [th as f64, 0.0];
}

// ---------------------------------------------------------------------------------
// §4.10 Winch (Rope winch) — Studio (1 row, unilateral)
#[derive(Clone)]
pub struct Winch {
    pub p_a: Vec3,       //
    pub p_b: Vec3,       //
    pub cur_len: f32,    // (written by the row build)
    pub act_len: f32,    // ℓ: the row acts while L ≥ ℓ (Length; moved by the callback)
    pub prev_len: f32,   // ℓ of the previous callback
    pub target: f32,     // WinchTarget
    pub max_speed: f32,  // WinchSpeed
    pub max_force: f32,  // WinchForce (clampF)
    pub k: f32,          // 0.25·R²
    pub d: f32,          // R
    /// The rope's Length as last applied (slot 0 writes ℓ only when it changes).
    pub length: f32,
}

pub fn winch(rows: &mut [DualRow], cx: &Ctx, c: &mut Winch) {
    let h = cx.h;
    // 1. d = pB − pA; L = |d|; = L
    let pa = cx.pa.point(c.p_a);
    let pb = cx.pb.point(c.p_b);
    let d = pb - pa;
    let l = d.length();
    c.cur_len = l;
    // 2. L < ℓ: inactive
    if l < c.act_len || l <= 0.0 {
        rows[0].inactive();
        return;
    }
    // 3. n = d/L; Ja = (n, rA × n); Jb = (−n, −(rB × n))  (J·v = −dL/dt)
    let n = d / l;
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    rows[0].set_j(n, ra.cross(n), -n, -(rb.cross(n)));
    // 4. u = (velpt_ref(B) − velpt_ref(A))·n;  t = h·((L − Lt)·K − u·D) + u, clamp ±speed
    let u = (velpt(cx.rb.v_ref, cx.rb.w_ref, rb) - velpt(cx.ra.v_ref, cx.ra.w_ref, ra)).dot(n);
    let t = clamp(h * ((l - c.target) * c.k - u * c.d) + u, -c.max_speed, c.max_speed);
    // 5. b = t + (velpt(B) − velpt(A))·n;  e = 0;  lo_v = 0, hi_v = h·F;  p lane 0
    let jv = (velpt(cx.rb.v, cx.rb.w, rb) - velpt(cx.ra.v, cx.ra.w, ra)).dot(n);
    rows[0].rhs = [t + jv, 0.0];
    rows[0].lo = [0.0, 0.0];
    rows[0].hi = [h * c.max_force, 0.0];
}

/// The winch's per-step callback Studio(rope, dt) (JOINT I.7 [J2]): ℓ toward
/// WinchTarget at most WinchSpeed. Runs on the rope's solve ticks with dt = h (lab J3:
/// ℓ drops 5/240 per solve at level 1 and at level 4 alike).
pub fn winch_step(c: &mut Winch, dt: f32) {
    let l = c.act_len;
    let s = clamp(((c.target - l) * c.k - ((l - c.prev_len) / dt) * c.d) * dt, -c.max_speed, c.max_speed);
    c.prev_len = l;
    let l2 = l + s * dt;
    c.act_len = if c.cur_len < l { l2 } else { l2.min(c.cur_len) };
}

// ---------------------------------------------------------------------------------
// D3.7 InsideOfSphere (rope length, spring / prismatic limits) — Studio
#[derive(Clone)]
pub struct Sphere {
    pub p_a: Vec3,         //
    pub p_b: Vec3,         //
    pub l0: f32,           //
    pub l1: f32,           //
    pub restitution: f32,  //
}

pub fn inside_sphere(rows: &mut [DualRow], cx: &Ctx, s: &Sphere) {
    let lmin = s.l0.min(s.l1);
    let lmax = s.l0.max(s.l1);
    let pa = cx.pa.point(s.p_a);
    let pb = cx.pb.point(s.p_b);
    let d = pb - pa;
    let l = d.length();
    // 2.
    if (lmin < l && l < lmax) || l < 0.01 {
        rows[0].inactive();
        return;
    }
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    let dv = velpt(cx.rb.v, cx.rb.w, rb) - velpt(cx.ra.v, cx.ra.w, ra);
    if (lmin - lmax).abs() >= 0.01 || lmax <= 0.01 {
        // 3. a range
        let (e_len, n) = if l >= lmin { (lmax - l, d / l) } else { (l - lmin, -d / l) };
        rows[0].set_j(n, ra.cross(n), -n, n.cross(rb));
        let mut u = (velpt(cx.rb.v_ref, cx.rb.w_ref, rb) - velpt(cx.ra.v_ref, cx.ra.w_ref, ra)).dot(n);
        if u < cx.cfg.restitution_threshold {
            u = 0.0;
        }
        let b = dv.dot(n) + u * s.restitution;
        let e = -(e_len + cx.cfg.slop) * cx.cfg.sphere_frac;
        rows[0].rhs = [b, e];
        rows[0].lo = [0.0, 0.0];
    } else {
        // exact length (JOINT2, last branch): two-sided, no restitution, no slop
        let n = d / l;
        rows[0].set_j(n, ra.cross(n), -n, -(rb.cross(n)));
        rows[0].rhs = [dv.dot(n), l - 0.5 * (lmin + lmax)];
    }
}

// ---------------------------------------------------------------------------------
// D3.8 FixedDistance (Rod) — Studio (1 row, mask 1)
#[derive(Clone)]
pub struct Rod {
    pub p_a: Vec3,     //
    pub p_b: Vec3,     //
    pub length: f32,   //
    pub dir: Vec3,     // (kept when L < eps)
}

pub fn fixed_distance(rows: &mut [DualRow], cx: &Ctx, r: &mut Rod) {
    let pa = cx.pa.point(r.p_a);
    let pb = cx.pb.point(r.p_b);
    let d = pb - pa;
    let l = d.length();
    // n stored at, kept when L < eps (Studio = 1e-6)
    if l >= 1e-6 {
        r.dir = d / l;
    }
    let n = r.dir;
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    rows[0].set_j(n, ra.cross(n), -n, -(rb.cross(n)));
    let jv = rows[0].jdotv(cx);
    // e = clamp((Length − L)·cfg, ±cfg), negated at
    let e = -clamp((r.length - l) * cx.cfg.lin_frac, -cx.cfg.max_lin, cx.cfg.max_lin);
    rows[0].rhs = [-jv, e];
}

// ---------------------------------------------------------------------------------
// D3.10 BallOnAxis — Studio (2 rows, mask 3)
#[derive(Clone)]
pub struct BallOnAxis {
    pub p_a: Vec3,   //
    pub p_b: Vec3,   //
    pub axis: Vec3,  //
    pub s1: Vec3,    //
    pub s2: Vec3,    //
}

pub fn ball_on_axis(rows: &mut [DualRow], cx: &Ctx, c: &BallOnAxis) {
    let pa = cx.pa.point(c.p_a);
    let pb = cx.pb.point(c.p_b);
    let w = cx.pa.rot(c.axis);
    let s1 = cx.pa.rot(c.s1);
    let s2 = cx.pa.rot(c.s2);
    // 2. q = B's point projected on A's axis (A's lever point)
    let q = pa + w * (pb - pa).dot(w);
    let ra = q - cx.ra.com;
    let rb = pb - cx.rb.com;
    // 3. row k: Ja = (−s, s × rA); Jb = (s, rB × s)
    rows[0].set_j(-s1, s1.cross(ra), s1, rb.cross(s1));
    rows[1].set_j(-s2, s2.cross(ra), s2, rb.cross(s2));
    // 4.
    let dv = velpt(cx.rb.v, cx.rb.w, rb) - velpt(cx.ra.v, cx.ra.w, ra);
    // 5. full perpendicular offset, length-capped at cfg (no factor)
    let dd = pb - q;
    let (e1, e2) = (dd.dot(s1), dd.dot(s2));
    let ll = (e1 * e1 + e2 * e2).sqrt();
    let (x0, x1) = if ll > 0.0 {
        let c = ll.min(cx.cfg.max_lin);
        (-(c / ll) * e1, -(c / ll) * e2)
    } else {
        (0.0, 0.0)
    };
    rows[0].rhs = [-dv.dot(s1), x0];
    rows[1].rhs = [-dv.dot(s2), x1];
}

// ---------------------------------------------------------------------------------
// D3.11 Perp2Axes — Studio (1 row)
#[derive(Clone)]
pub struct Perp2Axes {
    pub axis_a: Vec3,    //
    pub axis_b: Vec3,    //
    pub servo: bool,     //
    pub max_torque: f32, //
    pub max_speed: f32,  //
    pub k: f32,          //
    pub d: f32,          //
}

/// Studio: the row of Perp2Axes / VectorOnPlane given n, s, x.
fn perp_row(rows: &mut [DualRow], cx: &Ctx, p: &Perp2Axes, n: Vec3, s: f32, x: f32) {
    let h = cx.h;
    rows[0].set_j(Vec3::ZERO, n, Vec3::ZERO, -n);
    let dw = (cx.rb.w - cx.ra.w).dot(n);
    if !p.servo {
        let a = rad(cx.cfg.max_ang);
        rows[0].rhs = [dw, clamp(x, -a, a) * cx.cfg.ang_frac];
    } else {
        let theta = x.atan2(s);
        let u = (cx.rb.w_ref - cx.ra.w_ref).dot(n);
        let t = if cx.cfg.adaptive {
            // level-1 identity: t = u − (Kθ + Du)h = u + substep(h, K, D, θ, u)
            substep(h, p.k, p.d, theta, u, 1) + u
        } else {
            u - (p.k * theta + p.d * u) * h
        };
        let t = clamp(t, -p.max_speed, p.max_speed);
        rows[0].rhs = [dw - t, 0.0];
        rows[0].lo = [-h * p.max_torque, 0.0];
        rows[0].hi = [h * p.max_torque, 0.0];
    }
}

pub fn perp2axes(rows: &mut [DualRow], cx: &Ctx, p: &Perp2Axes) {
    let wa = cx.pa.rot(p.axis_a);
    let wb = cx.pb.rot(p.axis_b);
    let mut n = wa.cross(wb);
    let s = n.length();
    if s > 0.0 {
        n /= s;
    }
    if s < 1e-4 {
        // Studio(…, wA, 1): some unit vector ⟂ wA (not known)
        n = wa.any_orthonormal_vector();
    }
    perp_row(rows, cx, p, n, s, -(wa.dot(wb)));
}

/// D3.24 VectorOnPlane — Studio (ControllerManager only).
pub fn vector_on_plane(rows: &mut [DualRow], cx: &Ctx, p: &Perp2Axes, normal_a: Vec3) {
    let nrm = cx.pa.rot(normal_a);
    let v = cx.pa.rot(p.axis_a);
    let pp = (v - nrm * v.dot(nrm)).normalize_or_zero();
    let wb = cx.pb.rot(p.axis_b);
    let mut n = pp.cross(wb);
    let mut s = n.length();
    if s > 0.0 {
        n /= s;
    }
    if s < 1e-4 {
        n = pp.any_orthonormal_vector();
    }
    let pw = pp.dot(wb);
    let mut x = -pw;
    if n.dot(nrm) < 0.0 && pw.abs() < 0.3 {
        let phi = (pw / (pw + 1e-4)) * PI * 0.5;
        x = phi.sin();
        s = phi.cos();
    }
    perp_row(rows, cx, p, n, s, x);
}

// ---------------------------------------------------------------------------------
// D3.12 Twist (BallSocket twist limit) — Studio (1 row, unilateral)
#[derive(Clone)]
pub struct Twist {
    pub axis_a: Vec3,  //
    pub axis_b: Vec3,  //
    pub ref_a: Vec3,   //
    pub ref_b: Vec3,   //
    pub l0: f32,       //
    pub l1: f32,       //
    pub restitution: f32, //
}

pub fn twist(rows: &mut [DualRow], cx: &Ctx, t: &Twist) {
    let wa = cx.pa.rot(t.axis_a);
    let wb = cx.pb.rot(t.axis_b);
    let n = wa.cross(wb);
    if wa.dot(wb) <= -1.0 {
        rows[0].inactive();
        return;
    }
    let mut ta = cx.pa.rot(t.ref_a);
    let tb = cx.pb.rot(t.ref_b);
    let nl = n.length();
    if nl > 0.0 {
        // [J2] Studio(&M, n̂, s = |n|, c = wA·wB) → Studio: M = c·I + s·[n̂]× +
        // (1 − c)·n̂n̂ᵀ (the minimal swing taking wA to wB, from s and c directly; call @Studio)
        let k = n / nl;
        let c = wa.dot(wb);
        ta = ta * c + k.cross(ta) * nl + k * (k.dot(ta) * (1.0 - c));
    }
    let theta = ta.cross(tb).dot(wb).atan2(ta.dot(tb));
    let lo = t.l0.min(t.l1);
    let hi = t.l0.max(t.l1);
    if lo < theta && theta < hi {
        rows[0].inactive();
        return;
    }
    let slop = rad(cx.cfg.cone_slop);
    let cap = rad(cx.cfg.cone_max);
    let (ep, u) = if theta <= lo { (clamp((lo - theta) - slop, 0.0, cap), -wb) } else { (clamp((theta - hi) - slop, 0.0, cap), wb) };
    rows[0].set_j(Vec3::ZERO, u, Vec3::ZERO, -u);
    let b = t.restitution * (cx.rb.w_ref - cx.ra.w_ref).dot(u) + (cx.rb.w - cx.ra.w).dot(u);
    rows[0].rhs = [b, ep.sin()];
    rows[0].lo = [0.0, 0.0];
}

// ---------------------------------------------------------------------------------
// D3.13 PositionalCones (Rod end limits) — Studio (2 rows, unilateral)
#[derive(Clone)]
pub struct PosCones {
    pub p_a: Vec3,     //
    pub p_b: Vec3,     //
    pub axis_a: Vec3,  //
    pub axis_b: Vec3,  //
    pub half_a: f32,   //
    pub half_b: f32,   //
}

pub fn positional_cones(rows: &mut [DualRow], cx: &Ctx, c: &PosCones) {
    let slop = rad(cx.cfg.cone_slop);
    let emax = 1.0 - rad(cx.cfg.cone_max).cos();
    let pa = cx.pa.point(c.p_a);
    let pb = cx.pb.point(c.p_b);
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    let wa = cx.pa.rot(c.axis_a);
    let wb = cx.pb.rot(c.axis_b);
    let d = pb - pa;
    let l = d.length();
    let n = d * (1.0 / l);
    let ca = n.dot(wa);
    let cb = -(n.dot(wb));
    let ga = n * ca - wa;
    let gb = n * n.dot(wb) - wb;
    for r in rows.iter_mut().take(2) {
        r.lo = [0.0, 0.0];
    }
    if ca < c.half_a.cos() && l >= 1e-5 && ga.length_squared() >= 1e-5 {
        rows[0].set_j(ga, wa.cross(n) + ra.cross(ga), -ga, ga.cross(rb));
        let jv = rows[0].jdotv(cx);
        rows[0].rhs = [-jv, clamp((c.half_a + slop).cos() - ca, 0.0, emax)];
    } else {
        rows[0].inactive();
    }
    if cb < c.half_b.cos() && l >= 1e-5 && gb.length_squared() >= 1e-5 {
        rows[1].set_j(-gb, -(ra.cross(gb)), gb, rb.cross(gb) - wb.cross(n));
        let jv = rows[1].jdotv(cx);
        rows[1].rhs = [-jv, clamp((c.half_b + slop).cos() - cb, 0.0, emax)];
    } else {
        rows[1].inactive();
    }
}

// ---------------------------------------------------------------------------------
// D3.14 AchievePosition (AlignPosition) — Studio (3 rows)
#[derive(Clone)]
pub struct Achieve {
    pub p_a: Vec3,         //
    pub p_b: Vec3,         //
    pub dir: Vec3,         // (kept)
    pub t1: Vec3,          //
    pub t2: Vec3,          //
    pub at_com: bool,      // bit 0
    pub max_force: [f32; 3], //..
    pub max_speed: f32,    //
    pub k: f32,            //
    pub d: f32,            //
    pub line: bool,        //
    pub basis: Mat3,       // (columns a0, a1, a2)
    pub frame: u8,         //: 0 A, 1 B, 2 world
}

pub fn frame_basis(frame: u8, cx: &Ctx, b: &Mat3) -> Mat3 {
    // Studio: 0 → R_A·B, 1 → R_B·B, 2 → B as is
    match frame {
        0 => cx.pa.r * *b,
        1 => cx.pb.r * *b,
        _ => *b,
    }
}

pub fn achieve_position(rows: &mut [DualRow], cx: &Ctx, c: &mut Achieve) {
    let h = cx.h;
    let pa = if c.at_com { cx.ra.com } else { cx.pa.point(c.p_a) };
    let pb = cx.pb.point(c.p_b);
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    let d = pb - pa;
    let dv = velpt(cx.rb.v, cx.rb.w, rb) - velpt(cx.ra.v, cx.ra.w, ra);
    // 2. AnimationConstraint mode () stays 0 for AlignPosition (JOINT I.10)
    // 3.
    let vr = velpt(cx.rb.v_ref, cx.rb.w_ref, rb) - velpt(cx.ra.v_ref, cx.ra.w_ref, ra);
    let l = d.length();
    let s = if l > 0.01 { 1.0 } else { 0.0 };
    if l > 0.0 {
        c.dir = d / l;
    }
    // 4. basis
    let m = if !c.line {
        frame_basis(c.frame, cx, &c.basis)
    } else {
        let (t1, t2) = perps(c.dir);
        c.t1 = t1;
        c.t2 = t2;
        Mat3::from_cols(c.dir, t1, t2)
    };
    // 6. target velocity u of A relative to B
    let u = if !c.line {
        let raw = if cx.cfg.adaptive && s > 0.0 {
            // level-1 identity: −Σa = h(K·d + D·vr) → x = d, v = vr
            -substep3(h, c.k, c.d, d, vr, 1) - vr
        } else {
            (d * c.k + vr * c.d) * h - vr
        };
        Vec3::new(clamp(raw.x, -c.max_speed, c.max_speed), clamp(raw.y, -c.max_speed, c.max_speed), clamp(raw.z, -c.max_speed, c.max_speed)) * s
    } else {
        let n = c.dir;
        let rho = (-vr.dot(n)).max(0.0);
        let q = if cx.cfg.adaptive && s > 0.0 {
            // level-1 identity: ρ − Σa = ρ + (L·K − ρ·D)h → x = L, v = −ρ (= dL/dt)
            rho - substep(h, c.k, c.d, l, -rho, 1)
        } else {
            (l * c.k - rho * c.d) * h + rho
        };
        n * (clamp(q, -c.max_speed, c.max_speed) * s)
    };
    // 8. position pass only when arrived
    let z = if l < 0.01 && vr.length() * h < 0.01 { 1.0 } else { 0.0 };
    for k in 0..3 {
        let a = m.col(k);
        // 5. Ja = (a, rA × a); Jb = (−a, −(rB × a))
        rows[k].set_j(a, ra.cross(a), -a, -(rb.cross(a)));
        let f = h * c.max_force[k];
        rows[k].rhs = [(u + dv).dot(a), 0.2 * z * d.dot(a)];
        rows[k].lo = [-f, nan0(-f * z)];
        rows[k].hi = [f, nan0(f * z)];
    }
}

#[inline]
fn nan0(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x }
}

// ---------------------------------------------------------------------------------
// D3.15 Orientation (AlignOrientation, Prismatic's rotation lock) — Studio (3 rows)
#[derive(Clone)]
pub struct Orient {
    pub a1: Vec3,   //
    pub a2: Vec3,   //
    pub b1: Vec3,   //
    pub b2: Vec3,   //
    pub servo: bool, //
    pub max_torque: f32, //
    pub max_speed: f32,  //
    pub k: f32,     //
    pub d: f32,     //
    pub u: Vec3,    // (kept)
    pub t1: Vec3,   //
    pub t2: Vec3,   //
}

/// Axis of a rotation by ≈π (Studio, not known): the column of (Rr + I)
/// with the largest norm, normalised (JOINT-NOTES "Orientation θ≈π").
fn axis_of_half_turn(rr: &Mat3) -> Vec3 {
    let m = *rr + Mat3::IDENTITY;
    let c = [m.x_axis, m.y_axis, m.z_axis];
    let best = c.iter().copied().max_by(|a, b| a.length_squared().total_cmp(&b.length_squared())).unwrap();
    best.normalize_or_zero()
}

pub fn orientation(rows: &mut [DualRow], cx: &Ctx, o: &mut Orient) {
    let h = cx.h;
    let a1 = cx.pa.rot(o.a1);
    let a2 = cx.pa.rot(o.a2);
    let a3 = a1.cross(a2);
    let b1 = cx.pb.rot(o.b1);
    let b2 = cx.pb.rot(o.b2);
    let b3 = b1.cross(b2);
    let ma = Mat3::from_cols(a1, a2, a3);
    let mb = Mat3::from_cols(b1, b2, b3);
    let rr = mb * ma.transpose();
    // row-major Rr[k]: Rr[1] = (0,1), Rr[2] = (0,2), Rr[3] = (1,0), Rr[5] = (1,2), Rr[6] = (2,0), Rr[7] = (2,1)
    let at = |r: usize, c: usize| rr.col(c)[r];
    let v = Vec3::new(at(2, 1) - at(1, 2), at(0, 2) - at(2, 0), at(1, 0) - at(0, 1)) * 0.5;
    let s = v.length();
    let c = 0.5 * (at(0, 0) + at(1, 1) + at(2, 2) - 1.0);
    let theta = s.atan2(c.min(1.0));
    let dw = cx.rb.w - cx.ra.w;
    if !o.servo {
        // 3. rigid lock: world axes, b_i = (ω_B − ω_A)_i
        let lim = rad(cx.cfg.orient_max);
        let e = if theta <= lim {
            v * cx.cfg.orient_frac
        } else {
            let ax = if s <= 0.001 { axis_of_half_turn(&rr) } else { v / s };
            ax * lim.sin()
        };
        for i in 0..3 {
            let ei = AXES[i];
            rows[i].set_j(Vec3::ZERO, ei, Vec3::ZERO, -ei);
            rows[i].rhs = [dw[i], e[i]];
        }
        return;
    }
    // 4. servo
    if theta >= 1.745e-4 {
        let u = if s <= 0.001 { axis_of_half_turn(&rr) } else { v / s };
        o.u = u;
        let (t1, t2) = perps(u);
        o.t1 = t1;
        o.t2 = t2;
        rows[0].set_j(Vec3::ZERO, u, Vec3::ZERO, -u);
        rows[1].set_j(Vec3::ZERO, t1, Vec3::ZERO, -t1);
        rows[2].set_j(Vec3::ZERO, t2, Vec3::ZERO, -t2);
        let wr = (cx.ra.w_ref - cx.rb.w_ref).dot(u);
        let t = if cx.cfg.adaptive {
            // level-1 identity: (Kθ − Dωr)h = Σa with K' = −K, x = θ, D' = D, v = ωr
            substep(h, -o.k, o.d, theta, wr, 1) + wr
        } else {
            wr + (o.k * theta - o.d * wr) * h
        };
        let t = clamp(t, -o.max_speed, o.max_speed);
        let dwa = cx.ra.w - cx.rb.w;
        rows[0].rhs = [t - dwa.dot(u), 0.0];
        rows[1].rhs = [-dwa.dot(t1), 0.0];
        rows[2].rhs = [-dwa.dot(t2), 0.0];
    } else {
        let ax = [o.u, o.t1, o.t2];
        for k in 0..3 {
            rows[k].set_j(Vec3::ZERO, ax[k], Vec3::ZERO, -ax[k]);
            rows[k].rhs = [dw.dot(ax[k]), 0.0];
        }
    }
    for r in rows.iter_mut().take(3) {
        r.lo = [-h * o.max_torque, 0.0];
        r.hi = [h * o.max_torque, 0.0];
    }
}

// ---------------------------------------------------------------------------------
// D3.17 BodyAngularVelocity (AngularVelocity constraint, BodyGyro) — Studio (3 rows)
#[derive(Clone)]
pub struct BodyAngVel {
    pub target: Vec3,  //
    pub hi: Vec3,      //
    pub lo: Vec3,      //
    pub basis: Mat3,   //
    pub frame: u8,     //
    pub use_current: bool, //
    pub legacy: bool,  //
    /// BodyGyro's mover state when this connector is a BodyGyro's ([J3]).
    pub gyro: Option<Box<Gyro>>,
}

/// BodyGyro (mover fields, ctor Studio): CFrame rotation (row-major), MaxTorque
///, D, P; the three confidence filters {g, h0, h1, h2} at (local
/// x error), (local y), (local z), zero at construction.
#[derive(Clone, Debug)]
pub struct Gyro {
    pub target: Mat3,
    pub max_torque: Vec3,
    pub p: f32,
    pub d: f32,
    /// the part's rotation in its body's frame
    pub part_rot: Mat3,
    pub filt: [[f32; 4]; 3],
}

/// The confidence filter of Studio (asm): a 2nd-order predictor
/// S = (h0 + h1) + h2; e = expf(−2·g·|x − S| / (max(|S|, |x|) + 1e-5)) (Studio; −2 =
/// −(.data Studio)); then h2 = (x − h0) − h1, h1 = x − h0, h0 = x,
/// g = (1 − g)·0.1 + g (.rdata Studio).
fn gyro_filter(f: &mut [f32; 4], x: f32) -> f32 {
    let [g, h0, h1, h2] = *f;
    let s = h0 + h1 + h2;
    let num = (x - s).abs() * g;
    let den = s.abs().max(x.abs()) + 1e-5;
    let e = (num / den * -2.0).exp();
    let d1 = x - h0;
    *f = [(1.0 - g) * 0.1 + g, x, d1, d1 - h1];
    e
}

/// |x| within 1e-5·(|x| + 1) of 0 (the MaxTorque component tests of both helpers).
fn near0(x: f32) -> bool {
    if x == 0.0 {
        return true;
    }
    let t = x.abs() + 1.0;
    let tol = if t == f32::INFINITY { 1e-5 } else { t * 1e-5 };
    x.abs() <= tol
}

/// BodyGyro's slot 78 Studio on the body's own solve tick: the world torque of
/// Studio (about the part's x / z, aligning its Y with the goal's Y) + Studio
/// (about its y, from the goal's Z), turned into the D3.17 connector's target ω (part
/// frame) = Rᵀω + dt·I⁻¹(Rᵀτ) and bounds ±clampF(MaxTorque_k·I_k). `r` = the part's world
/// rotation, `w` its angular velocity, `i` the diagonal of the body's inertia tensor
///, `dt` the world step (Studio(world)).
pub fn gyro_step(c: &mut BodyAngVel, r: Mat3, w: Vec3, i: Vec3, dt: f32) {
    let Some(g) = c.gyro.as_mut() else { return };
    let rt = r.transpose();
    let wl = rt * w;
    let mut tau = Vec3::ZERO;
    // Studio
    if !(near0(g.max_torque.x) && near0(g.max_torque.z)) {
        let u = rt * g.target.col(1);
        let (p, d) = mover_kd(g.p, g.d, dt);
        let e0 = gyro_filter(&mut g.filt[0], u.x);
        let e2 = gyro_filter(&mut g.filt[2], u.z);
        let tx = (u.z * p - wl.x * d) * e2 * i.x;
        let tz = (-p * u.x - wl.z * d) * e0 * i.z;
        tau += r * Vec3::new(tx, 0.0, tz);
    }
    // Studio
    if !near0(g.max_torque.y) {
        let v = rt * -g.target.col(2);
        let (p, d) = mover_kd(g.p, g.d, dt);
        let e1 = gyro_filter(&mut g.filt[1], v.x);
        let ty = -e1 * i.y * (v.x * p + wl.y * d);
        tau += r * Vec3::new(0.0, ty, 0.0);
    }
    let t = rt * tau;
    c.target = Vec3::new(t.x / i.x, t.y / i.y, t.z / i.z) * dt + wl;
    let cf = |x: f32| x.max(-3.4028235e38).min(3.4028235e38);
    let m = g.max_torque * i;
    c.hi = Vec3::new(cf(m.x), cf(m.y), cf(m.z));
    c.lo = Vec3::new(cf(-m.x), cf(-m.y), cf(-m.z));
}

pub fn body_angular_velocity(rows: &mut [DualRow], cx: &Ctx, c: &BodyAngVel) {
    let h = cx.h;
    let (m, rel) = if c.legacy {
        (cx.pa.r, if c.use_current { cx.rb.w - cx.ra.w } else { cx.rb.w_ref - cx.ra.w_ref })
    } else {
        (frame_basis(c.frame, cx, &c.basis), cx.rb.w - cx.ra.w)
    };
    for k in 0..3 {
        let mk = m.col(k);
        rows[k].set_j(Vec3::ZERO, mk, Vec3::ZERO, -mk);
        rows[k].rhs = [rel.dot(mk) + c.target[k], 0.0];
        rows[k].lo = [h * c.lo[k], 0.0];
        rows[k].hi = [h * c.hi[k], 0.0];
    }
}

// D3.18 LegacyGyroAngularVelocity (BodyAngularVelocity) — Studio (3 rows, world axes)
pub fn legacy_gyro(rows: &mut [DualRow], cx: &Ctx, target: Vec3, hi: Vec3, lo: Vec3, use_current: bool) {
    let h = cx.h;
    let rel = if use_current { cx.rb.w - cx.ra.w } else { cx.rb.w_ref - cx.ra.w_ref };
    for i in 0..3 {
        let ei = AXES[i];
        rows[i].set_j(Vec3::ZERO, ei, Vec3::ZERO, -ei);
        rows[i].rhs = [rel[i] + target[i], 0.0];
        rows[i].lo = [h * lo[i], 0.0];
        rows[i].hi = [h * hi[i], 0.0];
    }
}

// D3.19 LinearSpring (BodyPosition) — Studio (3 rows, world axes)
pub fn linear_spring(rows: &mut [DualRow], cx: &Ctx, p_a: Vec3, p_b: Vec3, max_force: Vec3, k: f32, d: f32) {
    let h = cx.h;
    let pa = cx.pa.point(p_a);
    let pb = cx.pb.point(p_b);
    let ra = pa - cx.ra.com;
    let rb = pb - cx.rb.com;
    let dd = pa - pb;
    let vr = velpt(cx.ra.v_ref, cx.ra.w_ref, ra) - velpt(cx.rb.v_ref, cx.rb.w_ref, rb);
    let t = if cx.cfg.adaptive {
        // level-1 identity: vr − (D·vr + K·d)h = vr + Σa with x = d, v = vr
        vr + substep3(h, k, d, dd, vr, 1)
    } else {
        vr - (vr * d + dd * k) * h
    };
    let cur = velpt(cx.ra.v, cx.ra.w, ra) - velpt(cx.rb.v, cx.rb.w, rb);
    for i in 0..3 {
        let ei = AXES[i];
        rows[i].set_j(ei, ra.cross(ei), -ei, -(rb.cross(ei)));
        rows[i].rhs = [t[i] - cur[i], 0.0];
        rows[i].lo = [-h * max_force[i], 0.0];
        rows[i].hi = [h * max_force[i], 0.0];
    }
}

/// BodyPosition's connector fields as its slot 78 writes them per step.
#[derive(Clone)]
pub struct LinearSpring {
    /// the part's origin in its body's frame (A = the part; its pose → c − p_part)
    pub part: Vec3,
    /// BodyPosition.Position (mover), world
    pub target: Vec3,
    pub max_force: Vec3, //
    pub k: f32,          //
    pub d: f32,          //
}

/// The world step Studio(world) the movers' K / D clamp reads.
pub const MOVER_DT: f32 = 1.0 / 240.0;

/// Studio(&K, &D, dt) (asm read, [J3]): disc = 4K − D² (.rdata Studio = 4);
/// disc ≥ 0: s = √disc·0.5·dt, if s > π/6 (.data Studio) f = (π/6)/s;
/// disc < 0: if 0.5·D > 0.8/dt (.data Studio) f = 0.8/(0.5·D·dt); then D·f, K·f².
pub fn mover_kd(k: f32, d: f32, dt: f32) -> (f32, f32) {
    let disc = k * 4.0 - d * d;
    if disc >= 0.0 {
        let s = disc.sqrt() * 0.5 * dt;
        if s > 0.5235983_7 {
            let f = 0.5235983_7 / s;
            return (f * f * k, d * f);
        }
    } else {
        let s2 = d * 0.5;
        if s2 > 0.8 / dt {
            let f = 0.8 / (s2 * dt);
            return (f * f * k, d * f);
        }
    }
    (k, d)
}

/// BodyPosition: slot 78's points (A = R_Aᵀ(c − p_A), B = Position + c − p_A in the world
/// body) then D3.19.
pub fn body_position(rows: &mut [DualRow], cx: &Ctx, c: &LinearSpring) {
    let part_w = cx.pa.point(c.part);
    let pa_local = cx.pa.r.transpose() * (cx.ra.com - cx.pa.p);
    let pb = c.target + cx.ra.com - part_w;
    linear_spring(rows, cx, pa_local, cx.pb.r.transpose() * (pb - cx.pb.p), c.max_force, c.k, c.d);
}

// D3.20 ConstraintLinearVelocity (BodyVelocity) — Studio (3 rows, centre of mass)
pub fn constraint_linear_velocity(rows: &mut [DualRow], cx: &Ctx, max_force: Vec3, target: Vec3) {
    let h = cx.h;
    for i in 0..3 {
        let ei = AXES[i];
        rows[i].set_j(ei, Vec3::ZERO, -ei, Vec3::ZERO);
        rows[i].rhs = [(cx.rb.v - cx.ra.v)[i] + target[i], 0.0];
        rows[i].lo = [-h * max_force[i], 0.0];
        rows[i].hi = [h * max_force[i], 0.0];
    }
}

// D3.21 SingleAxisLinearVelocity — Studio (1 row)
#[derive(Clone)]
pub struct SingleLinVel {
    pub p_a: Vec3,       // (world point if)
    pub p_b: Vec3,       //
    pub axis_a: Vec3,    // (in A, or world)
    pub axis_b: Vec3,    //
    pub max_force: f32,  //
    pub max_acc: f32,    //
    pub target: f32,     //
    pub frame: u8,       //
    pub world_point: bool, //
    pub force_limited: bool, // bit 0
}

pub fn single_axis_linear_velocity(rows: &mut [DualRow], cx: &Ctx, c: &SingleLinVel) {
    let h = cx.h;
    let w = match c.frame {
        0 => cx.pa.rot(c.axis_a),
        1 => cx.pb.rot(c.axis_b),
        _ => c.axis_a,
    };
    let pa = if c.world_point { c.p_a } else { cx.pa.point(c.p_a) };
    let pb = cx.pb.point(c.p_b);
    let q = pa + w * (pb - pa).dot(w);
    let ra = pa - cx.ra.com;
    let rb = q - cx.rb.com;
    rows[0].set_j(-w, -(ra.cross(w)), w, rb.cross(w));
    let err = c.target - rows[0].jdotv(cx);
    if !c.force_limited {
        rows[0].rhs = [err, 0.0];
    } else {
        rows[0].rhs = [clamp(err, -h * c.max_acc, h * c.max_acc), 0.0];
        rows[0].lo[0] = -h * c.max_force;
        rows[0].hi[0] = h * c.max_force;
    }
    rows[0].lo[1] = 0.0;
    rows[0].hi[1] = 0.0;
}

// D3.23 TripleAxisLinearVelocity — Studio (3 rows; LinearVelocity Vector mode)
#[derive(Clone)]
pub struct TripleLinVel {
    pub basis: Mat3,     // (columns m_k)
    pub p_a: Vec3,       //
    pub p_b: Vec3,       //
    pub max_force: Vec3, //..
    pub target: Vec3,    // (in the frame)
    pub frame: u8,       // (0 A, 1 B, 2 world)
    pub bits: u8,        //: bit 0 force-limited, bit 1 magnitude mode
}

pub fn triple_axis_linear_velocity(rows: &mut [DualRow], cx: &Ctx, c: &TripleLinVel) {
    let h = cx.h;
    let pa = cx.pa.point(c.p_a);
    let ra = pa - cx.ra.com;
    // VB = 0 and rB = 0 unless frame 1
    let (rb, vb) = if c.frame == 1 {
        let pb = cx.pb.point(c.p_b);
        let rb = pb - cx.rb.com;
        (rb, velpt(cx.rb.v, cx.rb.w, rb))
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    };
    // M = frame·basis, T = frame·() (world for frame 2)
    let (m, t) = match c.frame {
        0 => (cx.pa.r * c.basis, cx.pa.rot(c.target)),
        1 => (cx.pb.r * c.basis, cx.pb.rot(c.target)),
        _ => (c.basis, c.target),
    };
    let err = (velpt(cx.ra.v, cx.ra.w, ra) - vb) - t;
    for k in 0..3 {
        let a = m.col(k);
        rows[k].set_j(a, ra.cross(a), -a, -(rb.cross(a)));
        rows[k].rhs = [-err.dot(a), 0.0];
        if c.bits & 1 != 0 {
            rows[k].lo[0] = -h * c.max_force[k];
            rows[k].hi[0] = h * c.max_force[k];
        }
        rows[k].lo[1] = 0.0;
        rows[k].hi[1] = 0.0;
    }
}

// D3.22 DoubleAxisLinearVelocity — Studio (2 rows; LinearVelocity Plane mode) [J3]
#[derive(Clone)]
pub struct DoubleLinVel {
    pub p_a: Vec3,          //
    pub p_b: Vec3,          //
    pub axes: [Vec3; 2],    // / (in the frame; Studio normalises)
    pub max_force: [f32; 2], // /
    pub target: [f32; 2],   // /
    pub frame: u8,          // (0 A, 1 B, 2 world)
    pub bits: u8,           //: bit 0 force-limited, bit 1 magnitude mode
    /// the Plane wrapper's world tangents ( primary, secondary)
    pub world_p: Vec3,
    pub world_s: Vec3,
}

impl DoubleLinVel {
    pub fn new() -> Self {
        DoubleLinVel { p_a: Vec3::ZERO, p_b: Vec3::ZERO, axes: [Vec3::X, Vec3::Y], max_force: [0.0; 2], target: [0.0; 2], frame: 2, bits: 0, world_p: Vec3::X, world_s: Vec3::Y }
    }

    /// PrimaryTangentAxis then SecondaryTangentAxis (Studio →
    /// Studio), each only when the normalised value differs from the stored one.
    pub fn set_world_tangents(&mut self, primary: Vec3, secondary: Vec3) {
        let eps = 1e-4; // .rdata Studio
        let l = primary.length();
        if l > 0.0 && primary * (1.0 / l) != self.world_p {
            let p = primary * (1.0 / l);
            if l >= eps {
                // secondary ⟂ new primary; if it vanishes, the old secondary turned by the rotation
                // Studio(old primary → new primary) (built from those two only) and re-projected
                let mut s = self.world_s - p * self.world_s.dot(p);
                if s.length() < eps {
                    let r = glam::Quat::from_rotation_arc(self.world_p, p);
                    let t = r * self.world_s;
                    s = t - p * t.dot(p);
                }
                self.world_p = p;
                self.world_s = s.normalize_or_zero();
            }
        }
        let l = secondary.length();
        if l > 0.0 && secondary * (1.0 / l) != self.world_s {
            let q = secondary * (1.0 / l);
            let s = q - self.world_p * q.dot(self.world_p);
            if s.length() > eps {
                self.world_s = s.normalize();
            }
        }
    }
}

// D3.22 / D3.23 Double / Triple-axis LinearVelocity — Studio
pub fn multi_axis_linear_velocity(rows: &mut [DualRow], cx: &Ctx, p_a: Vec3, p_b: Vec3, axes: &[Vec3], frame: u8, max_force: &[f32], target: &[f32], force_limited: bool, triple: bool) {
    let h = cx.h;
    let pa = cx.pa.point(p_a);
    let ra = pa - cx.ra.com;
    let (rb, vb) = if frame == 1 {
        let pb = cx.pb.point(p_b);
        let rb = pb - cx.rb.com;
        (rb, velpt(cx.rb.v, cx.rb.w, rb))
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    };
    let va = velpt(cx.ra.v, cx.ra.w, ra);
    for k in 0..axes.len() {
        let a = match frame {
            0 => cx.pa.rot(axes[k]),
            1 => cx.pb.rot(axes[k]),
            _ => axes[k],
        };
        rows[k].set_j(a, ra.cross(a), -a, -(rb.cross(a)));
        // D3.22: b_k = target_k − (velpt(A) − VB)·a_k ; D3.23: target is the frame vector (caller
        // passes T·m_k per axis, so the same expression holds)
        let _ = triple;
        rows[k].rhs = [target[k] - (va - vb).dot(a), 0.0];
        if force_limited {
            rows[k].lo[0] = -h * max_force[k];
            rows[k].hi[0] = h * max_force[k];
        }
        rows[k].lo[1] = 0.0;
        rows[k].hi[1] = 0.0;
    }
}

// ---------------------------------------------------------------------------------
// §4.9 Plane — Studio (1 row)
pub fn plane(rows: &mut [DualRow], cx: &Ctx, p_a: Vec3, p_b: Vec3, normal_a: Vec3) {
    let pa = cx.pa.point(p_a);
    let pb = cx.pb.point(p_b);
    let n = cx.pa.rot(normal_a);
    let d = pb - pa;
    let rb = pb - cx.rb.com;
    // Ja = (−n, n × (pB − cA)); Jb = (n, rB × n)
    rows[0].set_j(-n, n.cross(pb - cx.ra.com), n, rb.cross(n));
    let jv = rows[0].jdotv(cx);
    rows[0].rhs = [-jv, -(d.dot(n))];
}

// ---------------------------------------------------------------------------------
// §4.11 LinearServoMotor — Studio (1 row)
#[derive(Clone)]
pub struct LinServo {
    pub p_a: Vec3,       //
    pub p_b: Vec3,       //
    pub axis_a: Vec3,    //
    pub target: f32,     //
    pub max_speed: f32,  //
    pub max_force: f32,  //
    pub k: f32,          //
    pub d: f32,          //
}

pub fn linear_servo(rows: &mut [DualRow], cx: &Ctx, c: &LinServo) {
    let h = cx.h;
    let pa = cx.pa.point(c.p_a);
    let pb = cx.pb.point(c.p_b);
    let w = cx.pa.rot(c.axis_a);
    let s = (pb - pa).dot(w);
    let q = pa + w * s;
    let ra = q - cx.ra.com;
    let rb = q - cx.rb.com;
    rows[0].set_j(-w, -(ra.cross(w)), w, rb.cross(w));
    let u = (velpt(cx.rb.v_ref, cx.rb.w_ref, rb) - velpt(cx.ra.v_ref, cx.ra.w_ref, ra)).dot(w);
    let t = if cx.cfg.adaptive {
        // level-1 identity: h((T − s)K − uD) = Σa with x = s − T, v = u
        substep(h, c.k, c.d, s - c.target, u, 1) + u
    } else {
        h * ((c.target - s) * c.k - u * c.d) + u
    };
    let t = clamp(t, -c.max_speed, c.max_speed);
    let jv = rows[0].jdotv(cx);
    rows[0].rhs = [t - jv, 0.0];
    rows[0].lo = [-h * c.max_force, -0.0];
    rows[0].hi = [h * c.max_force, 0.0];
}
