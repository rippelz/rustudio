//! Terrain.Clouds: Studio's volumetric cloud layer, its Beer shadow map, and
//! the "sun cover" it reads back to soften and dim the sun (2026-11-12,
//! checks/grass-shadow-stability/clouds/FINDINGS.md).
//!
//! Ported from Studio (Studio build, shaders_glsl3.pack and the Vulkan pack
//! it draws with):
//!
//! * Parameters (Studio, from the Clouds instance): CloudsParams =
//!   DirDist (wind offset xyz in km, w = render time / 1000), LightColorSize,
//!   CoverDensityQuality (Cover² on the quality path, else 1.78·Cover⁴ +
//!   0.03; Density; 18 steps), Color (Clouds.Color), DetailScale (0.25, 0.25).
//!   GlobalWind is clamped to 600 long. Each frame the offset
//!   moves by −0.00084 · wind · dt km and wraps at 32.0513 (fmodf).
//! * Passes: CloudsBsmFS into a 512² Beer shadow map when the
//!   cover / density / quality or the sun's height change; CloudsSunCoverFS
//!   into one texel, read back; CloudsWindFS at a quarter of
//!   the view; CloudsReprojectFilterFS / CloudsUpsampleNoHistoryFS at half;
//!   CloudsCompFS over the scene. Textures: rbxasset://sky/clouds.dds (the
//!   256×256×32 distance field), cloudAdvection.dds, cloudDetail3D.dds, bn.dds
//!   (byte-identical to what a running Studio binds, RenderDoc 2026-11-12).
//! * Sun cover (Studio, getter Studio): target = max(texel, 0.5)
//!   (this), smoothed cover += (1 − 0.15^dt) · (target − cover), 1.0
//!   until the first read. It then
//!   - widens the sun's shadow blur (Studio, before Studio):
//!     softness = min(ShadowSoftness + 1 − cover, 1);
//!   - rides in Globals SkyAmbient.w (Studio → renderer), which
//!     the Vulkan pack's surface shaders use as: sun diffuse × cover, sun
//!     specular × cover², SkyAmbientNoIBL × skylight × (2 − cover)
//!     (DefaultUnified*, SmoothCluster, SmoothWater, Grass). rustudio keeps
//!     it in Globals.outdoor.w (shader.wgsl `sun_shadow`, `sun_cover`).

use crate::doc::Document;
use glam::Vec3;
use rbx_dom_weak::types::Variant;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;
use egui_wgpu::wgpu;

/// What the place asks for: the active Clouds (the first under Terrain).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudsLook {
    pub cover: f32,
    pub density: f32,
    /// Clouds.Color (0..1, as the shader takes it: not squared).
    pub color: [f32; 3],
    /// Workspace.GlobalWind (studs / s).
    pub wind: Vec3,
    /// Studio's FogParams: the clouds fade into
    /// the sky with it toward the horizon.
    pub fog: [f32; 4],
}

/// Clouds.Cover / Density / Color defaults (the class's).
const DEFAULT_COVER: f32 = 0.5;
const DEFAULT_DENSITY: f32 = 0.7;
/// CoverDensityQuality.z / .w at Studio's top quality levels (RenderDoc).
const QUALITY_STEPS: f32 = 18.0;
const QUALITY_W: f32 = 17.0;
/// DetailScale (RenderDoc, CloudsParams[4]).
const DETAIL_SCALE: [f32; 2] = [0.25, 0.25];
/// The wind offset per stud/s of GlobalWind per second, in km ([Studio]),
/// its wrap ([Studio]) and the wind's longest ([Studio]).
const WIND_KM: f32 = 0.00084;
const WIND_WRAP: f32 = 32.051_28;
const WIND_MAX: f32 = 600.0;
/// Sun cover: Params1 (40, 10) set in the constructor Studio, the floor
/// ( 0.5) and the smoothing base ([Studio] 0.15 per second).
const COVER_K: f32 = 40.0;
const COVER_DISK_TEXELS: f32 = 10.0;
const COVER_FLOOR: f32 = 0.5;
const COVER_SMOOTH: f32 = 0.15;
const BSM_SIZE: u32 = 512;

fn child_of_class(d: &Document, parent: rbx_dom_weak::types::Ref, class: &str) -> Option<rbx_dom_weak::types::Ref> {
    d.dom.get_by_ref(parent)?.children().iter().copied().find(|r| d.class_of(*r).is_some_and(|c| c == class))
}

/// The place's active clouds (Workspace.Terrain's Clouds, Enabled).
pub fn look(d: &Document) -> Option<CloudsLook> {
    let ws = d.workspace()?;
    let terrain = child_of_class(d, ws, "Terrain")?;
    let c = child_of_class(d, terrain, "Clouds")?;
    if matches!(d.prop(c, "Enabled"), Some(Variant::Bool(false))) {
        return None;
    }
    let f = |n: &str, dflt: f32| crate::scene::f32_of(d.prop(c, n)).unwrap_or(dflt);
    let color = match d.prop(c, "Color") {
        Some(Variant::Color3(c)) => [c.r, c.g, c.b],
        Some(Variant::Color3uint8(c)) => [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0],
        _ => [1.0; 3],
    };
    let wind = crate::scene::vec3_of(d.prop(ws, "GlobalWind")).unwrap_or(Vec3::ZERO);
    let fog = match crate::lighting::atmosphere(d) {
        // (0, Offset, −0.14427 · clamp(Density)⁴, 0): fog_k is that in natural log.
        Some(a) => [0.0, a.offset, -a.fog_k / std::f32::consts::LN_2, 0.0],
        None => {
            let l = d.find_child(d.dom.root_ref(), "Lighting");
            let g = |n: &str, dflt: f32| l.and_then(|l| crate::scene::f32_of(d.prop(l, n))).unwrap_or(dflt);
            let (s, e) = (g("FogStart", 0.0), g("FogEnd", 100_000.0));
            let span = (e - s).max(1e-3);
            [e / span, 0.0, -1.0 / span, 1.0]
        }
    };
    Some(CloudsLook {
        cover: f("Cover", DEFAULT_COVER).clamp(0.0, 1.0),
        density: f("Density", DEFAULT_DENSITY).clamp(0.0, 1.0),
        color,
        wind,
        fog,
    })
}

/// The smoothed sun cover (f32 bits): 1 = clear.
static COVER: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// The sun cover for this frame's lighting (1 without clouds). Resets when
/// the clouds go away.
pub fn sun_cover(active: bool) -> f32 {
    if !active {
        COVER.store(1.0f32.to_bits(), Ordering::Relaxed);
        return 1.0;
    }
    f32::from_bits(COVER.load(Ordering::Relaxed))
}

/// The sun's shadow softness with the clouds' share.
pub fn shadow_softness(softness: f32, cover: f32) -> f32 {
    (softness + 1.0 - cover).min(1.0)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CloudsRaw {
    dir_dist: [f32; 4],
    cdq: [f32; 4],
    color: [f32; 4],
    detail: [f32; 4],
    fogp: [f32; 4],
    quarter: [f32; 4],
    half: [f32; 4],
    full: [f32; 4],
    cover: [f32; 4],
}

/// One DDS's pixel data: (width, height, depth, mips as (w, h, d, bytes)).
struct Dds {
    mips: Vec<(u32, u32, u32, Vec<u8>)>,
}

/// Reads an uncompressed or BC1 DDS (2D or 3D) with its mips; BC1 decoded
/// to RGBA8, `bpp` bytes per texel otherwise.
fn read_dds(bytes: &[u8]) -> Option<(Dds, bool)> {
    if bytes.len() < 128 || &bytes[0..4] != b"DDS " {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let (h, w) = (u(12), u(16));
    let depth = u(24).max(1);
    let mips = u(28).max(1);
    let fourcc = &bytes[84..88];
    let (mut off, bc1, bpp, swap) = if fourcc == b"DX10" {
        // DXGI 61 R8_UNORM, 28 R8G8B8A8_UNORM.
        match u(128) {
            61 => (148usize, false, 1usize, false),
            28 => (148, false, 4, false),
            71 => (148, true, 0, false),
            _ => return None,
        }
    } else if fourcc == b"DXT1" {
        (128, true, 0, false)
    } else if u(88) == 32 {
        // 32-bit with masks: bn.dds is B8G8R8A8.
        (128, false, 4, u(92) == 0x00ff_0000)
    } else {
        return None;
    };
    let mut out = Vec::new();
    let (mut mw, mut mh, mut md) = (w, h, depth);
    for _ in 0..mips {
        let n = if bc1 {
            (mw.div_ceil(4) * mh.div_ceil(4) * 8) as usize * md as usize
        } else {
            (mw * mh * md) as usize * bpp
        };
        let src = bytes.get(off..off + n)?;
        off += n;
        let data = if bc1 {
            decode_bc1(src, mw, mh)
        } else if swap {
            src.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect()
        } else {
            src.to_vec()
        };
        out.push((mw, mh, md, data));
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
        md = (md / 2).max(1);
    }
    Some((Dds { mips: out }, bc1 || bpp == 4))
}

fn decode_bc1(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let c565 = |c: u16| {
        let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
        [((r << 3) | (r >> 2)) as f32, ((g << 2) | (g >> 4)) as f32, ((b << 3) | (b >> 2)) as f32]
    };
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let i = ((by * w.div_ceil(4) + bx) * 8) as usize;
            let b = &src[i..i + 8];
            let (c0, c1) = (u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]]));
            let (a, z) = (c565(c0), c565(c1));
            let mix = |t: f32, al: u8| [0, 1, 2].map(|k| (a[k] + (z[k] - a[k]) * t).round() as u8).into_iter().chain([al]).collect::<Vec<u8>>();
            let pal: [Vec<u8>; 4] = if c0 > c1 {
                [mix(0.0, 255), mix(1.0, 255), mix(1.0 / 3.0, 255), mix(2.0 / 3.0, 255)]
            } else {
                [mix(0.0, 255), mix(1.0, 255), mix(0.5, 255), vec![0, 0, 0, 0]]
            };
            let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
            for py in 0..4 {
                for px in 0..4 {
                    let (x, y) = (bx * 4 + px, by * 4 + py);
                    if x < w && y < h {
                        let k = ((bits >> (2 * (py * 4 + px))) & 3) as usize;
                        let o = ((y * w + x) * 4) as usize;
                        out[o..o + 4].copy_from_slice(&pal[k]);
                    }
                }
            }
        }
    }
    out
}

/// A texture from Studio's content (`rbxasset://sky/…`), all its mips; a
/// 1-texel stand-in when missing.
fn content_texture(device: &wgpu::Device, queue: &wgpu::Queue, id: &str, three_d: bool) -> wgpu::TextureView {
    let parsed = crate::assets::resolve(id, None)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| read_dds(&b));
    if parsed.is_none() {
        eprintln!("[clouds] {id} missing: clouds will not draw right");
    }
    let (dds, rgba) = parsed.unwrap_or_else(|| (Dds { mips: vec![(1, 1, 1, vec![255; 4])] }, true));
    let (w, h, d, _) = dds.mips[0];
    let format = if rgba { wgpu::TextureFormat::Rgba8Unorm } else { wgpu::TextureFormat::R8Unorm };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(id),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: d },
        mip_level_count: dds.mips.len() as u32,
        sample_count: 1,
        dimension: if three_d { wgpu::TextureDimension::D3 } else { wgpu::TextureDimension::D2 },
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bpp = if rgba { 4 } else { 1 };
    for (level, (mw, mh, md, data)) in dds.mips.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(mw * bpp), rows_per_image: Some(*mh) },
            wgpu::Extent3d { width: *mw, height: *mh, depth_or_array_layers: *md },
        );
    }
    tex.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(if three_d { wgpu::TextureViewDimension::D3 } else { wgpu::TextureViewDimension::D2 }),
        ..Default::default()
    })
}

struct Sized {
    full: [u32; 2],
    quarter: [u32; 2],
    half: [u32; 2],
    quarter_view: wgpu::TextureView,
    half_view: wgpu::TextureView,
}

pub struct CloudsGpu {
    buf: wgpu::Buffer,
    layout: wgpu::BindGroupLayout,
    dist: wgpu::TextureView,
    adv: wgpu::TextureView,
    detail: wgpu::TextureView,
    noise: wgpu::TextureView,
    bsm: wgpu::TextureView,
    cover_tex: wgpu::Texture,
    cover_view: wgpu::TextureView,
    cover_buf: wgpu::Buffer,
    dummy2d: wgpu::TextureView,
    dummy_depth: wgpu::TextureView,
    rep: wgpu::Sampler,
    dist_s: wgpu::Sampler,
    clamp: wgpu::Sampler,
    bsm_pipe: wgpu::RenderPipeline,
    cover_pipe: wgpu::RenderPipeline,
    wind_pipe: wgpu::RenderPipeline,
    up_pipe: wgpu::RenderPipeline,
    comp_pipe: wgpu::RenderPipeline,
    sized: Option<Sized>,
    /// What the BSM was last drawn for: (Cover², Density, steps, Lamp0Dir.y).
    bsm_key: Option<[f32; 4]>,
    started: Instant,
    last: Instant,
    offset: Vec3,
    /// The sun cover read back: 0 idle, 1 copy recorded (map after submit), 2 mapping.
    readback: u8,
    mapped: std::sync::Arc<std::sync::atomic::AtomicU8>,
    target: f32,
    cover: f32,
    have_cover: bool,
    /// Whether this frame draws clouds (set by `encode`, read by `composite`).
    active: bool,
}

impl CloudsGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, globals: &wgpu::BindGroupLayout, shader_source: &str, hdr: wgpu::TextureFormat) -> Self {
        let src = format!("{shader_source}\n{}", include_str!("clouds.wgsl"));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clouds"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        use wgpu::{BindGroupLayoutEntry as E, BindingType as T, ShaderStages as S};
        let tex = |binding: u32, dim: wgpu::TextureViewDimension| E {
            binding,
            visibility: S::FRAGMENT,
            ty: T::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: dim, multisampled: false },
            count: None,
        };
        let samp = |binding: u32| E { binding, visibility: S::FRAGMENT, ty: T::Sampler(wgpu::SamplerBindingType::Filtering), count: None };
        let d2 = wgpu::TextureViewDimension::D2;
        let d3 = wgpu::TextureViewDimension::D3;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("clouds"),
            entries: &[
                E { binding: 0, visibility: S::FRAGMENT, ty: T::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                tex(1, d3),
                tex(2, d2),
                tex(3, d3),
                tex(4, d2),
                tex(5, d2),
                samp(6),
                samp(7),
                tex(8, d2),
                samp(9),
                tex(10, d2),
                E { binding: 11, visibility: S::FRAGMENT, ty: T::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: d2, multisampled: true }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("clouds"),
            bind_group_layouts: &[Some(globals), None, None, Some(&layout)],
            immediate_size: 0,
        });
        let pipe = |label: &str, entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_clouds"), compilation_options: Default::default(), buffers: &[] },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let f16 = wgpu::TextureFormat::Rgba16Float;
        let bsm_pipe = pipe("clouds bsm", "fs_clouds_bsm", f16);
        let cover_pipe = pipe("clouds sun cover", "fs_clouds_cover", wgpu::TextureFormat::R8Unorm);
        let wind_pipe = pipe("clouds wind", "fs_clouds_wind", f16);
        let up_pipe = pipe("clouds upsample", "fs_clouds_up", f16);
        let comp_pipe = pipe("clouds comp", "fs_clouds_comp", hdr);
        let target = |label: &str, w: u32, h: u32, format: wgpu::TextureFormat, samples: u32, usage: wgpu::TextureUsages| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let rt = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let bsm = target("clouds bsm", BSM_SIZE, BSM_SIZE, f16, 1, rt).create_view(&Default::default());
        let cover_tex = target("clouds sun cover", 1, 1, wgpu::TextureFormat::R8Unorm, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC);
        let cover_view = cover_tex.create_view(&Default::default());
        let dummy2d = target("clouds none", 1, 1, f16, 1, rt).create_view(&Default::default());
        let dummy_depth = target("clouds no depth", 1, 1, wgpu::TextureFormat::Depth32Float, 4, rt).create_view(&Default::default());
        let sampler = |label: &str, w: wgpu::AddressMode, uv: wgpu::AddressMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: uv,
                address_mode_v: uv,
                address_mode_w: w,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            })
        };
        let rep = sampler("clouds repeat", wgpu::AddressMode::Repeat, wgpu::AddressMode::Repeat);
        let dist_s = sampler("clouds field", wgpu::AddressMode::ClampToEdge, wgpu::AddressMode::Repeat);
        let clamp = sampler("clouds clamp", wgpu::AddressMode::ClampToEdge, wgpu::AddressMode::ClampToEdge);
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clouds"),
            size: std::mem::size_of::<CloudsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cover_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clouds sun cover"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            buf,
            layout,
            dist: content_texture(device, queue, "rbxasset://sky/clouds.dds", true),
            adv: content_texture(device, queue, "rbxasset://sky/cloudAdvection.dds", false),
            detail: content_texture(device, queue, "rbxasset://sky/cloudDetail3D.dds", true),
            noise: content_texture(device, queue, "rbxasset://sky/bn.dds", false),
            bsm,
            cover_tex,
            cover_view,
            cover_buf,
            dummy2d,
            dummy_depth,
            rep,
            dist_s,
            clamp,
            bsm_pipe,
            cover_pipe,
            wind_pipe,
            up_pipe,
            comp_pipe,
            sized: None,
            bsm_key: None,
            started: Instant::now(),
            last: Instant::now(),
            offset: Vec3::ZERO,
            readback: 0,
            mapped: Default::default(),
            target: 1.0,
            cover: 1.0,
            have_cover: false,
            active: false,
        }
    }

    fn group(&self, device: &wgpu::Device, bsm: &wgpu::TextureView, input: &wgpu::TextureView, scene: &wgpu::TextureView, depth: &wgpu::TextureView) -> wgpu::BindGroup {
        use wgpu::BindingResource as R;
        fn e(binding: u32, resource: R<'_>) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding, resource }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clouds"),
            layout: &self.layout,
            entries: &[
                e(0, self.buf.as_entire_binding()),
                e(1, R::TextureView(&self.dist)),
                e(2, R::TextureView(&self.adv)),
                e(3, R::TextureView(&self.detail)),
                e(4, R::TextureView(&self.noise)),
                e(5, R::TextureView(bsm)),
                e(6, R::Sampler(&self.rep)),
                e(7, R::Sampler(&self.dist_s)),
                e(8, R::TextureView(input)),
                e(9, R::Sampler(&self.clamp)),
                e(10, R::TextureView(scene)),
                e(11, R::TextureView(depth)),
            ],
        })
    }

    fn ensure_size(&mut self, device: &wgpu::Device, full: [u32; 2]) {
        if self.sized.as_ref().is_some_and(|s| s.full == full) {
            return;
        }
        // (W + 1) >> 2 and >> 1, as Studio sizes them.
        let quarter = [((full[0] + 1) >> 2).max(1), ((full[1] + 1) >> 2).max(1)];
        let half = [((full[0] + 1) >> 1).max(1), ((full[1] + 1) >> 1).max(1)];
        let make = |label: &str, s: [u32; 2]| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: s[0], height: s[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        self.sized = Some(Sized { full, quarter, half, quarter_view: make("clouds quarter", quarter), half_view: make("clouds half", half) });
    }

    /// The sun cover read back (if it has arrived), smoothed by dt.
    fn update_cover(&mut self, device: &wgpu::Device, dt: f32) {
        if self.readback == 2 {
            let _ = device.poll(wgpu::PollType::Poll);
            match self.mapped.load(Ordering::Acquire) {
                1 => {
                    let byte = self.cover_buf.slice(..).get_mapped_range().ok().map(|d| d[0]);
                    self.cover_buf.unmap();
                    if let Some(b) = byte {
                        self.target = (b as f32 / 255.0).max(COVER_FLOOR);
                        self.have_cover = true;
                        if std::env::var_os("RUSTUDIO_CLOUDS_DEBUG").is_some() {
                            eprintln!("[clouds] sun cover texel {b} target {:.4} smoothed {:.4}", self.target, self.cover);
                        }
                    }
                    self.readback = 0;
                }
                2 => {
                    self.readback = 0;
                }
                _ => {}
            }
        }
        if self.have_cover {
            self.cover += (1.0 - COVER_SMOOTH.powf(dt)) * (self.target - self.cover);
        }
        let shown = if self.have_cover { self.cover } else { 1.0 };
        COVER.store(shown.to_bits(), Ordering::Relaxed);
    }

    /// Records the cloud passes up to the half-resolution layer (before the
    /// scene); `composite` lays it over the scene later.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        globals_bg: &wgpu::BindGroup,
        look: &CloudsLook,
        sun_y: f32,
        full: [u32; 2],
    ) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(1.0);
        self.last = now;
        self.active = true;
        self.update_cover(device, dt);
        let time = std::env::var("RUSTUDIO_CLOUDS_TIME")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or_else(|| self.started.elapsed().as_secs_f32());
        let wind = if look.wind.length() > WIND_MAX { look.wind.normalize() * WIND_MAX } else { look.wind };
        // fmodf: the sign follows the dividend.
        self.offset.x = (self.offset.x - WIND_KM * wind.x * dt) % WIND_WRAP;
        self.offset.z = (self.offset.z - WIND_KM * wind.z * dt) % WIND_WRAP;
        self.ensure_size(device, full);
        let s = self.sized.as_ref().unwrap();
        let v = |a: [u32; 2]| [a[0] as f32, a[1] as f32, 1.0 / a[0] as f32, 1.0 / a[1] as f32];
        let cdq = [look.cover * look.cover, look.density, QUALITY_STEPS, QUALITY_W];
        let raw = CloudsRaw {
            dir_dist: [self.offset.x, 0.0, self.offset.z, time / 1000.0],
            cdq,
            color: [look.color[0], look.color[1], look.color[2], 1.0],
            detail: [DETAIL_SCALE[0], DETAIL_SCALE[1], 0.0, 0.0],
            fogp: look.fog,
            quarter: v(s.quarter),
            half: v(s.half),
            full: v(s.full),
            cover: [COVER_K, COVER_DISK_TEXELS, 1.0 / BSM_SIZE as f32, 0.0],
        };
        queue.write_buffer(&self.buf, 0, bytemuck::bytes_of(&raw));
        let pass = |encoder: &mut wgpu::CommandEncoder, label: &str, view: &wgpu::TextureView, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup| {
            let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            p.set_pipeline(pipe);
            p.set_bind_group(0, globals_bg, &[]);
            p.set_bind_group(3, bg, &[]);
            p.draw(0..3, 0..1);
        };
        // The Beer shadow map, when what it depends on changed (Studio:
        // CoverDensityQuality, Lamp0Dir.y).
        let key = [cdq[0], cdq[1], cdq[2], sun_y];
        if self.bsm_key != Some(key) {
            self.bsm_key = Some(key);
            let bg = self.group(device, &self.dummy2d, &self.dummy2d, &self.dummy2d, &self.dummy_depth);
            pass(encoder, "clouds bsm", &self.bsm, &self.bsm_pipe, &bg);
        }
        let quarter_bg = self.group(device, &self.bsm, &self.dummy2d, &self.dummy2d, &self.dummy_depth);
        if self.readback == 0 {
            pass(encoder, "clouds sun cover", &self.cover_view, &self.cover_pipe, &quarter_bg);
            encoder.copy_texture_to_buffer(
                self.cover_tex.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.cover_buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(1) },
                },
                wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            );
            self.readback = 1;
        }
        pass(encoder, "clouds wind", &s.quarter_view, &self.wind_pipe, &quarter_bg);
        let half_bg = self.group(device, &self.bsm, &s.quarter_view, &self.dummy2d, &self.dummy_depth);
        pass(encoder, "clouds upsample", &s.half_view, &self.up_pipe, &half_bg);
    }

    /// Lays the clouds over the scene (`hdr`, whose copy is `scene`) where
    /// the depth shows nothing was drawn.
    pub fn composite(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        globals_bg: &wgpu::BindGroup,
        hdr: &wgpu::TextureView,
        scene: &wgpu::TextureView,
        depth: &wgpu::TextureView,
    ) {
        if !self.active {
            return;
        }
        let Some(s) = self.sized.as_ref() else { return };
        let bg = self.group(device, &self.bsm, &s.half_view, scene, depth);
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clouds comp"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: hdr,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_pipeline(&self.comp_pipe);
        p.set_bind_group(0, globals_bg, &[]);
        p.set_bind_group(3, &bg, &[]);
        p.draw(0..3, 0..1);
    }

    /// After the frame's submit: start mapping the sun cover copied this frame.
    pub fn after_submit(&mut self) {
        if self.readback == 1 {
            self.readback = 2;
            self.mapped.store(0, Ordering::Release);
            let m = self.mapped.clone();
            self.cover_buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                m.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release);
            });
        }
        self.active = false;
    }

    /// No clouds this frame: forget the cover.
    pub fn idle(&mut self) {
        self.active = false;
        self.have_cover = false;
        self.cover = 1.0;
        self.target = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softness_takes_the_clouds_share() {
        assert!((shadow_softness(0.2, 1.0) - 0.2).abs() < 1e-6);
        assert!((shadow_softness(0.0, 0.506) - 0.494).abs() < 1e-6);
        assert_eq!(shadow_softness(0.9, 0.5), 1.0);
    }

    #[test]
    fn studio_cloud_textures_parse() {
        for (id, dims) in [
            ("rbxasset://sky/clouds.dds", (256, 256, 32)),
            ("rbxasset://sky/cloudDetail3D.dds", (64, 64, 64)),
            ("rbxasset://sky/cloudAdvection.dds", (256, 256, 1)),
            ("rbxasset://sky/bn.dds", (128, 128, 1)),
        ] {
            let Some(b) = crate::assets::resolve(id, None).and_then(|p| std::fs::read(p).ok()) else {
                eprintln!("{id}: no Studio content here, skipped");
                continue;
            };
            let (d, _) = read_dds(&b).expect(id);
            let (w, h, z, _) = &d.mips[0];
            assert_eq!((*w, *h, *z), dims, "{id}");
        }
    }
}
