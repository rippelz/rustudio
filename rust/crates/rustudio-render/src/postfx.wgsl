// Studio-faithful post-processing shaders (port of Roblox Studio 0.748 shaders_glsl3.pack):
// 1. DepthOfField: CheapBokehFS (< 0.22), Far 1/2-res chain (DownSample2x, CoC Min/Avg mask,
//    DOFEdgeFattenFS, FixUpCoCFS, 45-tap BokehFarCoCFS, TentFilterFS, DOFResolveFarFS),
//    Near 1/2- or 1/4-res chain (DownSample2x/4x, CoC Max/Min mask, FixUpCoCFS,
//    45-tap BokehNearCoCFS, TentFilterFS, DOFResolveNearFS).
// 2. Bloom: DownSampleGjoelLinearFS / DownSampleGjoelMipFS + UpSampleTentFS (HQ 1/2-res & LQ 1/8-res).
// 3. SunRays: 4x4 max sky-depth downsample, SunRays{6,9,12}FS with 8x8 noise + HG phase + ray clip,
//    RadialBlur{3,6,9,12}FS.
// 4. Blur + FXCompositingLinearFS: 1/4-res encoded sqrt(clamp(L*exp*0.25,0,1)) downsample,
//    FXCompositingLinearFS (bloom + sunrays*exp before blur), separable Blur{1,3,5,7}FS.
// 5. ImageProcessFXCompositionLinearFS / ImageProcessBlurFXCompositionLinearFS:
//    Default & Retro rational tonemappers + 3x4 Studio ColorCorrection matrix.

struct PostFxUniform {
    // p0: (exposure, blur_weight, tonemap_mode, flags)
    // tonemap_mode: 0 = Default, 1 = Retro, 2 = RawSqrt (no tone curve / debug)
    // flags: bit0 = terrain_capture_id, bit1 = raw_debug
    p0: vec4<f32>,
    // p1: (bloom_intensity, bloom_threshold, upsample_weight, near_plane)
    p1: vec4<f32>,
    // p2: (ssao_half_diag_x, ssao_half_diag_y, is_ortho, post_fx_scale)
    p2: vec4<f32>,
    // dof: (focus_distance, in_focus_radius, near_intensity, far_intensity)
    dof: vec4<f32>,
    // Studio 3x4 ColorCorrection matrix (Contrast -> Brightness -> Saturation(0.34,0.55,0.11) -> TintColor)
    cc_r: vec4<f32>,
    cc_g: vec4<f32>,
    cc_b: vec4<f32>,
    // sun0: (sun_ndc.x, sun_ndc.y, sun_view.x, sun_view.y)
    sun0: vec4<f32>,
    // sun1: (cb_intensity, g, g*g, sun_view.z)
    sun1: vec4<f32>,
    // sun2: (sun_color.r, sun_color.g, sun_color.b, taps_count)
    sun2: vec4<f32>,
    // sun3: (inv_proj_x, inv_proj_y, 0, 0)
    sun3: vec4<f32>,
    // extra: pass-specific parameters
    extra: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: PostFxUniform;
@group(0) @binding(1) var samp_lin: sampler;
@group(0) @binding(2) var samp_near: sampler;
@group(0) @binding(3) var samp_repeat_near: sampler;

@group(1) @binding(0) var tex0: texture_2d<f32>;
@group(1) @binding(1) var tex1: texture_2d<f32>;
@group(1) @binding(2) var tex2: texture_2d<f32>;
@group(1) @binding(3) var tex3: texture_2d<f32>;

struct PostOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_post(@builtin(vertex_index) vid: u32) -> PostOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: PostOut;
    out.pos = vec4<f32>(p[vid], 0.0, 1.0);
    out.uv = vec2<f32>(p[vid].x * 0.5 + 0.5, 0.5 - p[vid].y * 0.5);
    return out;
}

// ------------------------------------------------------------------ Helpers

// Studio's scene target is R10G10B10A2_UNORM storing sqrt(clamp(L * exp * 0.25, 0, 1)).
// Bilinearly filtering the encoded target (before decoding 4 * e^2) prevents bright Neon
// pixels (> 4.0) from bleeding uncapped energy across texel edges into Bloom / Blur.
fn sample_scene_encoded(uv: vec2<f32>) -> vec3<f32> {
    let dim = vec2<i32>(textureDimensions(tex0));
    let p = uv * vec2<f32>(dim) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(p));
    let f = fract(p);
    var taps: array<vec3<f32>, 4>;
    for (var i = 0; i < 4; i++) {
        let q = clamp(base + vec2<i32>(i & 1, i >> 1), vec2<i32>(0), dim - vec2<i32>(1));
        taps[i] = sqrt(clamp(max(textureLoad(tex0, q, 0).rgb, vec3<f32>(0.0)) * u.p0.x * 0.25, vec3<f32>(0.0), vec3<f32>(1.0)));
    }
    return mix(mix(taps[0], taps[1], f.x), mix(taps[2], taps[3], f.x), f.y);
}

fn scene_post_exp_linear(rgb: vec3<f32>) -> vec3<f32> {
    let encoded = sqrt(clamp(max(rgb, vec3<f32>(0.0)) * u.p0.x * 0.25, vec3<f32>(0.0), vec3<f32>(1.0)));
    return 4.0 * encoded * encoded;
}

fn scene_clamped_pre_exp(rgb: vec3<f32>) -> vec3<f32> {
    let cap = vec3<f32>(4.0 / max(u.p0.x, 1e-4));
    return min(max(rgb, vec3<f32>(0.0)), cap);
}

fn uv_clamp(uv: vec2<f32>, dims: vec2<f32>) -> vec2<f32> {
    let half_t = 0.5 / dims;
    return clamp(uv, half_t, vec2<f32>(1.0) - half_t);
}

// ------------------------------------------------------------------ Bloom
// DownSampleGjoel8UVVS + DownSampleGjoelLinearFS / DownSampleGjoelMipFS + UpSampleTentFS

const GJOEL_OFFSETS: array<vec2<f32>, 9> = array<vec2<f32>, 9>(
    vec2<f32>(0.0, 0.0),
    vec2<f32>(0.75, 0.0),
    vec2<f32>(-0.75, 0.0),
    vec2<f32>(0.0, 0.75),
    vec2<f32>(0.0, -0.75),
    vec2<f32>(0.70512277, 0.70512277),
    vec2<f32>(-0.70512277, -0.70512277),
    vec2<f32>(0.70512277, -0.70512277),
    vec2<f32>(-0.70512277, 0.70512277),
);

@fragment
fn fs_bloom_extract(in: PostOut) -> @location(0) vec4<f32> {
    let src_dims = vec2<f32>(textureDimensions(tex0));
    // u.extra.xy = target texel size (1 / dst_w, 1 / dst_h).
    // DownSampleGjoel8UVVS offsets in target texels (CB1[0].zw).
    let dst_texel = u.extra.xy;
    var acc = vec3<f32>(0.0);
    for (var k = 0; k < 9; k++) {
        let uv = uv_clamp(in.uv + GJOEL_OFFSETS[k] * dst_texel, src_dims);
        let enc = sample_scene_encoded(uv);
        acc += 4.0 * enc * enc;
    }
    let avg = acc * (1.0 / 9.0);
    let peak = max(max(avg.r, avg.g), avg.b);
    let bloom = avg * (max(peak - u.p1.y, 0.0) / (peak + 0.001) * u.p1.x);
    return vec4<f32>(bloom, 1.0);
}

@fragment
fn fs_bloom_down(in: PostOut) -> @location(0) vec4<f32> {
    let src_dims = vec2<f32>(textureDimensions(tex0));
    let dst_texel = u.extra.xy;
    var acc = vec3<f32>(0.0);
    for (var k = 0; k < 9; k++) {
        let uv = uv_clamp(in.uv + GJOEL_OFFSETS[k] * dst_texel, src_dims);
        acc += textureSampleLevel(tex0, samp_lin, uv, 0.0).rgb;
    }
    return vec4<f32>(acc * (1.0 / 9.0), 1.0);
}

// UpSampleTentFS: 9-tap tent at +/- 2 * dst_texel (= +/- 1 * src_texel), weighted by u.p1.z.
@fragment
fn fs_bloom_up(in: PostOut) -> @location(0) vec4<f32> {
    let src_dims = vec2<f32>(textureDimensions(tex0));
    let step = u.extra.xy * 2.0;
    var acc = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let w = select(1.0, 2.0, x == 0) * select(1.0, 2.0, y == 0);
            let uv = uv_clamp(in.uv + vec2<f32>(f32(x), f32(y)) * step, src_dims);
            acc += textureSampleLevel(tex0, samp_lin, uv, 0.0).rgb * w;
        }
    }
    return vec4<f32>(acc * (0.0625 * u.p1.z), 1.0);
}

// ------------------------------------------------------------------ SunRays
// SunRays6/9/12FS + RadialBlur3/6/9/12FS

fn sunrays_clip_t(ndc: vec2<f32>, sun_ndc: vec2<f32>) -> f32 {
    let d = sun_ndc - ndc;
    let tx = select((sign(d.x) - ndc.x) / d.x, 1.0, abs(d.x) < 1e-6);
    let ty = select((sign(d.y) - ndc.y) / d.y, 1.0, abs(d.y) < 1e-6);
    return min(min(select(tx, 1.0, tx < 0.0), select(ty, 1.0, ty < 0.0)), 1.0);
}

// tex0 = 1/4-res depth (R16Float, sky >= 0.999), tex1 = 8x8 R8Unorm SunRays Noise.
@fragment
fn fs_sunrays_march(in: PostOut) -> @location(0) vec4<f32> {
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let sun_ndc = u.sun0.xy;
    let clip_t = sunrays_clip_t(ndc, sun_ndc);
    let taps = i32(u.sun2.w + 0.5);
    let inv_m = 1.0 / f32(taps);
    // In WGSL Y-down UV, uv - sun_uv = ((ndc.x - sun_ndc.x)*0.5, -(ndc.y - sun_ndc.y)*0.5).
    let delta_uv = vec2<f32>((ndc.x - sun_ndc.x) * 0.5, -(ndc.y - sun_ndc.y) * 0.5);
    let step_uv = delta_uv * clip_t * inv_m;

    let q_dims = vec2<f32>(textureDimensions(tex0));
    let noise_uv = (in.uv * q_dims) * 0.125;
    let jitter = textureSampleLevel(tex1, samp_repeat_near, noise_uv, 0.0).x;

    var tap_uv = in.uv + step_uv * jitter;
    var occ = 0.0;
    for (var i = 0; i < taps; i++) {
        tap_uv -= step_uv;
        let d = textureSampleLevel(tex0, samp_near, clamp(tap_uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).x;
        occ += select(0.0, 1.0, d >= 0.999);
    }

    let ray_view = normalize(vec3<f32>(ndc * u.sun3.xy + u.sun3.zw, -1.0));
    let sun_view = vec3<f32>(u.sun0.z, u.sun0.w, u.sun1.w);
    let g = u.sun1.y;
    let g2 = u.sun1.z;
    let cos_theta = dot(ray_view, sun_view);
    let inv_s = inverseSqrt(clamp(1.0 + g2 - 2.0 * g * cos_theta, 0.01, 1.0));
    let phase = (1.0 - g2) * inv_s * inv_s * inv_s;
    let weight = (12.0 * inv_m) * u.sun1.x * phase;
    return vec4<f32>(u.sun2.xyz * (occ * weight), 1.0);
}

// RadialBlur{3,6,9,12}FS: tex0 = 1/4-res sunrays buffer.
@fragment
fn fs_sunrays_radial(in: PostOut) -> @location(0) vec4<f32> {
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0);
    let sun_ndc = u.sun0.xy;
    let clip_t = sunrays_clip_t(ndc, sun_ndc);
    let taps = i32(u.sun2.w + 0.5);
    let inv_n = 1.0 / f32(taps);
    let delta_uv = vec2<f32>((ndc.x - sun_ndc.x) * 0.5, -(ndc.y - sun_ndc.y) * 0.5);
    let step_uv = delta_uv * clip_t * (0.5 * inv_n);
    let dims = vec2<f32>(textureDimensions(tex0));

    var tap_uv = in.uv;
    var acc = vec3<f32>(0.0);
    for (var i = 0; i < taps; i++) {
        tap_uv = uv_clamp(tap_uv - step_uv, dims);
        acc += textureSampleLevel(tex0, samp_lin, tap_uv, 0.0).rgb;
    }
    return vec4<f32>(acc * inv_n, 1.0);
}

// ------------------------------------------------------------------ Blur + FXCompositingLinearFS
// DownSample4x4AvgFS (tier >= 2) / single tap (tier < 2) in encoded sqrt space,
// FXCompositingLinearFS, separable Blur{1,3,5,7}FS.

@fragment
fn fs_blur_down(in: PostOut) -> @location(0) vec4<f32> {
    let src_dims = vec2<f32>(textureDimensions(tex0));
    // Encode scene into Studio's R10G10B10A2 sqrt space before 1/4-res downsample & blur.
    if (u.extra.x < 0.5) {
        return vec4<f32>(sample_scene_encoded(uv_clamp(in.uv, src_dims)), 1.0);
    }
    // DownSample4x4VS: +/- 0.25 * quarter_texel (= +/- 1 full-res pixel).
    let off = u.extra.yz * 0.25;
    let e0 = sample_scene_encoded(uv_clamp(in.uv + vec2<f32>(-off.x, -off.y), src_dims));
    let e1 = sample_scene_encoded(uv_clamp(in.uv + vec2<f32>( off.x, -off.y), src_dims));
    let e2 = sample_scene_encoded(uv_clamp(in.uv + vec2<f32>(-off.x,  off.y), src_dims));
    let e3 = sample_scene_encoded(uv_clamp(in.uv + vec2<f32>( off.x,  off.y), src_dims));
    return vec4<f32>((e0 + e1 + e2 + e3) * 0.25, 1.0);
}

// FXCompositingLinearFS: tex0 = 1/4-res encoded scene, tex1 = bloom, tex2 = sunrays.
@fragment
fn fs_fx_composite(in: PostOut) -> @location(0) vec4<f32> {
    let base_enc = textureSampleLevel(tex0, samp_lin, in.uv, 0.0).rgb;
    let bloom = textureSampleLevel(tex1, samp_lin, in.uv, 0.0).rgb;
    let sunrays = textureSampleLevel(tex2, samp_lin, in.uv, 0.0).rgb;
    let lin = base_enc * base_enc * 4.0 + bloom + sunrays * u.p0.x;
    let enc = sqrt(clamp(lin * 0.25, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(enc, 1.0);
}

// Blur{1,3,5,7}FS: u.extra = (step_x, step_y, sigma, k_pairs)
@fragment
fn fs_blur_1d(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let step = u.extra.xy;
    let sigma = max(u.extra.z, 1e-4);
    let k_pairs = i32(u.extra.w + 0.5);
    let denom = 2.0 * sigma * sigma;

    var acc = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0).rgb;
    var wsum = 1.0;
    for (var k = 0; k < k_pairs; k++) {
        let a = f32(2 * k + 1);
        let b = a + 1.0;
        let wb = exp(-(b * b) / denom);
        let w = exp(-(a * a) / denom) + wb;
        let off = step * (a + wb / max(w, 1e-8));
        let p = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + off, dims), 0.0).rgb;
        let m = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv - off, dims), 0.0).rgb;
        acc += (p + m) * w;
        wsum += 2.0 * w;
    }
    return vec4<f32>(acc / wsum, 1.0);
}

// ------------------------------------------------------------------ DOF (non-depth passes)

const DOF_DISK: array<vec2<f32>, 45> = array<vec2<f32>, 45>(
    vec2<f32>(0.22490672767162322998046875, 0.16340430080890655517578125),
    vec2<f32>(-0.085906721651554107666015625, 0.2643937170505523681640625),
    vec2<f32>(-0.27799999713897705078125, 3.4044057403885696227487100973264e-17),
    vec2<f32>(-0.085906721651554107666015625, -0.2643937170505523681640625),
    vec2<f32>(0.22490672767162322998046875, -0.16340430080890655517578125),
    vec2<f32>(0.540000021457672119140625, 0.0),
    vec2<f32>(0.436869204044342041015625, 0.3174040615558624267578125),
    vec2<f32>(0.16686917841434478759765625, 0.5135705471038818359375),
    vec2<f32>(-0.16686917841434478759765625, 0.5135705471038818359375),
    vec2<f32>(-0.436869204044342041015625, 0.3174040615558624267578125),
    vec2<f32>(-0.540000021457672119140625, 6.6128748929759884316731399778178e-17),
    vec2<f32>(-0.436869204044342041015625, -0.3174040615558624267578125),
    vec2<f32>(-0.16686917841434478759765625, -0.5135705471038818359375),
    vec2<f32>(0.16686917841434478759765625, -0.5135705471038818359375),
    vec2<f32>(0.436869204044342041015625, -0.3174040615558624267578125),
    vec2<f32>(0.7651021480560302734375, 0.18858073651790618896484375),
    vec2<f32>(0.58982646465301513671875, 0.522540628910064697265625),
    vec2<f32>(0.279428660869598388671875, 0.7367928028106689453125),
    vec2<f32>(-0.094982899725437164306640625, 0.78225457668304443359375),
    vec2<f32>(-0.4476350247859954833984375, 0.64851129055023193359375),
    vec2<f32>(-0.6977393627166748046875, 0.3662018477916717529296875),
    vec2<f32>(-0.78799998760223388671875, 4.4644126152591019802973182351025e-16),
    vec2<f32>(-0.6977393627166748046875, -0.3662018477916717529296875),
    vec2<f32>(-0.4476350247859954833984375, -0.64851129055023193359375),
    vec2<f32>(-0.094982899725437164306640625, -0.78225457668304443359375),
    vec2<f32>(0.279428660869598388671875, -0.7367928028106689453125),
    vec2<f32>(0.58982646465301513671875, -0.522540628910064697265625),
    vec2<f32>(0.7651021480560302734375, -0.18858073651790618896484375),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.93247222900390625, 0.3612416684627532958984375),
    vec2<f32>(0.73900890350341796875, 0.673695623874664306640625),
    vec2<f32>(0.4457383453845977783203125, 0.8951632976531982421875),
    vec2<f32>(0.09226836264133453369140625, 0.995734155178070068359375),
    vec2<f32>(-0.273662984371185302734375, 0.961825668811798095703125),
    vec2<f32>(-0.602634608745574951171875, 0.798017203807830810546875),
    vec2<f32>(-0.850217163562774658203125, 0.52643215656280517578125),
    vec2<f32>(-0.9829730987548828125, 0.18374951183795928955078125),
    vec2<f32>(-0.9829730987548828125, -0.18374951183795928955078125),
    vec2<f32>(-0.850217163562774658203125, -0.52643215656280517578125),
    vec2<f32>(-0.602634608745574951171875, -0.798017203807830810546875),
    vec2<f32>(-0.273662984371185302734375, -0.961825668811798095703125),
    vec2<f32>(0.09226836264133453369140625, -0.995734155178070068359375),
    vec2<f32>(0.4457383453845977783203125, -0.8951632976531982421875),
    vec2<f32>(0.73900890350341796875, -0.673695623874664306640625),
    vec2<f32>(0.93247222900390625, -0.3612416684627532958984375),
);

const DOF_CHEAP_DISK: array<vec2<f32>, 27> = array<vec2<f32>, 27>(
    vec2<f32>(0.5400000215, 0.0),
    vec2<f32>(0.436869204, 0.3174040616),
    vec2<f32>(0.1668691784, 0.5135705471),
    vec2<f32>(-0.1668691784, 0.5135705471),
    vec2<f32>(-0.436869204, 0.3174040616),
    vec2<f32>(-0.5400000215, 6.612874893e-17),
    vec2<f32>(-0.436869204, -0.3174040616),
    vec2<f32>(-0.1668691784, -0.5135705471),
    vec2<f32>(0.1668691784, -0.5135705471),
    vec2<f32>(0.436869204, -0.3174040616),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.932472229, 0.3612416685),
    vec2<f32>(0.7390089035, 0.6736956239),
    vec2<f32>(0.4457383454, 0.8951632977),
    vec2<f32>(0.09226836264, 0.9957341552),
    vec2<f32>(-0.2736629844, 0.9618256688),
    vec2<f32>(-0.6026346087, 0.7980172038),
    vec2<f32>(-0.8502171636, 0.5264321566),
    vec2<f32>(-0.9829730988, 0.1837495118),
    vec2<f32>(-0.9829730988, -0.1837495118),
    vec2<f32>(-0.8502171636, -0.5264321566),
    vec2<f32>(-0.6026346087, -0.7980172038),
    vec2<f32>(-0.2736629844, -0.9618256688),
    vec2<f32>(0.09226836264, -0.9957341552),
    vec2<f32>(0.4457383454, -0.8951632977),
    vec2<f32>(0.7390089035, -0.6736956239),
    vec2<f32>(0.932472229, -0.3612416685),
);

// CheapBokehFS (< 0.22): tex0 = full-res (rgb, coc). u.extra.x = intensity.
@fragment
fn fs_dof_cheap(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let center = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0);
    let c = center.a;
    let coc2 = c * c;
    let radius = coc2 * (1.4 * u.extra.x);
    let scale = vec2<f32>(dims.y / dims.x, 1.0) * 0.014;
    var sum = center * coc2;
    var weight = coc2;
    for (var i = 0; i < 27; i++) {
        let uv = in.uv + (DOF_CHEAP_DISK[i] * radius) * scale;
        if (all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0))) {
            let tap = textureSampleLevel(tex0, samp_lin, uv_clamp(uv, dims), 0.0);
            sum += tap * tap.a;
            weight += tap.a;
        }
    }
    let blurred = sum * clamp(1.0 / (weight + 0.001), 0.0, 1.0);
    let blend = select(0.0, coc2, u.extra.x > 0.0 && c > 0.0);
    return vec4<f32>(mix(center.rgb, blurred.rgb, blend), 1.0);
}

// DOFDownSampleColor2xFS: vertical 2-tap downsample of (rgb, coc).
// u.extra.xy = (0, 0.5 * dy).
@fragment
fn fs_dof_down_v(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let off = u.extra.xy;
    let a = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + off, dims), 0.0);
    let b = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv - off, dims), 0.0);
    return vec4<f32>((a.rgb + b.rgb) * 0.5, max(a.a, b.a));
}

// CoC mask separable 5-tap pass.
// u.extra = (step_x, step_y, op_mode, read_alpha)
// op_mode: 0 = Max, 1 = Min (Studio's asymmetric min(a, min(max(b,c), min(d,e)))), 2 = Avg (max(avg4, center))
fn read_coc_ch(uv: vec2<f32>, dims: vec2<f32>, read_alpha: bool) -> f32 {
    let s = textureSampleLevel(tex0, samp_lin, uv_clamp(uv, dims), 0.0);
    return select(s.r, s.a, read_alpha);
}

@fragment
fn fs_dof_coc_filter(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let step = u.extra.xy;
    let mode = i32(u.extra.z + 0.5);
    let ra = u.extra.w > 0.5;
    let a = read_coc_ch(in.uv, dims, ra);
    let b = read_coc_ch(in.uv + step, dims, ra);
    let c = read_coc_ch(in.uv + 2.0 * step, dims, ra);
    let d = read_coc_ch(in.uv - step, dims, ra);
    let e = read_coc_ch(in.uv - 2.0 * step, dims, ra);
    var res = a;
    if (mode == 0) {
        // DOFDownSampleCoCMax4xFS / ExtractAlphaMax4xFS
        res = max(a, max(max(b, c), max(d, e)));
    } else if (mode == 1) {
        // DOFDownSampleCoCMin4xFS: exact Studio formula
        res = min(a, min(max(b, c), min(d, e)));
    } else {
        // DOFDownSampleCoCAvg4xFS
        res = max((b + c + d + e) * 0.25, a);
    }
    return vec4<f32>(res, 0.0, 0.0, 1.0);
}

// DOFEdgeFattenFS: tex0 = half-res (rgb, far_coc).
@fragment
fn fs_dof_edge_fatten(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let t = 1.0 / dims;
    let c0 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>(-t.x, -t.y), dims), 0.0);
    let c1 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>( t.x, -t.y), dims), 0.0);
    let c2 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>(-t.x,  t.y), dims), 0.0);
    let c3 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>( t.x,  t.y), dims), 0.0);
    let center = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0);
    let m = max(max(c0.a, c1.a), max(c3.a, c2.a));
    let w = step(vec4<f32>(m), vec4<f32>(c0.a, c1.a, c2.a, c3.a));
    if (center.a * center.a <= 0.0028) {
        var out = (c0 * w.x + c1 * w.y + c2 * w.z + c3 * w.w) * min(1.0, 1.0 / (dot(w, vec4<f32>(1.0)) + 0.001));
        out.a = m;
        return out;
    }
    return center;
}

// FixUpCoCFS: tex0 = (rgb, coc), tex1 = coc_mask.
@fragment
fn fs_dof_fixup_coc(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let t = 1.0 / dims;
    let center = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0);
    let threshold = textureSampleLevel(tex1, samp_lin, uv_clamp(in.uv, dims), 0.0).r * 1.05;
    let offsets = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var sum = center.rgb;
    var count = 1.0;
    for (var i = 0; i < 4; i++) {
        let uv = uv_clamp(in.uv + offsets[i] * t, dims);
        let col = textureSampleLevel(tex0, samp_lin, uv, 0.0).rgb;
        let mask = textureSampleLevel(tex1, samp_lin, uv, 0.0).r;
        // Preserve exact Studio FixUpCoCFS register flow (index 1 tests col.r).
        if (select(mask, col.r, i == 1) <= threshold) {
            sum += col;
            count += 1.0;
        }
    }
    return vec4<f32>(sum / count, center.a);
}

// BokehFarCoCFS: 45-tap far bokeh with CoC rejection.
// tex0 = fixup (rgb, far_coc), tex1 = far_coc_mask. u.extra.x = far_intensity.
@fragment
fn fs_dof_bokeh_far(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let center = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0);
    let coc2 = center.a * center.a;
    let radius = coc2 * u.extra.x;
    let scale = vec2<f32>(dims.y / dims.x, 1.0) * 0.014;
    var sum = center * coc2;
    var weight = coc2;
    for (var i = 0; i < 45; i++) {
        let off = DOF_DISK[i] * radius;
        let uv = in.uv + off * scale;
        if (all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0))) {
            let cu = uv_clamp(uv, dims);
            let tap = textureSampleLevel(tex0, samp_lin, cu, 0.0);
            let mask = textureSampleLevel(tex1, samp_lin, cu, 0.0).r;
            let avg_coc = 0.5 * (mask + tap.a);
            if (avg_coc * avg_coc >= dot(off, off) * 0.995) {
                sum += tap * avg_coc;
                weight += avg_coc;
            }
        }
    }
    return sum * clamp(1.0 / (weight + 0.001), 0.0, 1.0);
}

// BokehNearCoCFS: 45-tap near bokeh.
// tex0 = fixup (rgb, near_coc), tex1 = near_coc_mask. u.extra.x = near_intensity.
@fragment
fn fs_dof_bokeh_near(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let center = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0);
    let mask = textureSampleLevel(tex1, samp_lin, uv_clamp(in.uv, dims), 0.0).r;
    let mask2 = mask * mask;
    let radius = mask2 * u.extra.x;
    let scale = vec2<f32>(dims.y / dims.x, 1.0) * 0.014;
    var sum = center;
    var weight = mask2;
    for (var i = 0; i < 45; i++) {
        let uv = in.uv + (DOF_DISK[i] * radius) * scale;
        if (all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0))) {
            let tap = textureSampleLevel(tex0, samp_lin, uv_clamp(uv, dims), 0.0);
            sum += tap * tap.a;
            weight += tap.a;
        }
    }
    return sum * clamp(1.0 / (weight + 0.001), 0.0, 1.0);
}

// TentFilterFS: 4-tap +/- 0.5 texel box tent filter.
@fragment
fn fs_dof_tent(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let h = 0.5 / dims;
    let s0 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>(-h.x, -h.y), dims), 0.0);
    let s1 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>( h.x, -h.y), dims), 0.0);
    let s2 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>(-h.x,  h.y), dims), 0.0);
    let s3 = textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv + vec2<f32>( h.x,  h.y), dims), 0.0);
    return (s0 + s1 + s2 + s3) * 0.25;
}

// ------------------------------------------------------------------ ImageProcess
// Exact Studio ImageProcessFXCompositionLinearFS / ImageProcessBlurFXCompositionLinearFS.
// tex0 = scene (post-DOF linear HDR), tex1 = bloom, tex2 = sunrays, tex3 = blur (1/4-res encoded).

fn apply_tonemap(x: vec3<f32>) -> vec3<f32> {
    let y = max(x, vec3<f32>(0.0));
    let mode = i32(u.p0.z + 0.5);
    if (mode == 2) {
        // Legacy no-tone / debug view: sqrt(clamp(y, 0, 1))
        return sqrt(clamp(y, vec3<f32>(0.0), vec3<f32>(1.0)));
    }
    if (mode == 1) {
        // Studio Retro tonemapper: (0.88, 0.16, 1.34, 0.028) * 2.18
        let ax = y * 0.88;
        return ((y * (ax + vec3<f32>(0.16))) / (y * (ax + vec3<f32>(1.34)) + vec3<f32>(0.028))) * 2.18;
    }
    // Studio Default tonemapper: (0.3, 0.4, 0.38, 0.12) * 1.0
    let ax = y * 0.3;
    return (y * (ax + vec3<f32>(0.4))) / (y * (ax + vec3<f32>(0.38)) + vec3<f32>(0.12));
}

@fragment
fn fs_image_process(in: PostOut) -> @location(0) vec4<f32> {
    let raw = textureSampleLevel(tex0, samp_lin, in.uv, 0.0).rgb;
    let flags = u32(u.p0.w + 0.5);
    if ((flags & 1u) != 0u) {
        // TERRAIN_CAPTURE_ID bypass
        return vec4<f32>(raw, 1.0);
    }
    if ((flags & 2u) != 0u) {
        // Raw debug views: sqrt(clamp(raw * exposure, 0, 1))
        let dbg = sqrt(clamp(max(raw, vec3<f32>(0.0)) * u.p0.x, vec3<f32>(0.0), vec3<f32>(1.0)));
        return vec4<f32>(dbg, 1.0);
    }

    let scene_lin = scene_post_exp_linear(raw);
    let bloom = textureSampleLevel(tex1, samp_lin, in.uv, 0.0).rgb;
    let sunrays = textureSampleLevel(tex2, samp_lin, in.uv, 0.0).rgb;
    var x = scene_lin + bloom + sunrays * u.p0.x;

    if (u.p0.y > 0.0) {
        let blur_dims = vec2<f32>(textureDimensions(tex3));
        let blur_enc = textureSampleLevel(tex3, samp_lin, uv_clamp(in.uv, blur_dims), 0.0).rgb;
        x = mix(x, blur_enc * blur_enc * 4.0, u.p0.y);
    }

    let tm = apply_tonemap(x);
    let cc = vec3<f32>(
        dot(tm, u.cc_r.xyz) + u.cc_r.w,
        dot(tm, u.cc_g.xyz) + u.cc_g.w,
        dot(tm, u.cc_b.xyz) + u.cc_b.w,
    );
    return vec4<f32>(clamp(cc, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
