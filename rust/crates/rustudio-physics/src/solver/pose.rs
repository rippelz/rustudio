//! Studio's rotation update (GEOM2 §B, [P]): Studio as called from the integration
//! Studio at Studio (the gyro byte is 1 on the legacy Finalize path, Studio).
//! All f64, each sum grouped as Studio 0.744 does (C left to right); results rounded
//! to f32. Helpers transcribed: Studio (the Newton solve on the rotation vector),
//! Studio (one Newton–Schulz step), Studio (A + B), Studio (A − B),
//! Studio (Mᵀ·v, three sum orders). Reference:
//! checks/humanoid-physics/solver/geom2/gyro.py (same transcription, Python).

use glam::Vec3;

/// Row-major 3×3 (as body.rs `m3::M3`).
pub type M3 = [f32; 9];
type D9 = [f64; 9];

const DBL_MIN: f64 = 2.2250738585072014e-308; // .rdata Studio
const SMALL: f64 = 1e-4; // .rdata Studio
const TAYLOR: f64 = 0.09; // .rdata Studio
const SIXTH: f64 = 0.16666666666666666; // .rdata Studio
const TWENTYFOURTH: f64 = 0.041666666666666664; // .rdata Studio

/// cfg: L = dt·240, tol = (L·5)/100, f32 (.rdata Studio = 240,
/// Studio = 5, Studio = 100). Studio's fifth argument is the batch cfg
/// (Studio passes closure slot 6 = Studio's param_11, the cfg whose the
/// same loop reads), not the body.
pub fn tol_of(dt: f32) -> f32 {
    let l = dt * 240.0;
    (l * 5.0) / 100.0
}

/// exp(−[r]×) = E and G = I + c[n]× + d[n]×² with n = −r, in Studio's loop order.
/// Branches: |n|² < 1e-4 first order (G = I + ½[n]×); < 0.09 series (s = 1 − n²/6,
/// c = ½ − n²/24, d = 1/6); else exact (s = sin n/n, c = (1−cos n)/n², d = (1−s)/n²).
fn ek(r: [f64; 3]) -> (D9, D9) {
    let [rx, ry, rz] = r;
    let (nx, ny, nz) = (-rx, -ry, -rz);
    let (nz2, nx2, ny2) = (nz * nz, nx * nx, ny * ny);
    let n2 = (nx2 + ny2) + nz2;
    if n2 < SMALL {
        let e = [1.0, rz, ny, nz, 1.0, rx, ry, nx, 1.0];
        let g = [1.0, -(nz * 0.5), ny * 0.5, nz * 0.5, 1.0, -(nx * 0.5), -(ny * 0.5), nx * 0.5, 1.0];
        return (e, g);
    }
    let k12 = nz * ny;
    let k00 = rz * nz - ny2;
    let k02 = nz * nx;
    let k22 = ny * ry - nx2;
    let k01 = ny * nx;
    let k11 = nx * rx - nz2;
    let (s, c, d) = if n2 >= TAYLOR {
        let n = n2.sqrt();
        let inv = 1.0 / n;
        let s = n.sin() * inv;
        let co = n.cos();
        (s, (1.0 - co) * inv * inv, (1.0 - s) * inv * inv)
    } else {
        (1.0 - n2 * SIXTH, 0.5 - n2 * TWENTYFOURTH, SIXTH)
    };
    let x = [1.0, -(s * nz), s * ny, s * nz, 1.0, -(s * nx), -(s * ny), s * nx, 1.0];
    let y = [c * k00, c * k01, c * k02, c * k01, c * k11, c * k12, c * k02, c * k12, c * k22];
    let e = add(&x, &y);
    let x = [1.0, -(c * nz), c * ny, c * nz, 1.0, -(c * nx), -(c * ny), c * nx, 1.0];
    let y = [d * k00, d * k01, d * k02, d * k01, d * k11, d * k12, d * k02, d * k12, d * k22];
    (e, add(&x, &y))
}

/// Studio.
fn add(a: &D9, b: &D9) -> D9 {
    std::array::from_fn(|i| a[i] + b[i])
}

/// Studio(a, I6 = [Ixx, Iyy, Izz, Iyz, Ixz, Ixy], tol): the rotation vector r of the
/// step. r ← a; one Newton update on F(r) = L − exp(−[a]×)·L (L = I·a), then up to two
/// more on F(r) = I·(2r − exp(−[r]×)·a) − exp(−[r]×)·L with the Jacobian 2I − P(G, …),
/// each while |Δr|² > |a|²·tol²; Cramer's rule, a |det| below DBL_MIN stops. Isotropic I:
/// F = 0 up to rounding, r = a.
pub fn solve_r(a: [f64; 3], p: &[f64; 6], tol: f64) -> [f64; 3] {
    let [a0, a1, a2] = a;
    let lx = (a1 * p[5] + a2 * p[4]) + a0 * p[0];
    let ly = (a0 * p[5] + a2 * p[3]) + a1 * p[1];
    let lz = (a1 * p[3] + a0 * p[4]) + a2 * p[2];
    let (mut rx, mut ry, mut rz) = (a0, a1, a2);
    let tolsq = (((rx * rx + ry * ry) + rz * rz) * tol) * tol;
    let (e, g) = ek([rx, ry, rz]);
    let elz = (e[7] * ly + e[6] * lx) + e[8] * lz;
    let ely = (e[5] * lz + e[3] * lx) + e[4] * ly;
    let elx = (e[2] * lz + e[1] * ly) + e[0] * lx;
    let t11 = a1 * p[4] - a0 * p[3];
    let t13 = a0 * p[3] - a2 * p[5];
    let t14 = a2 * p[5] - a1 * p[4];
    let t15 = (a0 * p[2] - a2 * p[4]) + elx;
    let t4 = (a2 * p[3] - a1 * p[2]) - ely;
    let t12 = (a2 * p[1] - a1 * p[3]) + elz;
    let t10 = (a1 * p[5] - a0 * p[1]) - elx;
    let t16 = (a1 * p[0] - a0 * p[5]) + ely;
    let t17 = (a0 * p[4] - a2 * p[0]) - elz;
    let [g00, g01, g02, g10, g11, g12, g20, g21, g22] = g;
    let j2 = (p[4] + p[4]) - ((g00 * t4 + g10 * t15) + g20 * t11);
    let j3 = (p[3] + p[3]) - ((g01 * t4 + g11 * t15) + g21 * t11);
    let j4 = (p[2] + p[2]) - ((g02 * t4 + g12 * t15) + g22 * t11);
    let j5 = (p[5] + p[5]) - ((g20 * t10 + g00 * t12) + g10 * t13);
    let j15 = (p[1] + p[1]) - ((g21 * t10 + g01 * t12) + g11 * t13);
    let j10 = (p[3] + p[3]) - ((g22 * t10 + g02 * t12) + g12 * t13);
    let j11 = (p[0] + p[0]) - ((g10 * t17 + g20 * t16) + g00 * t14);
    let j6 = (p[5] + p[5]) - ((g11 * t17 + g21 * t16) + g01 * t14);
    let j1 = (p[4] + p[4]) - ((g12 * t17 + g22 * t16) + g02 * t14);
    let det = ((j3 * j5 * j1 + j2 * j10 * j6) - ((j3 * j11 * j10 + j4 * j6 * j5) + j2 * j15 * j1)) + j11 * j15 * j4;
    if !(det.abs() >= DBL_MIN) {
        return [rx, ry, rz];
    }
    let (rsy, rsx, rsz) = (ly - ely, lx - elx, lz - elz);
    let inv = 1.0 / det;
    let dx = (((j3 * j1 - j4 * j6) * rsy + (j10 * j6 - j15 * j1) * rsz) + (j4 * j15 - j3 * j10) * rsx) * inv;
    let dy = (((j2 * j10 - j5 * j4) * rsx + (j5 * j1 - j11 * j10) * rsz) + (j4 * j11 - j2 * j1) * rsy) * inv;
    let dz = (((j5 * j3 - j2 * j15) * rsx + (j2 * j6 - j3 * j11) * rsy) + (j11 * j15 - j5 * j6) * rsz) * inv;
    rx -= dx;
    ry -= dy;
    rz -= dz;
    if !(tolsq < (dy * dy + dx * dx) + dz * dz) {
        return [rx, ry, rz];
    }
    let i2 = [p[0] + p[0], p[5] + p[5], p[4] + p[4], p[5] + p[5], p[1] + p[1], p[3] + p[3], p[4] + p[4], p[3] + p[3], p[2] + p[2]];
    let mut it = 0;
    loop {
        let (e, g) = ek([rx, ry, rz]);
        let ux = (e[2] * a2 + e[1] * a1) + e[0] * a0;
        let uy = (e[3] * a0 + e[5] * a2) + e[4] * a1;
        let elz = (lx * e[6] + e[7] * ly) + e[8] * lz;
        let elx = (e[2] * lz + e[1] * ly) + lx * e[0];
        let uz = (e[6] * a0 + e[7] * a1) + e[8] * a2;
        let ely = (lx * e[3] + e[5] * lz) + e[4] * ly;
        let v338 = [(uz * p[3] - uy * p[2]) - ely, (ux * p[2] - uz * p[4]) + elx, uy * p[4] - ux * p[3]];
        let v350 = [(uz * p[1] - uy * p[3]) + elz, ux * p[3] - uz * p[5], (uy * p[5] - ux * p[1]) - elx];
        let v368 = [uz * p[5] - uy * p[4], (ux * p[4] - uz * p[0]) - elz, (uy * p[0] - ux * p[5]) + ely];
        // Studio: the rows of P.
        let o22: [f64; 3] = std::array::from_fn(|j| (g[3 + j] * v368[1] + g[6 + j] * v368[2]) + g[j] * v368[0]);
        let o21: [f64; 3] = std::array::from_fn(|j| (g[6 + j] * v350[2] + g[j] * v350[0]) + g[3 + j] * v350[1]);
        let o20: [f64; 3] = std::array::from_fn(|j| (g[j] * v338[0] + g[3 + j] * v338[1]) + g[6 + j] * v338[2]);
        let pm = [o22[0], o22[1], o22[2], o21[0], o21[1], o21[2], o20[0], o20[1], o20[2]];
        let m: D9 = std::array::from_fn(|i| i2[i] - pm[i]); // Studio
        let s320 = m[5] * m[0];
        let det = ((m[7] * m[2] * m[3] + m[6] * m[1] * m[5]) - ((m[7] * s320 + m[3] * m[8] * m[1]) + m[6] * m[4] * m[2])) + m[8] * m[4] * m[0];
        if !(det.abs() >= DBL_MIN) {
            break;
        }
        let (wx, wy, wz) = ((rx + rx) - ux, (ry + ry) - uy, (rz + rz) - uz);
        let fz = ((wy * p[3] + wx * p[4]) + wz * p[2]) - elz;
        let fy = ((wz * p[3] + wx * p[5]) + wy * p[1]) - ely;
        let inv = 1.0 / det;
        let fx = ((wy * p[5] + wz * p[4]) + wx * p[0]) - elx;
        let dx = (((m[7] * m[2] - m[8] * m[1]) * fy + (m[1] * m[5] - m[4] * m[2]) * fz) + (m[4] * m[8] - m[5] * m[7]) * fx) * inv;
        let dy = (((m[6] * m[5] - m[8] * m[3]) * fx + (m[2] * m[3] - s320) * fz) + (m[8] * m[0] - m[6] * m[2]) * fy) * inv;
        rx -= dx;
        ry -= dy;
        let dz = (((m[6] * m[1] - m[7] * m[0]) * fz + (m[7] * m[3] - m[4] * m[6]) * fx) + (m[4] * m[0] - m[1] * m[3]) * fy) * inv;
        rz -= dz;
        if (dx * dx + dy * dy) + dz * dz <= tolsq {
            break;
        }
        it += 1;
        if 1 < it {
            break;
        }
    }
    [rx, ry, rz]
}

/// Studio: R' = M·E, E = (MᵀM − 3I)·(−½) (.rdata Studio = 3, Studio = −½).
pub fn newton_schulz(mm: &D9) -> D9 {
    let [m0, m1, m2, m3, m4, m5, m6, m7, m8] = *mm;
    let h = -0.5;
    let e12 = ((m1 * m2 + m4 * m5) + m7 * m8) * h;
    let e02 = ((m0 * m2 + m3 * m5) + m6 * m8) * h;
    let e01 = ((m0 * m1 + m3 * m4) + m6 * m7) * h;
    let e00 = (((m0 * m0 + m3 * m3) + m6 * m6) - 3.0) * h;
    let e11 = (((m1 * m1 + m4 * m4) + m7 * m7) - 3.0) * h;
    let e22 = (((m2 * m2 + m5 * m5) + m8 * m8) - 3.0) * h;
    [
        (e01 * m1 + e02 * m2) + e00 * m0,
        (e12 * m2 + e11 * m1) + e01 * m0,
        (e12 * m1 + m2 * e22) + e02 * m0,
        (e02 * m5 + e00 * m3) + e01 * m4,
        (e12 * m5 + e01 * m3) + e11 * m4,
        (m5 * e22 + e02 * m3) + e12 * m4,
        (e00 * m6 + e01 * m7) + e02 * m8,
        (e01 * m6 + e11 * m7) + e12 * m8,
        (e12 * m7 + e02 * m6) + m8 * e22,
    ]
}

/// Studio(R, ω, I = body (row-major, body frame), dt, cfg): returns (R', ω').
/// 1 widen, symmetrise I (½ = .rdata Studio); 2 ω_b = Rᵀω; 3 a = dt·ω_b;
/// 4 r = Studio(a, I, cfg); 5 Q = exp([r]×) by the three branches; 6 M = R·Q;
/// 7 R' = Studio(M); 8 R' to f32; 9 ω' = R'·I⁻¹·R'ᵀ·(R·I·ω_b) (adjugate / det), to f32.
pub fn gyro_step(r: &M3, w: Vec3, inertia: &M3, dt: f32, tol: f32) -> (M3, Vec3) {
    let rr: D9 = std::array::from_fn(|i| r[i] as f64);
    let ii: D9 = std::array::from_fn(|i| inertia[i] as f64);
    let [r00, r01, r02, r10, r11, r12, r20, r21, r22] = rr;
    let p = [ii[0], ii[4], ii[8], (ii[7] + ii[5]) * 0.5, (ii[2] + ii[6]) * 0.5, (ii[1] + ii[3]) * 0.5];
    let (w0, w1, w2) = (w.x as f64, w.y as f64, w.z as f64);
    let b0 = (w2 * r20 + w1 * r10) + r00 * w0;
    let b1 = (r01 * w0 + w2 * r21) + w1 * r11;
    let b2 = (r02 * w0 + w1 * r12) + w2 * r22;
    let d = dt as f64;
    let [rx, ry, rz] = solve_r([d * b0, d * b1, d * b2], &p, tol as f64);
    let n2 = (ry * ry + rx * rx) + rz * rz;
    let q: D9 = if n2 < SMALL {
        [1.0, -rz, ry, rz, 1.0, -rx, -ry, rx, 1.0]
    } else {
        let (s, c) = if n2 >= TAYLOR {
            let n = n2.sqrt();
            let inv = 1.0 / n;
            (n.sin() * inv, (1.0 - n.cos()) * inv * inv)
        } else {
            (1.0 - n2 * SIXTH, 0.5 - n2 * TWENTYFOURTH)
        };
        let (xz, yz, xy) = (rx * rz * c, ry * rz * c, rx * ry * c);
        [
            ((-rz) * rz - ry * ry) * c + 1.0,
            xy - rz * s,
            xz + ry * s,
            rz * s + xy,
            ((-rx) * rx - rz * rz) * c + 1.0,
            yz - rx * s,
            xz - ry * s,
            yz + rx * s,
            ((-ry) * ry - rx * rx) * c + 1.0,
        ]
    };
    let [q00, q01, q02, q10, q11, q12, q20, q21, q22] = q;
    let m = [
        (r02 * q20 + r01 * q10) + r00 * q00,
        (r02 * q21 + r01 * q11) + r00 * q01,
        (r02 * q22 + r01 * q12) + r00 * q02,
        (r12 * q20 + r10 * q00) + r11 * q10,
        (r10 * q01 + r12 * q21) + r11 * q11,
        (r10 * q02 + q22 * r12) + r11 * q12,
        (r21 * q10 + r20 * q00) + r22 * q20,
        (r20 * q01 + r21 * q11) + r22 * q21,
        (r21 * q12 + r20 * q02) + r22 * q22,
    ];
    let o = newton_schulz(&m);
    let [ixx, iyy, izz, iyz, ixz, ixy] = p;
    let lx = (ixz * b2 + ixy * b1) + ixx * b0;
    let ly = (iyz * b2 + ixy * b0) + iyy * b1;
    let lz = (iyz * b1 + ixz * b0) + izz * b2;
    let t19 = ixz * ixy * iyz;
    let wx = (lz * r02 + ly * r01) + lx * r00;
    let wy = (lx * r10 + lz * r12) + ly * r11;
    let wz = (ly * r21 + lx * r20) + lz * r22;
    let c16 = ixy * iyz - iyy * ixz;
    let invd = 1.0 / (((t19 + t19) - ((iyz * iyz * ixx + ixy * ixy * izz) + ixz * iyy * ixz)) + izz * iyy * ixx);
    let c1 = ixy * ixz - iyz * ixx;
    let c19 = iyz * ixz - ixy * izz;
    let uz = ((o[5] * wy + o[2] * wx) + o[8] * wz) * invd;
    let uy = ((o[7] * wz + o[1] * wx) + o[4] * wy) * invd;
    let ux = ((o[6] * wz + o[3] * wy) + o[0] * wx) * invd;
    let vx = ((iyy * izz - iyz * iyz) * ux + c16 * uz) + c19 * uy;
    let vy = ((izz * ixx - ixz * ixz) * uy + c1 * uz) + c19 * ux;
    let vz = (c1 * uy + c16 * ux) + (iyy * ixx - ixy * ixy) * uz;
    let wn = Vec3::new(((o[2] * vz + o[1] * vy) + o[0] * vx) as f32, ((o[5] * vz + o[3] * vx) + o[4] * vy) as f32, ((o[7] * vy + o[6] * vx) + o[8] * vz) as f32);
    (std::array::from_fn(|i| o[i] as f32), wn)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GEOM2 §B.4: Studio's rows t → t+1 of the spin labs and sphere_roll (testdata/pose_rows.txt,
    /// written by geom2/gyro.py `fixture`): R(t), ω(t+1), dt, I → R(t+1) bit for bit.
    #[test]
    fn studio_rows_bit_exact() {
        let txt = include_str!("testdata/pose_rows.txt");
        let mut per: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        for ln in txt.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let s: Vec<&str> = ln.split_whitespace().collect();
            let f: Vec<f32> = s[2..].iter().map(|x| f32::from_bits(u32::from_str_radix(x, 16).unwrap())).collect();
            let r: M3 = std::array::from_fn(|i| f[i]);
            let w = Vec3::new(f[9], f[10], f[11]);
            let dt = f[12];
            let ii: M3 = std::array::from_fn(|i| f[13 + i]);
            let want: M3 = std::array::from_fn(|i| f[22 + i]);
            let (got, _) = gyro_step(&r, w, &ii, dt, tol_of(dt));
            let ok = (0..9).all(|i| want[i].abs() <= 1e-3 || got[i].to_bits() == want[i].to_bits());
            let e = per.entry(s[0].to_string()).or_default();
            e.0 += 1;
            e.1 += ok as usize;
        }
        for (k, (n, ok)) in &per {
            assert_eq!(n, ok, "{k}: {ok} / {n} bit-exact");
        }
        assert_eq!(per.values().map(|v| v.0).sum::<usize>(), 142);
    }

    /// Isotropic inertia: Studio returns a (the residual is rounding only).
    #[test]
    fn isotropic_returns_a() {
        let a = [0.01, -0.02, 0.03];
        let r = solve_r(a, &[2.5, 2.5, 2.5, 0.0, 0.0, 0.0], 0.05);
        for k in 0..3 {
            assert!((r[k] - a[k]).abs() < 1e-15);
        }
    }

    /// Anisotropic free spin: |L| (world angular momentum) is carried over by step 9, and R stays
    /// orthonormal (one Newton–Schulz step per tick).
    #[test]
    fn bar_keeps_momentum_and_orthonormal() {
        let ii: M3 = [0.5, 0.0, 0.0, 0.0, 4.25, 0.0, 0.0, 0.0, 4.25];
        let mut r: M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let mut w = Vec3::new(3.0, 2.0, 1.0);
        let dt = 1.0 / 240.0;
        let lw = |r: &M3, w: Vec3| {
            let m = glam::Mat3::from_cols_array(r).transpose();
            let i = glam::Mat3::from_cols_array(&ii).transpose();
            m * i * m.transpose() * w
        };
        let l0 = lw(&r, w);
        for _ in 0..240 {
            (r, w) = gyro_step(&r, w, &ii, dt, tol_of(dt));
        }
        let l1 = lw(&r, w);
        assert!((l1 - l0).length() < 1e-3 * l0.length(), "{l0} {l1}");
        let m = glam::Mat3::from_cols_array(&r).transpose();
        let e = m * m.transpose() - glam::Mat3::IDENTITY;
        assert!(e.to_cols_array().iter().all(|x| x.abs() < 1e-6));
    }
}
