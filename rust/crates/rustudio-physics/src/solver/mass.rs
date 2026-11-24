//! Part mass properties as Studio computes them (STEP §5b [L2]): volume, centre of mass and
//! inertia tensor per shape from the unit table Studio, scaled to the part's size by
//! Studio, times the density. f32 throughout, in the
//! binary's order. Meshes / unions (Studio, their physics geometry) are not here.

use glam::{Mat3, Vec3};

/// prim, the physics shape code (GEOM §4 table).
pub const BALL: u8 = 0;
pub const CYLINDER: u8 = 1;
pub const BLOCK: u8 = 2;
pub const WEDGE: u8 = 3;
pub const CORNER_WEDGE: u8 = 4;

/// One part's mass record (Studio out: CoM, mass, I about the CoM, part frame,
/// row-major).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartMass {
    pub mass: f32,
    pub com: Vec3,
    pub i: [f32; 9],
}

impl PartMass {
    pub fn inertia(&self) -> Mat3 {
        super::body::m3::to_glam(&self.i)
    }
}

/// Studio(shape): {V, c(3), I(9)} for the unit size (exact bit patterns).
fn unit(shape: u8) -> (f32, [f32; 3], [f32; 9]) {
    let f = f32::from_bits;
    match shape {
        // π/6, I = π/60 (diameter-1 sphere)
        BALL => (f(0x3f060a92), [0.0; 3], [f(0x3d567750), 0.0, 0.0, 0.0, f(0x3d567750), 0.0, 0.0, 0.0, f(0x3d567750)]),
        // π/4; I_xx = π/32, I_yy = I_zz = 0x3dea9280 (axis along x)
        CYLINDER => (f(0x3f490fdb), [0.0; 3], [f(0x3dc90fdb), 0.0, 0.0, 0.0, f(0x3dea9280), 0.0, 0.0, 0.0, f(0x3dea9280)]),
        BLOCK => (1.0, [0.0; 3], [f(0x3e2aaaab), 0.0, 0.0, 0.0, f(0x3e2aaaab), 0.0, 0.0, 0.0, f(0x3e2aaaab)]),
        // V 1/2, c (0, −1/6, 1/6), I = [1/18, 0, 0; 0, 5/72, −1/72; 0, −1/72, 5/72]
        WEDGE => (
            0.5,
            [0.0, f(0xbe2aaaab), f(0x3e2aaaab)],
            [f(0x3d638e39), 0.0, 0.0, 0.0, f(0x3d8e38e4), f(0xbc638e39), 0.0, f(0xbc638e39), f(0x3d8e38e4)],
        ),
        // V 1/3, c (1/8, −1/4, −1/8)
        CORNER_WEDGE => (
            f(0x3eaaaaab),
            [0.125, -0.25, -0.125],
            [
                f(0x3d044444),
                f(0xbbcccccd),
                f(0x3b4ccccd),
                f(0xbbcccccd),
                f(0x3d222222),
                f(0x3bcccccd),
                f(0x3b4ccccd),
                f(0x3bcccccd),
                f(0x3d044444),
            ],
        ),
        // any other code: V 1, I = identity
        _ => (1.0, [0.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
    }
}

/// Studio (volume, centroid, unit-density inertia) and Studio's single-part
/// case (mass = density·V, I = density·I).
pub fn part(shape: u8, size: Vec3, density: f32) -> PartMass {
    // each axis: 0.05 < s ? s : 0.05 (.rdata Studio..)
    let cl = |s: f32| if 0.05 < s { s } else { 0.05 };
    let (sx, sy, sz) = (cl(size.x), cl(size.y), cl(size.z));
    let (v, c, i) = unit(shape);
    let (v, c, i) = if sx == 1.0 && sy == 1.0 && sz == 1.0 {
        (v, c, i)
    } else {
        let s = match shape {
            BALL => {
                let m = if sz <= sy { sz } else { sy };
                let m = if m <= sx { m } else { sx };
                [m, m, m]
            }
            CYLINDER => {
                let m = if sz <= sy { sz } else { sy };
                [sx, m, m]
            }
            _ => [sx, sy, sz],
        };
        scale(v, c, i, s)
    };
    let mut di = [0.0f32; 9];
    for k in 0..9 {
        di[k] = density * i[k];
    }
    PartMass { mass: density * v, com: Vec3::from(c), i: di }
}

/// Studio(unit, s, withInertia = 1) (asm order).
fn scale(v: f32, c: [f32; 3], i: [f32; 9], s: [f32; 3]) -> (f32, [f32; 3], [f32; 9]) {
    let [sx, sy, sz] = s;
    let vol = (sx * sy) * sz;
    let h = 0.5f32; // .rdata Studio
    let tr = (i[4] * h + i[0] * h) + i[8] * h;
    let qx = ((tr - i[0]) * sx) * sx;
    let qy = ((tr - i[4]) * sy) * sy;
    let qz = ((tr - i[8]) * sz) * sz;
    let i01 = (sx * i[1]) * sy;
    let i02 = (sx * i[2]) * sz;
    let i12 = (sy * i[5]) * sz;
    let o = [qz + qy, i01, i02, i01, qz + qx, i12, i02, i12, qy + qx];
    // Studio(I, vol): every entry · vol
    let mut r = [0.0f32; 9];
    for k in 0..9 {
        r[k] = o[k] * vol;
    }
    (vol * v, [sx * c[0], sy * c[1], sz * c[2]], r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lab STEP-E (Studio 0.747, Plastic 0.7, gravity 0): mass, CoM and ω = I⁻¹·J after one
    /// tick for a unit angular impulse (the tick's gyroscopic change is in the lab's numbers
    /// for asymmetric parts, so those are compared loosely).
    #[test]
    fn step_e() {
        let rho = 0.7;
        let check = |shape: u8, size: Vec3, mass: f32, com: Vec3, w: [f32; 3], tol: f32| {
            let p = part(shape, size, rho);
            assert!((p.mass - mass).abs() <= mass * 1e-6, "mass {shape} {size}: {} vs {mass}", p.mass);
            assert!((p.com - com).abs().max_element() <= 2.5e-4, "com {shape} {size}: {} vs {com}", p.com);
            let inv = p.inertia().inverse();
            for k in 0..3 {
                let got = inv.col(k)[k];
                assert!((got - w[k]).abs() <= w[k].abs() * tol, "I⁻¹ {shape} {size} axis {k}: {got} vs {}", w[k]);
            }
        };
        let one = Vec3::ONE;
        let s235 = Vec3::new(2.0, 3.0, 5.0);
        check(BALL, one, 0.366519153, Vec3::ZERO, [27.2837048, 27.2837067, 27.2837086], 1e-6);
        check(BLOCK, one, 0.699999988, Vec3::ZERO, [8.5714283, 8.57142735, 8.5714283], 1e-6);
        check(CYLINDER, one, 0.5497787, Vec3::ZERO, [14.5513096, 12.4725494, 12.478158], 2e-3);
        check(WEDGE, one, 0.349999994, Vec3::new(0.0, -0.166656, 0.166504), [25.7142849, 21.43, 21.43], 0.05);
        check(CORNER_WEDGE, one, 0.233333334, Vec3::new(0.125, -0.25, -0.125), [46.4363976, 41.7831154, 44.421257], 0.1);
        check(BALL, s235, 2.93215322, Vec3::ZERO, [0.852615774, 0.852615833, 0.852615714], 1e-6);
        check(BLOCK, s235, 21.0, Vec3::ZERO, [0.0168067254, 0.0197044387, 0.0439560562], 1e-5);
        check(CYLINDER, s235, 9.89601707, Vec3::ZERO, [0.0898228809, 0.112800822, 0.112800814], 1e-5);
        check(BLOCK, Vec3::new(4.0, 0.01, 4.0), 0.560000002, Vec3::ZERO, [1.3390764, 0.669663727, 1.33905578], 1e-4);
        // the wedge's products of inertia: I_yz = −ρ/72 for the unit wedge
        let w = part(WEDGE, one, rho);
        assert_eq!(w.i[5], rho * f32::from_bits(0xbc638e39));
        assert_eq!(w.i[1], 0.0);
    }
}
