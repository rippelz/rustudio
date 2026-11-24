//! Highlight instances, as Studio 0.747 draws them (checks/mesh-deform/
//! AUDIT.md (g), tools/highlight.py): over the finished image, per
//! Highlight,
//! - the fill: FillColor over every pixel of the adornee's silhouette,
//!   alpha 1 − FillTransparency, a plain blend of display values (green at
//!   0.5 over a 198 grey part: 99, 226, 99);
//! - the outline: 2 px along the silhouette's edge, one each side of it,
//!   OutlineColor at 1 − OutlineTransparency over what is there;
//! - DepthMode AlwaysOnTop: the whole silhouette, through anything in front
//!   (fill and outline drawn over a wall hiding half the part); Occluded:
//!   only where the adornee is the nearest surface, the outline also along
//!   the edge of what hides it.
//! The silhouette is the adornee's drawn geometry (skinned meshes deformed,
//! layered clothing on), rendered into a mask with and without the scene's
//! depth.

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use glam::Mat4;

/// One Highlight's geometry and look for the frame.
#[derive(Clone)]
pub struct HighlightDraw {
    /// The parts it covers (the renderer's items).
    pub items: Vec<rbx_dom_weak::types::Ref>,
    /// Display-value colours, alpha = 1 − transparency.
    pub fill: [f32; 4],
    pub outline: [f32; 4],
    /// DepthMode AlwaysOnTop.
    pub on_top: bool,
}

/// Studio draws at most this many Highlights at once.
pub const MAX: usize = 31;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Inst {
    pub r0: [f32; 4],
    pub r1: [f32; 4],
    pub r2: [f32; 4],
    pub size: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    fill: [f32; 4],
    outline: [f32; 4],
    /// x: 1 AlwaysOnTop, 0 Occluded.
    mode: [f32; 4],
    _pad: [f32; 36],
}

const SHADER: &str = r#"
struct Params { view_proj: mat4x4<f32>, fill: vec4<f32>, outline: vec4<f32>, mode: vec4<f32> };
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var mask: texture_2d<f32>;

struct In {
    @location(0) pos: vec3<f32>,
    @location(2) r0: vec4<f32>,
    @location(3) r1: vec4<f32>,
    @location(4) r2: vec4<f32>,
    @location(5) size: vec4<f32>,
};

@vertex
fn vs_mask(v: In) -> @builtin(position) vec4<f32> {
    // (as the part shader: part_world, then the same depth nudge, so the
    // adornee's own surfaces pass the depth test against the scene)
    let l = v.pos * v.size.xyz;
    let origin = vec3<f32>(v.r0.w, v.r1.w, v.r2.w);
    let world = v.r0.xyz * l.x + v.r1.xyz * l.y + v.r2.xyz * l.z + origin;
    var c = p.view_proj * vec4<f32>(world, 1.0);
    c.z = c.z * (1.0 + v.size.w);
    return c;
}

@fragment
fn fs_mask() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0);
}

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn inside(q: vec2<i32>) -> bool {
    let d = vec2<i32>(textureDimensions(mask));
    if (q.x < 0 || q.y < 0 || q.x >= d.x || q.y >= d.y) { return false; }
    let m = textureLoad(mask, q, 0);
    return select(m.g, m.r, p.mode.x > 0.5) > 0.5;
}

@fragment
fn fs_comp(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let q = vec2<i32>(pos.xy);
    let me = inside(q);
    let edge = inside(q + vec2<i32>(1, 0)) != me || inside(q - vec2<i32>(1, 0)) != me
        || inside(q + vec2<i32>(0, 1)) != me || inside(q - vec2<i32>(0, 1)) != me;
    let af = select(0.0, p.fill.a, me);
    let ao = select(0.0, p.outline.a, edge);
    if (af <= 0.0 && ao <= 0.0) { discard; }
    // fill over the image, then the outline over that (premultiplied)
    let rgb = p.fill.rgb * af * (1.0 - ao) + p.outline.rgb * ao;
    let a = 1.0 - (1.0 - af) * (1.0 - ao);
    return vec4<f32>(rgb, a);
}
"#;

pub struct HighlightPass {
    all: wgpu::RenderPipeline,
    visible: wgpu::RenderPipeline,
    comp: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    params_bg: wgpu::BindGroup,
    insts: wgpu::Buffer,
    inst_cap: usize,
    /// (size, MSAA mask, resolved mask, bind group)
    target: Option<([u32; 2], wgpu::TextureView, wgpu::TextureView, wgpu::BindGroup)>,
}

pub(crate) const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg8Unorm;
const PARAMS_STRIDE: u64 = std::mem::size_of::<Params>() as u64;

impl HighlightPass {
    pub fn new(
        device: &wgpu::Device,
        display: wgpu::TextureFormat,
        depth: wgpu::TextureFormat,
        samples: u32,
        vertex_stride: u64,
    ) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("highlight"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("highlight"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_STRIDE),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("highlight params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(PARAMS_STRIDE),
                },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("highlight"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let mask_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("highlight mask"),
            bind_group_layouts: &[Some(&params_layout)],
            immediate_size: 0,
        });
        let vbuf = [
            Some(wgpu::VertexBufferLayout {
                array_stride: vertex_stride,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Inst>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4],
            }),
        ];
        let mask_pipe = |label: &str, writes: wgpu::ColorWrites, depth_test: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&mask_pl),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_mask"),
                    compilation_options: Default::default(),
                    buffers: &vbuf,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: depth_test.then(|| wgpu::DepthStencilState {
                    format: depth,
                    depth_write_enabled: Some(false),
                    // reverse-Z: nearer is greater; the adornee's own
                    // surfaces are equal
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs_mask"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: MASK_FORMAT, blend: None, write_mask: writes })],
                }),
                multiview_mask: None,
                cache: crate::pipeline_cache::current(),
            })
        };
        let all = mask_pipe("highlight mask", wgpu::ColorWrites::RED, false);
        let visible = mask_pipe("highlight visible mask", wgpu::ColorWrites::GREEN, true);
        let premul = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let comp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("highlight composite"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_comp"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: display,
                    blend: Some(wgpu::BlendState { color: premul, alpha: premul }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: crate::pipeline_cache::current(),
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("highlight params"),
            size: PARAMS_STRIDE * MAX as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let insts = Self::inst_buf(device, 64);
        let params_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("highlight params"),
            layout: &params_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &params, offset: 0, size: wgpu::BufferSize::new(PARAMS_STRIDE) }),
            }],
        });
        HighlightPass { all, visible, comp, layout, params, params_bg, insts, inst_cap: 64, target: None }
    }

    fn inst_buf(device: &wgpu::Device, n: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("highlight instances"),
            size: (n * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Draws the highlights over `display`: per highlight, the meshes
    /// (mesh, instance) of its parts.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: [u32; 2],
        samples: u32,
        depth: &wgpu::TextureView,
        display: &wgpu::TextureView,
        view_proj: Mat4,
        list: &[(&HighlightDraw, Vec<(&crate::render::Mesh, Inst)>)],
    ) {
        if list.is_empty() {
            return;
        }
        if self.target.as_ref().is_none_or(|t| t.0 != size) {
            let tex = |label: &str, n: u32, usage: wgpu::TextureUsages| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: n,
                        dimension: wgpu::TextureDimension::D2,
                        format: MASK_FORMAT,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            let one = tex("highlight mask", 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
            let ms = if samples == 1 { one.clone() } else { tex("highlight mask msaa", samples, wgpu::TextureUsages::RENDER_ATTACHMENT) };
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("highlight"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.params,
                            offset: 0,
                            size: wgpu::BufferSize::new(PARAMS_STRIDE),
                        }),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&one) },
                ],
            });
            self.target = Some((size, ms, one, bg));
        }
        let n_inst: usize = list.iter().map(|(_, m)| m.len()).sum();
        if n_inst > self.inst_cap {
            self.inst_cap = n_inst.next_power_of_two();
            self.insts = Self::inst_buf(device, self.inst_cap);
        }
        let insts: Vec<Inst> = list.iter().flat_map(|(_, m)| m.iter().map(|x| x.1)).collect();
        if !insts.is_empty() {
            queue.write_buffer(&self.insts, 0, bytemuck::cast_slice(&insts));
        }
        for (i, (h, _)) in list.iter().enumerate().take(MAX) {
            let p = Params {
                view_proj: view_proj.to_cols_array_2d(),
                fill: h.fill,
                outline: h.outline,
                mode: [if h.on_top { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
                _pad: [0.0; 36],
            };
            queue.write_buffer(&self.params, i as u64 * PARAMS_STRIDE, bytemuck::bytes_of(&p));
        }
        let Some((_, ms, one, bg)) = self.target.as_ref() else { return };
        // The composite's pixels: the highlighted meshes' screen box and a
        // margin (outside it the mask and its four neighbours are empty, so
        // fs_comp discards); the whole view when a box reaches behind the
        // camera or a mesh is skinned (its vertices leave its box).
        let rect_of = |meshes: &Vec<(&crate::render::Mesh, Inst)>| -> [u32; 4] {
            let full = [0, 0, size[0], size[1]];
            let (mut lo, mut hi) = (glam::Vec2::splat(f32::INFINITY), glam::Vec2::splat(f32::NEG_INFINITY));
            for (mesh, inst) in meshes {
                let Some((blo, bhi)) = mesh.screen_bounds() else { return full };
                let axis = |r: [f32; 4]| glam::Vec3::new(r[0], r[1], r[2]);
                let origin = glam::Vec3::new(inst.r0[3], inst.r1[3], inst.r2[3]);
                let s = glam::Vec3::new(inst.size[0], inst.size[1], inst.size[2]);
                for c in 0..8 {
                    let l = glam::Vec3::new(
                        if c & 1 == 0 { blo.x } else { bhi.x },
                        if c & 2 == 0 { blo.y } else { bhi.y },
                        if c & 4 == 0 { blo.z } else { bhi.z },
                    ) * s;
                    let world = axis(inst.r0) * l.x + axis(inst.r1) * l.y + axis(inst.r2) * l.z + origin;
                    let clip = view_proj * world.extend(1.0);
                    if clip.w <= 1e-3 {
                        return full;
                    }
                    let ndc = glam::Vec2::new(clip.x, clip.y) / clip.w;
                    let px = glam::Vec2::new((ndc.x + 1.0) * 0.5 * size[0] as f32, (1.0 - ndc.y) * 0.5 * size[1] as f32);
                    lo = lo.min(px);
                    hi = hi.max(px);
                }
            }
            if !(lo.x.is_finite() && lo.y.is_finite() && hi.x.is_finite() && hi.y.is_finite()) {
                return full;
            }
            // (four pixels of margin: rasterization's own rounding plus the
            // outline's one-pixel reach)
            let x0 = (lo.x.floor() - 4.0).clamp(0.0, size[0] as f32) as u32;
            let y0 = (lo.y.floor() - 4.0).clamp(0.0, size[1] as f32) as u32;
            let x1 = (hi.x.ceil() + 4.0).clamp(0.0, size[0] as f32) as u32;
            let y1 = (hi.y.ceil() + 4.0).clamp(0.0, size[1] as f32) as u32;
            [x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)]
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("highlights") });
        let mut first = 0u32;
        for (i, (h, meshes)) in list.iter().enumerate().take(MAX) {
            let off = [(i as u64 * PARAMS_STRIDE) as u32];
            let range = first..first + meshes.len() as u32;
            first = range.end;
            let draw_meshes = |pass: &mut wgpu::RenderPass| {
                pass.set_bind_group(0, &self.params_bg, &off);
                pass.set_vertex_buffer(1, self.insts.slice(..));
                for (k, (mesh, _)) in meshes.iter().enumerate() {
                    let n = range.start + k as u32;
                    mesh.draw(pass, n..n + 1);
                }
            };
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("highlight mask"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: ms,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.all);
                draw_meshes(&mut pass);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("highlight visible mask"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: ms,
                        depth_slice: None,
                        resolve_target: (samples > 1).then_some(one),
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: depth,
                        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.visible);
                draw_meshes(&mut pass);
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("highlight composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: display,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                let [x, y, w, h] = rect_of(meshes);
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                pass.set_pipeline(&self.comp);
                pass.set_bind_group(0, bg, &off);
                pass.draw(0..3, 0..1);
            }
        }
        crate::gpu_timing::submit(device, queue, "gpu: highlight", [encoder.finish()]);
    }
}
