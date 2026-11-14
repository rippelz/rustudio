//! GPU side of Studio's scene environment probe (env_probe.rs): two 128 px
//! cube slots (PrefilteredEnvIndoorTexture, PrefilteredEnvBlendTargetTexture)
//! that start as the default indoor cube, the capture's source pyramid, and
//! the FastConvDownsample / FastConvolution passes (env_probe.wgsl).

use crate::env_probe::{self, Task};
use egui_wgpu::wgpu;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const LEVELS: u32 = 8; // 128 .. 1
const PARAM_STRIDE: u64 = 256;
const PARAM_SLOTS: u64 = 128;

pub struct ProbeGpu {
    pub slots: [wgpu::Texture; 2],
    pub slot_views: [wgpu::TextureView; 2],
    /// The slot the shader reads as PrefilteredEnvIndoorTexture.
    pub cur: usize,
    src: wgpu::Texture,
    src_view: wgpu::TextureView,
    src_level_views: Vec<wgpu::TextureView>,
    pub face_msaa: wgpu::TextureView,
    pub face_depth: wgpu::TextureView,
    face_resolve_view: wgpu::TextureView,
    pub globals_buf: wgpu::Buffer,
    params: wgpu::Buffer,
    params_cpu: Vec<[f32; 4]>,
    coeff: wgpu::Buffer,
    sampler: wgpu::Sampler,
    layout: wgpu::BindGroupLayout,
    default_fill: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    conv: wgpu::RenderPipeline,
}

fn cube_tex(device: &wgpu::Device, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: env_probe::SIZE, height: env_probe::SIZE, depth_or_array_layers: 6 },
        mip_level_count: LEVELS,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn cube_view(t: &wgpu::Texture, base_mip: u32, mips: Option<u32>) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::Cube),
        base_mip_level: base_mip,
        mip_level_count: mips,
        ..Default::default()
    })
}

fn face_view(t: &wgpu::Texture, mip: u32, face: u32) -> wgpu::TextureView {
    t.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2),
        base_mip_level: mip,
        mip_level_count: Some(1),
        base_array_layer: face,
        array_layer_count: Some(1),
        ..Default::default()
    })
}

impl ProbeGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, samples: u32, depth_format: wgpu::TextureFormat, globals_size: u64) -> Self {
        let slots = [cube_tex(device, "env probe slot a"), cube_tex(device, "env probe slot b")];
        let slot_views = [cube_view(&slots[0], 0, None), cube_view(&slots[1], 0, None)];
        let src = cube_tex(device, "env probe source pyramid");
        let src_view = cube_view(&src, 0, None);
        let src_level_views = (0..LEVELS).map(|m| cube_view(&src, m, Some(1))).collect();
        let size = wgpu::Extent3d { width: env_probe::SIZE, height: env_probe::SIZE, depth_or_array_layers: 1 };
        let face_msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("env probe face"), size, mip_level_count: 1, sample_count: samples,
            dimension: wgpu::TextureDimension::D2, format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
        }).create_view(&Default::default());
        let face_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("env probe face depth"), size, mip_level_count: 1, sample_count: samples,
            dimension: wgpu::TextureDimension::D2, format: depth_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
        }).create_view(&Default::default());
        let face_resolve_view = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("env probe face resolve"), size, mip_level_count: 1, sample_count: 1,
            dimension: wgpu::TextureDimension::D2, format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[],
        }).create_view(&Default::default());
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("env probe globals"), size: globals_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("env probe params"), size: PARAM_STRIDE * PARAM_SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let coeff = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("env probe FastConvolution CB4"), size: 960,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        queue.write_buffer(&coeff, 0, bytemuck::cast_slice(&env_probe::CONVOLUTION));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("env probe"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let tex = |binding: u32, dim: wgpu::TextureViewDimension, filterable: bool| wgpu::BindGroupLayoutEntry {
            binding, visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: dim, multisampled: false },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("env probe"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(16) },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(960) },
                    count: None,
                },
                tex(2, wgpu::TextureViewDimension::Cube, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 3, visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None,
                },
                tex(4, wgpu::TextureViewDimension::D2, true),
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("env probe"),
            source: wgpu::ShaderSource::Wgsl(include_str!("env_probe.wgsl").into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("env probe"), bind_group_layouts: &[Some(&layout)], immediate_size: 0,
        });
        let pipe = |fs: &str| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(fs),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_probe"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader, entry_point: Some(fs), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        ProbeGpu {
            default_fill: pipe("fs_probe_default"),
            blit: pipe("fs_probe_blit"),
            down: pipe("fs_probe_down"),
            conv: pipe("fs_probe_conv"),
            slots, slot_views, cur: 0, src, src_view, src_level_views,
            face_msaa, face_depth, face_resolve_view, globals_buf, params,
            params_cpu: Vec::new(), coeff, sampler, layout,
        }
    }

    pub fn next(&self) -> usize {
        1 - self.cur
    }

    fn pass(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::RenderPipeline,
            target: &wgpu::TextureView, env: &wgpu::TextureView, p: [f32; 4]) {
        if self.params_cpu.len() as u64 >= PARAM_SLOTS {
            return;
        }
        let index = self.params_cpu.len() as u32;
        self.params_cpu.push(p);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("env probe pass"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.params, offset: 0, size: wgpu::BufferSize::new(16) }) },
                wgpu::BindGroupEntry { binding: 1, resource: self.coeff.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(env) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.face_resolve_view) },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("env probe"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bg, &[index * PARAM_STRIDE as u32]);
        pass.draw(0..3, 0..1);
    }

    /// Writes this frame's pass parameters (before the encoder is submitted).
    pub fn flush(&mut self, queue: &wgpu::Queue) {
        for (i, p) in self.params_cpu.drain(..).enumerate() {
            queue.write_buffer(&self.params, i as u64 * PARAM_STRIDE, bytemuck::cast_slice(&p));
        }
    }

    /// Both slots, every level, from the default indoor cube (`indoor`,
    /// `indoor_size` px at its level 0): probe mip m = indoor lod log2(size / 128) + m.
    pub fn fill_default(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, indoor: &wgpu::TextureView, indoor_size: u32) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("env probe default") });
        let offset = (indoor_size.max(1) as f32 / env_probe::SIZE as f32).log2().max(0.0);
        for s in 0..2 {
            for m in 0..LEVELS {
                for f in 0..6 {
                    let target = face_view(&self.slots[s], m, f);
                    let pipeline = self.default_fill.clone();
                    self.pass(device, &mut encoder, &pipeline, &target, indoor, [offset + m as f32, f as f32, 0.0, 0.0]);
                }
            }
        }
        self.flush(queue);
        queue.submit([encoder.finish()]);
    }

    /// After a face is drawn into face_msaa (resolved to face_resolve):
    /// mirror it into the source pyramid's level 0.
    pub fn store_face(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, face: u32) {
        let target = face_view(&self.src, 0, face);
        let pipeline = self.blit.clone();
        let env = self.src_level_views[1].clone();
        self.pass(device, encoder, &pipeline, &target, &env, [0.0, face as f32, 0.0, 0.0]);
    }

    pub fn face_resolve(&self) -> &wgpu::TextureView {
        &self.face_resolve_view
    }

    /// Pyramid and convolution steps (Task::Face is drawn by the renderer).
    pub fn encode(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, task: Task) {
        match task {
            Task::Face(_) => {}
            Task::Pyramid => {
                // FastConvDownsamplePS: each level from the one above,
                // CB3[0].z = 1 / its size (fits Studio's captured mips best:
                // checks/laser-tag-parity/render/findings.md).
                for m in 1..LEVELS {
                    let size = (env_probe::SIZE >> m).max(1) as f32;
                    for f in 0..6 {
                        let target = face_view(&self.src, m, f);
                        let env = self.src_level_views[m as usize - 1].clone();
                        let pipeline = self.down.clone();
                        self.pass(device, encoder, &pipeline, &target, &env, [0.0, f as f32, 1.0 / size, 0.0]);
                    }
                }
                // The capture itself is the slot's level 0.
                let next = self.next();
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo { texture: &self.src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyTextureInfo { texture: &self.slots[next], mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: env_probe::SIZE, height: env_probe::SIZE, depth_or_array_layers: 6 },
                );
            }
            Task::Convolve { mip, first, count } => {
                let next = self.next();
                for f in first..(first + count).min(6) {
                    let target = face_view(&self.slots[next], mip, f);
                    let env = self.src_view.clone();
                    let pipeline = self.conv.clone();
                    self.pass(device, encoder, &pipeline, &target, &env, [(mip - 1) as f32, f as f32, 0.0, 0.0]);
                }
            }
        }
    }
}
