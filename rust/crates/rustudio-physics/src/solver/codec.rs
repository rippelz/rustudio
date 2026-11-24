//! Studio's pose-compression codec (DFFlagSAuthLocallyCompressPVsSolver, STEP §4.2 [L3]).
//!
//! Literal port of Studio 0.747.0.7471481: encode Studio, decode Studio,
//! the vector quantiser Studio, the rotation lattice built once by
//! Studio (entries by Studio), the rotation
//! encoder Studio (with CoordinateFrame → axis·angle Studio) and decoder
//! Studio = Studio then Studio (→ Studio). f32 / f64 as the binary has them,
//! operations in its order (each step cites the instruction address it was read from).
//!
//! Not bit-exact by construction where Studio calls the MSVC UCRT libm (cos, sin, asin, acos,
//! atan2 in f64; sinf, cosf, atan2f in f32): those are std's (the platform libm) here. They differ
//! from UCRT by at most an ulp in rare cases; the lattice table only depends on them through
//! ceil()/round() of values far from a boundary (checked in a test), the codec through round()
//! of the ring / azimuth index and the last bit of the decoded f32 axis.
//!
//! Where it applies (write-back Studio, see STEP §4.2): only when
//! DFFlagSAuthLocallyCompressPVsSolver (byte Studio) AND cfg (= solver, the
//! server-authority switch set through the solver's slot 31 Studio) are both set.

use std::sync::OnceLock;

// ---- constants (.rdata / .data, value = bits read from the exe) --------------------------------
/// .rdata Studio (0x400921fb54442d18)
const PI_D: f64 = 3.141592653589793;
/// .rdata Studio (0x401921fb54442d18)
const TWO_PI_D: f64 = 6.283185307179586;
/// .rdata Studio (0x3ff921fb54442d18)
const HALF_PI_D: f64 = 1.5707963267948966;
/// .rdata Studio (0x3fc45f306dc9c883) = 1/2π
const INV_TWO_PI_D: f64 = 0.15915494309189535;
/// .rdata Studio
const HALF_D: f64 = 0.5;
/// .rdata Studio
const ONE_D: f64 = 1.0;
/// .rdata Studio
const NEG_ONE_D: f64 = -1.0;
/// .rdata Studio
const TWO_D: f64 = 2.0;
/// .rdata Studio
const EIGHT_D: f64 = 8.0;
/// .rdata Studio = 2^63 (f64 → u64 conversion in Studio)
const TWO_POW_63_D: f64 = 9.223372036854775808e18;
/// .rdata Studio = 2^-12: a vector whose largest |component| is ≤ this is sent as zero
const VEC_ZERO_D: f64 = 0.000244140625;
/// .rdata Studio (0x3f847ae147ae147b): absolute precision the vector quantiser targets
const VEC_PREC_D: f64 = 0.01;
/// .rdata Studio
const ONE_F: f32 = 1.0;
/// .rdata Studio
const HALF_F: f32 = 0.5;
/// .rdata Studio..Studio (four lanes, all 0.5)
const HALF4_F: f32 = 0.5;
/// .rdata Studio (0x3c23d70a): rotation-level threshold and the small-angle
/// cut of Studio
const CENTI_F: f32 = f32::from_bits(0x3c23d70a);
/// .rdata Studio
const THREE_F: f32 = 3.0;
/// .rdata Studio (0x3fb504f3)
const SQRT2_F: f32 = f32::from_bits(0x3fb504f3);
/// .rdata Studio (0x3e4ccccd): |w|² below this takes the series branch of Studio
const SERIES_F: f32 = f32::from_bits(0x3e4ccccd);
/// .rdata Studio..Studio: Taylor coefficients of Studio
const C_1_40320: f32 = f32::from_bits(0x37d00d01); // Studio
const C_1_5040: f32 = f32::from_bits(0x39500d01); // Studio
const C_1_720: f32 = f32::from_bits(0x3ab60b61); // Studio
const C_1_120: f32 = f32::from_bits(0x3c088889); // Studio
const C_1_24: f32 = f32::from_bits(0x3d2aaaab); // Studio
const C_1_6: f32 = f32::from_bits(0x3e2aaaab); // Studio
/// .data Studio (f32[3], static, read only by Studio): per-level
/// rotation error, picked by tol·T[k] ≤ 0.01
const LEVEL_ERR: [f32; 3] = [
    f32::from_bits(0x3b427301), // 0.0029670598
    f32::from_bits(0x3a001b95), // 0.00048869225
    f32::from_bits(0x38afb0cb), // 8.3775805e-05
];
/// .rdata Studio: the tolerance Studio passes to Studio
pub const SOLVER_TOL: f32 = 10.0;
/// DFFlagRejectOutOfRangeAxisCode (byte Studio; StudioAppSettings.json: True), read
/// by Studio @Studio
const REJECT_OUT_OF_RANGE_AXIS_CODE: bool = true;

// ---- public pose -------------------------------------------------------------------------------

/// A pose: row-major rotation R[9] (CoordinateFrame layout: R[0..3] = row 0), position p,
/// linear v, angular w. f32. Byte layout in Studio: R.., p, v, w.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pv {
    pub r: [f32; 9],
    pub p: [f32; 3],
    pub v: [f32; 3],
    pub w: [f32; 3],
}

/// Studio(pv, 10.0) then Studio: what Studio's narrow phase / joints / Lua read.
pub fn roundtrip(pv: &Pv) -> Pv {
    decode(&encode(pv, SOLVER_TOL))
}

// ---- packed record (0x70 bytes in Studio) ------------------------------------------------------

/// One quantised vector, 0x20 bytes (Studio's output): +0 zero flag, +1 exponent E,
/// +2 bit count, +8// the three i64 codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VecCode {
    pub zero: u8,
    pub e: u8,
    pub bits: u8,
    pub q: [i64; 3],
}

/// Studio's output: position, rotation level byte and code, v, w.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packed {
    pub p: VecCode,
    pub level: u8,
    pub code: u64,
    pub v: VecCode,
    pub w: VecCode,
}

// ---- vector quantiser --------------------------------------------------------------------------

/// f64 with the given biased-exponent field and zero mantissa (`(k as i64) << 52` reinterpreted).
#[inline]
fn pow2_bits(biased: i64) -> f64 {
    f64::from_bits((biased << 52) as u64)
}

/// Studio(x, k): x ∈ [−1, 1] → i64 in [−2^k, 2^k − 1].
fn quant(x: f64, k: i32) -> i64 {
    let s = pow2_bits((k + 0x3ff) as i64); // @Studio..f61: 2^k from its bits
    let l = s as i64; // @Studio cvttsd2si
    let y = s * x; // @Studio
    let neg_l = l.wrapping_neg(); // @Studio
    let l1 = l - 1; // @Studio
    let r = (HALF_D.copysign(y) + y) as i64; // @Studio copysign(0.5, y) (import Studio), addsd, cvttsd2si
    if l1 < r {
        return l1; // @Studio jge not taken
    }
    if r < neg_l { neg_l } else { r } // @Studio cmovl
}

/// Studio(out, v[3] f64, ebits): shared-exponent fixed point, precision 0.01 absolute,
/// mantissa width rounded up to 12/20/28/36/44/52.
pub fn encode_vec(v: [f64; 3], ebits: u32) -> VecCode {
    // @Studio..d58: m = max(|v0|, |v1|, |v2|) (andps with Studio)
    let a = [v[0].abs(), v[1].abs(), v[2].abs()];
    let m01 = if a[1] <= a[0] { a[0] } else { a[1] }; // cmovbe @Studio
    let m = if a[2] > m01 { a[2] } else { m01 }; // ja @Studio
    // @Studio: 2^-12 >= m → the zero record {1, 0, 0, 0, 0, 0}
    if VEC_ZERO_D >= m {
        return VecCode { zero: 1, e: 0, bits: 0, q: [0; 3] };
    }
    // @Studio ilogb (import Studio); E = clamp(ilogb(m) + 1, 0, 2^ebits − 1)
    let il = ilogb(m);
    let emax = (1i32 << ebits) - 1; // @Studio..dc3
    let e1 = il + 1;
    let e = if emax < e1 { emax } else if e1 < 0 { 0 } else { e1 }; // @Studio..dd0
    // @Studio..e4b: c_i = min(1, max(−1, 2^-E · v_i))
    let sc = pow2_bits((0x3ff - e) as i64);
    let c0 = ONE_D.min_sse(NEG_ONE_D.max_sse(sc * v[0]));
    let c1 = ONE_D.min_sse(NEG_ONE_D.max_sse(sc * v[1]));
    let c2 = ONE_D.min_sse(NEG_ONE_D.max_sse(sc * v[2]));
    // @Studio..e6f: n = max(0, (int)ceil(E − log2(0.01))) + 1 (log2 = import Studio,
    // ceil = thunk Studio)
    let t = ((e as f64) - VEC_PREC_D.log2()).ceil();
    let ti = t as i32; // cvttsd2si eax
    let n = if ti > 0 { ti } else { 0 } + 1;
    // @Studio..ead: round the width up
    let bits: i32 = if n <= 12 {
        12
    } else if n <= 20 {
        20
    } else if n <= 28 {
        28
    } else if n <= 36 {
        36
    } else if n <= 44 {
        44
    } else {
        52
    };
    // @Studio..ee6: {0, E, bits}, q_i = Studio(c_i, bits − 1)
    VecCode {
        zero: 0,
        e: e as u8,
        bits: bits as u8,
        q: [quant(c0, bits - 1), quant(c1, bits - 1), quant(c2, bits - 1)],
    }
}

/// Studio: VecCode → f64[3] (zero flag → +0).
pub fn decode_vec(c: &VecCode) -> [f64; 3] {
    if c.zero != 0 {
        return [0.0; 3]; // @Studio
    }
    // @Studio..cbd: d = 2^(1−bits) · 2^E (two f64 built from exponent bits)
    let d = pow2_bits(0x400 - c.bits as i64) * pow2_bits(c.e as i64 + 0x3ff);
    [c.q[0] as f64 * d, c.q[1] as f64 * d, c.q[2] as f64 * d] // @Studio..ce1
}

/// ilogb for finite non-zero normal/subnormal f64 (UCRT ilogb, import Studio).
fn ilogb(x: f64) -> i32 {
    let b = x.to_bits();
    let ex = ((b >> 52) & 0x7ff) as i32;
    if ex == 0 {
        // subnormal (unreachable: callers have m > 2^-12)
        let m = b & ((1u64 << 52) - 1);
        return -1023 - (m.leading_zeros() as i32 - 12);
    }
    ex - 1023
}

/// SSE minsd / maxsd semantics (dst op src: returns src unless dst strictly wins).
trait SseMinMax {
    fn min_sse(self, src: f64) -> f64;
    fn max_sse(self, src: f64) -> f64;
}
impl SseMinMax for f64 {
    #[inline]
    fn min_sse(self, src: f64) -> f64 {
        if self < src { self } else { src }
    }
    #[inline]
    fn max_sse(self, src: f64) -> f64 {
        if self > src { self } else { src }
    }
}

// ---- rotation lattice --------------------------------------------------------------------------

/// One 0x58-byte table entry (Studio layout).
#[derive(Clone, Debug)]
pub struct Lattice {
    ///: angle bits e0
    pub e0: u32,
    ///: axis bits e1 = ceil(log2(count))
    pub e1: u32,
    ///: angle step 2π / 2^e0
    pub ang_step: f64,
    ///: 1 / ang_step
    pub inv_ang_step: f64,
    ///: number of polar rings n (ring 0 = the pole, ring n−1 = the equator)
    pub rings: u64,
    ///: number of axis points on the hemisphere
    pub count: u64,
    ///: the spacing parameter d the ring search settled on
    pub d: f64,
    ///: polar step (π/2) / (n − 1)
    pub dpolar: f64,
    ///: 1 / dpolar
    pub inv_dpolar: f64,
    ///..: std::vector<u64> of ring start offsets, len n + 1 (off[0] = 0, off[1] = 1)
    pub off: Vec<u64>,
}

/// Studio(n, d): number of axis points with n rings for spacing d.
fn ring_count(n: i64, d: f64) -> i64 {
    let dp = HALF_PI_D / ((n - 1) as f64); // @Studio..713
    let h = dp * HALF_D; // @Studio
    let ca = h.cos(); // @Studio cos (thunk Studio)
    let sb = ((h + d) * HALF_D).sin(); // @Studio..73b sin (thunk Studio)
    let sc = ((d - h) * HALF_D).sin(); // @Studio..751
    let k = sc * (sb + sb); // @Studio..76f
    let nf = n as f64;
    let mut total: i64 = 1; // @Studio: the pole
    let mut i = ONE_D;
    while nf > i {
        // @Studio / @Studio
        total += ring_points(i, dp, h, ca, k); // @Studio..7ea
        i += ONE_D; // @Studio
    }
    total
}

/// Points on ring i (shared body of Studio @Studio @Studio and
/// Studio @Studio): ceil((π/2) / asin(sqrt(K / (cos h − cos(2·i·Δ + h))))).
#[inline]
fn ring_points(i: f64, dp: f64, h: f64, ca: f64, k: f64) -> i64 {
    let mut x = i * dp;
    x += x;
    x += h;
    let c = x.cos(); // cos (thunk Studio)
    let val = k / (ca - c);
    let s = val.sqrt(); // sqrtsd (negative → thunk Studio sqrt: NaN either way)
    let a = s.asin(); // asin (thunk Studio)
    let r = HALF_PI_D / a;
    let t = r as i64; // cvttsd2si
    t + (r > t as f64) as i64 // seta / add
}

/// Studio(d) → (rings n, count): starts at n = ceil((π/2)/(√2·d)) + 1 and walks n down
/// while count(n−1) < count(n), up while count(n+1) < count(n), at most 999 moves.
fn ring_search(d: f64) -> (i64, i64) {
    let s2 = TWO_D.sqrt(); // @Studio..dfc
    let n0 = (HALF_PI_D / (s2 * d)).ceil(); // @Studio..e23 (ceil thunk Studio)
    let mut r15: i64 = n0 as i64; // @Studio
    let mut rdi: i64 = r15 + 1; // @Studio
    let mut r12: i64 = rdi - 2; // @Studio
    let mut iter: i32 = 1; // r14d @Studio
    loop {
        // @Studio..f42: rbx = count(r15) inline (Δ from r12 = r15 − 1)
        let dp = HALF_PI_D / (r12 as f64);
        let h = dp * HALF_D;
        let ca = h.cos();
        let sb = ((h + d) * HALF_D).sin();
        let sc = ((d - h) * HALF_D).sin();
        let k = sc * (sb + sb);
        let nf = r15 as f64;
        let mut rbx: i64 = 1;
        let mut i = ONE_D;
        while nf > i {
            rbx += ring_points(i, dp, h, ca, k);
            i += ONE_D;
        }
        let rsi = ring_count(rdi, d); // @Studio
        let rax = ring_count(rdi + 1, d); // @Studio
        let r8 = rdi; // @Studio
        let step: i64;
        if rsi == -1 {
            step = 1; // @Studio → f85
            rdi += 1;
        } else if rbx < rsi && rbx != -1 {
            rdi -= 1; // @Studio
            step = -1;
        } else if rsi <= rax {
            return (r8, rsi); // @Studio → fa6: out = {r8, rsi}
        } else {
            step = 1; // @Studio
            rdi += 1;
        }
        iter += 1; // @Studio
        r12 += step;
        r15 += step;
        if iter >= 1000 {
            return (r8, rsi); // @Studio
        }
    }
}

/// Studio(T = {d, n, count}) → the.. part of an entry: polar step and the ring
/// start offsets.
fn ring_offsets(d: f64, n: u64, count: u64) -> (f64, f64, Vec<u64>) {
    let dp = HALF_PI_D / ((n as f64) - ONE_D); // @Studio..382 (u64 → f64)
    let inv = ONE_D / dp; // @Studio..3a5
    let h = dp * HALF_D; // @Studio
    let ca = h.cos(); // @Studio
    let sb = ((d + h) * HALF_D).sin(); // @Studio..3d7
    let sc = ((d - h) * HALF_D).sin(); // @Studio..3ef
    let k = sc * (sb + sb); // @Studio..3fb
    let _ = count;
    let mut off = Vec::with_capacity(n as usize + 1);
    off.push(0u64); // @Studio
    let mut acc: u64 = 1; // @Studio
    let mut i = ONE_D;
    let nf = n as f64;
    while nf > i {
        // @Studio / @Studio
        let cnt = ring_points(i, dp, h, ca, k) as u64; // @Studio..508
        off.push(acc); // @Studio
        acc = acc.wrapping_add(cnt); // @Studio
        i += ONE_D; // @Studio
    }
    off.push(acc); // @Studio
    (dp, inv, off)
}

/// Studio(e0, nb): one lattice entry for 2^e0 angle steps and at most 2^nb axis points.
fn build_entry(e0: u32, nb: u32) -> Lattice {
    let ang_step = TWO_PI_D / ((1u64 << e0) as f64); // @Studio..04f
    let inv_ang_step = ONE_D / ang_step; // @Studio..066
    let npts: u64 = 1u64 << nb; // @Studio
    let nf = npts as f64; // @Studio..096
    let a = PI_D / (nf * EIGHT_D); // @Studio..0b2
    let b = (a * a + PI_D / nf).sqrt(); // @Studio..0cf
    let d0 = b + a; // @Studio
    let r0 = ring_search(d0); // @Studio
    let mut rbx = r0.1 as u64; // @Studio
    let mut f = ((rbx as f64) / nf).sqrt(); // @Studio..12f
    let mut d6 = f * d0; // @Studio
    let mut r1 = ring_search(d6); // @Studio
    let mut d = d0; // xmm8
    let (fd, fnr, fcnt): (f64, u64, u64);
    if f == ONE_D {
        // @Studio..163
        fd = d;
        fnr = r0.0 as u64;
        fcnt = rbx;
    } else {
        let mut rcx = rbx; // @Studio
        let mut rsi = r0.0 as u64; // @Studio
        loop {
            rbx = r1.1 as u64; // @Studio
            if rbx > npts && rcx <= npts {
                // @Studio..17d: the new spacing overflows 2^nb, the previous did not: keep it
                fd = d;
                fnr = rsi;
                fcnt = rcx;
                break;
            }
            d = d6; // @Studio
            rsi = r1.0 as u64; // @Studio
            if rbx == npts {
                // @Studio
                fd = d;
                fnr = rsi;
                fcnt = rbx;
                break;
            }
            f = ((rbx as f64) / nf).sqrt(); // @Studio..1ca
            d6 = f * d; // @Studio
            r1 = ring_search(d6); // @Studio
            rcx = rbx; // @Studio
            if f == ONE_D {
                // @Studio
                fd = d;
                fnr = rsi;
                fcnt = rbx;
                break;
            }
        }
    }
    // @Studio..248: e1 = (int)ceil(log2(count)) (log2 import Studio, ceil thunk Studio)
    let e1 = ((fcnt as f64).log2().ceil()) as i64 as u32;
    let (dpolar, inv_dpolar, off) = ring_offsets(fd, fnr, fcnt); // @Studio, moved by Studio
    Lattice {
        e0,
        e1,
        ang_step,
        inv_ang_step,
        rings: fnr,
        count: fcnt,
        d: fd,
        dpolar,
        inv_dpolar,
        off,
    }
}

/// Studio: the table at Studio, built on first use by Studio.
pub fn lattice() -> &'static [Lattice; 3] {
    static T: OnceLock<[Lattice; 3]> = OnceLock::new();
    T.get_or_init(|| {
        [
            build_entry(0xb, 0x15),  // @Studio
            build_entry(0xe, 0x1a),  // @Studio
            build_entry(0x10, 0x20), // @Studio
        ]
    })
}

// ---- rotation ----------------------------------------------------------------------------------

/// Studio(R, I): exact (f32 ==) identity test.
fn is_identity(r: &[f32; 9]) -> bool {
    const I: [f32; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]; // Studio ×2, Studio
    r.iter().zip(I.iter()).all(|(a, b)| a == b)
}

/// Studio: CoordinateFrame → rotation vector (axis·angle), f32.
pub fn axis_angle(r: &[f32; 9]) -> [f32; 3] {
    let tr = ((r[0] + r[4]) + r[8]) - ONE_F; // @Studio..8a3
    // @Studio..91d: t = R − Rᵀ, then negated (xorps Studio): −(a − b), kept literal
    let z = -(r[1] - r[3]); // xmm9
    let x = -(r[5] - r[7]); // xmm11
    let y = -(r[6] - r[2]); // xmm10
    let n2 = (y * y + x * x) + z * z; // @Studio..93a
    let s = n2.sqrt(); // @Studio sqrtss
    let th = s.atan2(tr); // @Studio atan2f (thunk Studio)
    if CENTI_F > th {
        // @Studio: θ < 0.01
        return [x * HALF_F, y * HALF_F, z * HALF_F];
    }
    if THREE_F > th {
        // @Studio: θ < 3
        let k = th / s;
        return [k * x, k * y, k * z];
    }
    // @Studio..: near π, from the symmetric part S = (R + Rᵀ)/2
    let s0 = (r[0] + r[0]) * HALF4_F;
    let s1 = (r[1] + r[3]) * HALF4_F;
    let s2 = (r[2] + r[6]) * HALF4_F;
    let s3 = (r[3] + r[1]) * HALF4_F;
    let s4 = (r[4] + r[4]) * HALF4_F;
    let s5 = (r[5] + r[7]) * HALF4_F;
    let s6 = (r[6] + r[2]) * HALF4_F;
    let s7 = (r[7] + r[5]) * HALF4_F;
    let s8 = (r[8] + r[8]) * HALF_F; // @Studio..a26
    let sn = (th * HALF_F).sin() * SQRT2_F; // @Studio..a53 sinf (thunk Studio)
    let k = th / sn; // @Studio
    let qx = (((s0 - s4) - s8) + ONE_F) * HALF_F; // @Studio..a92
    let qy = (((s4 - s8) - s0) + ONE_F) * HALF_F; // @Studio..a9b
    let qz = (((s8 - s0) - s4) + ONE_F) * HALF_F; // @Studio..aa2
    let sgn = |v: f32, ref_: f32| if ref_ >= 0.0 { v } else { -v };
    if qx > qy {
        if qx > qz {
            // @Studio
            let c = sgn(qx.sqrt(), x);
            return [k * c, (s3 / c) * k, (s6 / c) * k];
        }
    } else if qy > qz {
        // @Studio
        let c = sgn(qy.sqrt(), y);
        return [(s1 / c) * k, k * c, (s7 / c) * k];
    }
    // @Studio
    let c = sgn(qz.sqrt(), z);
    [(s2 / c) * k, (s5 / c) * k, k * c]
}

/// Studio(entry, R): rotation → 64-bit code (angle e0 bits signed << e1 | axis index).
pub fn encode_rot(e: &Lattice, r: &[f32; 9]) -> u64 {
    let rv = axis_angle(r); // @Studio
    let (rx, ry, rz) = (rv[0] as f64, rv[1] as f64, rv[2] as f64); // @Studio..b65
    let n = ((ry * ry + rx * rx) + rz * rz).sqrt(); // @Studio..b95
    let mut m = PI_D.min_sse(n); // @Studio minsd(π, n)
    let s = ONE_D / m; // @Studio
    let mut ax = s * rx;
    let mut ay = s * ry;
    let mut az = s * rz;
    if 0.0 > az {
        // @Studio: flip to the upper hemisphere (sign mask Studio)
        ax = -ax;
        ay = -ay;
        az = -az;
        m = -m;
    }
    let q = (m * e.inv_ang_step).round() as i64; // @Studio..c11 round (import Studio)
    let mut lo: u64 = 0;
    if q.wrapping_abs() > 0 {
        // @Studio..c24
        let pol = (ay * ay + ax * ax).sqrt().atan2(az); // @Studio..c5b atan2 (thunk Studio)
        let ring = (pol * e.inv_dpolar).round() as i64 as i32; // @Studio..c70
        if ring != 0 {
            let r1 = (ring as u32 as u64) + 1; // @Studio lea ebx,[rcx+1]
            let len = e.off.len() as u64; // @Studio..c86
            if r1 < len {
                let ri = ring as u32 as usize; // @Studio mov ebp,ecx
                let cnt = e.off[r1 as usize].wrapping_sub(e.off[ri]); // @Studio..ca8
                let az_ = ay.atan2(ax); // @Studio
                let t = (az_ + PI_D) * INV_TWO_PI_D; // @Studio..cbf
                let f = ((cnt as f64) * t).round(); // @Studio..cef
                let k = f64_to_u64_msvc(f); // @Studio..d23
                let idx = if k != cnt { k } else { 0 }; // @Studio..d29
                lo = idx.wrapping_add(e.off[ri]); // @Studio..d31
            }
        }
    }
    // @Studio..d77
    ((1u64 << (e.e0 & 63)).wrapping_sub(1) & (q as u64)) << (e.e1 & 63) | lo
}

/// MSVC's inline f64 → u64 (@Studio..d23).
fn f64_to_u64_msvc(f: f64) -> u64 {
    let mut x = f;
    let mut add: u64 = 0;
    if x >= TWO_POW_63_D {
        x -= TWO_POW_63_D;
        if x < TWO_POW_63_D {
            add = 0x8000_0000_0000_0000;
        }
    }
    cvttsd2si_i64(x).wrapping_add(add)
}

/// cvttsd2si r64: out of range / NaN → 0x8000000000000000.
fn cvttsd2si_i64(x: f64) -> u64 {
    if x.is_nan() || x >= TWO_POW_63_D || x < -TWO_POW_63_D {
        0x8000_0000_0000_0000
    } else {
        x as i64 as u64
    }
}

/// Studio(entry, code): 64-bit code → rotation vector (axis·angle), f32.
pub fn decode_rotvec(e: &Lattice, code: u64) -> [f32; 3] {
    // @Studio..85c: sign-extended e0-bit field above e1
    let sh = (0x40u32.wrapping_sub(e.e0) & 0xff) & 63;
    let q = (((code >> (e.e1 & 63)) << sh) as i64) >> sh;
    if q == 0 {
        return [0.0; 3]; // @Studio
    }
    // @Studio..8bc: angle = min(|q|·step, π), sign of q
    let mut ang = (q.wrapping_abs() as f64 * e.ang_step).min_sse(PI_D);
    if q < 0 {
        ang = -ang;
    }
    let lo = ((1u64 << (e.e1 & 63)).wrapping_sub(1)) & code; // @Studio..8d2
    let (x, y, z): (f32, f32, f32);
    if lo == 0 || (REJECT_OUT_OF_RANGE_AXIS_CODE && lo >= e.count) {
        // @Studio / @Studio..8f8: the pole
        x = 0.0;
        y = 0.0;
        z = ONE_F;
    } else {
        let n_off = e.off.len() as i64; // @Studio..928
        // @Studio..973: polar guess from the area: acos(1 − lo·d²/π) (acos thunk Studio)
        let t = ONE_D - ((lo as f64) * e.d * e.d) / PI_D;
        let g = (t.acos() * e.inv_dpolar).floor(); // @Studio..979 floor (thunk Studio)
        let g = cvttsd2si_i64(g) as i64; // @Studio
        let mut rdi = n_off - 2; // @Studio
        if !(rdi < g) {
            rdi = if g < 0 { 0 } else { g }; // @Studio..994
        }
        // @Studio..9c6: walk up while off[ring + 1] <= lo
        while ((rdi + 2) as u64) <= n_off as u64 {
            if e.off[(rdi + 1) as usize] > lo {
                break;
            }
            rdi += 1;
        }
        let ring = rdi;
        let pol = ((ring as f64) * e.dpolar) as f32; // @Studio..9d6 cvtpd2ps
        let cp = pol.cos(); // @Studio cosf (thunk Studio)
        let sp = pol.sin(); // @Studio sinf (thunk Studio)
        let base = e.off[ring as usize];
        let cnt = e.off[ring as usize + 1].wrapping_sub(base); // @Studio..9fe
        let step = TWO_PI_D / (cnt as f64); // @Studio..a3f
        let azm = (step * (lo.wrapping_sub(base) as f64) - PI_D) as f32; // @Studio..a6c
        y = azm.sin() * sp; // @Studio..a7e
        x = azm.cos() * sp; // @Studio..a8d
        z = cp;
    }
    let a = ang as f32; // @Studio cvtsd2ss
    [x * a, y * a, z * a] // @Studio..ad4
}

/// Studio(axis (unit), θ): Rodrigues, f32.
fn axis_angle_matrix(a: [f32; 3], th: f32) -> [f32; 9] {
    let c = th.cos(); // @Studio cosf
    let s = th.sin(); // @Studio sinf
    let (x, y, z) = (a[0], a[1], a[2]);
    let t = ONE_F - c; // @Studio
    let zx = z * x; // xmm7
    let yx = y * x; // xmm4
    let zy = z * y; // xmm6
    let ys = y * s; // xmm3
    let zs = z * s; // xmm2
    let xs = x * s; // xmm5
    let zyt = zy * t;
    let zxt = zx * t;
    let yxt = yx * t;
    let mut m = [0.0f32; 9];
    m[0] = (x * x) * t + c; // @Studio..fd8
    m[1] = yxt - zs; // @Studio..ff3
    m[3] = zs + yxt; // @Studio
    m[2] = ys + zxt; // @Studio
    m[4] = (y * y) * t + c; // @Studio..011
    m[8] = (z * z) * t + c; // @Studio..01b
    m[6] = zxt - ys; // @Studio
    m[5] = zyt - xs; // @Studio..031
    m[7] = xs + zyt; // @Studio
    m
}

/// Studio(R, w): rotation vector → CoordinateFrame rotation, f32 (series below |w|² < 0.2).
pub fn rotvec_matrix(w: [f32; 3]) -> [f32; 9] {
    let (x, y, z) = (w[0], w[1], w[2]);
    let xx = x * x; // xmm12
    let n2 = (y * y + xx) + z * z; // @Studio..0d8
    if SERIES_F > n2 {
        // @Studio..1e2
        let a = ONE_F - (C_1_6 - (C_1_120 - n2 * C_1_5040) * n2) * n2; // sinθ/θ
        let b = HALF_F - (C_1_24 - (C_1_720 - n2 * C_1_40320) * n2) * n2; // (1−cosθ)/θ²
        let ya = y * a; // xmm11
        let za = z * a; // xmm10
        let xa = x * a; // xmm9
        let xy = (x * y) * b; // xmm3
        let zx = (z * x) * b; // xmm5
        let zy = (z * y) * b; // xmm4
        let mut m = [0.0f32; 9];
        m[1] = xy - za; // @Studio
        m[2] = ya + zx; // @Studio
        m[3] = za + xy; // @Studio
        m[0] = (xx - n2) * b + ONE_F; // @Studio, @Studio..1c3
        m[6] = zx - ya; // @Studio
        m[4] = (y * y - n2) * b + ONE_F; // @Studio..1f9
        m[5] = zy - xa; // @Studio
        m[7] = xa + zy; // @Studio
        m[8] = (z * z - n2) * b + ONE_F; // @Studio..230
        return m;
    }
    // @Studio..296: normalise, then Studio with θ = |w|
    let n = n2.sqrt();
    let inv = ONE_F / n;
    axis_angle_matrix([x * inv, y * inv, z * inv], n)
}

// ---- the record --------------------------------------------------------------------------------

/// Studio(out, pv, tol).
pub fn encode(pv: &Pv, tol: f32) -> Packed {
    // @Studio..60a: position as f64, 6 exponent bits
    let p = encode_vec([pv.p[0] as f64, pv.p[1] as f64, pv.p[2] as f64], 6);
    let (level, code);
    if is_identity(&pv.r) {
        // @Studio..64d
        level = 0u8;
        code = 0u64;
    } else {
        let t = lattice(); // @Studio..663
        // @Studio..6bc: first k with 0.01f >= tol·T[k], else the last entry
        let mut k = t.len() - 1;
        for (i, err) in LEVEL_ERR.iter().enumerate().take(t.len()) {
            if CENTI_F >= tol * *err {
                k = i;
                break;
            }
        }
        let e = &t[k];
        code = encode_rot(e, &pv.r); // @Studio
        level = (((e.e1.wrapping_add(e.e0)) >> 3) as u8).wrapping_sub(3); // @Studio..6d7
    }
    // @Studio..721 v, @Studio..76a w: 4 exponent bits
    let v = encode_vec([pv.v[0] as f64, pv.v[1] as f64, pv.v[2] as f64], 4);
    let w = encode_vec([pv.w[0] as f64, pv.w[1] as f64, pv.w[2] as f64], 4);
    Packed { p, level, code, v, w }
}

/// Studio(pk, out).
pub fn decode(pk: &Packed) -> Pv {
    let p = decode_vec(&pk.p); // Studio
    let r = if pk.level == 0 {
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] // identity
    } else {
        // Studio(table[level − 1], code) = Studio then Studio
        let e = &lattice()[pk.level as usize - 1];
        rotvec_matrix(decode_rotvec(e, pk.code))
    };
    let v = decode_vec(&pk.v);
    let w = decode_vec(&pk.w);
    Pv {
        r,
        p: [p[0] as f32, p[1] as f32, p[2] as f32],
        v: [v[0] as f32, v[1] as f32, v[2] as f32],
        w: [w[0] as f32, w[1] as f32, w[2] as f32],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident_pv() -> Pv {
        Pv { r: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], p: [0.0; 3], v: [0.0; 3], w: [0.0; 3] }
    }

    /// Tiny deterministic generator (no deps).
    struct Lcg(u64);
    impl Lcg {
        fn f(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
    }

    fn rot_from(axis: [f64; 3], ang: f64) -> [f32; 9] {
        let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        let (x, y, z) = (axis[0] / n, axis[1] / n, axis[2] / n);
        let (s, c) = ang.sin_cos();
        let t = 1.0 - c;
        [
            (t * x * x + c) as f32,
            (t * x * y - s * z) as f32,
            (t * x * z + s * y) as f32,
            (t * x * y + s * z) as f32,
            (t * y * y + c) as f32,
            (t * y * z - s * x) as f32,
            (t * x * z - s * y) as f32,
            (t * y * z + s * x) as f32,
            (t * z * z + c) as f32,
        ]
    }

    /// ≈ rotation angle between A and B for small differences: ‖A − B‖_F / √2.
    fn rot_err(a: &[f32; 9], b: &[f32; 9]) -> f64 {
        let s: f64 = a.iter().zip(b.iter()).map(|(x, y)| ((*x as f64) - (*y as f64)).powi(2)).sum();
        (s / 2.0).sqrt()
    }

    fn ortho_err(r: &[f32; 9]) -> f64 {
        let mut e = 0.0f64;
        for i in 0..3 {
            for j in 0..3 {
                let mut d = 0.0f64;
                for k in 0..3 {
                    d += r[i * 3 + k] as f64 * r[j * 3 + k] as f64;
                }
                e = e.max((d - if i == j { 1.0 } else { 0.0 }).abs());
            }
        }
        e
    }

    #[test]
    fn identity_is_exact() {
        let pv = ident_pv();
        let pk = encode(&pv, SOLVER_TOL);
        assert_eq!(pk.level, 0);
        assert_eq!(pk.code, 0);
        assert_eq!(roundtrip(&pv), pv);
    }

    #[test]
    fn level_is_entry_1() {
        let t = lattice();
        let r = rot_from([0.3, -0.5, 0.8], 0.7);
        let pv = Pv { r, ..ident_pv() };
        let pk = encode(&pv, SOLVER_TOL);
        assert_eq!(pk.level as u32, ((t[1].e0 + t[1].e1) >> 3) - 3);
        assert_eq!(pk.level, 2);
    }

    #[test]
    fn table_sizes() {
        for (k, e) in lattice().iter().enumerate() {
            let total = *e.off.last().unwrap();
            println!(
                "entry {k}: e0={} e1={} rings={} count={} d={:.6e} dpolar={:.6e} off.len={} total={} 2^e1={} level={}",
                e.e0, e.e1, e.rings, e.count, e.d, e.dpolar, e.off.len(), total, 1u64 << e.e1,
                ((e.e0 + e.e1) >> 3) - 3
            );
            assert_eq!(e.off.len() as u64, e.rings + 1);
            assert_eq!(total, e.count);
            assert!(total <= 1u64 << e.e1);
            assert_eq!(e.off[0], 0);
            assert_eq!(e.off[1], 1);
            assert_eq!(((e.e0 + e.e1) >> 3) - 3, k as u32 + 1);
        }
    }

    /// The table depends on libm only via ceil(): every ring's (π/2)/asin(..) must sit well
    /// away from an integer so that UCRT and std agree.
    #[test]
    fn table_margins() {
        for e in lattice().iter() {
            let dp = e.dpolar;
            let h = dp * HALF_D;
            let ca = h.cos();
            let sb = ((e.d + h) * HALF_D).sin();
            let sc = ((e.d - h) * HALF_D).sin();
            let k = sc * (sb + sb);
            let mut worst = 1.0f64;
            let mut i = 1.0;
            while (e.rings as f64) > i {
                let mut x = i * dp;
                x += x;
                x += h;
                let r = HALF_PI_D / (k / (ca - x.cos())).sqrt().asin();
                worst = worst.min((r - r.round()).abs());
                i += 1.0;
            }
            println!("e0={} worst distance of a ring count from an integer: {worst:.3e}", e.e0);
            assert!(worst > 1e-9);
        }
    }

    #[test]
    fn random_rotations() {
        let mut g = Lcg(12345);
        let mut worst_o = 0.0f64;
        let mut worst_e = 0.0f64;
        for _ in 0..20000 {
            let axis = [g.f() * 2.0 - 1.0, g.f() * 2.0 - 1.0, g.f() * 2.0 - 1.0];
            let ang = g.f() * std::f64::consts::PI;
            let r = rot_from(axis, ang);
            let pv = Pv { r, p: [g.f() as f32 * 50.0, 3.0, -7.25], v: [1.0, 2.0, 3.0], w: [0.1, 0.0, -0.2] };
            let out = roundtrip(&pv);
            worst_o = worst_o.max(ortho_err(&out.r));
            worst_e = worst_e.max(rot_err(&r, &out.r));
            // idempotent: a decoded pose is a fixed point
            let again = roundtrip(&out);
            assert_eq!(again.p, out.p);
            assert_eq!(again.v, out.v);
            assert_eq!(again.w, out.w);
        }
        println!("worst orthonormality {worst_o:.3e}, worst rotation error {worst_e:.3e} rad");
        assert!(worst_o < 2e-6); // f32 Rodrigues at large angles
        assert!(worst_e < 1.5e-3);
    }

    #[test]
    fn rotation_fixed_points() {
        let mut g = Lcg(777);
        let (mut same, mut n) = (0, 0);
        let mut worst = 0.0f64;
        for _ in 0..5000 {
            let axis = [g.f() * 2.0 - 1.0, g.f() * 2.0 - 1.0, g.f() * 2.0 - 1.0];
            let r = rot_from(axis, g.f() * 3.0);
            let a = roundtrip(&Pv { r, ..ident_pv() });
            let b = roundtrip(&a);
            n += 1;
            if a.r == b.r {
                same += 1;
            }
            worst = worst.max(rot_err(&a.r, &b.r));
        }
        println!("decoded rotations that are fixed points: {same}/{n}, worst re-encode drift {worst:.3e} rad");
        assert_eq!(same, n);
    }

    #[test]
    fn vector_quantiser() {
        let c = encode_vec([3.996582031, 0.0, 0.0], 6);
        assert_eq!((c.zero, c.e, c.bits), (0, 2, 12));
        let d = decode_vec(&c);
        assert_eq!(d[0], 2046.0 / 512.0);
        let z = encode_vec([1e-4, -2e-4, 0.0], 4);
        assert_eq!(z.zero, 1);
        assert_eq!(decode_vec(&z), [0.0; 3]);
    }

    fn rec_path(name: &str) -> Option<std::path::PathBuf> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("checks/humanoid-physics/ticks/solver/studio")
            .join(name);
        p.exists().then_some(p)
    }

    /// Studio's recordings: is each recorded pose a fixed point of roundtrip()?
    #[test]
    fn recordings_fixed_points() {
        for name in ["wedge_drop.r1.txt.gz", "hinge_free.r1.txt.gz", "stack3.r1.txt.gz"] {
            let Some(path) = rec_path(name) else {
                println!("{name}: not found, skipped");
                continue;
            };
            // (flate2, not `zcat`: Windows has none)
            let mut text = String::new();
            std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap()), &mut text)
                .expect("gunzip");
            let (mut rows, mut fx_all, mut fx_p, mut fx_r, mut fx_v, mut fx_w) = (0, 0, 0, 0, 0, 0);
            let (mut dp, mut dr) = (0.0f64, 0.0f64);
            for line in text.lines() {
                if line.starts_with('#') || line.trim().is_empty() {
                    continue;
                }
                let f: Vec<&str> = line.split_whitespace().collect();
                let x: Vec<f32> = f[2..].iter().map(|s| s.parse::<f32>().unwrap()).collect();
                let pv = Pv {
                    p: [x[0], x[1], x[2]],
                    r: [x[3], x[4], x[5], x[6], x[7], x[8], x[9], x[10], x[11]],
                    v: [x[12], x[13], x[14]],
                    w: [x[15], x[16], x[17]],
                };
                let o = roundtrip(&pv);
                rows += 1;
                fx_all += (o == pv) as u32;
                fx_p += (o.p == pv.p) as u32;
                fx_r += (o.r == pv.r) as u32;
                fx_v += (o.v == pv.v) as u32;
                fx_w += (o.w == pv.w) as u32;
                for i in 0..3 {
                    dp = dp.max((o.p[i] - pv.p[i]).abs() as f64);
                }
                dr = dr.max(rot_err(&o.r, &pv.r));
            }
            println!(
                "{name}: rows {rows}; fixed points: all {fx_all}, p {fx_p}, R {fx_r}, v {fx_v}, w {fx_w}; max |Δp| {dp:.3e}, max rot Δ {dr:.3e} rad"
            );
        }
    }
}
