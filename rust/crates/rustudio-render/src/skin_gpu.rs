//! GPU skinning: a compute pass that writes a skinned part's deformed
//! positions and normals into that part's own vertex buffer.
//!
//! - Per skinned mesh (content id), once: a storage buffer of its corners
//!   (rest position and normal in the renderer's unit box, four weights and
//!   four bone indices), 64 bytes a corner.
//! - Per skinned part: its mesh slot (a copy of the mesh made with STORAGE
//!   usage, uvs and vertex colours written once), a palette buffer (3 × vec4
//!   per bone: rows of current × bind⁻¹ in part space) and its parameters.
//! - Per frame the CPU only computes the palette (skin.rs `matrices`) and
//!   writes it (48 bytes a bone); the dispatch rewrites floats 0..6 of each
//!   64-byte vertex (shader `Vertex`: pos, normal) and leaves uv, projection
//!   and colour alone.
//!
//! A part whose bones are all at rest draws the shared mesh and costs
//! nothing here.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Vec3;
use egui_wgpu::wgpu;
use wgpu::util::DeviceExt;

use rustudio_core::math::Xform;

const SHADER: &str = r#"
struct Params {
    to_part: vec4<f32>,
    from_part: vec4<f32>,
    extent: vec4<f32>,
    // x: corners, y: bones
    counts: vec4<u32>,
    // the world origin in part space (studs)
    origin: vec4<f32>,
};
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> palette: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= params.counts.x) {
        return;
    }
    let pp = src[i * 4u].xyz * params.to_part.xyz;
    let n = src[i * 4u + 1u].xyz / params.extent.xyz;
    let w = src[i * 4u + 2u];
    let ids = bitcast<vec4<u32>>(src[i * 4u + 3u]);
    var q = vec3<f32>(0.0);
    var m = vec3<f32>(0.0);
    var total = 0.0;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let wk = w[k];
        let b = ids[k];
        if (wk > 0.0 && b < params.counts.y) {
            let r0 = palette[b * 3u];
            let r1 = palette[b * 3u + 1u];
            let r2 = palette[b * 3u + 2u];
            q = q + vec3<f32>(dot(r0.xyz, pp) + r0.w, dot(r1.xyz, pp) + r1.w, dot(r2.xyz, pp) + r2.w) * wk;
            m = m + vec3<f32>(dot(r0.xyz, n), dot(r1.xyz, n), dot(r2.xyz, n)) * wk;
            total = total + wk;
        }
    }
    // Weight short of 1 pulls toward the world origin (Studio blends in world space).
    // (u8 weights: a real shortfall is ≥ 1/255; below 1e-4 it is rounding.)
    let short = 1.0 - total;
    if (abs(short) > 1e-4) {
        q = q + params.origin.xyz * short;
    }
    let p = q * params.from_part.xyz;
    var nn = vec3<f32>(0.0);
    if (dot(m, m) > 0.0) {
        nn = normalize(m) * params.extent.xyz;
    }
    let o = i * 16u;
    dst[o] = p.x;
    dst[o + 1u] = p.y;
    dst[o + 2u] = p.z;
    dst[o + 3u] = nn.x;
    dst[o + 4u] = nn.y;
    dst[o + 5u] = nn.z;
}
"#;

/// A skinned mesh's corners for the GPU.
struct Source {
    buf: wgpu::Buffer,
    corners: u32,
}

/// One skinned part's GPU state.
struct Job {
    palette: wgpu::Buffer,
    params: wgpu::Buffer,
    bind: wgpu::BindGroup,
    bones: u32,
    corners: u32,
    /// The mesh slot (Renderer::meshes index without built-ins) it writes.
    slot: u32,
    /// The vertex buffer the bind group writes into (to notice a reslot).
    vbuf_id: usize,
}

pub struct SkinGpu {
    /// None without compute shaders (OpenGL < 4.3): skinned parts then draw
    /// their mesh at rest.
    pipeline: Option<wgpu::ComputePipeline>,
    sources: HashMap<String, Arc<Source>>,
    jobs: HashMap<u32, Job>,
    due: Vec<u32>,
    /// Bytes written to the GPU for skinning since the last `take_bytes`.
    pub bytes: u64,
}

/// What one part needs skinned: its mesh's corners and this frame's
/// palette (current × bind⁻¹ per mesh bone, part space).
pub struct Request<'a> {
    pub key: &'a str,
    pub base: &'a crate::skin::Base,
    pub palette: &'a [Xform],
    pub scale: Vec3,
    /// The world origin in part space (skin::origin_of).
    pub origin: Vec3,
}

impl SkinGpu {
    pub fn new(device: &wgpu::Device, compute: bool) -> Self {
        if !compute {
            return SkinGpu { pipeline: None, sources: HashMap::new(), jobs: HashMap::new(), due: Vec::new(), bytes: 0 };
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinning"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("skinning"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: crate::pipeline_cache::current(),
        });
        SkinGpu { pipeline: Some(pipeline), sources: HashMap::new(), jobs: HashMap::new(), due: Vec::new(), bytes: 0 }
    }

    fn source(&mut self, device: &wgpu::Device, key: &str, base: &crate::skin::Base) -> Arc<Source> {
        if let Some(s) = self.sources.get(key) {
            return s.clone();
        }
        let n = base.positions.len();
        let mut data: Vec<[f32; 4]> = Vec::with_capacity(n * 4);
        for i in 0..n {
            let p = base.positions[i];
            let nm = base.normals.get(i).copied().unwrap_or(Vec3::Y);
            let ws = base.skin.corner_weights.get(i).copied().unwrap_or([0.0; 4]);
            let ids = base.skin.corner_bones.get(i).copied().unwrap_or([0; 4]);
            data.push([p.x, p.y, p.z, 0.0]);
            data.push([nm.x, nm.y, nm.z, 0.0]);
            data.push(ws);
            data.push(ids.map(|b| f32::from_bits(b as u32)));
        }
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("skin source"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::STORAGE,
        });
        self.bytes += (data.len() * 16) as u64;
        rustudio_core::bench::add("skin_upload_bytes", 0.0, (data.len() * 16) as u64);
        let s = Arc::new(Source { buf, corners: n as u32 });
        self.sources.insert(key.to_string(), s.clone());
        s
    }

    /// Queues `slot`'s vertices to be rewritten from `req` this frame.
    /// `vbuf` is the slot's vertex buffer (made with STORAGE usage).
    pub fn queue(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        vbuf: &wgpu::Buffer,
        req: &Request,
    ) {
        let Some(pipeline) = self.pipeline.clone() else { return };
        let src = self.source(device, req.key, req.base);
        let bones = req.palette.len().max(1) as u32;
        let vbuf_id = vbuf as *const wgpu::Buffer as usize;
        let stale = self
            .jobs
            .get(&slot)
            .is_none_or(|j| j.bones != bones || j.corners != src.corners || j.vbuf_id != vbuf_id || j.slot != slot);
        if stale {
            let palette = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skin palette"),
                size: bones as u64 * 48,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skin params"),
                size: 80,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("skinning"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: src.buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: palette.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: vbuf.as_entire_binding() },
                ],
            });
            self.jobs.insert(slot, Job { palette, params, bind, bones, corners: src.corners, slot, vbuf_id });
        }
        let job = &self.jobs[&slot];
        let rows: Vec<[f32; 4]> = req
            .palette
            .iter()
            .flat_map(|x| {
                let r = x.rot.transpose(); // columns of the transpose = rows
                [
                    [r.x_axis.x, r.x_axis.y, r.x_axis.z, x.pos.x],
                    [r.y_axis.x, r.y_axis.y, r.y_axis.z, x.pos.y],
                    [r.z_axis.x, r.z_axis.y, r.z_axis.z, x.pos.z],
                ]
            })
            .collect();
        if !rows.is_empty() {
            queue.write_buffer(&job.palette, 0, bytemuck::cast_slice(&rows));
            self.bytes += (rows.len() * 16) as u64;
        }
        let to_part = req.base.extent * req.scale;
        let from_part = Vec3::ONE / to_part.max(Vec3::splat(1e-6));
        let e = req.base.extent;
        let o = req.origin;
        let params: [[f32; 4]; 5] = [
            [to_part.x, to_part.y, to_part.z, 0.0],
            [from_part.x, from_part.y, from_part.z, 0.0],
            [e.x, e.y, e.z, 0.0],
            [f32::from_bits(src.corners), f32::from_bits(req.palette.len() as u32), 0.0, 0.0],
            [o.x, o.y, o.z, 0.0],
        ];
        queue.write_buffer(&job.params, 0, bytemuck::cast_slice(&params));
        self.bytes += 80;
        rustudio_core::bench::add("skin_upload_bytes", 0.0, rows.len() as u64 * 16 + 80);
        if !self.due.contains(&slot) {
            self.due.push(slot);
        }
    }

    /// Queues `slot` again with the palette and parameters it already
    /// holds (its vertex buffer was rewritten with the rest pose).
    pub fn again(&mut self, slot: u32) {
        if self.jobs.contains_key(&slot) && !self.due.contains(&slot) {
            self.due.push(slot);
        }
    }

    /// Forgets a part's job (its slot went back to the shared mesh or away).
    pub fn drop_slot(&mut self, slot: u32) {
        self.jobs.remove(&slot);
        self.due.retain(|s| *s != slot);
    }

    /// Records this frame's dispatches.
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(pipeline) = self.pipeline.as_ref().filter(|_| !self.due.is_empty()) else {
            self.due.clear();
            return;
        };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("skinning"), timestamp_writes: None });
        pass.set_pipeline(pipeline);
        for slot in self.due.drain(..) {
            if let Some(j) = self.jobs.get(&slot) {
                pass.set_bind_group(0, &j.bind, &[]);
                pass.dispatch_workgroups(j.corners.div_ceil(64), 1, 1);
            }
        }
    }

    pub fn take_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.bytes)
    }
}
