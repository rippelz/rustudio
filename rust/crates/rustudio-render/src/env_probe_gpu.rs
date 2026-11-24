//! GPU side of Studio's scene environment probe (env_probe.rs): two 128 px
//! cube slots (PrefilteredEnvIndoorTexture, PrefilteredEnvBlendTargetTexture)
//! that start as the default indoor cube, the capture's source pyramid, and
//! the FastConvDownsample / FastConvolution passes (env_probe.wgsl).

use egui_wgpu::wgpu;
use glam::Vec3;

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
    /// The face is drawn here: multisampled and resolved to
    /// face_resolve_view, or that view itself at one sample (OpenGL).
    face_msaa: wgpu::TextureView,
    samples: u32,
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
        size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 6 },
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
        let size = wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 };
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
        let face_msaa = if samples == 1 { face_resolve_view.clone() } else { device.create_texture(&wgpu::TextureDescriptor {
            label: Some("env probe face"), size, mip_level_count: 1, sample_count: samples,
            dimension: wgpu::TextureDimension::D2, format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[],
        }).create_view(&Default::default()) };
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
        queue.write_buffer(&coeff, 0, bytemuck::cast_slice(&CONVOLUTION));
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
            source: wgpu::ShaderSource::Wgsl(crate::gpu_compat::probe_source(include_str!("env_probe.wgsl")).into()),
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("env probe"), bind_group_layouts: &[Some(&layout)], immediate_size: 0,
        });
        let pipe = |fs: &str| crate::gpu_compat::pipeline(device, &wgpu::RenderPipelineDescriptor {
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
            cache: crate::pipeline_cache::current(),
        });
        ProbeGpu {
            default_fill: pipe("fs_probe_default"),
            blit: pipe("fs_probe_blit"),
            down: pipe("fs_probe_down"),
            conv: pipe("fs_probe_conv"),
            slots, slot_views, cur: 0, src, src_view, src_level_views,
            face_msaa, samples, face_depth, face_resolve_view, globals_buf, params,
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
        let offset = (indoor_size.max(1) as f32 / SIZE as f32).log2().max(0.0);
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
        crate::gpu_timing::submit(device, queue, "gpu: env probe default", [encoder.finish()]);
    }

    /// After a face is drawn into face_msaa (resolved to face_resolve):
    /// mirror it into the source pyramid's level 0.
    pub fn store_face(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, face: u32) {
        let target = face_view(&self.src, 0, face);
        let pipeline = self.blit.clone();
        let env = self.src_level_views[1].clone();
        self.pass(device, encoder, &pipeline, &target, &env, [0.0, face as f32, 0.0, 0.0]);
    }

    /// The colour attachment a face is drawn to (resolved when multisampled).
    pub fn face_color(&self) -> wgpu::RenderPassColorAttachment<'_> {
        let multisampled = self.samples > 1;
        wgpu::RenderPassColorAttachment {
            view: &self.face_msaa,
            depth_slice: None,
            resolve_target: multisampled.then_some(&self.face_resolve_view),
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: if multisampled { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
            },
        }
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
                    let size = (SIZE >> m).max(1) as f32;
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
                    wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 6 },
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

// ---------------------------------------------------- the probe frame
// The types and constants the renderer shares with env_probe.rs (the scene
// probe's work): they live here, in render, so render needs nothing above core.

/// Probe cube: 128 px for PBR, 90-degree faces, far 3000
///.
pub const SIZE: u32 = 128;
pub const FAR: f32 = 3000.0;

/// One GPU step of a capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    /// Draw cube face `0..6` of the scene at `centre` into the next slot.
    Face(u32),
    /// Build the FastConvDownsample source pyramid from the captured faces.
    Pyramid,
    /// FastConvolution of mip `mip` (1 or 2), faces `first..first+count`.
    Convolve { mip: u32, first: u32, count: u32 },
}

/// The capture task list, one entry a frame: six faces, then the
/// convolution (Studio: one face a call for mip 1, two for mips 2..3).
pub fn capture_tasks() -> Vec<Task> {
    let mut t: Vec<Task> = (0..6).map(Task::Face).collect();
    t.push(Task::Pyramid);
    t.extend((0..6).map(|f| Task::Convolve { mip: 1, first: f, count: 1 }));
    t.extend((0..3).map(|f| Task::Convolve { mip: 2, first: f * 2, count: 2 }));
    t
}

/// What the renderer does with the probe this frame.
#[derive(Clone, Debug, Default)]
pub struct ProbeFrame {
    /// The capture position (centre of the probe box).
    pub centre: Vec3,
    /// GPU work this frame, in order.
    pub tasks: Vec<Task>,
    /// CB0[32].w: how far the shader blends from the current slot to the next.
    pub blend: f32,
    /// The next slot becomes the current one before drawing.
    pub swap: bool,
}


/// The face's view basis (forward, up), cube order +X, -X, +Y, -Y, +Z, -Z
/// (Studio; D3D / wgpu cube faces).
pub fn face_basis(face: u32) -> (Vec3, Vec3) {
    match face {
        0 => (Vec3::X, Vec3::Y),
        1 => (-Vec3::X, Vec3::Y),
        2 => (Vec3::Y, -Vec3::Z),
        3 => (-Vec3::Y, Vec3::Z),
        4 => (Vec3::Z, Vec3::Y),
        _ => (-Vec3::Z, Vec3::Y),
    }
}


/// FastConvolutionPS CB4: the 960 bytes Studio uploads (copied from
/// .rdata Studio..Studio; checks/laser-tag-parity/render/
/// native-traced/convolution-cb4.json). Per level (mip 1, mip 2): 30 vec4,
/// tangent / bitangent / normal weights, source lod, tap weight (6 each).
pub const CONVOLUTION: [[f32; 4]; 60] = [
    [-0.047896601259708405, 0.0022376899141818285, 0.13721300661563873, -0.14268499612808228],
    [0.11079200357198715, -0.048012200742959976, -0.004811530001461506, 0.02609799988567829],
    [-0.04865869879722595, -0.03231469914317131, 0.12428499758243561, -0.08737579733133316],
    [0.09280289709568024, -0.0064116897992789745, -0.04859580099582672, 0.03342840075492859],
    [-0.02262910082936287, -0.06591200083494186, 0.10839200019836426, -0.09438200294971466],
    [0.10435599833726883, -0.029181700199842453, -0.0050018299371004105, 0.006752519868314266],
    [-0.03361539915204048, -0.03066210076212883, 0.014751099981367588, 0.017846299335360527],
    [-0.0310651995241642, 0.011960700154304504, 0.006071330048143864, 0.01514929998666048],
    [0.012825800105929375, -0.0040247500874102116, 0.016707399860024452, -0.01684579998254776],
    [-0.05792130157351494, 0.021790800616145134, -0.007918530143797398, 0.0215901006013155],
    [-0.03485039994120598, -0.0032204100862145424, -0.028062399476766586, -0.016979800537228584],
    [-0.02272370085120201, 0.018890999257564545, 5.3588599257636815e-05, 0.037955399602651596],
    [0.996815025806427, 0.994388997554779, 0.9810929894447327, 0.9833130240440369],
    [0.9915739893913269, 0.9951609969139099, 0.999879002571106, 0.9983410239219666],
    [0.9976639747619629, 0.9984059929847717, 0.9793969988822937, 0.9896960258483887],
    [0.9916139841079712, 0.9988129734992981, 0.9976040124893188, 0.9975969791412354],
    [0.9971839785575867, 0.9959679841995239, 0.9866899847984314, 0.9871580004692078],
    [0.993815004825592, 0.9975690245628357, 0.9999669790267944, 0.9984369874000549],
    [2.672990083694458, 3.6537699699401855, 2.87103009223938, 2.8844099044799805],
    [3.0511300563812256, 2.3980400562286377, 1.9938299655914307, 2.415450096130371],
    [2.810889959335327, 3.819659948348999, 2.9271399974823, 2.921689987182617],
    [3.2918100357055664, 2.4230000972747803, 1.9989500045776367, 2.137350082397461],
    [2.3872199058532715, 3.312659978866577, 2.9524900913238525, 3.029860019683838],
    [2.9255900382995605, 2.4142000675201416, 1.9309899806976318, 2.0014500617980957],
    [0.5689849853515625, 0.3531000018119812, 0.973406970500946, 0.8651319742202759],
    [0.7326409816741943, 1.384950041770935, 1.5516899824142456, 1.457900047302246],
    [1.127020001411438, 0.7165160179138184, 0.9421409964561462, 0.9906229972839355],
    [0.9753149747848511, 1.7410099506378174, 1.442829966545105, 1.4955799579620361],
    [0.7084370255470276, 0.556538999080658, 1.0517100095748901, 0.7899979948997498],
    [0.9671270251274109, 1.3909800052642822, 1.573349952697754, 1.2821600437164307],
    [-0.0694521963596344, -0.054684098809957504, 0.26948198676109314, -0.48583298921585083],
    [0.3975369930267334, -0.05444389954209328, 0.08300969749689102, -0.20476700365543365],
    [-0.07158230245113373, -0.07419479638338089, 0.2483379989862442, -0.48693400621414185],
    [0.355663001537323, -0.025955699384212494, 0.054478999227285385, -0.19293299317359924],
    [-0.06279610097408295, -0.055908799171447754, 0.2013150006532669, -0.42486900091171265],
    [0.42616501450538635, 0.010062700137495995, 0.0889727994799614, -0.22596800327301025],
    [0.054569099098443985, -0.2913289964199066, 0.13726100325584412, -0.004152089823037386],
    [-0.16085900366306305, 0.28030699491500854, -0.06945829838514328, -0.13830700516700745],
    [0.07551079988479614, -0.2936210036277771, 0.10127600282430649, -0.06112419813871384],
    [-0.14244399964809418, 0.2934190034866333, -0.07422740012407303, -0.09664910286664963],
    [0.04261770099401474, -0.26699700951576233, 0.08656840026378632, -0.02063559927046299],
    [-0.030395999550819397, 0.30966299772262573, -0.08135200291872025, -0.13887600600719452],
    [0.9949929714202881, 0.9509490132331848, 0.9523249864578247, 0.8693649768829346],
    [0.8976889848709106, 0.9541260004043579, 0.9930970072746277, 0.9621040225028992],
    [0.9941409826278687, 0.9483960270881653, 0.9630360007286072, 0.8638520240783691],
    [0.9198269844055176, 0.9513630270957947, 0.9941009879112244, 0.9704750180244446],
    [0.9959679841995239, 0.9590579867362976, 0.9744129776954651, 0.9016939997673035],
    [0.8975729942321777, 0.945268988609314, 0.9916499853134155, 0.9569230079650879],
    [3.491489887237549, 3.9926199913024902, 4.155479907989502, 4.767980098724365],
    [5.026559829711914, 4.073729991912842, 3.4915199279785156, 3.8488099575042725],
    [3.4601500034332275, 3.6936099529266357, 4.029660224914551, 4.756750106811523],
    [4.919229984283447, 4.336880207061768, 3.511280059814453, 3.899319887161255],
    [3.5485899448394775, 3.955549955368042, 4.2572197914123535, 5.208270072937012],
    [4.674950122833252, 4.198890209197998, 3.345679998397827, 3.716559886932373],
    [1.5162999629974365, 0.7468050122261047, 0.9939900040626526, 0.7289569973945618],
    [0.7255610227584839, 0.7039139866828918, 1.3796299695968628, 0.7575200200080872],
    [1.5814599990844727, 0.6280959844589233, 1.0032299757003784, 0.756367027759552],
    [1.0983500480651855, 1.1569299697875977, 1.5953700542449951, 1.0459599494934082],
    [1.8529499769210815, 0.8183349967002869, 1.0174599885940552, 0.7971770167350769],
    [0.9702339768409729, 0.9149870276451111, 1.0250999927520752, 0.5558300018310547],
];
