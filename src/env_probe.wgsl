// Studio's scene environment probe passes (env_probe.rs): the default
// indoor fill, the face mirror blit, FastConvDownsamplePS and
// FastConvolutionPS (shaders_glsl3.pack, unpacked in
// checks/laser-tag-parity/render/shaders-traced).

struct ProbeParams {
    // x: source lod (fill / down) or coefficient level (conv), y: face,
    // z: 1 / destination size (CB3[0].z), w: unused
    p: vec4<f32>,
};
@group(0) @binding(0) var<uniform> pp: ProbeParams;
// FastConvolutionPS CB4[60]: the 960 bytes Studio uploads.
@group(0) @binding(1) var<uniform> coeff: array<vec4<f32>, 60>;
@group(0) @binding(2) var env: texture_cube<f32>;
@group(0) @binding(3) var env_sampler: sampler;
@group(0) @binding(4) var face_src: texture_2d<f32>;

struct ProbeOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_probe(@builtin(vertex_index) i: u32) -> ProbeOut {
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    var o: ProbeOut;
    o.pos = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return o;
}

// A face-local point (x right, y up, in [-1, 1]) to a cube direction, as
// FastConvDownsamplePS's per-face switch builds them.
fn face_dir(face: i32, x: f32, y: f32) -> vec3<f32> {
    switch (face) {
        case 0: { return vec3<f32>(1.0, y, -x); }
        case 1: { return vec3<f32>(-1.0, y, x); }
        case 2: { return vec3<f32>(x, 1.0, -y); }
        case 3: { return vec3<f32>(x, -1.0, y); }
        case 4: { return vec3<f32>(x, y, 1.0); }
        default: { return vec3<f32>(-x, y, -1.0); }
    }
}

// Both slots start as Studio's default indoor cube (textures/sky/indoor512,
// stored gamma-encoded: squared here), level for level with the lod
// env_reflect read it at before (probe mip m = indoor lod 2 + m).
@fragment
fn fs_probe_default(in: ProbeOut) -> @location(0) vec4<f32> {
    let d = face_dir(i32(pp.p.y), in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let c = textureSampleLevel(env, env_sampler, d, pp.p.x).rgb;
    return vec4<f32>(c * c, 1.0);
}

// The face is drawn with an ordinary right-handed camera; cube faces are
// mirrored left to right relative to it.
@fragment
fn fs_probe_blit(in: ProbeOut) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(face_src));
    let p = vec2<i32>(in.pos.xy);
    return vec4<f32>(textureLoad(face_src, vec2<i32>(size.x - 1 - p.x, p.y), 0).rgb, 1.0);
}

// FastConvDownsamplePS: four taps of the level above, weighted by their
// solid angle (r^3) plus 1/8.
@fragment
fn fs_probe_down(in: ProbeOut) -> @location(0) vec4<f32> {
    let z = pp.p.z;
    let face = i32(pp.p.y);
    let x0 = in.uv.x * 2.0 - 0.75 * z - 1.0;
    let x1 = in.uv.x * 2.0 + 0.75 * z - 1.0;
    let y0 = -in.uv.y * 2.0 + 0.75 * z + 1.0;
    let y1 = -in.uv.y * 2.0 - 0.75 * z + 1.0;
    let r00 = x0 * x0 + y0 * y0 + 1.0;
    let r10 = x1 * x1 + y0 * y0 + 1.0;
    let r01 = x0 * x0 + y1 * y1 + 1.0;
    let r11 = x1 * x1 + y1 * y1 + 1.0;
    let w00 = r00 * sqrt(r00);
    let w10 = r10 * sqrt(r10);
    let w01 = r01 * sqrt(r01);
    let w11 = r11 * sqrt(r11);
    let norm = 0.5 / (w00 + w10 + w01 + w11);
    let lod = pp.p.x;
    let c = textureSampleLevel(env, env_sampler, face_dir(face, x0, y0), lod) * (w00 * norm + 0.125)
        + textureSampleLevel(env, env_sampler, face_dir(face, x1, y0), lod) * (w10 * norm + 0.125)
        + textureSampleLevel(env, env_sampler, face_dir(face, x0, y1), lod) * (w01 * norm + 0.125)
        + textureSampleLevel(env, env_sampler, face_dir(face, x1, y1), lod) * (w11 * norm + 0.125);
    return vec4<f32>(c.rgb, 1.0);
}

// FastConvolutionPS: for each axis the direction is away from (weight
// (max of the other two |components| - 0.75) x 4 > 0), eight taps in its
// tangent frame from the coefficient table, each at its own source lod.
@fragment
fn fs_probe_conv(in: ProbeOut) -> @location(0) vec4<f32> {
    let level = i32(pp.p.x);
    let v1 = face_dir(i32(pp.p.y), in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let n = normalize(v1);
    let a = abs(v1);
    var acc = vec4<f32>(0.0);
    for (var i = 0; i < 3; i++) {
        let hi = i >> 1;
        let first = (1 - (i & 1)) - hi;
        let second = 2 - hi;
        let w_axis = (max(a[first], a[second]) - 0.75) * 4.0;
        if (w_axis > 0.0) {
            var axis = vec3<f32>(0.0, 0.0, 1.0);
            if (i == 0) { axis = vec3<f32>(1.0, 0.0, 0.0); }
            if (i == 1) { axis = vec3<f32>(0.0, 1.0, 0.0); }
            let t = normalize(cross(axis, n));
            let b = cross(n, t);
            for (var j = 0; j < 2; j++) {
                let k = 2 * i + j;
                let base = level * 30;
                let ca = coeff[base + k];
                let cb = coeff[base + 6 + k];
                let cc = coeff[base + 12 + k];
                let cl = coeff[base + 18 + k];
                let cw = coeff[base + 24 + k];
                for (var c = 0; c < 4; c++) {
                    let d = t * ca[c] + b * cb[c] + n * cc[c];
                    let w = cw[c] * w_axis;
                    let dc = d / max(abs(d.x), max(abs(d.y), abs(d.z)));
                    let lod = min(5.0, cl[c] + 0.75 * log2(dot(dc, dc)));
                    acc += vec4<f32>(textureSampleLevel(env, env_sampler, dc, lod).rgb * w, w);
                }
            }
        }
    }
    let r = acc / acc.w;
    return vec4<f32>(max(r.rgb, vec3<f32>(0.0)), 1.0);
}
