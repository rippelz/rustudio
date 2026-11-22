//! 3D viewport adornments (Team Create collaborator camera gizmos), matching
//! Studio 0.748 `CloudEditAdornable` and
//! `AdornRender::sphere` / `AdornRender::ray`.
//!
//! Geometry (fixed world-space size, depth-tested against scene depth, alpha 0.25):
//! - Sphere: radius 0.75 studs, 18 longitude x 9 latitude segments
//!   (`20 deg` step).
//! - Look arrow (`RbxRay` origin `(0, 0, 1.2)`, dir `(0, 0, -3.0)`):
//!   - Cylinder shaft: length `3.0 / 1.3`, radius `(3.0 / 1.3) * 0.025`, 24 radial segments.
//!   - Cone head: height `(3.0 / 1.3) * 0.3`, base radius `(3.0 / 1.3) * 0.075`, 24 radial segments.

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use glam::{Mat3, Mat4, Vec3};
use rustudio_core::math::Xform;

/// Sphere radius in studs.
pub const COLLAB_SPHERE_RADIUS: f32 = 0.75;
/// Look-ray local origin Z in studs.
pub const COLLAB_RAY_ORIGIN_Z: f32 = 1.2;
/// Look-ray local direction Z in studs.
pub const COLLAB_RAY_DIR_Z: f32 = -3.0;
/// Collaborator 3D gizmo alpha.
pub const COLLAB_GIZMO_ALPHA: f32 = 0.25;
/// `AdornRender::ray` shaft divisor.
const RAY_SHAFT_DIV: f32 = 1.3;
/// `AdornRender::ray` cylinder radius factor.
const RAY_CYL_RADIUS_K: f32 = 0.025;
/// `AdornRender::ray` cone height factor.
const RAY_CONE_HEIGHT_K: f32 = 0.3;
/// `AdornRender::ray` cone base radius factor.
const RAY_CONE_RADIUS_K: f32 = 0.075;
/// Sphere longitude segments (`2*pi / 0.34906584` = 18).
pub const SPHERE_LON_SEGS: u32 = 18;
/// Sphere latitude rings (`pi / 0.34906584` = 9).
pub const SPHERE_LAT_SEGS: u32 = 9;
/// Cylinder and cone radial segments.
pub const RADIAL_SEGS: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdornShape {
    Sphere,
    Cylinder,
    Cone,
}

/// One 3D adornment mesh instance to draw in world space, depth-tested against
/// the scene depth buffer (`zIndex = -1`, `alwaysOnTop = false`).
#[derive(Clone, Copy, Debug)]
pub struct AdornDraw {
    pub xf: Xform,
    pub scale: Vec3,
    pub shape: AdornShape,
    /// Display-space RGB + alpha.
    pub color: [f32; 4],
}

/// Build an orthonormal basis whose `+X` axis points along unit vector `dir`,
/// matching `AdornRender::ray`.
fn ray_basis(dir: Vec3) -> Mat3 {
    let x = dir.normalize_or_zero();
    if x == Vec3::ZERO {
        return Mat3::IDENTITY;
    }
    let up = if x.y.abs() < 0.7 { Vec3::Y } else { Vec3::X };
    let z = x.cross(up).normalize();
    let y = z.cross(x).normalize();
    Mat3::from_cols(x, y, z)
}

/// Build the 3 `AdornDraw`s (cylinder shaft, cone head, sphere) for one
/// collaborator camera at `xf` in `color` (`CollaboratorColor3`).
pub fn collaborator_camera_draws(xf: Xform, color: [f32; 3]) -> [AdornDraw; 3] {
    let col = [color[0], color[1], color[2], COLLAB_GIZMO_ALPHA];
    let origin_local = Vec3::new(0.0, 0.0, COLLAB_RAY_ORIGIN_Z);
    let dir_local = Vec3::new(0.0, 0.0, COLLAB_RAY_DIR_Z);
    let len = dir_local.length();
    let shaft_len = len / RAY_SHAFT_DIV;
    let inv_div = 1.0 / RAY_SHAFT_DIV;

    let world_dir = xf.rot * dir_local;
    let basis = ray_basis(world_dir);

    let cyl_center = xf.apply(origin_local + dir_local * (0.5 * inv_div));
    let cyl = AdornDraw {
        xf: Xform { pos: cyl_center, rot: basis },
        scale: Vec3::new(shaft_len * 0.5, shaft_len * RAY_CYL_RADIUS_K, shaft_len * RAY_CYL_RADIUS_K),
        shape: AdornShape::Cylinder,
        color: col,
    };

    let cone_base = xf.apply(origin_local + dir_local * inv_div);
    let cone = AdornDraw {
        xf: Xform { pos: cone_base, rot: basis },
        scale: Vec3::new(shaft_len * RAY_CONE_HEIGHT_K, shaft_len * RAY_CONE_RADIUS_K, shaft_len * RAY_CONE_RADIUS_K),
        shape: AdornShape::Cone,
        color: col,
    };

    let sphere = AdornDraw {
        xf,
        scale: Vec3::splat(COLLAB_SPHERE_RADIUS),
        shape: AdornShape::Sphere,
        color: col,
    };

    [cyl, cone, sphere]
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct AdornVertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct AdornInst {
    r0: [f32; 4],
    r1: [f32; 4],
    r2: [f32; 4],
    scale: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
}

struct GpuMesh {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    indices: u32,
}

fn build_sphere_mesh() -> (Vec<AdornVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut idxs = Vec::new();
    for lat in 0..=SPHERE_LAT_SEGS {
        let theta = (lat as f32 / SPHERE_LAT_SEGS as f32) * std::f32::consts::PI;
        let sin_t = theta.sin();
        let cos_t = theta.cos();
        for lon in 0..=SPHERE_LON_SEGS {
            let phi = (lon as f32 / SPHERE_LON_SEGS as f32) * std::f32::consts::TAU;
            let n = [sin_t * phi.cos(), cos_t, sin_t * phi.sin()];
            verts.push(AdornVertex { pos: n, normal: n });
        }
    }
    let stride = SPHERE_LON_SEGS + 1;
    for lat in 0..SPHERE_LAT_SEGS {
        for lon in 0..SPHERE_LON_SEGS {
            let a = lat * stride + lon;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            idxs.extend_from_slice(&[a, b, c, b, d, c]);
        }
    }
    (verts, idxs)
}

/// Unit cylinder along X in `[-1, +1]`, radius `1.0`, `RADIAL_SEGS` segments.
fn build_cylinder_mesh() -> (Vec<AdornVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut idxs = Vec::new();
    for i in 0..=RADIAL_SEGS {
        let a = (i as f32 / RADIAL_SEGS as f32) * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let n = [0.0, c, s];
        verts.push(AdornVertex { pos: [-1.0, c, s], normal: n });
        verts.push(AdornVertex { pos: [1.0, c, s], normal: n });
    }
    for i in 0..RADIAL_SEGS {
        let b = i * 2;
        idxs.extend_from_slice(&[b, b + 2, b + 1, b + 1, b + 2, b + 3]);
    }
    // End caps
    for &(x, nx) in &[(-1.0_f32, -1.0_f32), (1.0_f32, 1.0_f32)] {
        let center = verts.len() as u32;
        verts.push(AdornVertex { pos: [x, 0.0, 0.0], normal: [nx, 0.0, 0.0] });
        let ring = verts.len() as u32;
        for i in 0..RADIAL_SEGS {
            let a = (i as f32 / RADIAL_SEGS as f32) * std::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            verts.push(AdornVertex { pos: [x, c, s], normal: [nx, 0.0, 0.0] });
        }
        for i in 0..RADIAL_SEGS {
            let next = (i + 1) % RADIAL_SEGS;
            if nx > 0.0 {
                idxs.extend_from_slice(&[center, ring + i, ring + next]);
            } else {
                idxs.extend_from_slice(&[center, ring + next, ring + i]);
            }
        }
    }
    (verts, idxs)
}

/// Unit cone along X from base `x = 0` (radius `1.0`) to tip `x = 1`, `RADIAL_SEGS` segments.
fn build_cone_mesh() -> (Vec<AdornVertex>, Vec<u32>) {
    let mut verts = Vec::new();
    let mut idxs = Vec::new();
    let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
    for i in 0..=RADIAL_SEGS {
        let a = (i as f32 / RADIAL_SEGS as f32) * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let n = [inv_sqrt2, c * inv_sqrt2, s * inv_sqrt2];
        verts.push(AdornVertex { pos: [0.0, c, s], normal: n });
        verts.push(AdornVertex { pos: [1.0, 0.0, 0.0], normal: n });
    }
    for i in 0..RADIAL_SEGS {
        let b = i * 2;
        idxs.extend_from_slice(&[b, b + 2, b + 1]);
    }
    // Base cap at x = 0 facing -X
    let center = verts.len() as u32;
    verts.push(AdornVertex { pos: [0.0, 0.0, 0.0], normal: [-1.0, 0.0, 0.0] });
    let ring = verts.len() as u32;
    for i in 0..RADIAL_SEGS {
        let a = (i as f32 / RADIAL_SEGS as f32) * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        verts.push(AdornVertex { pos: [0.0, c, s], normal: [-1.0, 0.0, 0.0] });
    }
    for i in 0..RADIAL_SEGS {
        let next = (i + 1) % RADIAL_SEGS;
        idxs.extend_from_slice(&[center, ring + next, ring + i]);
    }
    (verts, idxs)
}

fn upload_mesh(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, verts: &[AdornVertex], idxs: &[u32]) -> GpuMesh {
    let vb = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: std::mem::size_of_val(verts) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vb, 0, bytemuck::cast_slice(verts));
    let ib = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: std::mem::size_of_val(idxs) as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&ib, 0, bytemuck::cast_slice(idxs));
    GpuMesh { vb, ib, indices: idxs.len() as u32 }
}

const SHADER_BODY: &str = r#"
struct Params {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
};
@group(0) @binding(0) var<uniform> p: Params;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) r0: vec4<f32>,
    @location(3) r1: vec4<f32>,
    @location(4) r2: vec4<f32>,
    @location(5) scale: vec4<f32>,
    @location(6) color: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    let l = v.pos * v.scale.xyz;
    let origin = vec3<f32>(v.r0.w, v.r1.w, v.r2.w);
    let world = v.r0.xyz * l.x + v.r1.xyz * l.y + v.r2.xyz * l.z + origin;
    let inv_s = vec3<f32>(1.0 / max(v.scale.x, 1e-5), 1.0 / max(v.scale.y, 1e-5), 1.0 / max(v.scale.z, 1e-5));
    let ln = v.normal * inv_s;
    let wn = normalize(v.r0.xyz * ln.x + v.r1.xyz * ln.y + v.r2.xyz * ln.z);
    var out: VOut;
    out.clip = p.view_proj * vec4<f32>(world, 1.0);
    out.normal = wn;
    out.color = v.color;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.clip.xy);
    let scene_d = sample_depth(px);
    if in.clip.z < scene_d {
        discard;
    }
    let n = normalize(in.normal);
    let ndl = max(dot(n, p.light_dir.xyz), 0.0);
    let shade = 0.55 + 0.45 * ndl;
    return vec4<f32>(in.color.rgb * shade, in.color.a);
}
"#;

const DEPTH_MSAA: &str = r#"
@group(0) @binding(1) var depth_tex: texture_depth_multisampled_2d;
fn sample_depth(px: vec2<i32>) -> f32 {
    return textureLoad(depth_tex, px, 0);
}
"#;

const DEPTH_SINGLE: &str = r#"
@group(0) @binding(1) var depth_tex: texture_depth_2d;
fn sample_depth(px: vec2<i32>) -> f32 {
    return textureLoad(depth_tex, px, 0);
}
"#;

pub struct AdornPass {
    pipe: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    insts: wgpu::Buffer,
    inst_cap: usize,
    sphere: GpuMesh,
    cylinder: GpuMesh,
    cone: GpuMesh,
}

impl AdornPass {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, display: wgpu::TextureFormat, samples: u32) -> Self {
        let src = format!("{}{}", if samples > 1 { DEPTH_MSAA } else { DEPTH_SINGLE }, SHADER_BODY);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("adorn3d"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("adorn3d"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: samples > 1,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("adorn3d"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let vbuf = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<AdornVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<AdornInst>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    2 => Float32x4,
                    3 => Float32x4,
                    4 => Float32x4,
                    5 => Float32x4,
                    6 => Float32x4,
                ],
            }),
        ];
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("adorn3d"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &vbuf,
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: display,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adorn3d params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let inst_cap = 64;
        let insts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adorn3d insts"),
            size: (inst_cap * std::mem::size_of::<AdornInst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (sv, si) = build_sphere_mesh();
        let (cv, ci) = build_cylinder_mesh();
        let (kv, ki) = build_cone_mesh();
        Self {
            pipe,
            layout,
            params,
            insts,
            inst_cap,
            sphere: upload_mesh(device, queue, "adorn sphere", &sv, &si),
            cylinder: upload_mesh(device, queue, "adorn cylinder", &cv, &ci),
            cone: upload_mesh(device, queue, "adorn cone", &kv, &ki),
        }
    }

    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        depth_view: &wgpu::TextureView,
        display_view: &wgpu::TextureView,
        view_proj: Mat4,
        list: &[AdornDraw],
    ) {
        if list.is_empty() {
            return;
        }
        if list.len() > self.inst_cap {
            self.inst_cap = list.len().next_power_of_two();
            self.insts = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("adorn3d insts"),
                size: (self.inst_cap * std::mem::size_of::<AdornInst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let light = Vec3::new(0.4, 0.8, 0.5).normalize();
        let params = Params {
            view_proj: view_proj.to_cols_array_2d(),
            light_dir: [light.x, light.y, light.z, 0.0],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let inst_data: Vec<AdornInst> = list
            .iter()
            .map(|d| {
                let c0 = d.xf.rot.x_axis;
                let c1 = d.xf.rot.y_axis;
                let c2 = d.xf.rot.z_axis;
                AdornInst {
                    r0: [c0.x, c0.y, c0.z, d.xf.pos.x],
                    r1: [c1.x, c1.y, c1.z, d.xf.pos.y],
                    r2: [c2.x, c2.y, c2.z, d.xf.pos.z],
                    scale: [d.scale.x, d.scale.y, d.scale.z, 0.0],
                    color: d.color,
                }
            })
            .collect();
        queue.write_buffer(&self.insts, 0, bytemuck::cast_slice(&inst_data));

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adorn3d"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(depth_view) },
            ],
        });

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("adorn3d") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("adorn3d"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: display_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipe);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_vertex_buffer(1, self.insts.slice(..));
            for (idx, item) in list.iter().enumerate() {
                let mesh = match item.shape {
                    AdornShape::Sphere => &self.sphere,
                    AdornShape::Cylinder => &self.cylinder,
                    AdornShape::Cone => &self.cone,
                };
                pass.set_vertex_buffer(0, mesh.vb.slice(..));
                pass.set_index_buffer(mesh.ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.indices, 0, idx as u32..(idx as u32 + 1));
            }
        }
        // (after the frame's own commands, in order: render_thread.rs)
        crate::render_thread::perform(queue, vec![enc], None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collaborator_gizmo_matches_studio_re() {
        let xf = Xform { pos: Vec3::new(10.0, 5.0, -4.0), rot: Mat3::IDENTITY };
        let [cyl, cone, sphere] = collaborator_camera_draws(xf, [1.0, 0.25, 0.5]);

        // Sphere: radius 0.75 at camera origin, alpha 0.25.
        assert_eq!(sphere.shape, AdornShape::Sphere);
        assert!((sphere.xf.pos - xf.pos).length() < 1e-5);
        assert!((sphere.scale - Vec3::splat(0.75)).length() < 1e-5);
        assert!((sphere.color[3] - 0.25).abs() < 1e-6);

        // Arrow spans local z = +1.2 to z = -1.8 (origin (0,0,1.2), dir (0,0,-3.0)).
        let shaft_len = 3.0 / 1.3;
        assert_eq!(cyl.shape, AdornShape::Cylinder);
        let cyl_start = cyl.xf.apply(Vec3::new(-cyl.scale.x, 0.0, 0.0));
        let cyl_end = cyl.xf.apply(Vec3::new(cyl.scale.x, 0.0, 0.0));
        assert!((cyl_start - Vec3::new(10.0, 5.0, -4.0 + 1.2)).length() < 1e-4);
        assert!((cyl_end - Vec3::new(10.0, 5.0, -4.0 + 1.2 - shaft_len)).length() < 1e-4);
        assert!((cyl.scale.y - shaft_len * 0.025).abs() < 1e-5);

        assert_eq!(cone.shape, AdornShape::Cone);
        let cone_tip = cone.xf.apply(Vec3::new(cone.scale.x, 0.0, 0.0));
        assert!((cone.xf.pos - cyl_end).length() < 1e-4);
        assert!((cone_tip - Vec3::new(10.0, 5.0, -4.0 - 1.8)).length() < 1e-4);
        assert!((cone.scale.y - shaft_len * 0.075).abs() < 1e-5);
    }

    #[test]
    fn primitive_tessellation_matches_studio_re() {
        let (sv, si) = build_sphere_mesh();
        assert_eq!(sv.len(), ((SPHERE_LAT_SEGS + 1) * (SPHERE_LON_SEGS + 1)) as usize);
        assert_eq!(si.len(), (SPHERE_LAT_SEGS * SPHERE_LON_SEGS * 6) as usize);

        let (cv, ci) = build_cylinder_mesh();
        assert_eq!(ci.len(), (RADIAL_SEGS * 6 + RADIAL_SEGS * 6) as usize);

        let (kv, ki) = build_cone_mesh();
        assert_eq!(ki.len(), (RADIAL_SEGS * 3 + RADIAL_SEGS * 3) as usize);

        for (name, verts, idxs) in [("sphere", &sv, &si), ("cylinder", &cv, &ci), ("cone", &kv, &ki)] {
            for tri in idxs.chunks_exact(3) {
                let p0 = Vec3::from(verts[tri[0] as usize].pos);
                let p1 = Vec3::from(verts[tri[1] as usize].pos);
                let p2 = Vec3::from(verts[tri[2] as usize].pos);
                let cross = (p1 - p0).cross(p2 - p0);
                if cross.length_squared() < 1e-10 {
                    continue;
                }
                let n = Vec3::from(verts[tri[0] as usize].normal)
                    + Vec3::from(verts[tri[1] as usize].normal)
                    + Vec3::from(verts[tri[2] as usize].normal);
                assert!(cross.dot(n) > 0.0, "{name} triangle has inward winding");
            }
        }
    }
}
