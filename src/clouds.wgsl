// ---------------------------------------------------------------- clouds
// Terrain.Clouds, ported from Studio's Clouds*FS (shaders_glsl3.pack and
// shaders_vulkan_desktop.pack, same code; src/clouds.rs has the passes and
// where each constant comes from). Appended to shader.wgsl's source, so `g`
// (Globals) and the sky lookups are shared. Units: the cloud shaders work in
// km (studs × 0.00028) around a planet of radius 971 km whose surface is at
// the camera's y = 0; the cloud layer is the shell 973 .. 975.5 km.

struct CloudsU {
    dir_dist: vec4<f32>,  // CloudsParams.DirDist: xyz wind offset (km), w time / 1000
    cdq: vec4<f32>,       // CloudsParams.CoverDensityQuality: Cover², Density, steps, -
    color: vec4<f32>,     // CloudsParams.Color: Clouds.Color
    detail: vec4<f32>,    // CloudsParams.DetailScale
    fogp: vec4<f32>,      // Studio's FogParams (CB0[18])
    quarter: vec4<f32>,   // the raymarch target: size, 1 / size
    half: vec4<f32>,      // the filtered target: size, 1 / size
    full: vec4<f32>,      // the view: size, 1 / size
    cover: vec4<f32>,     // sun cover Params1: x 40, y disk radius 10 (texels), z 1 / BSM size
};

@group(3) @binding(0) var<uniform> cu: CloudsU;
@group(3) @binding(1) var cl_dist: texture_3d<f32>;     // rbxasset://sky/clouds.dds (CloudsDistanceFieldTexture)
@group(3) @binding(2) var cl_adv: texture_2d<f32>;      // sky/cloudAdvection.dds (AdvectionTexTexture)
@group(3) @binding(3) var cl_detail: texture_3d<f32>;   // sky/cloudDetail3D.dds (DetailTex3DTexture)
@group(3) @binding(4) var cl_noise: texture_2d<f32>;    // sky/bn.dds (BlueNoiseTexTexture)
@group(3) @binding(5) var cl_bsm: texture_2d<f32>;      // the Beer shadow map (CloudsBsmFS)
@group(3) @binding(6) var cl_rep: sampler;              // linear, repeat
@group(3) @binding(7) var cl_dist_s: sampler;           // linear, repeat in x / z, clamped in height
@group(3) @binding(8) var cl_in: texture_2d<f32>;       // the previous step's clouds
@group(3) @binding(9) var cl_clamp: sampler;            // linear, clamped
@group(3) @binding(10) var cl_scene: texture_2d<f32>;   // the scene before the clouds (CompFS blends over it)
@group(3) @binding(11) var cl_depth: texture_depth_multisampled_2d;

// CloudsWindFS's phase table (48 entries over (0.5 + 0.5 cos θ) × 46.5).
const CL_PHASE = array<f32, 48>(15.717910, 12.894090, 7.719984, 5.107763, 3.957092, 3.128542, 2.467976, 1.940606, 1.519268, 1.172060, 0.906811, 0.696639, 0.530980, 0.403742, 0.304674, 0.230043, 0.172465, 0.128985, 0.096062, 0.072035, 0.054671, 0.041893, 0.032902, 0.026549, 0.022501, 0.020169, 0.018803, 0.018286, 0.018086, 0.022949, 0.028057, 0.033098, 0.033073, 0.036000, 0.051087, 0.108068, 0.172808, 0.207800, 0.132369, 0.111766, 0.110401, 0.105231, 0.104189, 0.108498, 0.127474, 0.207567, 0.319779, 0.467940);

// CloudsSunCoverFS's 28 taps: the centre, 10 at radius 0.54, 17 on the unit circle.
const CL_DISK = array<vec2<f32>, 27>(
    vec2<f32>(0.54, 0.0), vec2<f32>(0.4368692, 0.31740406), vec2<f32>(0.16686918, 0.51357054), vec2<f32>(-0.16686918, 0.51357054), vec2<f32>(-0.4368692, 0.31740406),
    vec2<f32>(-0.54, 0.0), vec2<f32>(-0.4368692, -0.31740406), vec2<f32>(-0.16686918, -0.51357054), vec2<f32>(0.16686918, -0.51357054), vec2<f32>(0.4368692, -0.31740406),
    vec2<f32>(1.0, 0.0), vec2<f32>(0.9324722, 0.36124167), vec2<f32>(0.7390089, 0.67369562), vec2<f32>(0.44573835, 0.8951633), vec2<f32>(0.09226836, 0.99573416),
    vec2<f32>(-0.27366298, 0.96182567), vec2<f32>(-0.6026346, 0.7980172), vec2<f32>(-0.85021716, 0.52643216), vec2<f32>(-0.9829731, 0.18374951), vec2<f32>(-0.9829731, -0.18374951),
    vec2<f32>(-0.85021716, -0.52643216), vec2<f32>(-0.6026346, -0.7980172), vec2<f32>(-0.27366298, -0.96182567), vec2<f32>(0.09226836, -0.99573416), vec2<f32>(0.44573835, -0.8951633),
    vec2<f32>(0.7390089, -0.67369562), vec2<f32>(0.9324722, -0.36124167));

struct ClOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_clouds(@builtin(vertex_index) i: u32) -> ClOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: ClOut;
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    return o;
}

/// The view ray through pixel `px` of a target of `size` covering the view
/// (Studio's RayFrame: FoV.zw × (fragCoord × FoV.xy − 1) along ViewRight /
/// ViewUp, minus ViewDir: the same pinhole, so the camera's own inverse).
fn cl_ray(px: vec2<f32>, size: vec2<f32>) -> vec3<f32> {
    let ndc = vec2<f32>(2.0 * px.x / size.x - 1.0, 1.0 - 2.0 * px.y / size.y);
    let far = g.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    return normalize(far.xyz / far.w);
}

/// Distances (km) along `dir` from the camera to the spheres of radius² `r2`
/// about the planet's centre (971 km below y = 0): the far roots.
fn cl_shell(dir: vec3<f32>, cam: vec3<f32>, r2: vec2<f32>) -> vec2<f32> {
    var o = cam;
    o.y = cam.y + 971.0;
    let b = dot(dir, o);
    let b2 = 2.0 * b;
    return (vec2<f32>(b * -2.0) + sqrt(vec2<f32>(b2 * b2) - (vec2<f32>(dot(o, o)) - r2) * 4.0)) * 0.5;
}

const CL_LAYER: vec2<f32> = vec2<f32>(946729.0, 951600.25); // 973², 975.5²

fn cl_cam() -> vec3<f32> {
    return g.cam_pos.xyz * 0.00028;
}

/// Lamp0Dir: the direction the light travels.
fn cl_ldir() -> vec3<f32> {
    return -normalize(g.sun_dir.xyz);
}

/// The cloud field at `p` (x, z in km, y 0..1 up the layer): x the distance
/// field plus detail, y the density (Density, thinning above 6000 studs).
fn cl_field(p: vec3<f32>, lod: f32) -> vec2<f32> {
    let a = textureSampleLevel(cl_adv, cl_rep, p.xz * 0.125 + vec2<f32>(0.0, cu.dir_dist.w * -17.5), lod).xy * 2.0 - vec2<f32>(1.0);
    let adv = vec3<f32>(a.x, 0.0, a.y);
    var q = p;
    q.x = p.x + cu.dir_dist.w * 39.0625;
    let dens = cu.cdq.y * clamp(1.0 + 0.001 * (6000.0 - g.cam_pos.y), 0.0, 1.0);
    let m = mix(vec2<f32>(0.15, 0.07), vec2<f32>(0.075, 0.05), vec2<f32>(dens));
    let det = vec2<f32>(cu.detail.x, 0.35 * cu.detail.x) * (vec2<f32>(
        textureSampleLevel(cl_detail, cl_rep, q * (0.5 * cu.detail.y) + adv * m.x, lod).x,
        textureSampleLevel(cl_detail, cl_rep, q * (2.0 * cu.detail.y) + adv * m.y, lod).x) * 2.0 - vec2<f32>(1.0));
    let d = textureSampleLevel(cl_dist, cl_dist_s, ((p + adv * 0.75) * vec3<f32>(0.03125, 1.0, 0.03125)).xzy, lod).x;
    return vec2<f32>(d + (det.x + det.y), dens);
}

/// CloudsBsmFS: the Beer shadow map, 512² over the 32 km tile. Toward the
/// light through the layer (13 steps): x 1 − ½ × the optical depth through
/// it, y its mean per step, z 1 − ½ × the first step's (1.5 = none), w the
/// optical depth back from the far side.
@fragment
fn fs_clouds_bsm(in: ClOut) -> @location(0) vec4<f32> {
    let uv = in.pos.xy * cu.cover.z;
    var l = -cl_ldir();
    l.y = select(max(0.3, l.y), min(-0.3, l.y), l.y < 0.0);
    l = normalize(l);
    let step_len = 0.083333336 / max(abs(l.y), 1e-4);
    let p0 = vec3<f32>(32.0 * uv.x, select(0.015625, 0.984375, l.y <= 0.0), 32.0 * uv.y);
    let lod = f32(cu.cdq.z <= 9.0);
    let thr = 1.04 * cu.cdq.x;
    var first = -1.0;
    var sum = 0.0;
    var scaled = 0.0;
    var tr = 1.0;
    var n = 1;
    for (var i = 0; i <= 12; i++) {
        let f = cl_field(p0 + l * f32(i) * step_len, lod);
        if (f.x < thr && tr > 1e-5) {
            let s = f.y * (thr - f.x);
            sum += s;
            scaled = sum * 1.4880953;
            if (first < 0.0) {
                first = scaled;
            }
            tr *= exp2(s * -0.12022459);
            n += 1;
        }
    }
    var back = 0.0;
    if (n > 1) {
        let pe = p0 + l * 12.0 * step_len;
        var t2 = tr;
        var acc = 0.0;
        for (var i = 0; i <= 12; i++) {
            let f = cl_field(pe - l * f32(i) * step_len, lod);
            if (f.x < thr) {
                let s = f.y * (thr - f.x);
                let t3 = t2 * exp2(s * -0.12022459);
                acc += s;
                if (t3 <= 1e-5) {
                    break;
                }
                t2 = t3;
            }
        }
        back = acc;
    }
    return vec4<f32>(1.0 - 0.5 * scaled, sum / f32(n), 1.0 - 0.5 * first, back);
}

/// CloudsSunCoverFS: how much sun gets through where the sun's ray from the
/// camera leaves the layer: 1 − 40 × (the BSM's mean optical depth over a
/// 10-texel disk)². The CPU reads it back (clouds.rs `sun_cover`).
@fragment
fn fs_clouds_cover(in: ClOut) -> @location(0) vec4<f32> {
    let cam = cl_cam();
    let l = -cl_ldir();
    let t = cl_shell(l, cam, CL_LAYER);
    let c = (((cam + l * t.x) + l * (t.y - t.x)) + (cu.dir_dist.xyz * cu.dir_dist.w) * 50.0).xz * 0.03125;
    let r = cu.cover.y * cu.cover.z;
    var sum = textureSampleLevel(cl_bsm, cl_rep, c, 0.0).y;
    for (var i = 0; i < 27; i++) {
        sum += textureSampleLevel(cl_bsm, cl_rep, c + CL_DISK[i] * r, 0.0).y;
    }
    let avg = sum * 0.035714287;
    return vec4<f32>(max(0.0, 1.0 - cu.cover.x * (avg * avg)));
}

/// CloudsWindFS at a quarter of the view: the march through the layer
/// (CoverDensityQuality.z steps), lit by the sun through the BSM and by the
/// sky, faded into the sky toward the horizon. Premultiplied colour, alpha.
@fragment
fn fs_clouds_wind(in: ClOut) -> @location(0) vec4<f32> {
    let dir = cl_ray(in.pos.xy, cu.quarter.xy);
    if (dir.y < 0.0) {
        discard;
    }
    let cam = cl_cam();
    let t = cl_shell(dir, cam, CL_LAYER);
    let t0 = t.x;
    let span = t.y - t0;
    let ldir = cl_ldir();
    let mu = dot(ldir, dir);
    let fi = (0.5 + 0.5 * mu) * 46.5;
    let ii = clamp(i32(fi), 0, 46);
    let ph = mix(CL_PHASE[ii], CL_PHASE[ii + 1], fract(fi));
    let ph8 = ph * 0.125;
    var entry = cam + dir * t0;
    entry.y = 0.0;
    let steps = i32(cu.cdq.z);
    let nf = f32(steps);
    let t_step = (1.0 / nf) * span;
    let p_step = (1.0 / max(dir.y, 1e-4)) / nf;
    // (Studio samples the noise at gl_FragCoord in UV units, i.e. one texel.)
    let bn = textureSampleLevel(cl_noise, cl_rep, in.pos.xy, 0.0).x;
    var p = entry + (dir * span) * (0.15 + 0.95 * bn);
    p.y = 0.0;
    p += cu.dir_dist.xyz;
    let lamp0 = g.sun_color.rgb * g.sun_dir.w;
    let d2 = dir * dir;
    let neg = select(vec3<f32>(0.0), d2, dir < vec3<f32>(0.0));
    let pos = d2 - neg;
    // AmbientCube[0..5] + [6..11] toward the ray (g.env: the sky cube with the indoor share).
    let cube = g.env[0].rgb * pos.x + g.env[2].rgb * pos.y + g.env[4].rgb * pos.z
        + g.env[1].rgb * neg.x + g.env[3].rgb * neg.y + g.env[5].rgb * neg.z;
    let fade_sun = clamp(exp2(ldir.y), 0.0, 1.0);
    let lod0 = f32(cu.cdq.z <= 9.0);
    var col = vec3<f32>(0.0);
    var tr = 1.0;
    var acc = 0.0;
    var ti = t0;
    var hit = false;
    var end_tr = 1.0;
    var end_acc = 0.0;
    var done = false;
    for (var i = 0; i < steps; i++) {
        let lod = lod0 + pow(ti, 3.0) * 2.631579e-5;
        let f = cl_field(p, lod);
        let s = (f.y * 50.0) * (cu.cdq.x - f.x);
        let inside = f.x < cu.cdq.x;
        if (inside) {
            let b = textureSampleLevel(cl_bsm, cl_rep, p.xz * 0.03125, 0.0);
            let od = min(b.z * 0.5, (b.y + 0.35 * bn) * max(0.0, (1.0 - (f32(i) / nf + f.x / nf)) - b.x));
            let acc2 = acc + s;
            let grad = mix(vec3<f32>(0.15 + 0.3 * p.y), mix(g.sky_grad[1].rgb, g.sky_grad[0].rgb, vec3<f32>(p.y)), vec3<f32>(fade_sun));
            let amb = max(grad * cube, g.outdoor.rgb + g.ambient.rgb);
            let lobes = ((0.29 * exp2(od * -23.70142)) + ((0.169 * mix(0.08, 1.0, smoothstep(0.0, 1.5, abs(mu)))) * exp2(od * -659.51776)))
                + (((1.0 - f.y) * 320.0) * clamp(mu * ph8, 0.0, 1.0)) * exp2(od * -10.0);
            let sun = lamp0 * ((4.0 * (lobes * (ph * 0.02925) + 0.092 * exp2(b.w * -2.325))) + (0.0425 + ((s / nf) * 0.135) * p.y));
            let k = (1.442695 / nf) * s;
            let src = ((amb + sun * 2.1) * cu.color.rgb) * k;
            let e = exp2(-k);
            col += ((src - src * e) * tr) / vec3<f32>(select(k, 1e-4, k < 1e-4));
            let tr2 = tr * e;
            if (tr2 <= 1e-5) {
                end_tr = 0.0;
                end_acc = acc2;
                done = true;
                break;
            }
            tr = tr2;
            acc = acc2;
        }
        hit = hit || inside;
        ti += t_step;
        p += dir * p_step;
    }
    if (!done) {
        end_tr = tr;
        end_acc = acc;
    }
    let alpha = 1.0 - end_tr;
    col += ((((lamp0 * clamp(1.0 - (0.08 * end_acc) / nf, 0.0, 1.0)) * 2.0) * clamp(-mu, 0.0, 1.0)) * ph8 * alpha)
        * exp2(0.11 - 0.035 * end_acc) * cu.color.rgb;
    if (!hit) {
        discard;
    }
    // Toward the horizon the clouds fade into the sky (FogParams.z per km²).
    let fogf = pow(max(exp2((cu.fogp.z * 3.5714285) * (t0 * t0)), 1e-4), 0.125);
    var sky: vec3<f32>;
    if (cu.fogp.w != 0.0) {
        sky = g.fog_color.rgb;
    } else {
        sky = sky_color_lod(dir, max(cu.fogp.y, fogf) * 5.0);
    }
    return vec4<f32>(mix(sky, col, vec3<f32>(fogf)), alpha * max(fogf, cu.fogp.y));
}

fn cl_tent(uv: vec2<f32>, texel: vec2<f32>) -> vec4<f32> {
    let o = vec4<f32>(1.0, 1.0, -1.0, 0.0) * vec4<f32>(texel, texel);
    return (textureSampleLevel(cl_in, cl_clamp, uv, 0.0) * 4.0
        + textureSampleLevel(cl_in, cl_clamp, uv - o.xy, 0.0)
        + textureSampleLevel(cl_in, cl_clamp, uv - o.wy, 0.0) * 2.0
        + textureSampleLevel(cl_in, cl_clamp, uv - o.zy, 0.0)
        + textureSampleLevel(cl_in, cl_clamp, uv + o.zw, 0.0) * 2.0
        + textureSampleLevel(cl_in, cl_clamp, uv + o.xw, 0.0) * 2.0
        + textureSampleLevel(cl_in, cl_clamp, uv + o.zy, 0.0)
        + textureSampleLevel(cl_in, cl_clamp, uv + o.wy, 0.0) * 2.0
        + textureSampleLevel(cl_in, cl_clamp, uv + o.xy, 0.0)) * 0.0625;
}

/// CloudsUpsampleNoHistoryFS (what CloudsReprojectFilterFS converges to for
/// a still camera) at half the view: a tent over the march, faded out past
/// 227 km (CloudsDistFS's distance to the layer's far side).
@fragment
fn fs_clouds_up(in: ClOut) -> @location(0) vec4<f32> {
    let dir = cl_ray(in.pos.xy, cu.half.xy);
    if (dir.y < -0.5) {
        discard;
    }
    let uv = in.pos.xy * cu.half.zw;
    var c = cl_tent(uv, cu.half.zw);
    let cam = cl_cam();
    let t = cl_shell(dir, cam, CL_LAYER);
    var far = (cam + dir * t.x) + dir * (t.y - t.x);
    far.y = 0.0;
    let dist = length(far - cam) + (t.y - t.x);
    let k = clamp((dist - 227.45) * 0.09756097, 0.0, 1.0);
    c.w = mix(c.w, 0.0, k * k);
    return c;
}

/// CloudsCompFS over the full view where nothing was drawn: alpha w², the
/// colour blended in Studio's stored form sqrt(c × Exposure) (its
/// R10G10B10A2 scene), skipped below w 0.12.
@fragment
fn fs_clouds_comp(in: ClOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.pos.xy);
    if (textureLoad(cl_depth, px, 0) > 0.0) {
        discard;
    }
    let dir = cl_ray(in.pos.xy, cu.full.xy);
    if (dir.y < -0.01) {
        discard;
    }
    let uv = in.pos.xy * cu.full.zw;
    let c = cl_tent(uv, cu.full.zw);
    if (!(c.w > 0.12)) {
        discard;
    }
    let e = g.params.y * 0.25;
    let ec = sqrt(clamp(c.rgb * e, vec3<f32>(0.0), vec3<f32>(1.0)));
    let a = c.w * c.w;
    let dst = textureLoad(cl_scene, px, 0).rgb;
    let ed = sqrt(clamp(dst * e, vec3<f32>(0.0), vec3<f32>(1.0)));
    let o = ec * a + ed * (1.0 - a);
    return vec4<f32>(o * o / max(e, 1e-6), 1.0);
}
