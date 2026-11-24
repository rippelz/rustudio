//! Studio's outdoor environment mip pyramid after AdvSkyLinearEnvFS.
//! FastConvDownsamplePS builds the source pyramid. FastConvolutionPS
//! uses it for the first two filtered mips, then SpecularConvFS samples
//! the preceding filtered mip. Each pass stores R11G11B10 radiance.

use glam::Vec3;

use super::{cube_face_dir, cube_face_st, env_probe_gpu::CONVOLUTION};

struct CubeLevel {
    width: usize,
    pixels: Vec<Vec3>,
}

impl CubeLevel {
    fn render(width: usize, sample: impl Fn(usize, f32, f32) -> Vec3 + Sync) -> Self {
        let pixels = std::thread::scope(|scope| {
            let faces: Vec<_> = (0..6)
                .map(|face| {
                    let sample = &sample;
                    scope.spawn(move || {
                        let mut pixels = Vec::with_capacity(width * width);
                        for y in 0..width {
                            for x in 0..width {
                                let u = (x as f32 + 0.5) / width as f32;
                                let v = (y as f32 + 0.5) / width as f32;
                                pixels.push(r11g11b10(sample(face, u, v)));
                            }
                        }
                        pixels
                    })
                })
                .collect();
            faces
                .into_iter()
                .flat_map(|face| face.join().unwrap())
                .collect()
        });
        Self { width, pixels }
    }

    fn at(&self, face: usize, x: i32, y: i32) -> Vec3 {
        let size = self.width as i32;
        if (0..size).contains(&x) && (0..size).contains(&y) {
            return self.pixels
                [face * self.width * self.width + y as usize * self.width + x as usize];
        }
        let width = size as f32;
        let ray = cube_face_dir(face, (x as f32 + 0.5) / width, (y as f32 + 0.5) / width);
        let (face, u, v) = cube_face_st(ray);
        let x = ((u * width) as usize).min(self.width - 1);
        let y = ((v * width) as usize).min(self.width - 1);
        self.pixels[face * self.width * self.width + y * self.width + x]
    }

    fn sample(&self, ray: Vec3) -> Vec3 {
        let (face, u, v) = cube_face_st(ray);
        let x = u * self.width as f32 - 0.5;
        let y = v * self.width as f32 - 0.5;
        let x0 = x.floor() as i32;
        let y0 = y.floor() as i32;
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        self.at(face, x0, y0)
            .lerp(self.at(face, x0 + 1, y0), fx)
            .lerp(
                self.at(face, x0, y0 + 1)
                    .lerp(self.at(face, x0 + 1, y0 + 1), fx),
                fy,
            )
    }

    fn downsample(&self) -> Self {
        let width = (self.width / 2).max(1);
        // FastConvDownsamplePS offsets the NDC taps by ±0.75 / width.
        let offset = 0.375 / width as f32;
        Self::render(width, |face, u, v| {
            let mut colors = Vec3::ZERO;
            let mut weighted = Vec3::ZERO;
            let mut total = 0.0;
            for (du, dv) in [
                (-offset, -offset),
                (offset, -offset),
                (-offset, offset),
                (offset, offset),
            ] {
                let (s, t) = (u + du, v + dv);
                let (x, y) = (2.0 * s - 1.0, 1.0 - 2.0 * t);
                let r2 = x * x + y * y + 1.0;
                let weight = r2 * r2.sqrt();
                let color = self.sample(cube_face_dir(face, s, t));
                colors += color;
                weighted += weight * color;
                total += weight;
            }
            colors * 0.125 + weighted * (0.5 / total)
        })
    }
}

// The GPU capture's floating-point render target truncates on each write.
// Subnormal values have a fixed step instead of an exponent-relative one.
fn r11g11b10(value: Vec3) -> Vec3 {
    let pack = |x: f32, bits: u32| {
        let x = x.clamp(0.0, if bits == 6 { 65024.0 } else { 64512.0 });
        if x < 2.0_f32.powi(-14) {
            let step = 2.0_f32.powi(-14 - bits as i32);
            (x / step).floor() * step
        } else {
            let mask = !((1 << (23 - bits)) - 1);
            f32::from_bits(x.to_bits() & mask)
        }
    };
    Vec3::new(pack(value.x, 6), pack(value.y, 6), pack(value.z, 5))
}

fn fast_convolution(source: &[CubeLevel], mip: usize, face: usize, u: f32, v: f32) -> Vec3 {
    let n = cube_face_dir(face, u, v);
    let a = n.abs() / n.abs().max_element();
    let base = (mip - 1) * 30;
    let mut color = Vec3::ZERO;
    let mut total = 0.0;
    for (i, axis) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().enumerate() {
        let shift = i >> 1;
        let axis_weight = (a[(1 - (i & 1)) - shift].max(a[2 - shift]) - 0.75) * 4.0;
        if axis_weight <= 0.0 {
            continue;
        }
        let t = axis.cross(n).normalize();
        let b = n.cross(t);
        for k in 2 * i..2 * i + 2 {
            let [ct, cb, cn, lod, weight] =
                [0, 6, 12, 18, 24].map(|row| CONVOLUTION[base + row + k]);
            for tap in 0..4 {
                let d = ct[tap] * t + cb[tap] * b + cn[tap] * n;
                let d = d / d.abs().max_element();
                let lod = (lod[tap] + 0.75 * d.length_squared().log2())
                    .clamp(0.0, (source.len() - 1).min(5) as f32);
                let lo = lod.floor() as usize;
                let hi = (lo + 1).min(source.len() - 1);
                let c = source[lo].sample(d).lerp(source[hi].sample(d), lod.fract());
                let w = axis_weight * weight[tap];
                color += c * w;
                total += w;
            }
        }
    }
    (color / total).max(Vec3::ZERO)
}

fn specular_convolution(previous: &CubeLevel, mip: usize) -> CubeLevel {
    let a2 = (mip as f32 / 5.0).powi(4);
    // SpecularConvFS's 32 Hammersley directions, expressed in its tangent frame.
    let taps: [(Vec3, f32); 32] = std::array::from_fn(|i| {
        let u = (i as u32).reverse_bits() as f32 / 4294967296.0;
        let phi = std::f32::consts::TAU * i as f32 / 32.0;
        let cos_theta = ((1.0 - u) / (1.0 + (a2 - 1.0) * u)).sqrt();
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let (sin_phi, cos_phi) = phi.sin_cos();
        let half = Vec3::new(sin_theta * cos_phi, sin_theta * sin_phi, cos_theta);
        let ray = 2.0 * cos_theta * half - Vec3::Z;
        (ray, ray.z.clamp(0.0, 1.0))
    });
    let total: f32 = taps.iter().map(|&(_, weight)| weight).sum();
    CubeLevel::render((previous.width / 2).max(1), |face, u, v| {
        let n = cube_face_dir(face, u, v);
        let up = if n.z.abs() < 0.999 { Vec3::Z } else { Vec3::X };
        let t = up.cross(n).normalize();
        let b = n.cross(t);
        let mut color = Vec3::ZERO;
        for &(ray, weight) in &taps {
            if weight > 0.0 {
                color += previous.sample(t * ray.x + b * ray.y + n * ray.z) * weight;
            }
        }
        color / total
    })
}

/// Mips 1 and 2 use CB4's coefficients and the downsample pyramid.
/// Mips 3..5 use the previous filtered level and the mip's roughness.
pub(super) fn filter(base: Vec<[f32; 3]>, width: u32, levels: usize) -> Vec<(u32, Vec<u8>)> {
    let base = CubeLevel {
        width: width as usize,
        pixels: base
            .into_iter()
            .map(|c| r11g11b10(Vec3::from_array(c)))
            .collect(),
    };
    let mut source = vec![base];
    for mip in 1..levels.min(6) {
        source.push(source[mip - 1].downsample());
    }
    let mut filtered = Vec::new();
    for mip in 1..levels {
        let level = if mip <= 2 {
            CubeLevel::render(source[mip].width, |face, u, v| {
                fast_convolution(&source, mip, face, u, v)
            })
        } else {
            let previous = &filtered[mip - 2];
            if mip <= 5 {
                specular_convolution(previous, mip)
            } else {
                previous.downsample()
            }
        };
        filtered.push(level);
    }
    std::iter::once(source.remove(0))
        .chain(filtered)
        .map(|level| {
            // The renderer shares its RGBA8 sky cube with the visible skybox.
            // Encode the linear environment levels with its existing HDR mapping.
            let mut bytes = Vec::with_capacity(level.pixels.len() * 4);
            for c in level.pixels {
                let [r, g, b] = c
                    .to_array()
                    .map(|v| ((v * 0.25).clamp(0.0, 1.0).sqrt() * 255.0).round() as u8);
                bytes.extend_from_slice(&[r, g, b, 255]);
            }
            (level.width as u32, bytes)
        })
        .collect()
}
