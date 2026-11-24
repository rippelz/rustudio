//! Roblox Studio-faithful post-processing pipeline (`postfx.wgsl` + `postfx_depth.wgsl`).
//!
//! Pass order matches Studio 0.748 (`SceneManager::renderView`):
//! 1. DepthOfField (Far then Near, directly modifying the scene target; tier >= 3 / L16+).
//! 2. Bloom / Neon glow (HQ 1/2-res pyramid at tier >= 2 / L12+, LQ 1/8-res at L6..11, off L1..5).
//! 3. SunRays (1/4-res depth mask -> SunRays{6,9,12}FS -> RadialBlur{3,6,9,12}FS; tier >= 3 / L16+).
//! 4. BlurEffect + FXCompositingLinearFS (1/4-res encoded sqrt space; all quality levels).
//! 5. ImageProcess (Default or Retro rational tonemapper + Studio 3x4 ColorCorrection matrix).

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use glam::Vec3;

/// Studio's exact cutoff between CheapBokehFS (< 0.22) and the reduced-res DoF chain (>= 0.22).
pub const DOF_CHEAP_BELOW: f32 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TonemapMode {
    /// Studio Default rational curve: (0.3, 0.4, 0.38, 0.12) * 1.0.
    #[default]
    Default = 0,
    /// Studio Retro rational curve: (0.88, 0.16, 1.34, 0.028) * 2.18.
    /// Used for `ColorGradingEffect.TonemapperPreset == Retro` or `Technology == Compatibility`.
    Retro = 1,
    /// Raw `sqrt(clamp(x, 0, 1))` for legacy places without saved Technology or debug views.
    RawSqrt = 2,
}

/// Complete per-frame post-processing parameters computed by `lighting::globals`
/// and gated by `quality::Profile` in `game_view.rs`.
#[derive(Clone, Copy, Debug)]
pub struct PostFxParams {
    pub exposure: f32,
    pub tonemap_mode: TonemapMode,
    pub raw_debug: bool,
    pub terrain_capture_id: bool,
    /// Studio post-FX quality tier: 0 (L1..7), 1 (L8..11), 2 (L12..15), 3 (L16..21).
    pub post_fx_tier: u8,
    /// Studio `bloom_frm_flag` (L6..21, or user performance setting).
    pub bloom_allowed: bool,
    /// Whether an enabled `BloomEffect` instance exists (vs default Neon glow).
    pub bloom_effect_enabled: bool,
    pub bloom_intensity: f32,
    pub bloom_size: f32,
    pub bloom_threshold: f32,
    /// `BlurEffect.Size` (max across enabled instances).
    pub blur_size: f32,
    /// `DepthOfFieldEffect`: `[FocusDistance, InFocusRadius, NearIntensity, FarIntensity]`.
    pub dof: [f32; 4],
    /// `ColorCorrectionEffect` (summed B/C/S, component-multiplied TintColor in `[0..1]`).
    pub cc_enabled: bool,
    pub cc_brightness: f32,
    pub cc_contrast: f32,
    pub cc_saturation: f32,
    pub cc_tint: [f32; 3],
    /// `SunRaysEffect` (summed Intensity, max Spread) + precomputed sun/camera terms.
    pub sun_rays_enabled: bool,
    pub sun_rays_intensity: f32,
    pub sun_rays_spread: f32,
    pub sun_dir_world: [f32; 3],
    pub cam_forward: [f32; 3],
    pub sun_ndc: [f32; 2],
    pub sun_view: [f32; 3],
    pub inv_proj_xy: [f32; 2],
    /// Camera projection terms for depth linearization (`ssao.xy`, `near`, `is_ortho`).
    pub ssao_half_diag: [f32; 2],
    pub near_plane: f32,
    pub is_ortho: bool,
}

impl Default for PostFxParams {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            tonemap_mode: TonemapMode::Default,
            raw_debug: false,
            terrain_capture_id: false,
            post_fx_tier: 3,
            bloom_allowed: true,
            bloom_effect_enabled: false,
            bloom_intensity: rustudio_core::mech::DEFAULT_GLOW_INTENSITY,
            bloom_size: rustudio_core::mech::DEFAULT_GLOW_SIZE_PX,
            bloom_threshold: rustudio_core::mech::DEFAULT_GLOW_THRESHOLD,
            blur_size: 0.0,
            dof: [0.0; 4],
            cc_enabled: false,
            cc_brightness: 0.0,
            cc_contrast: 0.0,
            cc_saturation: 0.0,
            cc_tint: [1.0; 3],
            sun_rays_enabled: false,
            sun_rays_intensity: 0.0,
            sun_rays_spread: 0.1,
            sun_dir_world: [0.0, 1.0, 0.0],
            cam_forward: [0.0, 0.0, -1.0],
            sun_ndc: [0.0, 0.0],
            sun_view: [0.0, 0.0, -1.0],
            inv_proj_xy: [1.0, 1.0],
            ssao_half_diag: [0.5, 0.5],
            near_plane: rustudio_core::mech::NEAR,
            is_ortho: false,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostFxUniform {
    p0: [f32; 4],
    p1: [f32; 4],
    p2: [f32; 4],
    dof: [f32; 4],
    cc_r: [f32; 4],
    cc_g: [f32; 4],
    cc_b: [f32; 4],
    sun0: [f32; 4],
    sun1: [f32; 4],
    sun2: [f32; 4],
    sun3: [f32; 4],
    extra: [f32; 4],
}

/// Studio's exact 3x4 ColorCorrection matrix:
/// Contrast -> Brightness -> Saturation (luma `0.34, 0.55, 0.11`) -> TintColor.
pub fn studio_cc_matrix(
    enabled: bool,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    tint: [f32; 3],
) -> [[f32; 4]; 3] {
    if !enabled {
        return [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
    }
    const LUMA: [f32; 3] = [0.34, 0.55, 0.11];
    let scale = 1.0 + contrast;
    let s_on = 1.0 + saturation;
    let s_off = -saturation;
    let b_term = brightness - 0.5 * contrast;
    let mut rows = [[0.0f32; 4]; 3];
    for i in 0..3 {
        let t = tint[i];
        for j in 0..3 {
            let diag = if i == j { s_on } else { 0.0 };
            rows[i][j] = t * scale * (diag + s_off * LUMA[j]);
        }
        rows[i][3] = t * b_term;
    }
    rows
}

/// Studio's exact SunRays parameters.
/// Returns `(cb_intensity, g, sun_rays_color_linear)`.
pub fn studio_sun_rays_params(
    sun_dir: Vec3,
    cam_forward: Vec3,
    intensity: f32,
    spread: f32,
) -> (f32, f32, [f32; 3]) {
    let sun_factor = (sun_dir.y * 10.0).clamp(0.0, 1.0) * cam_forward.dot(sun_dir).clamp(0.0, 1.0);
    let cb_intensity = sun_factor * intensity.max(0.0) * 0.05;
    let g = 0.99 - spread.clamp(0.0, 1.0) * 0.39;
    let t = ((sun_dir.y - 0.25) * 6.6666665).clamp(0.0, 1.0);
    let r = 1.0 - 0.25 * t;
    let gr = 0.6196079 + 0.1803921 * t;
    let b = 0.007843138 + 0.9921569 * t;
    (cb_intensity, g, [r * r, gr * gr, b * b])
}

/// Studio's deterministic 8x8 `SunRays Noise` texture.
pub fn studio_sun_rays_noise() -> [u8; 64] {
    let mut buf = [0u8; 64];
    for (i, slot) in buf.iter_mut().enumerate() {
        *slot = ((i as f32) * 0.015625 * 255.0) as u8;
    }
    let mut seed: u32 = 0x9e3779b9;
    for i in 0..64 {
        seed = seed.wrapping_mul(0x19660d).wrapping_add(0x3c6ef35f);
        let j = ((seed >> 8) & 63) as usize;
        buf.swap(i, j);
    }
    buf
}

/// Studio's `post_fx_scale = sqrt((w * h) / (1920 * 1080))`.
pub fn post_fx_scale(size: [u32; 2]) -> f32 {
    ((size[0] as f32 * size[1] as f32) / 2_073_600.0).sqrt().max(1e-4)
}

/// Studio's Bloom pyramid schedule: `(start_level, levels, lod)`.
pub fn studio_bloom_schedule(size: [u32; 2], bloom_size: f32, post_fx_tier: u8) -> (usize, usize, f32) {
    let scale = post_fx_scale(size);
    let mut lod = (3.0 + (scale.log2() + (bloom_size.max(1e-4) / 7.0).log2().max(0.0)).max(-1.0))
        .min(5.999);
    let mut levels = (lod as usize).clamp(2, 5);
    let start_level = if post_fx_tier < 2 {
        levels = levels.max(3);
        lod = lod.max(3.0);
        2
    } else {
        0
    };
    (start_level, levels, lod)
}

/// Studio's BlurEffect sigma and bilinear tap-pair count.
pub fn studio_blur_params(size: [u32; 2], blur_size: f32, post_fx_tier: u8) -> (f32, f32, u32) {
    let scale = post_fx_scale(size);
    let weight = (blur_size * 0.125 * (2.0 / 3.0)).clamp(0.0, 1.0);
    let sigma_base = (blur_size * 0.125).clamp(1.0, 7.0);
    let max_r = if post_fx_tier < 2 { 3.0 } else { 7.0 };
    let sigma = sigma_base * scale;
    let target = sigma_base.min(max_r) * scale;
    let k_pairs = [1u32, 3, 5, 7]
        .into_iter()
        .find(|&k| (k as f32) >= target)
        .unwrap_or(7);
    (weight, sigma, k_pairs)
}

/// Studio's SunRays tap counts: `(ray_m, radial_n)`.
pub fn studio_sun_rays_taps(size: [u32; 2]) -> (u32, u32) {
    let target = post_fx_scale(size) * 12.0;
    let radial_n = [3u32, 6, 9, 12]
        .into_iter()
        .find(|&n| (n as f32) >= target)
        .unwrap_or(12);
    let ray_m = radial_n.max(6);
    (ray_m, radial_n)
}

/// Studio's Near DoF divisor: 2 (half-res) or 4 (quarter-res).
pub fn studio_near_dof_div(height: u32, near_intensity: f32) -> u32 {
    if height <= 540 || near_intensity < 0.36 {
        2
    } else {
        4
    }
}

struct DepthPipelines {
    sunrays_depth: wgpu::RenderPipeline,
    dof_cheap_prep: wgpu::RenderPipeline,
    dof_far_down_h: wgpu::RenderPipeline,
    dof_near_down_h: wgpu::RenderPipeline,
    dof_resolve_far: wgpu::RenderPipeline,
    dof_resolve_near: wgpu::RenderPipeline,
}

struct PostFxTargets {
    size: [u32; 2],
    samples: u32,
    depth_bg: wgpu::BindGroup,
    // Full-res DoF scratch targets (HDR_FORMAT).
    dof_full_prep: wgpu::TextureView,
    dof_scene_a: wgpu::TextureView,
    dof_scene_b: wgpu::TextureView,
    // Half-res ((w+(w&1))/2, (h+(h&1))/2) DoF targets (used for Far HQ and Near div=2).
    half_size: [u32; 2],
    dof_half_h: wgpu::TextureView,     // hw x h
    dof_half_col: wgpu::TextureView,   // hw x hh
    dof_half_a: wgpu::TextureView,     // hw x hh
    dof_half_b: wgpu::TextureView,     // hw x hh
    dof_half_coc_a: wgpu::TextureView, // hw x hh (R16Float)
    dof_half_coc_b: wgpu::TextureView, // hw x hh (R16Float)
    // Quarter-res ((w+(w&1))/4, (h+(h&1))/4) DoF targets (used for Near div=4).
    quarter_dof_size: [u32; 2],
    dof_q_h: wgpu::TextureView,        // qw x h
    dof_q_col: wgpu::TextureView,      // qw x qh
    dof_q_a: wgpu::TextureView,        // qw x qh
    dof_q_b: wgpu::TextureView,        // qw x qh
    dof_q_coc_a: wgpu::TextureView,    // qw x qh (R16Float)
    dof_q_coc_b: wgpu::TextureView,    // qw x qh (R16Float)
    // Bloom pyramid (levels 0..5 -> >>1..>>5).
    bloom_sizes: [[u32; 2]; 5],
    bloom_textures: Vec<wgpu::Texture>,
    bloom_views: Vec<wgpu::TextureView>,
    bloom_bright_capture: Option<wgpu::Texture>,
    // Quarter-res (w>>2, h>>2) SunRays & Blur targets.
    quarter_size: [u32; 2],
    sr_depth: wgpu::TextureView,       // R16Float
    sr_march: wgpu::TextureView,       // HDR_FORMAT
    sr_radial: wgpu::TextureView,      // HDR_FORMAT
    blur_a: wgpu::TextureView,         // HDR_FORMAT
    blur_b: wgpu::TextureView,         // HDR_FORMAT
    // Pre-built texture bind groups (zero per-frame allocation!).
    // Scene sources: [0] = hdr_view, [1] = dof_scene_a, [2] = dof_scene_b
    bg_scene: [wgpu::BindGroup; 3],
    bg_dof_cheap: wgpu::BindGroup,
    bg_dof_half_down_v: wgpu::BindGroup,
    bg_dof_half_coc_from_col: wgpu::BindGroup,
    bg_dof_half_coc_a: wgpu::BindGroup,
    bg_dof_half_coc_b: wgpu::BindGroup,
    bg_dof_half_edge: wgpu::BindGroup,
    bg_dof_half_fixup_far: wgpu::BindGroup,   // (dof_half_a, coc_a)
    bg_dof_half_fixup_near: wgpu::BindGroup,  // (dof_half_col, coc_a)
    bg_dof_half_bokeh: wgpu::BindGroup,       // (dof_half_b, coc_a)
    bg_dof_half_tent: wgpu::BindGroup,        // (dof_half_a)
    bg_dof_resolve_half: [wgpu::BindGroup; 3],
    bg_dof_q_down_v: wgpu::BindGroup,
    bg_dof_q_coc_from_col: wgpu::BindGroup,
    bg_dof_q_coc_a: wgpu::BindGroup,
    bg_dof_q_coc_b: wgpu::BindGroup,
    bg_dof_q_fixup_near: wgpu::BindGroup,     // (dof_q_col, coc_a)
    bg_dof_q_bokeh: wgpu::BindGroup,          // (dof_q_b, coc_a)
    bg_dof_q_tent: wgpu::BindGroup,           // (dof_q_a)
    bg_dof_resolve_q: [wgpu::BindGroup; 3],
    bg_bloom_down: [wgpu::BindGroup; 4],      // levels 0->1, 1->2, 2->3, 3->4
    bg_bloom_up: [wgpu::BindGroup; 4],        // levels 1->0, 2->1, 3->2, 4->3
    bg_sr_march: wgpu::BindGroup,             // (sr_depth, noise)
    bg_sr_radial: wgpu::BindGroup,            // (sr_march)
    bg_blur_h: wgpu::BindGroup,               // (blur_a)
    bg_blur_v: wgpu::BindGroup,               // (blur_b)
    // FXComposite bind groups indexed by [bloom_slot 0..3][sr_on 0..2]
    // bloom_slot: 0 = black, 1 = bloom[0] (HQ), 2 = bloom[2] (LQ)
    bg_fx_comp: [[wgpu::BindGroup; 2]; 3],
    // ImageProcess bind groups indexed by [scene_idx 0..3][bloom_slot 0..3][sr_on 0..2]
    bg_image_process: [[[wgpu::BindGroup; 2]; 3]; 3],
}

pub struct PostFx {
    hdr_format: wgpu::TextureFormat,
    ubo_stride: u32,
    ubo_capacity_slots: usize,
    ubo: wgpu::Buffer,
    ubo_scratch: Vec<u8>,
    uniform_bg: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    depth_layout_1x: wgpu::BindGroupLayout,
    depth_layout_ms: wgpu::BindGroupLayout,
    pl_depth_1x: wgpu::PipelineLayout,
    pl_depth_ms: wgpu::PipelineLayout,
    black_view: wgpu::TextureView,
    noise_view: wgpu::TextureView,
    // Non-depth pipelines
    bloom_extract: wgpu::RenderPipeline,
    bloom_down: wgpu::RenderPipeline,
    bloom_up: wgpu::RenderPipeline,
    sunrays_march: wgpu::RenderPipeline,
    sunrays_radial: wgpu::RenderPipeline,
    blur_down: wgpu::RenderPipeline,
    fx_composite: wgpu::RenderPipeline,
    blur_1d: wgpu::RenderPipeline,
    dof_cheap: wgpu::RenderPipeline,
    dof_down_v: wgpu::RenderPipeline,
    dof_coc_filter: wgpu::RenderPipeline,
    dof_edge_fatten: wgpu::RenderPipeline,
    dof_fixup_coc: wgpu::RenderPipeline,
    dof_bokeh_far: wgpu::RenderPipeline,
    dof_bokeh_near: wgpu::RenderPipeline,
    dof_tent: wgpu::RenderPipeline,
    image_process: wgpu::RenderPipeline,
    // Depth-reading pipelines: [0] = 1x, [1] = MSAA (>1x)
    depth_pipes: [Option<DepthPipelines>; 2],
    targets: Option<PostFxTargets>,
}

const INITIAL_UBO_SLOTS: usize = 64;

impl PostFx {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        hdr_format: wgpu::TextureFormat,
        display_format: wgpu::TextureFormat,
    ) -> Self {
        let align = device.limits().min_uniform_buffer_offset_alignment.max(256);
        let ubo_stride = ((std::mem::size_of::<PostFxUniform>() as u32).div_ceil(align)) * align;
        let ubo = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("postfx ubo"),
            size: (ubo_stride as u64) * (INITIAL_UBO_SLOTS as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let samp_lin = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("postfx linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let samp_near = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("postfx nearest clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let samp_repeat_near = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("postfx nearest repeat"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("postfx uniform"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<PostFxUniform>() as u64,
                        ),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let uniform_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("postfx uniform"),
            layout: &uniform_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &ubo,
                        offset: 0,
                        size: std::num::NonZeroU64::new(
                            std::mem::size_of::<PostFxUniform>() as u64,
                        ),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&samp_lin),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&samp_near),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&samp_repeat_near),
                },
            ],
        });

        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("postfx textures"),
            entries: &[tex_entry(0), tex_entry(1), tex_entry(2), tex_entry(3)],
        });

        let depth_layout = |ms: bool| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(if ms { "postfx depth ms" } else { "postfx depth 1x" }),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: crate::gpu_caps::depth_sample_type(device),
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: ms,
                    },
                    count: None,
                }],
            })
        };
        let depth_layout_1x = depth_layout(false);
        let depth_layout_ms = depth_layout(true);

        let pl_nod = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("postfx"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let pl_depth_1x = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("postfx depth 1x"),
            bind_group_layouts: &[
                Some(&uniform_layout),
                Some(&tex_layout),
                Some(&depth_layout_1x),
            ],
            immediate_size: 0,
        });
        let pl_depth_ms = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("postfx depth ms"),
            bind_group_layouts: &[
                Some(&uniform_layout),
                Some(&tex_layout),
                Some(&depth_layout_ms),
            ],
            immediate_size: 0,
        });

        let black_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("postfx black"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: hdr_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            black_tex.as_image_copy(),
            &[0u8; 8],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let black_view = black_tex.create_view(&Default::default());

        let noise_bytes = studio_sun_rays_noise();
        let noise_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("SunRays Noise"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            noise_tex.as_image_copy(),
            &noise_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: Some(8),
            },
            wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
        );
        let noise_view = noise_tex.create_view(&Default::default());

        let src = crate::gpu_caps::shader_for(device, include_str!("postfx.wgsl"));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("postfx"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });

        let make_pipe = |label: &str,
                         fs: &str,
                         format: wgpu::TextureFormat,
                         blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl_nod),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_post"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: crate::pipeline_cache::current(),
            })
        };

        let additive = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        });

        let bloom_extract = make_pipe("postfx bloom extract", "fs_bloom_extract", hdr_format, None);
        let bloom_down = make_pipe("postfx bloom down", "fs_bloom_down", hdr_format, None);
        let bloom_up = make_pipe("postfx bloom up", "fs_bloom_up", hdr_format, additive);
        let sunrays_march = make_pipe("postfx sunrays march", "fs_sunrays_march", hdr_format, None);
        let sunrays_radial = make_pipe("postfx sunrays radial", "fs_sunrays_radial", hdr_format, None);
        let blur_down = make_pipe("postfx blur down", "fs_blur_down", hdr_format, None);
        let fx_composite = make_pipe("postfx fx composite", "fs_fx_composite", hdr_format, None);
        let blur_1d = make_pipe("postfx blur 1d", "fs_blur_1d", hdr_format, None);
        let dof_cheap = make_pipe("postfx dof cheap", "fs_dof_cheap", hdr_format, None);
        let dof_down_v = make_pipe("postfx dof down v", "fs_dof_down_v", hdr_format, None);
        let dof_coc_filter = make_pipe(
            "postfx dof coc filter",
            "fs_dof_coc_filter",
            wgpu::TextureFormat::R16Float,
            None,
        );
        let dof_edge_fatten = make_pipe("postfx dof edge fatten", "fs_dof_edge_fatten", hdr_format, None);
        let dof_fixup_coc = make_pipe("postfx dof fixup coc", "fs_dof_fixup_coc", hdr_format, None);
        let dof_bokeh_far = make_pipe("postfx dof bokeh far", "fs_dof_bokeh_far", hdr_format, None);
        let dof_bokeh_near = make_pipe("postfx dof bokeh near", "fs_dof_bokeh_near", hdr_format, None);
        let dof_tent = make_pipe("postfx dof tent", "fs_dof_tent", hdr_format, None);
        let image_process = make_pipe("postfx image process", "fs_image_process", display_format, None);

        Self {
            hdr_format,
            ubo_stride,
            ubo_capacity_slots: INITIAL_UBO_SLOTS,
            ubo,
            ubo_scratch: Vec::with_capacity((ubo_stride as usize) * INITIAL_UBO_SLOTS),
            uniform_bg,
            tex_layout,
            depth_layout_1x,
            depth_layout_ms,
            pl_depth_1x,
            pl_depth_ms,
            black_view,
            noise_view,
            bloom_extract,
            bloom_down,
            bloom_up,
            sunrays_march,
            sunrays_radial,
            blur_down,
            fx_composite,
            blur_1d,
            dof_cheap,
            dof_down_v,
            dof_coc_filter,
            dof_edge_fatten,
            dof_fixup_coc,
            dof_bokeh_far,
            dof_bokeh_near,
            dof_tent,
            image_process,
            depth_pipes: [None, None],
            targets: None,
        }
    }

    fn ensure_depth_pipes(&mut self, device: &wgpu::Device, samples: u32) {
        let slot = usize::from(samples > 1);
        if self.depth_pipes[slot].is_some() {
            return;
        }
        let combined = format!("{}\n{}", include_str!("postfx.wgsl"), include_str!("postfx_depth.wgsl"));
        let combined = if samples == 1 {
            combined.replace("texture_depth_multisampled_2d", "texture_depth_2d")
        } else {
            combined
        };
        let src = crate::gpu_caps::shader_for(device, &combined);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(if samples > 1 { "postfx depth ms" } else { "postfx depth 1x" }),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let pl = if samples > 1 {
            &self.pl_depth_ms
        } else {
            &self.pl_depth_1x
        };
        let make = |label: &str, fs: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_post"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: crate::pipeline_cache::current(),
            })
        };
        self.depth_pipes[slot] = Some(DepthPipelines {
            sunrays_depth: make("postfx sunrays depth", "fs_sunrays_depth", wgpu::TextureFormat::R16Float),
            dof_cheap_prep: make("postfx dof cheap prep", "fs_dof_cheap_prep", self.hdr_format),
            dof_far_down_h: make("postfx dof far down h", "fs_dof_far_down_h", self.hdr_format),
            dof_near_down_h: make("postfx dof near down h", "fs_dof_near_down_h", self.hdr_format),
            dof_resolve_far: make("postfx dof resolve far", "fs_dof_resolve_far", self.hdr_format),
            dof_resolve_near: make("postfx dof resolve near", "fs_dof_resolve_near", self.hdr_format),
        });
    }

    /// Rebuilds resolution-dependent targets and bind groups whenever the viewport
    /// target is created or resized.
    pub fn rebuild_targets(
        &mut self,
        device: &wgpu::Device,
        size: [u32; 2],
        samples: u32,
        hdr_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
    ) {
        self.ensure_depth_pipes(device, samples);
        let mk_tex = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat, copy_src: bool| {
            let mut usage =
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
            if copy_src {
                usage |= wgpu::TextureUsages::COPY_SRC;
            }
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w.max(1),
                    height: h.max(1),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let mk_view = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat| {
            mk_tex(label, w, h, format, false).create_view(&Default::default())
        };
        let bg4 = |label: &str,
                   t0: &wgpu::TextureView,
                   t1: &wgpu::TextureView,
                   t2: &wgpu::TextureView,
                   t3: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.tex_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(t0),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(t1),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(t2),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(t3),
                    },
                ],
            })
        };
        let bg1 = |label: &str, t0: &wgpu::TextureView| {
            bg4(label, t0, &self.black_view, &self.black_view, &self.black_view)
        };
        let bg2 = |label: &str, t0: &wgpu::TextureView, t1: &wgpu::TextureView| {
            bg4(label, t0, t1, &self.black_view, &self.black_view)
        };

        let [w, h] = size;
        let half_size = [((w + (w & 1)) / 2).max(1), ((h + (h & 1)) / 2).max(1)];
        let quarter_dof_size = [((w + (w & 1)) / 4).max(1), ((h + (h & 1)) / 4).max(1)];
        let quarter_size = [(w >> 2).max(1), (h >> 2).max(1)];
        let r16 = wgpu::TextureFormat::R16Float;

        let depth_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("postfx depth"),
            layout: if samples > 1 {
                &self.depth_layout_ms
            } else {
                &self.depth_layout_1x
            },
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(depth_view),
            }],
        });

        let dof_full_prep = mk_view("postfx dof prep", w, h, self.hdr_format);
        let dof_scene_a = mk_view("postfx dof scene a", w, h, self.hdr_format);
        let dof_scene_b = mk_view("postfx dof scene b", w, h, self.hdr_format);

        let dof_half_h = mk_view("postfx dof half h", half_size[0], h, self.hdr_format);
        let dof_half_col = mk_view("postfx dof half col", half_size[0], half_size[1], self.hdr_format);
        let dof_half_a = mk_view("postfx dof half a", half_size[0], half_size[1], self.hdr_format);
        let dof_half_b = mk_view("postfx dof half b", half_size[0], half_size[1], self.hdr_format);
        let dof_half_coc_a = mk_view("postfx dof half coc a", half_size[0], half_size[1], r16);
        let dof_half_coc_b = mk_view("postfx dof half coc b", half_size[0], half_size[1], r16);

        let dof_q_h = mk_view("postfx dof q h", quarter_dof_size[0], h, self.hdr_format);
        let dof_q_col = mk_view(
            "postfx dof q col",
            quarter_dof_size[0],
            quarter_dof_size[1],
            self.hdr_format,
        );
        let dof_q_a = mk_view(
            "postfx dof q a",
            quarter_dof_size[0],
            quarter_dof_size[1],
            self.hdr_format,
        );
        let dof_q_b = mk_view(
            "postfx dof q b",
            quarter_dof_size[0],
            quarter_dof_size[1],
            self.hdr_format,
        );
        let dof_q_coc_a = mk_view("postfx dof q coc a", quarter_dof_size[0], quarter_dof_size[1], r16);
        let dof_q_coc_b = mk_view("postfx dof q coc b", quarter_dof_size[0], quarter_dof_size[1], r16);

        let mut bloom_sizes = [[1u32; 2]; 5];
        let mut bloom_textures = Vec::with_capacity(5);
        for level in 0..5 {
            let bw = (w >> (level + 1)).max(1);
            let bh = (h >> (level + 1)).max(1);
            bloom_sizes[level] = [bw, bh];
            bloom_textures.push(mk_tex("postfx bloom", bw, bh, self.hdr_format, level == 0));
        }
        let bloom_views: Vec<_> = bloom_textures
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let bloom_bright_capture = std::env::var_os("RUSTUDIO_DUMP_HDR").map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("postfx bloom bright capture"),
                size: wgpu::Extent3d {
                    width: bloom_sizes[0][0],
                    height: bloom_sizes[0][1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.hdr_format,
                usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });

        let sr_depth = mk_view("postfx sunrays depth", quarter_size[0], quarter_size[1], r16);
        let sr_march = mk_view(
            "postfx sunrays march",
            quarter_size[0],
            quarter_size[1],
            self.hdr_format,
        );
        let sr_radial = mk_view(
            "postfx sunrays radial",
            quarter_size[0],
            quarter_size[1],
            self.hdr_format,
        );
        let blur_a = mk_view(
            "postfx blur a",
            quarter_size[0],
            quarter_size[1],
            self.hdr_format,
        );
        let blur_b = mk_view(
            "postfx blur b",
            quarter_size[0],
            quarter_size[1],
            self.hdr_format,
        );

        let scenes = [hdr_view, &dof_scene_a, &dof_scene_b];
        let bg_scene = std::array::from_fn(|i| bg1("postfx scene", scenes[i]));
        let bg_dof_cheap = bg1("postfx dof cheap", &dof_full_prep);
        let bg_dof_half_down_v = bg1("postfx dof half down v", &dof_half_h);
        let bg_dof_half_coc_from_col = bg1("postfx dof half coc col", &dof_half_col);
        let bg_dof_half_coc_a = bg1("postfx dof half coc a", &dof_half_coc_a);
        let bg_dof_half_coc_b = bg1("postfx dof half coc b", &dof_half_coc_b);
        let bg_dof_half_edge = bg1("postfx dof half edge", &dof_half_col);
        let bg_dof_half_fixup_far = bg2("postfx dof half fixup far", &dof_half_a, &dof_half_coc_a);
        let bg_dof_half_fixup_near = bg2("postfx dof half fixup near", &dof_half_col, &dof_half_coc_a);
        let bg_dof_half_bokeh = bg2("postfx dof half bokeh", &dof_half_b, &dof_half_coc_a);
        let bg_dof_half_tent = bg1("postfx dof half tent", &dof_half_a);
        let bg_dof_resolve_half =
            std::array::from_fn(|i| bg2("postfx dof resolve half", scenes[i], &dof_half_b));

        let bg_dof_q_down_v = bg1("postfx dof q down v", &dof_q_h);
        let bg_dof_q_coc_from_col = bg1("postfx dof q coc col", &dof_q_col);
        let bg_dof_q_coc_a = bg1("postfx dof q coc a", &dof_q_coc_a);
        let bg_dof_q_coc_b = bg1("postfx dof q coc b", &dof_q_coc_b);
        let bg_dof_q_fixup_near = bg2("postfx dof q fixup near", &dof_q_col, &dof_q_coc_a);
        let bg_dof_q_bokeh = bg2("postfx dof q bokeh", &dof_q_b, &dof_q_coc_a);
        let bg_dof_q_tent = bg1("postfx dof q tent", &dof_q_a);
        let bg_dof_resolve_q =
            std::array::from_fn(|i| bg2("postfx dof resolve q", scenes[i], &dof_q_b));

        let bg_bloom_down = std::array::from_fn(|i| bg1("postfx bloom down", &bloom_views[i]));
        let bg_bloom_up = std::array::from_fn(|i| bg1("postfx bloom up", &bloom_views[i + 1]));
        let bg_sr_march = bg2("postfx sr march", &sr_depth, &self.noise_view);
        let bg_sr_radial = bg1("postfx sr radial", &sr_march);
        let bg_blur_h = bg1("postfx blur h", &blur_a);
        let bg_blur_v = bg1("postfx blur v", &blur_b);

        let bloom_sources = [&self.black_view, &bloom_views[0], &bloom_views[2]];
        let sr_sources = [&self.black_view, &sr_radial];

        let bg_fx_comp = std::array::from_fn(|b| {
            std::array::from_fn(|s| {
                bg4(
                    "postfx fx comp",
                    &blur_b,
                    bloom_sources[b],
                    sr_sources[s],
                    &self.black_view,
                )
            })
        });

        let bg_image_process = std::array::from_fn(|sc| {
            std::array::from_fn(|b| {
                std::array::from_fn(|s| {
                    bg4(
                        "postfx image process",
                        scenes[sc],
                        bloom_sources[b],
                        sr_sources[s],
                        &blur_a,
                    )
                })
            })
        });

        self.targets = Some(PostFxTargets {
            size,
            samples,
            depth_bg,
            dof_full_prep,
            dof_scene_a,
            dof_scene_b,
            half_size,
            dof_half_h,
            dof_half_col,
            dof_half_a,
            dof_half_b,
            dof_half_coc_a,
            dof_half_coc_b,
            quarter_dof_size,
            dof_q_h,
            dof_q_col,
            dof_q_a,
            dof_q_b,
            dof_q_coc_a,
            dof_q_coc_b,
            bloom_sizes,
            bloom_textures,
            bloom_views,
            bloom_bright_capture,
            quarter_size,
            sr_depth,
            sr_march,
            sr_radial,
            blur_a,
            blur_b,
            bg_scene,
            bg_dof_cheap,
            bg_dof_half_down_v,
            bg_dof_half_coc_from_col,
            bg_dof_half_coc_a,
            bg_dof_half_coc_b,
            bg_dof_half_edge,
            bg_dof_half_fixup_far,
            bg_dof_half_fixup_near,
            bg_dof_half_bokeh,
            bg_dof_half_tent,
            bg_dof_resolve_half,
            bg_dof_q_down_v,
            bg_dof_q_coc_from_col,
            bg_dof_q_coc_a,
            bg_dof_q_coc_b,
            bg_dof_q_fixup_near,
            bg_dof_q_bokeh,
            bg_dof_q_tent,
            bg_dof_resolve_q,
            bg_bloom_down,
            bg_bloom_up,
            bg_sr_march,
            bg_sr_radial,
            bg_blur_h,
            bg_blur_v,
            bg_fx_comp,
            bg_image_process,
        });
    }

    pub fn bloom_dump_textures(&self) -> Option<(&wgpu::Texture, Option<&wgpu::Texture>, [u32; 2])> {
        let t = self.targets.as_ref()?;
        Some((
            &t.bloom_textures[0],
            t.bloom_bright_capture.as_ref(),
            t.bloom_sizes[0],
        ))
    }

    /// Encodes all active post-processing passes into `encoder` and writes the
    /// final tone-mapped, color-corrected display image into `display_view`.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        display_view: &wgpu::TextureView,
        params: &PostFxParams,
        with_gpu_timing: bool,
    ) {
        let Some(ref t) = self.targets else { return };
        let samples = t.samples;
        let dp_slot = usize::from(samples > 1);
        if self.depth_pipes[dp_slot].is_none() {
            self.ensure_depth_pipes(device, samples);
        }
        let Some(ref t) = self.targets else { return };
        let Some(ref dp) = self.depth_pipes[dp_slot] else { return };

        let size = t.size;
        let scale = post_fx_scale(size);
        let [cc_r, cc_g, cc_b] = studio_cc_matrix(
            params.cc_enabled && !params.raw_debug,
            params.cc_brightness,
            params.cc_contrast,
            params.cc_saturation,
            params.cc_tint,
        );
        let (blur_weight, blur_sigma, blur_k_pairs) = if params.blur_size > 0.0 && !params.raw_debug
        {
            studio_blur_params(size, params.blur_size, params.post_fx_tier)
        } else {
            (0.0, 1.0, 1)
        };
        let (sr_cb_intensity, sr_g, sr_color) = studio_sun_rays_params(
            Vec3::from(params.sun_dir_world),
            Vec3::from(params.cam_forward),
            params.sun_rays_intensity,
            params.sun_rays_spread,
        );
        let (sr_ray_m, sr_radial_n) = studio_sun_rays_taps(size);

        let flags = (u32::from(params.terrain_capture_id)) | (u32::from(params.raw_debug) << 1);
        let base_u = PostFxUniform {
            p0: [
                params.exposure,
                blur_weight,
                params.tonemap_mode as u32 as f32,
                flags as f32,
            ],
            p1: [
                params.bloom_intensity,
                params.bloom_threshold,
                1.0,
                params.near_plane,
            ],
            p2: [
                params.ssao_half_diag[0],
                params.ssao_half_diag[1],
                if params.is_ortho { 1.0 } else { 0.0 },
                scale,
            ],
            dof: params.dof,
            cc_r,
            cc_g,
            cc_b,
            sun0: [
                params.sun_ndc[0],
                params.sun_ndc[1],
                params.sun_view[0],
                params.sun_view[1],
            ],
            sun1: [sr_cb_intensity, sr_g, sr_g * sr_g, params.sun_view[2]],
            sun2: [sr_color[0], sr_color[1], sr_color[2], sr_ray_m as f32],
            sun3: [params.inv_proj_xy[0], params.inv_proj_xy[1], 0.0, 0.0],
            extra: [0.0; 4],
        };

        self.ubo_scratch.clear();
        let stride = self.ubo_stride as usize;
        let mut push_u = |u: PostFxUniform| -> u32 {
            let offset = self.ubo_scratch.len() as u32;
            let bytes = bytemuck::bytes_of(&u);
            self.ubo_scratch.extend_from_slice(bytes);
            self.ubo_scratch.resize(offset as usize + stride, 0);
            offset
        };

        let draw_pass = |encoder: &mut wgpu::CommandEncoder,
                         label: &str,
                         pipe: &wgpu::RenderPipeline,
                         target_view: &wgpu::TextureView,
                         load: wgpu::LoadOp<wgpu::Color>,
                         uniform_bg: &wgpu::BindGroup,
                         dyn_off: u32,
                         tex_bg: &wgpu::BindGroup,
                         depth_bg: Option<&wgpu::BindGroup>| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, uniform_bg, &[dyn_off]);
            pass.set_bind_group(1, tex_bg, &[]);
            if let Some(dbg) = depth_bg {
                pass.set_bind_group(2, dbg, &[]);
            }
            pass.draw(0..3, 0..1);
        };

        let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
        let load = wgpu::LoadOp::Load;

        // ----------------------------------------------------------- Stage 1: DepthOfField
        let bypass = params.raw_debug || params.terrain_capture_id;
        let dof_allowed = !bypass && !params.is_ortho && params.post_fx_tier >= 3;
        let far_i = if dof_allowed { params.dof[3] } else { 0.0 };
        let near_i = if dof_allowed { params.dof[2] } else { 0.0 };
        let mut scene_idx: usize = 0; // 0 = hdr_view, 1 = dof_scene_a, 2 = dof_scene_b

        if far_i > 0.0 {
            if far_i < DOF_CHEAP_BELOW {
                let mut u0 = base_u;
                u0.extra = [0.0, 0.0, 0.0, 0.0]; // far
                let o0 = push_u(u0);
                draw_pass(
                    encoder,
                    "dof far cheap prep",
                    &dp.dof_cheap_prep,
                    &t.dof_full_prep,
                    clear,
                    &self.uniform_bg,
                    o0,
                    &t.bg_scene[scene_idx],
                    Some(&t.depth_bg),
                );
                let mut u1 = base_u;
                u1.extra = [far_i, 0.0, 0.0, 0.0];
                let o1 = push_u(u1);
                draw_pass(
                    encoder,
                    "dof far cheap",
                    &self.dof_cheap,
                    &t.dof_scene_a,
                    clear,
                    &self.uniform_bg,
                    o1,
                    &t.bg_dof_cheap,
                    None,
                );
            } else {
                // Studio Far HQ 1/2-res chain.
                let full_w = size[0] as f32;
                let full_h = size[1] as f32;
                let hw = t.half_size[0] as f32;
                let hh = t.half_size[1] as f32;
                let red_x = 2.0 * (full_w - hw) / full_w;
                let red_y = (full_w - hh) / full_w;

                let mut uh = base_u;
                uh.extra = [0.5 * red_x / hw, 0.0, 0.0, 0.0];
                let oh = push_u(uh);
                draw_pass(
                    encoder,
                    "dof far down h",
                    &dp.dof_far_down_h,
                    &t.dof_half_h,
                    clear,
                    &self.uniform_bg,
                    oh,
                    &t.bg_scene[scene_idx],
                    Some(&t.depth_bg),
                );
                let mut uv = base_u;
                uv.extra = [0.0, 0.5 * red_y / hh, 0.0, 0.0];
                let ov = push_u(uv);
                draw_pass(
                    encoder,
                    "dof far down v",
                    &self.dof_down_v,
                    &t.dof_half_col,
                    clear,
                    &self.uniform_bg,
                    ov,
                    &t.bg_dof_half_down_v,
                    None,
                );

                // 6-pass Far CoC mask: Min H, Min V, Min H, Min V, Avg H, Avg V.
                let coc_steps = [
                    ([red_x / hw, 0.0], 1.0, 1.0, &t.bg_dof_half_coc_from_col, &t.dof_half_coc_b),
                    ([0.0, red_y / hh], 1.0, 0.0, &t.bg_dof_half_coc_b, &t.dof_half_coc_a),
                    ([far_i / hw, 0.0], 1.0, 0.0, &t.bg_dof_half_coc_a, &t.dof_half_coc_b),
                    ([0.0, far_i / hh], 1.0, 0.0, &t.bg_dof_half_coc_b, &t.dof_half_coc_a),
                    ([far_i / hw, 0.0], 2.0, 0.0, &t.bg_dof_half_coc_a, &t.dof_half_coc_b),
                    ([0.0, far_i / hh], 2.0, 0.0, &t.bg_dof_half_coc_b, &t.dof_half_coc_a),
                ];
                for (step, mode, read_alpha, bg, dst) in coc_steps {
                    let mut uc = base_u;
                    uc.extra = [step[0], step[1], mode, read_alpha];
                    let oc = push_u(uc);
                    draw_pass(
                        encoder,
                        "dof far coc mask",
                        &self.dof_coc_filter,
                        dst,
                        clear,
                        &self.uniform_bg,
                        oc,
                        bg,
                        None,
                    );
                }

                // DOFEdgeFattenFS -> FixUpCoCFS -> BokehFarCoCFS -> TentFilterFS -> DOFResolveFarFS
                let oe = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof far edge fatten",
                    &self.dof_edge_fatten,
                    &t.dof_half_a,
                    clear,
                    &self.uniform_bg,
                    oe,
                    &t.bg_dof_half_edge,
                    None,
                );
                let of = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof far fixup coc",
                    &self.dof_fixup_coc,
                    &t.dof_half_b,
                    clear,
                    &self.uniform_bg,
                    of,
                    &t.bg_dof_half_fixup_far,
                    None,
                );
                let mut ub = base_u;
                ub.extra = [far_i, 0.0, 0.0, 0.0];
                let ob = push_u(ub);
                draw_pass(
                    encoder,
                    "dof far bokeh",
                    &self.dof_bokeh_far,
                    &t.dof_half_a,
                    clear,
                    &self.uniform_bg,
                    ob,
                    &t.bg_dof_half_bokeh,
                    None,
                );
                let ot = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof far tent",
                    &self.dof_tent,
                    &t.dof_half_b,
                    clear,
                    &self.uniform_bg,
                    ot,
                    &t.bg_dof_half_tent,
                    None,
                );
                let or = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof far resolve",
                    &dp.dof_resolve_far,
                    &t.dof_scene_a,
                    clear,
                    &self.uniform_bg,
                    or,
                    &t.bg_dof_resolve_half[scene_idx],
                    Some(&t.depth_bg),
                );
                let _ = full_h;
            }
            scene_idx = 1;
        }

        if near_i > 0.0 {
            if near_i < DOF_CHEAP_BELOW {
                let mut u0 = base_u;
                u0.extra = [1.0, 0.0, 0.0, 0.0]; // near
                let o0 = push_u(u0);
                draw_pass(
                    encoder,
                    "dof near cheap prep",
                    &dp.dof_cheap_prep,
                    &t.dof_full_prep,
                    clear,
                    &self.uniform_bg,
                    o0,
                    &t.bg_scene[scene_idx],
                    Some(&t.depth_bg),
                );
                let mut u1 = base_u;
                u1.extra = [near_i, 0.0, 0.0, 0.0];
                let o1 = push_u(u1);
                draw_pass(
                    encoder,
                    "dof near cheap",
                    &self.dof_cheap,
                    &t.dof_scene_b,
                    clear,
                    &self.uniform_bg,
                    o1,
                    &t.bg_dof_cheap,
                    None,
                );
            } else {
                // Studio Near HQ chain: div=2 (h<=540 || near<0.36) else div=4.
                let div = studio_near_dof_div(size[1], near_i);
                let full_w = size[0] as f32;
                let (rw, rh, taps, v_h, v_col, v_a, v_b, v_ca, v_cb, bg_dv, bg_ccol, bg_ca, bg_cb, bg_fx, bg_bk, bg_tn, bg_rs) =
                    if div == 2 {
                        (
                            t.half_size[0] as f32,
                            t.half_size[1] as f32,
                            2.0f32,
                            &t.dof_half_h,
                            &t.dof_half_col,
                            &t.dof_half_a,
                            &t.dof_half_b,
                            &t.dof_half_coc_a,
                            &t.dof_half_coc_b,
                            &t.bg_dof_half_down_v,
                            &t.bg_dof_half_coc_from_col,
                            &t.bg_dof_half_coc_a,
                            &t.bg_dof_half_coc_b,
                            &t.bg_dof_half_fixup_near,
                            &t.bg_dof_half_bokeh,
                            &t.bg_dof_half_tent,
                            &t.bg_dof_resolve_half[scene_idx],
                        )
                    } else {
                        (
                            t.quarter_dof_size[0] as f32,
                            t.quarter_dof_size[1] as f32,
                            4.0f32,
                            &t.dof_q_h,
                            &t.dof_q_col,
                            &t.dof_q_a,
                            &t.dof_q_b,
                            &t.dof_q_coc_a,
                            &t.dof_q_coc_b,
                            &t.bg_dof_q_down_v,
                            &t.bg_dof_q_coc_from_col,
                            &t.bg_dof_q_coc_a,
                            &t.bg_dof_q_coc_b,
                            &t.bg_dof_q_fixup_near,
                            &t.bg_dof_q_bokeh,
                            &t.bg_dof_q_tent,
                            &t.bg_dof_resolve_q[scene_idx],
                        )
                    };
                let red_x = 2.0 * (full_w - rw) / full_w;
                let red_y = (full_w - rh) / full_w;

                let mut uh = base_u;
                uh.extra = [red_x / rw, taps, 0.0, 0.0];
                let oh = push_u(uh);
                draw_pass(
                    encoder,
                    "dof near down h",
                    &dp.dof_near_down_h,
                    v_h,
                    clear,
                    &self.uniform_bg,
                    oh,
                    &t.bg_scene[scene_idx],
                    Some(&t.depth_bg),
                );
                let mut uv = base_u;
                uv.extra = [0.0, 0.5 * red_y / rh, 0.0, 0.0];
                let ov = push_u(uv);
                draw_pass(
                    encoder,
                    "dof near down v",
                    &self.dof_down_v,
                    v_col,
                    clear,
                    &self.uniform_bg,
                    ov,
                    bg_dv,
                    None,
                );

                // 6-pass Near CoC mask: Max H, Max V, Max H, Max V, Min H, Min V.
                let coc_steps = [
                    ([red_x / rw, 0.0], 0.0, 1.0, bg_ccol, v_cb),
                    ([0.0, red_y / rh], 0.0, 0.0, bg_cb, v_ca),
                    ([near_i / rw, 0.0], 0.0, 0.0, bg_ca, v_cb),
                    ([0.0, near_i / rh], 0.0, 0.0, bg_cb, v_ca),
                    ([near_i / rw, 0.0], 1.0, 0.0, bg_ca, v_cb),
                    ([0.0, near_i / rh], 1.0, 0.0, bg_cb, v_ca),
                ];
                for (step, mode, read_alpha, bg, dst) in coc_steps {
                    let mut uc = base_u;
                    uc.extra = [step[0], step[1], mode, read_alpha];
                    let oc = push_u(uc);
                    draw_pass(
                        encoder,
                        "dof near coc mask",
                        &self.dof_coc_filter,
                        dst,
                        clear,
                        &self.uniform_bg,
                        oc,
                        bg,
                        None,
                    );
                }

                let of = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof near fixup coc",
                    &self.dof_fixup_coc,
                    v_b,
                    clear,
                    &self.uniform_bg,
                    of,
                    bg_fx,
                    None,
                );
                let mut ub = base_u;
                ub.extra = [near_i, 0.0, 0.0, 0.0];
                let ob = push_u(ub);
                draw_pass(
                    encoder,
                    "dof near bokeh",
                    &self.dof_bokeh_near,
                    v_a,
                    clear,
                    &self.uniform_bg,
                    ob,
                    bg_bk,
                    None,
                );
                let ot = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof near tent",
                    &self.dof_tent,
                    v_b,
                    clear,
                    &self.uniform_bg,
                    ot,
                    bg_tn,
                    None,
                );
                let or = push_u(base_u);
                draw_pass(
                    encoder,
                    "dof near resolve",
                    &dp.dof_resolve_near,
                    &t.dof_scene_b,
                    clear,
                    &self.uniform_bg,
                    or,
                    bg_rs,
                    Some(&t.depth_bg),
                );
            }
            scene_idx = 2;
        }
        if with_gpu_timing {
            crate::gpu_timing::mark(encoder, "gpu: dof");
        }

        // ----------------------------------------------------------- Stage 2: Bloom
        let bloom_on = !bypass && params.bloom_allowed && params.bloom_intensity > 0.0001;
        let mut bloom_slot: usize = 0; // 0 = black, 1 = bloom[0] (HQ), 2 = bloom[2] (LQ)
        if bloom_on {
            let (start_level, levels, lod) =
                studio_bloom_schedule(size, params.bloom_size, params.post_fx_tier);
            for i in start_level..levels {
                let dst_sz = t.bloom_sizes[i];
                let mut ub = base_u;
                ub.extra = [1.0 / dst_sz[0] as f32, 1.0 / dst_sz[1] as f32, 0.0, 0.0];
                let ob = push_u(ub);
                let (pipe, bg) = if i == start_level {
                    (&self.bloom_extract, &t.bg_scene[scene_idx])
                } else {
                    (&self.bloom_down, &t.bg_bloom_down[i - 1])
                };
                draw_pass(
                    encoder,
                    "postfx bloom down",
                    pipe,
                    &t.bloom_views[i],
                    clear,
                    &self.uniform_bg,
                    ob,
                    bg,
                    None,
                );
                if i == 0 {
                    if let Some(capture) = &t.bloom_bright_capture {
                        encoder.copy_texture_to_texture(
                            t.bloom_textures[0].as_image_copy(),
                            capture.as_image_copy(),
                            capture.size(),
                        );
                    }
                }
            }
            for i in (start_level..levels - 1).rev() {
                let dst_sz = t.bloom_sizes[i];
                let weight = if i == levels - 2 {
                    lod - (levels as f32)
                } else {
                    1.0
                };
                let mut ub = base_u;
                ub.p1[2] = weight;
                ub.extra = [1.0 / dst_sz[0] as f32, 1.0 / dst_sz[1] as f32, 0.0, 0.0];
                let ob = push_u(ub);
                draw_pass(
                    encoder,
                    "postfx bloom up",
                    &self.bloom_up,
                    &t.bloom_views[i],
                    load,
                    &self.uniform_bg,
                    ob,
                    &t.bg_bloom_up[i],
                    None,
                );
            }
            bloom_slot = if start_level == 0 { 1 } else { 2 };
        }
        if with_gpu_timing {
            crate::gpu_timing::mark(encoder, "gpu: bloom");
        }

        // ----------------------------------------------------------- Stage 3: SunRays
        let sr_on = !bypass
            && !params.is_ortho
            && params.post_fx_tier >= 3
            && params.sun_rays_enabled
            && sr_cb_intensity > 0.0;
        let sr_slot = usize::from(sr_on);
        if sr_on {
            let od = push_u(base_u);
            draw_pass(
                encoder,
                "postfx sunrays depth",
                &dp.sunrays_depth,
                &t.sr_depth,
                clear,
                &self.uniform_bg,
                od,
                &t.bg_scene[0],
                Some(&t.depth_bg),
            );
            let mut um = base_u;
            um.sun2[3] = sr_ray_m as f32;
            let om = push_u(um);
            draw_pass(
                encoder,
                "postfx sunrays march",
                &self.sunrays_march,
                &t.sr_march,
                clear,
                &self.uniform_bg,
                om,
                &t.bg_sr_march,
                None,
            );
            let mut ur = base_u;
            ur.sun2[3] = sr_radial_n as f32;
            let or = push_u(ur);
            draw_pass(
                encoder,
                "postfx sunrays radial",
                &self.sunrays_radial,
                &t.sr_radial,
                clear,
                &self.uniform_bg,
                or,
                &t.bg_sr_radial,
                None,
            );
        }

        // ----------------------------------------------------------- Stage 4: Blur + FXCompositing
        if blur_weight > 0.0 {
            let qw = t.quarter_size[0] as f32;
            let qh = t.quarter_size[1] as f32;
            let hq_down = if params.post_fx_tier >= 2 { 1.0 } else { 0.0 };
            let need_fx_comp = bloom_slot != 0 || sr_slot != 0;

            let mut ud = base_u;
            ud.extra = [hq_down, 1.0 / qw, 1.0 / qh, 0.0];
            let od = push_u(ud);
            // If FXCompositing runs, downsample into blur_b then composite into blur_a;
            // otherwise downsample directly into blur_a!
            draw_pass(
                encoder,
                "postfx blur down",
                &self.blur_down,
                if need_fx_comp { &t.blur_b } else { &t.blur_a },
                clear,
                &self.uniform_bg,
                od,
                &t.bg_scene[scene_idx],
                None,
            );
            if need_fx_comp {
                let oc = push_u(base_u);
                draw_pass(
                    encoder,
                    "postfx fx composite",
                    &self.fx_composite,
                    &t.blur_a,
                    clear,
                    &self.uniform_bg,
                    oc,
                    &t.bg_fx_comp[bloom_slot][sr_slot],
                    None,
                );
            }
            let mut uh = base_u;
            uh.extra = [1.0 / qw, 0.0, blur_sigma, blur_k_pairs as f32];
            let oh = push_u(uh);
            draw_pass(
                encoder,
                "postfx blur h",
                &self.blur_1d,
                &t.blur_b,
                clear,
                &self.uniform_bg,
                oh,
                &t.bg_blur_h,
                None,
            );
            let mut uv = base_u;
            uv.extra = [0.0, 1.0 / qh, blur_sigma, blur_k_pairs as f32];
            let ov = push_u(uv);
            draw_pass(
                encoder,
                "postfx blur v",
                &self.blur_1d,
                &t.blur_a,
                clear,
                &self.uniform_bg,
                ov,
                &t.bg_blur_v,
                None,
            );
        }

        // ----------------------------------------------------------- Stage 5: ImageProcess
        let op = push_u(base_u);
        draw_pass(
            encoder,
            "postfx image process",
            &self.image_process,
            display_view,
            clear,
            &self.uniform_bg,
            op,
            &t.bg_image_process[scene_idx][bloom_slot][sr_slot],
            None,
        );
        if with_gpu_timing {
            crate::gpu_timing::mark(encoder, "gpu: post");
        }

        // Upload all uniform slots for this frame in a single write before submission.
        let needed_slots = self.ubo_scratch.len() / stride;
        if needed_slots > self.ubo_capacity_slots {
            // Unreachable under current pass counts (< 40 slots), but safe-guard anyway.
            self.ubo_capacity_slots = needed_slots.next_power_of_two();
            self.ubo = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("postfx ubo"),
                size: (self.ubo_stride as u64) * (self.ubo_capacity_slots as u64),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.ubo, 0, &self.ubo_scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc_matrix_matches_studio_order_and_luma() {
        // Contrast before Brightness, Studio luma (0.34, 0.55, 0.11), TintColor multiply.
        let b = 0.15;
        let c = 0.4;
        let s = -0.5;
        let tint = [1.0, 220.0 / 255.0, 180.0 / 255.0];
        let m = studio_cc_matrix(true, b, c, s, tint);

        let y = [0.2, 0.6, 0.8];
        let out = [
            m[0][0] * y[0] + m[0][1] * y[1] + m[0][2] * y[2] + m[0][3],
            m[1][0] * y[0] + m[1][1] * y[1] + m[1][2] * y[2] + m[1][3],
            m[2][0] * y[0] + m[2][1] * y[1] + m[2][2] * y[2] + m[2][3],
        ];
        // Direct formula: Contrast -> Brightness -> Saturation -> TintColor.
        let c1 = [
            (y[0] - 0.5) * (1.0 + c) + 0.5 + b,
            (y[1] - 0.5) * (1.0 + c) + 0.5 + b,
            (y[2] - 0.5) * (1.0 + c) + 0.5 + b,
        ];
        let luma = 0.34 * c1[0] + 0.55 * c1[1] + 0.11 * c1[2];
        let expected = [
            (luma + (c1[0] - luma) * (1.0 + s)) * tint[0],
            (luma + (c1[1] - luma) * (1.0 + s)) * tint[1],
            (luma + (c1[2] - luma) * (1.0 + s)) * tint[2],
        ];
        for i in 0..3 {
            assert!((out[i] - expected[i]).abs() < 1e-6, "ch {i}: {out:?} vs {expected:?}");
        }
    }

    #[test]
    fn sun_rays_params_and_noise_are_deterministic() {
        let (cb_i, g, col) = studio_sun_rays_params(
            Vec3::new(0.0, 0.5, -0.8660254).normalize(),
            Vec3::new(0.0, 0.5, -0.8660254).normalize(),
            0.25,
            1.0,
        );
        assert!((cb_i - 0.0125).abs() < 1e-5);
        assert!((g - 0.6).abs() < 1e-5);
        // Above y = 0.4, t = 1 -> sRGB = (0.75, 0.80, 1.0) -> linear = (0.5625, 0.64, 1.0)
        assert!((col[0] - 0.5625).abs() < 1e-4);
        assert!((col[1] - 0.64).abs() < 1e-4);
        assert!((col[2] - 1.0).abs() < 1e-4);

        let n1 = studio_sun_rays_noise();
        let n2 = studio_sun_rays_noise();
        assert_eq!(n1, n2);
        // Verify permutation contains 64 distinct values from 0..=251.
        let mut sorted = n1;
        sorted.sort();
        assert_eq!(sorted[0], 0);
        assert_eq!(sorted[63], ((63.0f32 * 0.015625 * 255.0) as u8));
    }

    #[test]
    fn blur_and_bloom_schedules_match_studio() {
        // 1920x1080 -> post_fx_scale = 1.0
        let (w, sigma, k) = studio_blur_params([1920, 1080], 56.0, 3);
        assert!((w - 1.0).abs() < 1e-6);
        assert!((sigma - 7.0).abs() < 1e-6);
        assert_eq!(k, 7);
        // Low quality tier (<2) clamps tap-pair search radius to 3.
        let (_, sigma_lq, k_lq) = studio_blur_params([1920, 1080], 56.0, 1);
        assert!((sigma_lq - 7.0).abs() < 1e-6);
        assert_eq!(k_lq, 3);
        // Weight ramps linearly 0..12
        assert!((studio_blur_params([1920, 1080], 6.0, 3).0 - 0.5).abs() < 1e-6);

        // HQ vs LQ bloom start level
        let (s_hq, lv_hq, _) = studio_bloom_schedule([1920, 1080], 24.0, 2);
        assert_eq!(s_hq, 0);
        assert_eq!(lv_hq, 4);
        let (s_lq, lv_lq, _) = studio_bloom_schedule([1920, 1080], 24.0, 1);
        assert_eq!(s_lq, 2);
        assert!(lv_lq >= 3);

        // Near DOF divisor
        assert_eq!(studio_near_dof_div(540, 1.0), 2);
        assert_eq!(studio_near_dof_div(804, 0.25), 2);
        assert_eq!(studio_near_dof_div(804, 0.36), 4);
        assert_eq!(studio_near_dof_div(804, 0.5), 4);
    }
}
