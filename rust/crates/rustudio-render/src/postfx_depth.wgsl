// Depth-reading post-processing entry points (compiled for MSAA and 1x depth).
// Prepended with postfx.wgsl at pipeline creation.

@group(2) @binding(0) var fx_depth_ms: texture_depth_multisampled_2d;

fn fx_depth_at(px: vec2<i32>) -> f32 {
    let size = vec2<i32>(textureDimensions(fx_depth_ms));
    let p = clamp(px, vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(fx_depth_ms, p, 0);
}

fn fx_depth_uv(uv: vec2<f32>) -> f32 {
    let size = vec2<i32>(textureDimensions(fx_depth_ms));
    let p = clamp(vec2<i32>(uv * vec2<f32>(size)), vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(fx_depth_ms, p, 0);
}

fn fx_distance(uv: vec2<f32>) -> f32 {
    let z = u.p1.w / max(fx_depth_uv(uv), 1e-8);
    let ray = vec2<f32>(
        (uv.x * 2.0 - 1.0) / (2.0 * max(u.p2.x, 1e-6)),
        (1.0 - uv.y * 2.0) / (2.0 * max(u.p2.y, 1e-6)),
    );
    return min(z * sqrt(1.0 + dot(ray, ray)), 500.0);
}

fn dof_far_coc_uv(uv: vec2<f32>) -> f32 {
    let d = fx_distance(uv);
    return clamp((d - u.dof.x - u.dof.y - 0.5) / d, 0.0, 1.0);
}

fn dof_near_coc_uv(uv: vec2<f32>) -> f32 {
    let d = fx_distance(uv);
    return clamp((u.dof.x - u.dof.y + 0.5 - d) / d, 0.0, 1.0);
}

// 1/4-res SunRays sky depth downsample (DownSample4x4MaxFS):
// Four 2x2 quadrant averages of linearized depth (sky = 1.0, z*0.002 <= 1.0), taking max.
@fragment
fn fs_sunrays_depth(in: PostOut) -> @location(0) vec4<f32> {
    let full = vec2<i32>(textureDimensions(fx_depth_ms));
    let base = vec2<i32>(in.pos.xy) * 4;
    var best = 0.0;
    for (var qy = 0; qy < 2; qy++) {
        for (var qx = 0; qx < 2; qx++) {
            var avg = 0.0;
            for (var py = 0; py < 2; py++) {
                for (var px = 0; px < 2; px++) {
                    let d = fx_depth_at(base + vec2<i32>(qx * 2 + px, qy * 2 + py));
                    let lin = select(min((u.p1.w / d) * 0.002, 1.0), 1.0, d <= 1e-6);
                    avg += lin;
                }
            }
            best = max(best, avg * 0.25);
        }
    }
    return vec4<f32>(best, 0.0, 0.0, 1.0);
}

// Full-res prep for CheapBokehFS (< 0.22): u.extra.x < 0.5 = far, >= 0.5 = near.
@fragment
fn fs_dof_cheap_prep(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let c = scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, dims), 0.0).rgb);
    let coc = select(dof_far_coc_uv(in.uv), dof_near_coc_uv(in.uv), u.extra.x >= 0.5);
    return vec4<f32>(c, coc);
}

// Horizontal 2x downsample + Far CoC (DOFDownSampleAndMaskFarFieldColorHorizontal2xFS).
// u.extra.x = 0.5 * dx (half-step in UV).
@fragment
fn fs_dof_far_down_h(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let dx = u.extra.x;
    let uv0 = uv_clamp(in.uv - vec2<f32>(dx, 0.0), dims);
    let uv1 = uv_clamp(in.uv + vec2<f32>(dx, 0.0), dims);
    let c0 = scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv0, 0.0).rgb);
    let c1 = scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv1, 0.0).rgb);
    let coc = max(dof_far_coc_uv(uv0), dof_far_coc_uv(uv1));
    return vec4<f32>((c0 + c1) * 0.5, coc);
}

// Horizontal 2x or 4x downsample + Near CoC (DOFDownSampleAndMaskNearFieldColorHorizontal{2x,4x}FS).
// u.extra.x = step_dx, u.extra.y = taps (2.0 or 4.0).
@fragment
fn fs_dof_near_down_h(in: PostOut) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(tex0));
    let dx = u.extra.x;
    let taps = i32(u.extra.y + 0.5);
    var color = vec3<f32>(0.0);
    var coc = 0.0;
    if (taps == 2) {
        let uv0 = uv_clamp(in.uv - vec2<f32>(0.5 * dx, 0.0), dims);
        let uv1 = uv_clamp(in.uv + vec2<f32>(0.5 * dx, 0.0), dims);
        color = (scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv0, 0.0).rgb)
               + scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv1, 0.0).rgb)) * 0.5;
        coc = max(dof_near_coc_uv(uv0), dof_near_coc_uv(uv1));
    } else {
        for (var i = 0; i < 4; i++) {
            let uv = uv_clamp(in.uv + vec2<f32>((f32(i) - 1.5) * dx, 0.0), dims);
            color += scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv, 0.0).rgb) * 0.25;
            coc = max(coc, dof_near_coc_uv(uv));
        }
    }
    return vec4<f32>(color, coc);
}

// DOFResolveFarFS: tex0 = full-res scene, tex1 = half-res blurred far tent.
// Studio sets CB1[2].z = 0 in DOFResolveFarFS (single bilinear sample of tent-filtered bokeh).
@fragment
fn fs_dof_resolve_far(in: PostOut) -> @location(0) vec4<f32> {
    let full_dims = vec2<f32>(textureDimensions(tex0));
    let blur_dims = vec2<f32>(textureDimensions(tex1));
    let orig = scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, full_dims), 0.0).rgb);
    let far_coc = dof_far_coc_uv(in.uv);
    let a = min(far_coc * 1.5, 1.0);
    let w = select(0.0, a * a, u.dof.w > 0.0 && far_coc > 0.0);
    let blurred = max(textureSampleLevel(tex1, samp_lin, uv_clamp(in.uv, blur_dims), 0.0).rgb, vec3<f32>(0.0));
    return vec4<f32>(mix(orig, blurred, w), 1.0);
}

// DOFResolveNearFS: tex0 = full-res scene (after far), tex1 = reduced-res blurred near tent.
// 9-tap tent at +/- 4 full-res pixels, blend weight = min(max(blurred.a, near_coc) * 1.5, 1.0)^2.
@fragment
fn fs_dof_resolve_near(in: PostOut) -> @location(0) vec4<f32> {
    let full_dims = vec2<f32>(textureDimensions(tex0));
    let blur_dims = vec2<f32>(textureDimensions(tex1));
    let orig = scene_clamped_pre_exp(textureSampleLevel(tex0, samp_lin, uv_clamp(in.uv, full_dims), 0.0).rgb);
    let step = vec2<f32>(4.0) / full_dims;
    var blurred = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let weight = select(1.0, 2.0, x == 0) * select(1.0, 2.0, y == 0);
            let uv = uv_clamp(in.uv + vec2<f32>(f32(x), f32(y)) * step, blur_dims);
            blurred += textureSampleLevel(tex1, samp_lin, uv, 0.0) * (weight * 0.0625);
        }
    }
    let near_coc = dof_near_coc_uv(in.uv);
    let amount = min(max(blurred.a, near_coc) * 1.5, 1.0);
    return vec4<f32>(mix(orig, max(blurred.rgb, vec3<f32>(0.0)), amount * amount), 1.0);
}
