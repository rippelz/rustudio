//! The Terrain Editor's Generate, Import and Sea Level, as Studio's
//! TerrainEditor plugin does them (BuiltInPlugins/TerrainEditor.rbxm):
//! Src/Util/Operations/GenerateOperation (onStart / onStep),
//! Src/Util/Generation/{ComputeMasterSeed, NoisySeed, Perlin,
//! ProcessPerlin, ProcessVoxel, Filter, Fractalize,
//! FindBiomeTransitionValue}, Src/Util/BiomeGenerators/* and
//! Src/Tools/RegionSeaLevel. `math.noise` is Luau's float32 improved
//! Perlin with FixMathNoisePrecision off (as Studio runs it: checked in
//! the offline Studio), `math.random` Luau's PCG32.
//!
//! Import's pixel → height and colour → material happen inside Studio's
//! C++ HeightmapImporterService, which the plugin only calls: here the
//! red channel (bilinear) maps linearly from the box's bottom to its top,
//! and a colormap pixel takes the material whose Studio colour is
//! nearest (the colours Roblox documents for colormaps).

use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use rustudio_core::terrain::{Voxels, WATER};

// SmoothGrid material indices used here.
const ROCK: u8 = 8;
const GRASS: u8 = 2;
const SAND: u8 = 6;
const SLATE: u8 = 3;
const MUD: u8 = 12;
const GROUND: u8 = 14;
const SNOW: u8 = 10;
const SANDSTONE: u8 = 11;
const GLACIER: u8 = 9;
const CRACKED_LAVA: u8 = 15;
const BASALT: u8 = 13;
const AIR: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    Arctic,
    Dunes,
    Canyons,
    Lavascape,
    Water,
    Mountains,
    Hills,
    Plains,
    Marsh,
}

impl Biome {
    /// The order Studio lists them (pairs(Types.Biome)), which is also the
    /// order the enabled ones are numbered in for picking a cell's biome.
    pub const ALL: [Biome; 9] = [
        Biome::Arctic,
        Biome::Dunes,
        Biome::Canyons,
        Biome::Lavascape,
        Biome::Water,
        Biome::Mountains,
        Biome::Hills,
        Biome::Plains,
        Biome::Marsh,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Biome::Arctic => "Arctic",
            Biome::Dunes => "Dunes",
            Biome::Canyons => "Canyons",
            Biome::Lavascape => "Lavascape",
            Biome::Water => "Water",
            Biome::Mountains => "Mountains",
            Biome::Hills => "Hills",
            Biome::Plains => "Plains",
            Biome::Marsh => "Marsh",
        }
    }
}

/// Mountains, Hills and Plains (Tools.RegionGenerate's defaults).
pub const DEFAULT_BIOMES: [bool; 9] = [false, false, false, false, false, true, true, true, false];
/// Biome Size must be above 0.
pub const BIOME_SIZE_MIN: f32 = 1.0;
pub const BIOME_SIZE_MAX: f32 = 1_000_000.0;

pub struct Options {
    pub biomes: Vec<Biome>,
    pub blending: f32,
    pub caves: bool,
    pub biome_size: f32,
    pub seed: i64,
}

// ------------------------------------------------------------ Luau math

const PERLIN_HASH: [u8; 257] = [
    151, 160, 137, 91, 90, 15, 131, 13, 201, 95, 96, 53, 194, 233, 7, 225, 140, 36, 103, 30, 69, 142, 8, 99, 37, 240, 21, 10, 23,
    190, 6, 148, 247, 120, 234, 75, 0, 26, 197, 62, 94, 252, 219, 203, 117, 35, 11, 32, 57, 177, 33, 88, 237, 149, 56, 87, 174, 20,
    125, 136, 171, 168, 68, 175, 74, 165, 71, 134, 139, 48, 27, 166, 77, 146, 158, 231, 83, 111, 229, 122, 60, 211, 133, 230, 220, 105, 92,
    41, 55, 46, 245, 40, 244, 102, 143, 54, 65, 25, 63, 161, 1, 216, 80, 73, 209, 76, 132, 187, 208, 89, 18, 169, 200, 196, 135, 130,
    116, 188, 159, 86, 164, 100, 109, 198, 173, 186, 3, 64, 52, 217, 226, 250, 124, 123, 5, 202, 38, 147, 118, 126, 255, 82, 85, 212, 207,
    206, 59, 227, 47, 16, 58, 17, 182, 189, 28, 42, 223, 183, 170, 213, 119, 248, 152, 2, 44, 154, 163, 70, 221, 153, 101, 155, 167, 43,
    172, 9, 129, 22, 39, 253, 19, 98, 108, 110, 79, 113, 224, 232, 178, 185, 112, 104, 218, 246, 97, 228, 251, 34, 242, 193, 238, 210, 144,
    12, 191, 179, 162, 241, 81, 51, 145, 235, 249, 14, 239, 107, 49, 192, 214, 31, 181, 199, 106, 157, 184, 84, 204, 176, 115, 121, 50, 45,
    127, 4, 150, 254, 138, 236, 205, 93, 222, 114, 67, 29, 24, 72, 243, 141, 128, 195, 78, 66, 215, 61, 156, 180, 151,
];

const PERLIN_GRAD: [[f32; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}

fn grad(hash: u8, x: f32, y: f32, z: f32) -> f32 {
    let g = PERLIN_GRAD[(hash & 15) as usize];
    g[0] * x + g[1] * y + g[2] * z
}

/// Luau's math.noise (lmathlib.cpp perlin), float32 throughout.
pub fn noise(x: f64, y: f64, z: f64) -> f64 {
    let (x, y, z) = (x as f32, y as f32, z as f32);
    let (xflr, yflr, zflr) = (x.floor(), y.floor(), z.floor());
    let (xi, yi, zi) = ((xflr as i32 & 255) as usize, (yflr as i32 & 255) as usize, (zflr as i32 & 255) as usize);
    let (xf, yf, zf) = (x - xflr, y - yflr, z - zflr);
    let (u, v, w) = (fade(xf), fade(yf), fade(zf));
    let p = &PERLIN_HASH;
    let a = (p[xi] as usize + yi) & 255;
    let aa = (p[a] as usize + zi) & 255;
    let ab = (p[a + 1] as usize + zi) & 255;
    let b = (p[xi + 1] as usize + yi) & 255;
    let ba = (p[b] as usize + zi) & 255;
    let bb = (p[b + 1] as usize + zi) & 255;
    let la = lerp(u, grad(p[aa], xf, yf, zf), grad(p[ba], xf - 1.0, yf, zf));
    let lb = lerp(u, grad(p[ab], xf, yf - 1.0, zf), grad(p[bb], xf - 1.0, yf - 1.0, zf));
    let la1 = lerp(u, grad(p[aa + 1], xf, yf, zf - 1.0), grad(p[ba + 1], xf - 1.0, yf, zf - 1.0));
    let lb1 = lerp(u, grad(p[ab + 1], xf, yf - 1.0, zf - 1.0), grad(p[bb + 1], xf - 1.0, yf - 1.0, zf - 1.0));
    lerp(w, lerp(v, la, lb), lerp(v, la1, lb1)) as f64
}

/// Luau's PCG32 (math.random's generator).
struct Pcg(u64);

impl Pcg {
    fn next(&mut self) -> u32 {
        let old = self.0;
        self.0 = old.wrapping_mul(6364136223846793005).wrapping_add(105 | 1);
        let x = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        x.rotate_right(rot)
    }

    /// math.randomseed(n).
    fn seeded(n: i32) -> Pcg {
        let mut p = Pcg(0);
        p.next();
        p.0 = p.0.wrapping_add(n as i64 as u64);
        p.next();
        p
    }

    /// math.random() with no arguments.
    fn real(&mut self) -> f64 {
        let lo = self.next() as u64;
        let hi = self.next() as u64;
        (lo | (hi << 32)) as f64 * (2.0f64).powi(-64)
    }
}

/// Util.Generation.ComputeMasterSeed over tostring(seed).
pub fn master_seed(s: &str) -> f64 {
    let mut seed: f64 = 0.0;
    for ch in s.chars() {
        seed = match ch.to_digit(10) {
            Some(d) => (seed + 6.0) * (d as f64 + 5.0),
            None => (seed + 7.0) * (ch as u32 as f64 + 3.0),
        };
        seed = seed.rem_euclid(61803.0);
    }
    seed
}

fn noise_table() -> &'static [f64; 999] {
    use std::sync::OnceLock;
    static T: OnceLock<[f64; 999]> = OnceLock::new();
    T.get_or_init(|| {
        let mut p = Pcg::seeded(6180339);
        std::array::from_fn(|_| p.real())
    })
}

#[derive(Clone, Copy)]
struct V3 {
    x: f64,
    y: f64,
    z: f64,
}

fn v3(x: f64, y: f64, z: f64) -> V3 {
    V3 { x, y, z }
}

/// Perlin(seed) / NoisySeed(seed).
struct Gen {
    seed: f64,
}

impl Gen {
    /// getPerlin(pos, a, b): math.noise(X/b + a·17 + seed, Y/b − seed, Z/b − a²).
    fn perlin(&self, p: V3, a: f64, b: f64) -> f64 {
        noise(p.x / b + a * 17.0 + self.seed, p.y / b - self.seed, p.z / b - a * a)
    }

    /// PP(perlin): 0..1.
    fn pp(&self, p: V3, a: f64, b: f64) -> f64 {
        self.perlin(p, a, b) * 0.5 + 0.5
    }

    /// NoisySeed's getNoise: a hash of the position into the fixed table.
    fn hash(&self, v: V3) -> f64 {
        let s = self.seed;
        let n = (v.x + v.y + v.z + 7.0 + s) + (s - v.x) * (v.z + 7.0) + (7.0 - v.y) * (s + v.z);
        let t = noise_table();
        let mut idx = (n % 999.0).floor();
        if n < 0.0 {
            idx = (idx + 999.0 - 1.0).clamp(0.0, 998.0);
        }
        t[(idx as i64).clamp(0, 998) as usize]
    }
}

fn threshold(v: f64, t: f64, w: f64) -> f64 {
    if w != 0.0 {
        ((v - t) / w).clamp(0.0, 1.0)
    } else if v <= t {
        0.0
    } else {
        1.0
    }
}

fn ridge(v: f64) -> f64 {
    if v < 0.5 {
        v * 2.0
    } else {
        2.0 - v * 2.0
    }
}

fn ridge_flipped(v: f64) -> f64 {
    if v < 0.5 {
        1.0 - v * 2.0
    } else {
        v * 2.0 - 1.0
    }
}

const WL: f64 = 0.48;
const ST: f64 = 0.018;

/// One biome's (value, surface, fill) at `pos` (voxel units; Y the row)
/// with the noisy depth `hn` (Util.BiomeGenerators.*).
fn biome_value(g: &Gen, b: Biome, pos: V3, hn: f64, map_height: f64) -> (f64, u8, u8) {
    let (x, y, z) = (pos.x, pos.y, pos.z);
    let p0 = v3(x, 0.0, z);
    match b {
        Biome::Water => {
            let value = 0.36 + g.pp(pos, 2.0, 50.0) * 0.08;
            (value, if 1.0 - hn < 0.44 { SLATE } else { SAND }, ROCK)
        }
        Biome::Plains => {
            let warp = v3(x + g.pp(pos, 17.0, 40.0) * 25.0, 0.0, z + g.pp(pos, 19.0, 40.0) * 25.0);
            let river = ridge_flipped(g.pp(warp, 2.0, 200.0));
            let river_mask = threshold(river, 0.01, 0.0);
            let rocks = threshold(ridge_flipped(g.pp(p0, 101.0, 7.0)), 0.3, 0.7) * threshold(g.pp(p0, 102.0, 50.0), 0.6, 0.05);
            let value = 0.5 + g.pp(pos, 2.0, 100.0) * 0.02 + river * 0.05 + rocks * 0.05 + river_mask * 0.005;
            // MapHeight here is 0.5 / rows, as the plugin passes it.
            let hl = 1.0 - (y - 1.0) / (map_height - 1.0);
            let dd = hl * 0.5 + value * 0.5;
            let band = 0.5 - ST * 0.4 < dd && dd < 0.5 + ST * 0.4;
            let surf = if rocks > 0.0 {
                ROCK
            } else if !band {
                MUD
            } else if river_mask <= 0.0 {
                WATER
            } else if 1.0 - hn < WL - 0.01 {
                SAND
            } else {
                GRASS
            };
            (value, surf, if rocks > 0.0 { ROCK } else { SANDSTONE })
        }
        Biome::Hills => {
            // The plugin leaves X out of the warp (kept).
            let warp = v3(g.pp(pos, 17.0, 20.0) * 20.0, 0.0, z + g.pp(pos, 19.0, 20.0) * 20.0);
            let r = ridge_flipped(g.pp(warp, 2.0, 200.0)).powf(0.5);
            let n = g.pp(pos, 3.0, 60.0);
            let value = 0.48 + n * 0.05 + (0.05 + n * 0.1 + g.pp(pos, 4.0, 25.0) * 0.125) * r;
            let t = (1.0 - hn) * 0.9 + r * 0.1;
            let surf = if t < WL - 0.015 {
                MUD
            } else if t < WL {
                GROUND
            } else {
                GRASS
            };
            (value, surf, SLATE)
        }
        Biome::Mountains => {
            let warp = v3(x + g.pp(pos, 17.0, 20.0) * 20.0, 0.0, z + g.pp(pos, 19.0, 20.0) * 20.0);
            let river = ridge_flipped(g.pp(warp, 2.0, 200.0));
            let fp = v3(x, y / 20.0, z);
            let (mut sum, mut norm) = (0.0, 0.0);
            for i in 1..=8 {
                let amp = 0.65f64.powi(i - 1);
                norm += amp;
                let fi = i as f64;
                sum += ridge(g.pp(fp, fi + 100.0, (1.0 / fi) * 160.0)) * amp;
            }
            let f = sum / norm * 1.2;
            let value = -0.4 + f + river * 0.2;
            let h = 1.0 - hn;
            let surf = if hn < 0.275 {
                SNOW
            } else if hn < 0.35 {
                ROCK
            } else if hn < 0.4 {
                GROUND
            } else if h < WL {
                ROCK
            } else if h < WL + 0.01 {
                MUD
            } else if h < WL + 0.015 {
                GROUND
            } else {
                GRASS
            };
            (value, surf, ROCK)
        }
        Biome::Marsh => {
            let warp = v3(
                x + g.perlin(p0, 5.0, 7.0) * 10.0 + g.perlin(p0, 6.0, 30.0) * 50.0,
                0.0,
                z + g.perlin(p0, 9.0, 7.0) * 10.0 + g.perlin(p0, 10.0, 30.0) * 50.0,
            );
            let m = g.pp(warp, 2.0, 70.0);
            let mt = threshold(m, 0.65, 0.0);
            let value = WL - 0.04 + m * mt * 0.025 + g.pp(pos, 4.0, 100.0) * 0.035 + g.pp(pos, 3.0, 20.0) * 0.025;
            let h = 1.0 - hn;
            let surf = if mt >= 1.0 {
                GRASS
            } else if h < WL - 0.01 {
                MUD
            } else if h < WL + 0.01 {
                GROUND
            } else {
                GRASS
            };
            (value, surf, SLATE)
        }
        Biome::Dunes => {
            let off = g.pp(p0, 227.0, 20.0) * 24.0;
            let value = 0.4 + 0.1 * (ridge(g.pp(p0, 201.0, 40.0)) + ridge(g.pp(v3(x / 10.0 + off, 0.0, z + off), 200.0, 48.0)));
            (value, SAND, SANDSTONE)
        }
        Biome::Arctic => {
            let w = v3(x + g.perlin(p0, 5.0, 8.0) * 5.0, y / 8.0, z + g.perlin(p0, 9.0, 8.0) * 5.0);
            let r = ridge(g.pp(w, 2.0, 20.0));
            let s = g.pp(v3(x, y / 4.0, z), 436.0, 2.0);
            let cr = threshold(r, 0.8, 0.1);
            let k = g.pp(p0, 6.0, 74.0) - 0.5;
            let e = if k < 0.0 {
                if 1.0 + k * 0.5 < r {
                    -0.17
                } else {
                    0.0
                }
            } else {
                cr * 0.1 * s * k
            };
            let value = 0.55 + r * 0.05 * k + e + g.pp(p0, 123.0, 25.0) * 0.025;
            let surf = if 1.0 - hn < WL - 0.1 || (cr > 0.6 && k > 0.1 && s > 0.5) { GLACIER } else { SNOW };
            (value, surf, GLACIER)
        }
        Biome::Canyons => {
            const LUT: [u8; 10] = [ROCK, MUD, SAND, SAND, SANDSTONE, SANDSTONE, SANDSTONE, SANDSTONE, SANDSTONE, SANDSTONE];
            let big = ridge_flipped(g.pp(p0, 2.0, 200.0));
            let w = v3(x + g.perlin(p0, 5.0, 20.0) * 20.0, 0.0, z + g.perlin(p0, 9.0, 20.0) * 20.0);
            let c = ridge_flipped(g.pp(w, 2.0, 200.0));
            let a1 = threshold(c, 0.0, 0.05);
            let a2 = threshold(c, 0.125, 0.0);
            let a3 = threshold(big, 0.33, 0.12);
            let a4 = threshold(c, 0.49, 0.0);
            let value = 0.42
                + g.pp(pos, 2.0, 70.0) * 0.05
                + big * 0.05
                + a1 * 0.04
                + threshold(c, 0.05, 0.0) * 0.08
                + threshold(c, 0.05, 0.075) * 0.04
                + a2 * 0.01
                + threshold(c, 0.0575, 0.2725) * 0.01
                + a3 * 0.06
                + threshold(c, 0.45, 0.0) * 0.14
                + threshold(c, 0.45, 0.04) * 0.025
                + a4 * 0.02;
            let surf = if 1.0 - hn < WL + 0.015 || (0.0 < a1 && a1 < 1.0) { SAND } else { SANDSTONE };
            let li = ((1.0 - g.hash(v3(1.0, y, 2.0))) * 10.0).ceil() as i64;
            (value, surf, LUT[(li - 1).clamp(0, 9) as usize])
        }
        Biome::Lavascape => {
            let q = v3(x, y * 0.25, z);
            let wx = x + g.perlin(q, 21.0, 8.0) * 5.0;
            let wy = y + g.perlin(q, 22.0, 8.0) * 5.0;
            let wz = z + g.perlin(q, 23.0, 8.0) * 5.0;
            let w1 = v3(wx, wy, wz);
            let w2 = v3(wx + g.perlin(pos, 22.0, 30.0) * 30.0, wy, wz + g.perlin(pos, 24.0, 30.0) * 30.0);
            let r1 = ridge(g.pp(w2, 2.0, 120.0));
            let r2 = ridge(g.pp(w1, 3.0, 40.0)) * (0.75 + r1 * 0.25);
            let r3 = ridge(g.pp(w1, 4.0, 20.0)) * (0.75 + r2 * 0.25);
            let hills = threshold(g.pp(pos, 9.0, 40.0), 0.25, 0.5) * g.pp(pos, 10.0, 60.0);
            let lava = (1.0 - threshold(r1, 0.975, 0.0) - threshold(r2, 0.925, 0.0) - threshold(r3, 0.9, 0.0)).max(0.0);
            // CFrame.Angles(0.7, 0.7, 0) * w1 (Rx · Ry · Rz).
            let rot = glam::DMat3::from_rotation_x(0.7) * glam::DMat3::from_rotation_y(0.7);
            let rp = rot * glam::DVec3::new(w1.x, w1.y, w1.z);
            let rock = threshold(g.pp(v3(rp.x / 40.0, rp.y / 300.0, rp.z / 30.0), 123.0, 1.0), 0.6, 0.4);
            let value = WL + 0.02
                + lava * (0.5 + hills * 0.5) * 0.02
                + hills * 0.05
                + rock * 0.3
                + if WL + 0.01 < 1.0 - hn || rock > 0.0 { 0.04 } else { 0.0 };
            let m = if rock > 0.0 {
                ROCK
            } else if lava < 1.0 {
                CRACKED_LAVA
            } else {
                BASALT
            };
            let s2 = if m == CRACKED_LAVA && 1.0 - hn < WL { BASALT } else { m };
            (value, s2, m)
        }
    }
}

/// FindBiomeTransitionValue.
fn transition(b: Biome, weight: f64, value: f64, total: f64) -> f64 {
    match b {
        Biome::Arctic => if weight > 0.2 { value } else { 0.0 },
        Biome::Canyons => if weight > 0.7 { value } else { 0.0 },
        Biome::Mountains => {
            let w3 = weight.powi(3);
            total * (1.0 - w3) + value * w3
        }
        _ => total * (1.0 - weight) + value * weight,
    }
}

/// Everything one Generate shares between columns (GenerateOperation's
/// State plus ProcessVoxel's settings).
struct Ctx<'a> {
    g: Gen,
    biomes: &'a [Biome],
    blend: f64,
    blend_inv: f64,
    bs: f64,
    slice_y: f64,
    map_height: f64,
    caves: bool,
    lo: [i32; 3],
    size: [i32; 3],
}

impl<'a> Ctx<'a> {
    fn new(lo: [i32; 3], size: [i32; 3], o: &'a Options) -> Ctx<'a> {
        let slice_y = size[1] as f64;
        let blend = o.blending.clamp(0.0, 1.0) as f64;
        Ctx {
            g: Gen { seed: master_seed(&o.seed.to_string()) },
            biomes: &o.biomes,
            blend,
            blend_inv: 1.0 - blend,
            bs: o.biome_size.max(BIOME_SIZE_MIN) as f64,
            slice_y,
            map_height: 0.5 / slice_y,
            caves: o.caves,
            lo,
            size,
        }
    }

    /// Column (`xi`, `zi`) of the box, bottom up, appended to `out` until
    /// the column stopped (GenerateOperation.onStep's inner loop with
    /// ProcessVoxel). Every row from `out`'s new length up is Air.
    fn column(&self, xi: i32, zi: i32, out: &mut Vec<(u8, f32)>) {
        let (g, biomes, blend, blend_inv, bs) = (&self.g, self.biomes, self.blend, self.blend_inv, self.bs);
        let (slice_y, map_height) = (self.slice_y, self.map_height);
        let (wx, wz) = ((self.lo[0] + xi) as f64, (self.lo[2] + zi) as f64);
        let wp = v3(wx, 0.0, wz);
        // Jittered-grid biome cells around the column.
        let bx = wx / bs + g.pp(wp, 233.0, bs * 0.3) * 0.25 + g.pp(wp, 235.0, bs * 0.05) * 0.075;
        let bz = wz / bs + g.pp(wp, 234.0, bs * 0.3) * 0.25 + g.pp(wp, 236.0, bs * 0.05) * 0.075;
        let mut min_d = 1e7f64;
        let mut points = [(0.0f64, 0.0f64); 9];
        for i in -1..=1i32 {
            for j in -1..=1i32 {
                let cx = (bx + i as f64 + 0.5).floor();
                let cz = (bz + j as f64 + 0.5).floor();
                let px = cx + (g.hash(v3(cx, cz, 53.0)) - 0.5) * 0.75;
                let pz = cz + (g.hash(v3(cx, cz, 73.0)) - 0.5) * 0.75;
                let d2 = (px - bx).powi(2) + (pz - bz).powi(2);
                if d2 < min_d {
                    min_d = d2;
                }
                points[((i + 1) + (j + 1) * 3) as usize] = (d2, g.hash(v3(cx, cz, 0.0)));
            }
        }
        let mut weights: Vec<(Biome, f64)> = Vec::with_capacity(4);
        let mut total = 0.0;
        for (d2, bn) in points {
            let mut w = if d2 == min_d { 1.0 } else { (min_d.sqrt() / d2.sqrt() - blend_inv) / blend };
            if w > 0.0 {
                w = w.powf(2.1);
                total += w;
                let k = ((biomes.len() as f64 * (1.0 - bn)).ceil() as usize).clamp(1, biomes.len());
                let b = biomes[k - 1];
                match weights.iter_mut().find(|e| e.0 == b) {
                    Some(e) => e.1 += w,
                    None => weights.push((b, w)),
                }
            }
        }
        let mut has_arctic = false;
        for e in weights.iter_mut() {
            e.1 /= total;
            has_arctic |= e.0 == Biome::Arctic;
        }
        let mut vals: Vec<(f64, u8, u8)> = Vec::with_capacity(weights.len());
        for yi in 1..=self.size[1] {
            let y = yi as f64;
            let pos = v3(wx, y, wz);
            let h = if slice_y > 1.0 { 1.0 - (y - 1.0) / (slice_y - 1.0) } else { 1.0 };
            let hn = h * 0.9 + 0.1 * g.pp(pos, 107.0, 15.0);
            let (mut value, mut surf, mut fill) = (0.0, CRACKED_LAVA, ROCK);
            if h > 0.65 || h < 0.1 {
                value = 0.5;
            } else if biomes.len() == 1 {
                (value, surf, fill) = biome_value(g, biomes[0], pos, hn, map_height);
            } else {
                vals.clear();
                let mut sum = 0.0;
                for (b, wt) in &weights {
                    let r = biome_value(g, *b, pos, hn, map_height);
                    sum += r.0 * wt;
                    vals.push(r);
                }
                for ((b, wt), r) in weights.iter().zip(vals.iter()) {
                    let t = transition(*b, *wt, r.0, sum);
                    if value < t {
                        (value, surf, fill) = (t, r.1, r.2);
                    }
                }
            }
            let d = h * 0.5 + value * 0.5;
            let mut is_surface = 0.5 - ST < d && d < 0.5 + ST;
            let mut cave = 0.0;
            let caves_here = !has_arctic || h > 0.65;
            let not_shallow = !(is_surface && (1.0 - h) < WL + 0.005);
            let not_high = !(is_surface && (1.0 - h) > WL + 0.58);
            if self.caves && caves_here && not_shallow && not_high {
                let a = threshold(ridge(g.pp(pos, 4.0, 30.0)), 0.84, 0.01);
                let b = threshold(ridge(g.pp(pos, 5.0, 30.0)), 0.84, 0.01);
                let c = threshold(ridge(g.pp(pos, 6.0, 30.0)), 0.84, 0.01);
                let m = if is_surface { threshold(g.pp(v3(pos.x, 0.0, pos.z), 143.0, 62.0), 0.35, 0.0) } else { 0.0 };
                cave = (a * b * c - m).clamp(0.0, 1.0);
            }
            let mut occ = threshold(d - cave, 0.5, map_height);
            if (1.0 - h) < WL && d <= 0.5 && occ <= 0.0 {
                occ = 1.0;
                surf = WATER;
                fill = WATER;
                is_surface = true;
            }
            let out_occ = if yi == 1 { 1.0 } else { occ };
            let mat = if yi == 1 {
                CRACKED_LAVA
            } else if occ <= 0.0 {
                AIR
            } else if is_surface {
                surf
            } else {
                fill
            };
            if is_surface && (out_occ <= 0.0 || mat == AIR) {
                return;
            }
            out.push((mat, out_occ as f32));
        }
    }
}

/// Columns `x0 .. x0 + nx` of the box: per column (z fastest) how many
/// rows it filled, and those rows' (material, occupancy) one after another.
struct Slab {
    x0: i32,
    nx: i32,
    rows: Vec<u32>,
    cells: Vec<(u8, f32)>,
}

/// Columns per unit of work handed to a thread.
const SLAB_COLUMNS: i32 = 4;

/// Generate over the voxel box `lo` .. `lo + size` (GenerateOperation):
/// every voxel of the box is written (Air above the ground), the bottom
/// row is CrackedLava, water fills below 0.48 of the box's height.
pub fn generate(v: &mut Voxels, lo: [i32; 3], size: [i32; 3], o: &Options) {
    generate_with_progress(v, lo, size, o, None);
}

/// `generate`, the columns spread over the CPU's threads; `done` (when
/// given) counts the columns written so far, out of `size.x · size.z`.
pub fn generate_with_progress(v: &mut Voxels, lo: [i32; 3], size: [i32; 3], o: &Options, done: Option<&AtomicUsize>) {
    if o.biomes.is_empty() || size.iter().any(|s| *s < 1) {
        return;
    }
    let ctx = Ctx::new(lo, size, o);
    let jobs = (size[0] + SLAB_COLUMNS - 1) / SLAB_COLUMNS;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 32).min(jobs as usize);
    let next = AtomicI32::new(0);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Slab>(threads * 2);
    std::thread::scope(|s| {
        for _ in 0..threads {
            let (tx, ctx, next) = (tx.clone(), &ctx, &next);
            s.spawn(move || loop {
                let j = next.fetch_add(1, Ordering::Relaxed);
                if j >= jobs {
                    break;
                }
                let x0 = j * SLAB_COLUMNS;
                let nx = SLAB_COLUMNS.min(size[0] - x0);
                let mut slab = Slab { x0, nx, rows: Vec::with_capacity((nx * size[2]) as usize), cells: Vec::new() };
                for xi in x0..x0 + nx {
                    for zi in 0..size[2] {
                        let before = slab.cells.len();
                        ctx.column(xi, zi, &mut slab.cells);
                        slab.rows.push((slab.cells.len() - before) as u32);
                    }
                }
                if tx.send(slab).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        // Air is only written over chunks that held terrain before (the
        // rest of the box is Air already); keys are voxel >> 5.
        let mut held = std::collections::HashSet::new();
        if v.count() > 0 {
            for cx in lo[0] >> 5..=(lo[0] + size[0] - 1) >> 5 {
                for cy in lo[1] >> 5..=(lo[1] + size[1] - 1) >> 5 {
                    for cz in lo[2] >> 5..=(lo[2] + size[2] - 1) >> 5 {
                        if v.chunk_has_voxels([cx << 5, cy << 5, cz << 5]) {
                            held.insert([cx, cy, cz]);
                        }
                    }
                }
            }
        }
        // Written here as the slabs come in, while the threads go on.
        let held = &held;
        v.set_many(rx.into_iter().flat_map(|slab| {
            if let Some(d) = done {
                d.fetch_add((slab.nx * size[2]) as usize, Ordering::Relaxed);
            }
            let Slab { x0, nx, rows, cells } = slab;
            let mut cells = cells.into_iter();
            let mut out = Vec::with_capacity(cells.len());
            let mut col = 0;
            for xi in x0..x0 + nx {
                for zi in 0..size[2] {
                    let n = rows[col] as i32;
                    col += 1;
                    let (x, z) = (lo[0] + xi, lo[2] + zi);
                    for yi in 0..n {
                        let (m, occ) = cells.next().expect("slab cells");
                        out.push(([x, lo[1] + yi, z], m, occ));
                    }
                    if !held.is_empty() {
                        for yi in n..size[1] {
                            let at = [x, lo[1] + yi, z];
                            if held.contains(&[x >> 5, at[1] >> 5, z >> 5]) {
                                out.push((at, AIR, 0.0));
                            }
                        }
                    }
                }
            }
            out
        }));
    });
}

// ------------------------------------------------------------ import

/// An RGBA8 image.
pub struct Image<'a> {
    pub w: u32,
    pub h: u32,
    pub rgba: &'a [u8],
}

impl Image<'_> {
    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y.min(self.h - 1) * self.w + x.min(self.w - 1)) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    /// The red channel at (u, v) in 0..1, bilinear.
    fn red(&self, u: f32, v: f32) -> f32 {
        let fx = (u * self.w as f32 - 0.5).clamp(0.0, (self.w - 1) as f32);
        let fy = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let r = |x: u32, y: u32| self.px(x, y)[0] as f32 / 255.0;
        let a = r(x0, y0) + (r(x0 + 1, y0) - r(x0, y0)) * tx;
        let b = r(x0, y0 + 1) + (r(x0 + 1, y0 + 1) - r(x0, y0 + 1)) * tx;
        a + (b - a) * ty
    }

    fn nearest(&self, u: f32, v: f32) -> [u8; 4] {
        self.px((u * self.w as f32) as u32, (v * self.h as f32) as u32)
    }
}

/// The colours a colormap picks materials by (Roblox's colormap table).
const COLORMAP: [(u8, [u8; 3]); 22] = [
    (16, [115, 123, 107]),
    (13, [30, 30, 37]),
    (5, [138, 86, 62]),
    (17, [132, 123, 90]),
    (4, [127, 102, 63]),
    (15, [232, 156, 74]),
    (9, [101, 176, 234]),
    (2, [106, 127, 63]),
    (14, [102, 92, 59]),
    (18, [129, 194, 224]),
    (19, [115, 132, 74]),
    (21, [206, 173, 148]),
    (12, [58, 46, 36]),
    (22, [148, 148, 140]),
    (8, [102, 108, 111]),
    (20, [198, 189, 181]),
    (6, [143, 126, 95]),
    (11, [137, 90, 71]),
    (3, [63, 127, 107]),
    (10, [195, 199, 218]),
    (1, [12, 84, 92]),
    (7, [139, 109, 79]),
];

/// The material whose colormap colour is nearest.
pub fn colormap_material(c: [u8; 3]) -> u8 {
    COLORMAP
        .iter()
        .min_by_key(|(_, k)| (0..3).map(|i| (c[i] as i32 - k[i] as i32).pow(2)).sum::<i32>())
        .map_or(GRASS, |e| e.0)
}

/// Import: the heightmap stretched over the box's X (width) and Z
/// (height); each column filled from the box's bottom to the height the
/// red channel gives (0 = bottom, 1 = top), the top voxel partly. The box
/// is cleared first.
pub fn import(v: &mut Voxels, lo: [i32; 3], size: [i32; 3], hm: &Image, cm: Option<&Image>, default_material: u8) {
    if hm.w == 0 || hm.h == 0 {
        return;
    }
    let mut writes = Vec::with_capacity((size[0] * size[1] * size[2]).max(0) as usize);
    for xi in 0..size[0] {
        for zi in 0..size[2] {
            let (u, w) = ((xi as f32 + 0.5) / size[0] as f32, (zi as f32 + 0.5) / size[2] as f32);
            let height = hm.red(u, w) * size[1] as f32;
            let mat = match cm {
                Some(c) if c.w > 0 && c.h > 0 => {
                    let p = c.nearest(u, w);
                    if p[3] < 128 {
                        default_material
                    } else {
                        colormap_material([p[0], p[1], p[2]])
                    }
                }
                _ => default_material,
            };
            for yi in 0..size[1] {
                let occ = (height - yi as f32).clamp(0.0, 1.0);
                let at = [lo[0] + xi, lo[1] + yi, lo[2] + zi];
                if occ > 0.0 {
                    writes.push((at, mat, occ));
                } else {
                    writes.push((at, AIR, 0.0));
                }
            }
        }
    }
    v.edit(writes);
}

/// Sea Level: Create replaces Air with Water in the box, Evaporate Water
/// with Air (FillOperation in Replace mode, as RegionSeaLevel sets it up).
pub fn sea_level(v: &mut Voxels, lo: [i32; 3], size: [i32; 3], evaporate: bool) {
    if evaporate {
        rustudio_core::terrain_region::replace_box(v, lo, size, WATER, AIR);
    } else {
        rustudio_core::terrain_region::replace_box(v, lo, size, AIR, WATER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luau_noise_and_random_match_studio() {
        // Values read from the offline Studio (math.noise; FixMathNoisePrecision off).
        assert!((noise(16777216.5, 0.3, 0.7) - 0.191513926).abs() < 1e-7);
        assert!((noise(0.5, 0.3, 0.7) - -0.368834108).abs() < 1e-7);
        assert!((noise(1.25, 2.5, 3.75) - -0.038363457).abs() < 1e-7);
        // math.randomseed(6180339); math.random(), math.random()
        let t = noise_table();
        assert!((t[0] - 0.920225847393).abs() < 1e-11);
        assert!((t[1] - 0.611466174423).abs() < 1e-11);
    }

    #[test]
    fn master_seed_vectors() {
        assert_eq!(master_seed("618033988"), 52668.0);
        assert_eq!(master_seed("1"), 36.0);
        assert_eq!(master_seed("12345"), 31191.0);
    }

    fn opts() -> Options {
        Options {
            biomes: vec![Biome::Mountains, Biome::Hills, Biome::Plains],
            blending: 0.25,
            caves: false,
            biome_size: 100.0,
            seed: 618033988,
        }
    }

    /// Studio's Generate with the defaults on a 128 × 64 × 128 box at the
    /// origin (offline Studio): a near-flat slab whose top voxel (row 8, y −4..0)
    /// is 0.67..0.86 full, Sandstone on Rock on a CrackedLava floor.
    #[test]
    fn generate_matches_studio_defaults() {
        let mut v = Voxels::empty();
        generate(&mut v, [-16, -8, -16], [32, 16, 32], &opts());
        for (x, z) in [(-48, -48), (-16, 0), (0, 0), (16, 16), (40, -24), (56, 56), (-56, 40)] {
            let (vx, vz) = (x / 4, z / 4);
            assert_eq!(v.get(vx, -8, vz), (CRACKED_LAVA, 1.0), "lava floor at ({x},{z})");
            assert_eq!(v.get(vx, -7, vz).0, ROCK, "rock at ({x},{z})");
            let (m, o) = v.get(vx, -1, vz);
            assert_eq!(m, SANDSTONE, "top at ({x},{z})");
            assert!(o > 0.55 && o < 0.95, "top occupancy {o} at ({x},{z})");
            assert_eq!(v.get(vx, 0, vz).0, 0, "air above at ({x},{z})");
        }
    }

    #[test]
    fn generate_fills_water_and_is_deterministic() {
        let o = Options { biomes: vec![Biome::Water], ..opts() };
        let mut a = Voxels::empty();
        generate(&mut a, [0, 0, 0], [16, 16, 16], &o);
        let water = (0..16).any(|y| a.get(4, y, 4).0 == WATER);
        assert!(water, "the Water biome makes water");
        let mut b = Voxels::empty();
        generate(&mut b, [0, 0, 0], [16, 16, 16], &o);
        assert!(a.same_solid(&b));
        let mut c = Voxels::empty();
        generate(&mut c, [0, 0, 0], [16, 16, 16], &Options { seed: 7, ..o });
        assert!(c.count() > 0);
    }

    #[test]
    fn import_heightmap_and_colormap() {
        // 2×1: left black (height 0), right white (full).
        let hm = [0u8, 0, 0, 255, 255, 255, 255, 255];
        let img = Image { w: 2, h: 1, rgba: &hm };
        let mut v = Voxels::empty();
        import(&mut v, [0, 0, 0], [4, 4, 2], &img, None, 8);
        assert_eq!(v.get(0, 0, 0).0, 0, "black: nothing");
        assert_eq!(v.get(3, 3, 0), (8, 1.0), "white: full height");
        // A mid grey colormap of Grass's colour.
        let cm = [106u8, 127, 63, 255];
        let c = Image { w: 1, h: 1, rgba: &cm };
        let grey = [128u8, 0, 0, 255];
        let h = Image { w: 1, h: 1, rgba: &grey };
        let mut v = Voxels::empty();
        import(&mut v, [0, 0, 0], [2, 4, 2], &h, Some(&c), 8);
        assert_eq!(v.get(0, 0, 0).0, GRASS);
        assert_eq!(v.get(0, 3, 0).0, 0);
        assert!((v.get(0, 2, 0).1 - 0.0).abs() < 0.05 || v.get(0, 2, 0).0 == 0 || v.get(0, 1, 0).1 > 0.9);
        assert_eq!(colormap_material([12, 84, 92]), WATER);
    }

    /// `RUSTUDIO_TERRAIN_DUMP=<path> cargo test terrain_gen::tests::dump_columns -- --ignored`:
    /// per column (x fastest, then z) "height_in_tenths_of_a_voxel:material"
    /// for the all-biomes 256 × 64 × 256 box centred on the origin, the
    /// same shape a Studio ReadVoxels dump gives (for comparing the two).
    #[test]
    #[ignore]
    fn dump_columns() {
        let Ok(path) = std::env::var("RUSTUDIO_TERRAIN_DUMP") else { return };
        let o = Options { biomes: Biome::ALL.to_vec(), ..opts() };
        let (lo, size) = ([-128, -32, -128], [256, 64, 256]);
        let t = std::time::Instant::now();
        let mut v = Voxels::empty();
        generate(&mut v, lo, size, &o);
        eprintln!("generate: {:?}", t.elapsed());
        let mut out = String::new();
        for z in 0..size[2] {
            for x in 0..size[0] {
                let (mut h, mut m) = (0.0, "Air");
                for y in (0..size[1]).rev() {
                    let (mat, occ) = v.get(lo[0] + x, lo[1] + y, lo[2] + z);
                    if mat != AIR && occ > 0.0 {
                        h = y as f32 + occ;
                        m = rustudio_core::terrain::material_name(mat);
                        break;
                    }
                }
                out.push_str(&format!("{}:{} ", (h * 10.0).round() as i32, m));
            }
        }
        std::fs::write(path, out).unwrap();
    }

    /// `RUSTUDIO_TERRAIN_PLACE=<path.rbxl> cargo test terrain_gen::tests::write_generated_place -- --ignored`:
    /// a place with no baseplate whose terrain is Generate over Studio's
    /// box Position (0, 160, 0), Size (2048, 336, 2048), all biomes, the
    /// other settings default, for screenshots next to Studio's.
    #[test]
    #[ignore]
    fn write_generated_place() {
        let Ok(path) = std::env::var("RUSTUDIO_TERRAIN_PLACE") else { return };
        let mut doc = rustudio_core::doc::Document::new_place();
        let ws = doc.workspace().expect("workspace");
        for name in ["Baseplate", "SpawnLocation"] {
            if let Some(r) = doc.find_child(ws, name) {
                doc.dom.destroy(r);
            }
        }
        let t = doc.find_child(ws, "Terrain").expect("terrain");
        let mut v = Voxels::empty();
        let o = Options { biomes: Biome::ALL.to_vec(), ..opts() };
        generate(&mut v, [-256, -2, -256], [512, 84, 512], &o);
        let bytes = v.encode();
        doc.dom.get_by_ref_mut(t).unwrap().properties.insert(
            rbx_dom_weak::ustr("SmoothGrid"),
            rbx_dom_weak::types::Variant::BinaryString(bytes.into()),
        );
        std::fs::write(path, doc.encode_all(false).unwrap()).unwrap();
    }

    #[test]
    fn sea_level_fills_air_only() {
        let mut v = Voxels::empty();
        v.set([0, 0, 0], ROCK, 1.0);
        sea_level(&mut v, [-2, -2, -2], [4, 4, 4], false);
        assert_eq!(v.get(0, 0, 0).0, ROCK);
        assert_eq!(v.get(1, 1, 1).0, WATER);
        sea_level(&mut v, [-2, -2, -2], [4, 4, 4], true);
        assert_eq!(v.get(1, 1, 1).0, 0);
        assert_eq!(v.count(), 1);
    }
}
