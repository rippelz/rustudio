// WGSL / shader_mobile.wgsl / wgpu 30 / Android compatibility profile
// Bounded diffuse shading: no material detiling, refraction or PBR convolution.
fn mobile_light(world: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let n = normalize(normal);
    var light = g.ambient.rgb + g.outdoor.rgb + g.sun_color.rgb * g.sun_dir.w * max(dot(n, g.sun_dir.xyz), 0.0);
    let count = min(min(u32(g.params.x), arrayLength(&lights)), MOBILE_LIGHT_LIMIT);
    for (var i = 0u; i < count; i++) {
        let delta = lights[i].pos_range.xyz - world;
        let distance = max(length(delta), 0.001);
        let falloff = max(1.0 - distance / max(lights[i].pos_range.w, 0.001), 0.0);
        light += lights[i].color.rgb * falloff * falloff * max(dot(n, delta / distance), 0.0);
    }
    return light;
}

@fragment
fn fs_part(in: PartOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let t = textureSample(tex, tex_sampler, in.uv);
    let mode = u32(in.extra.w + 0.5);
    var base = in.color.rgb * in.vtint.rgb;
    var alpha = in.color.a;
    if (alpha < 0.999) { alpha *= in.vtint.a; }
    if (mode == 1u) { base = mix(base, t.rgb * in.emissive_albedo * in.vtint.rgb, t.a); }
    if (mode == 2u || mode == 5u) { base *= t.rgb; }
    if (mode == 3u || mode == 4u) { base = t.rgb * in.emissive_albedo * in.vtint.rgb; }
    if (mode == 2u || (mode >= 3u && alpha < 0.999)) { alpha *= t.a; }
    if (mode == 4u && in.color.a >= 0.999 && t.a < 0.5) { discard; }
    let n = select(-in.normal, in.normal, front);
    var color = base * mobile_light(in.world, n) + in.emissive;
    if ((u32(in.extra.x + 0.5) & 7u) == 1u) { color = base * NEON_GLOW; }
    if (g.fog.z > 0.5) {
        let fog = fog_amount(length(in.world - g.cam_pos.xyz));
        color = mix(color, fog_rgb_at(in.world), fog);
    }
    return vec4<f32>(color * alpha, alpha);
}

@fragment
fn fs_terrain(in: TerrainOut) -> @location(0) vec4<f32> {
    let a = tp.p[in.palette.x * 4u + 3u].rgb;
    let b = tp.p[in.palette.y * 4u + 3u].rgb;
    let c = tp.p[in.palette.z * 4u + 3u].rgb;
    let base = a * in.bary.x + b * in.bary.y + c * in.bary.z;
    return vec4<f32>(base * base * mobile_light(in.world, in.normal), 1.0);
}

@fragment
fn fs_water(in: PartOut) -> @location(0) vec4<f32> {
    // Occlusion uses the existing scene depth attachment, not a sampled MSAA image.
    let uv = clamp(in.clip.xy / wp.view.xy, vec2<f32>(0.0), vec2<f32>(1.0));
    let background = textureSampleLevel(scene_color, water_sampler, uv, 0.0).rgb;
    let color = in.color.rgb * mobile_light(in.world, in.normal);
    return vec4<f32>(mix(background, color, clamp(in.color.a, 0.0, 1.0)), 1.0);
}

@fragment
fn fs_post(in: PostOut) -> @location(0) vec4<f32> {
    let color = max(textureSampleLevel(hdr_tex, hdr_sampler, in.uv, 0.0).rgb * g.params.y, vec3<f32>(0.0));
    return vec4<f32>(sqrt(color / (vec3<f32>(1.0) + color)), 1.0);
}
