//! ViewportFrames drawn by the 3D view's own renderer (a child module of
//! render.rs). A ViewportFrame's parts go through the same scene and draw
//! resolution as the world's (scene.rs `Scene::viewport_frames`, the
//! viewport's `sync_draws`): file meshes, skinning, live EditableMeshes,
//! vertex colours, textures, Decals / Textures, SurfaceAppearances and
//! material packs are the world's own; only the light differs. Each frame
//! is its own layer of renderer items, drawn into its own target with the
//! part shader's ViewportFrame lighting (shader.wgsl `part_shade`, lamp1.w):
//! the frame's Ambient + LightColor × N·L (from LightDirection), clamped to
//! 1, times the surface colour, in sRGB; no shadows, sky, fog or local
//! lights. Then it is turned into the 8-bit premultiplied sRGB image the
//! GUI composites (ImageColor3 / ImageTransparency there).
//!
//! A frame is drawn again only when what it shows changed: one of its parts
//! (any `set_item` / removal in its layer), its camera, size or lighting.
//! A static frame is drawn once.
use std::collections::HashMap;
use std::time::Instant;

use egui_wgpu::wgpu;
use glam::{Mat4, Vec3};
use rbx_dom_weak::types::Ref;

use super::{
    draw_key, pass_of, radius_of, shows, sphere_visible, frustum, to_raw, transparent_order, Globals,
    InstanceRaw, Pass, Renderer, TexMode, DEPTH_FORMAT, HDR_FORMAT,
};
use rustudio_core::math::Xform;

/// What a ViewportFrame shows, as the GUI walk read it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    /// Render size, physical pixels.
    pub size: [u32; 2],
    /// Its CurrentCamera: CFrame and vertical field of view (radians).
    pub cam: Xform,
    pub fov_y: f32,
    /// Ambient, LightColor (sRGB 0..1) and the direction light travels.
    pub ambient: [f32; 3],
    pub light: [f32; 3],
    pub light_dir: Vec3,
    /// BackgroundColor3 (sRGB) and 1 - BackgroundTransparency: the image's
    /// colour where no part is.
    pub background: [f32; 4],
}

/// Each frame's parts and how often they changed.
#[derive(Default)]
pub struct Layers {
    /// Item → the ViewportFrame it is drawn in.
    pub layer_of: HashMap<Ref, Ref>,
    /// Frame → its parts, each with its instances as the frame draws them
    /// (made when its draws change, `set_item`: a redraw only gathers and
    /// sorts them).
    items: HashMap<Ref, rustc_hash::FxHashMap<Ref, Vec<Prep>>>,
    gen: HashMap<Ref, u64>,
    targets: HashMap<Ref, Target>,
    blit: Option<Blit>,
    /// The world's globals from the last frame (material params, exposure).
    pub(super) base: Option<Globals>,
    /// Frames drawn in the last call (bench / checks).
    pub drawn: u32,
}

impl Layers {
    pub fn touch(&mut self, layer: Ref) {
        *self.gen.entry(layer).or_default() += 1;
    }

    /// Part `r`'s draws changed (`set_item`).
    pub(super) fn prepare(&mut self, layer: Ref, r: Ref, draws: &[super::PartDraw], bias: f32) {
        let solid_host = draws.first().is_none_or(|h| {
            h.tex_mode != TexMode::Decal && (!shows(h) || (h.alpha >= 0.999 && h.material_id != 1584))
        });
        let mut out = Vec::with_capacity(draws.len());
        for d in draws {
            if !shows(d) {
                continue;
            }
            let class = match pass_of(d) {
                Some(Pass::Opaque) => Class::Opaque,
                Some(Pass::Decal) => Class::Decal,
                // (terrain water has no place in a frame)
                None if d.custom_mesh.is_some() && d.material == 5.0 => continue,
                None if solid_host && d.tex_mode == TexMode::Decal && d.texture.is_some() => Class::SurfaceDecal,
                None => Class::Blended,
            };
            out.push(Prep { class, key: draw_key(d), raw: to_raw(d, bias), pos: d.xf.pos, radius: radius_of(d) });
        }
        self.items.entry(layer).or_default().insert(r, out);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Opaque,
    Decal,
    SurfaceDecal,
    Blended,
}

struct Prep {
    class: Class,
    key: (usize, usize),
    raw: InstanceRaw,
    pos: Vec3,
    radius: f32,
}

struct Target {
    size: [u32; 2],
    id: egui::TextureId,
    msaa: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolve: wgpu::TextureView,
    out: wgpu::Texture,
    blit_bg: wgpu::BindGroup,
    globals_buf: wgpu::Buffer,
    inst_buf: wgpu::Buffer,
    inst_cap: usize,
    /// What it was last drawn from (request, layer generation).
    drawn: Option<(Request, u64)>,
    used: Instant,
}

struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

const BLIT: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}
// The frame was shaded as sRGB² (premultiplied): back to 8-bit sRGB,
// premultiplied, which the GUI composites.
@fragment fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(src, vec2<i32>(p.xy), 0);
    let a = clamp(c.a, 0.0, 1.0);
    if (a <= 0.0) { return vec4<f32>(0.0); }
    return vec4<f32>(sqrt(max(c.rgb / a, vec3<f32>(0.0))) * a, a);
}
"#;

impl Renderer {
    /// Puts item `r` in ViewportFrame `layer` (None: the world). Its draws
    /// are dropped; `set_item` hands them again.
    pub fn set_layer(&mut self, r: Ref, layer: Option<Ref>) {
        let old = self.vf.layer_of.get(&r).copied();
        if old == layer {
            return;
        }
        match old {
            Some(o) => {
                self.items.remove(&r);
                self.vf.layer_of.remove(&r);
                if let Some(s) = self.vf.items.get_mut(&o) {
                    s.remove(&r);
                }
                self.vf.touch(o);
            }
            None => self.remove_item(r),
        }
        if let Some(l) = layer {
            self.vf.layer_of.insert(r, l);
            self.vf.items.entry(l).or_default().insert(r, Vec::new());
            self.vf.touch(l);
        }
    }

    /// Removes `r` if it is a ViewportFrame's part (a part that moved to
    /// the world keeps its world item).
    pub fn remove_layered_item(&mut self, r: Ref) {
        if self.vf.layer_of.contains_key(&r) {
            self.set_layer(r, None);
        }
    }

    /// Every ViewportFrame part goes (their scene is rebuilt).
    pub fn clear_layered_items(&mut self) {
        let all: Vec<Ref> = self.vf.layer_of.keys().copied().collect();
        for r in all {
            self.set_layer(r, None);
        }
    }

    /// Something every frame may show changed in place (a live mesh's
    /// vertices): all are drawn again.
    pub fn touch_layers(&mut self) {
        let layers: Vec<Ref> = self.vf.items.keys().copied().collect();
        for l in layers {
            self.vf.touch(l);
        }
    }

    /// The texture a ViewportFrame was last drawn into, and its size.
    pub fn vf_texture(&self, vf: Ref) -> Option<(egui::TextureId, [u32; 2])> {
        self.vf.targets.get(&vf).map(|t| (t.id, t.size))
    }

    /// Draws the ViewportFrames in `reqs` whose parts, camera, size or
    /// lighting changed since their last draw; frees the targets of frames
    /// not asked for in 30 s. After the frame's `render` (skinned vertices
    /// and the world's globals are this frame's).
    pub fn render_viewport_frames(&mut self, rs: &egui_wgpu::RenderState, reqs: &HashMap<Ref, Request>) {
        self.pool.flush();
        self.vf.drawn = 0;
        let now = Instant::now();
        let mut gone = Vec::new();
        self.vf.targets.retain(|r, t| {
            let keep = reqs.contains_key(r) || t.used.elapsed().as_secs() < 30;
            if !keep {
                gone.push(t.id);
            }
            keep
        });
        for id in gone {
            rs.renderer.write().free_texture(&id);
            crate::world_gui::unregister_native(id);
        }
        // Layers no frame asks for and nothing holds: forget their generation.
        let Renderer { vf, .. } = self;
        vf.gen.retain(|l, _| vf.items.get(l).is_some_and(|s| !s.is_empty()) || vf.targets.contains_key(l));
        let (Some(base), Some(glass_bg)) = (self.vf.base, self.target.as_ref().map(|t| t.glass_bg.clone())) else {
            return;
        };
        let device = &rs.device;
        if self.vf.blit.is_none() {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("viewport frame blit"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("viewport frame blit"),
                source: wgpu::ShaderSource::Wgsl(BLIT.into()),
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("viewport frame blit"),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
            self.vf.blit = Some(Blit { pipeline, layout });
        }
        for (&vf, req) in reqs {
            let gen = self.vf.gen.get(&vf).copied().unwrap_or(0);
            let size = [req.size[0].clamp(1, 4096), req.size[1].clamp(1, 4096)];
            if self.vf.targets.get(&vf).is_none_or(|t| t.size != size) {
                if let Some(old) = self.vf.targets.remove(&vf) {
                    rs.renderer.write().free_texture(&old.id);
                    crate::world_gui::unregister_native(old.id);
                }
                let t = self.vf_target(rs, size);
                self.vf.targets.insert(vf, t);
            }
            let t = self.vf.targets.get_mut(&vf).unwrap();
            t.used = now;
            if t.drawn == Some((*req, gen)) {
                continue;
            }
            t.drawn = Some((*req, gen));
            let _s = rustudio_core::bench::scope("vf_draw");
            self.vf.drawn += 1;
            self.draw_vf(rs, vf, req, size, base, &glass_bg);
        }
    }

    fn vf_target(&self, rs: &egui_wgpu::RenderState, size: [u32; 2]) -> Target {
        let device = &rs.device;
        let tex = |label: &str, format: wgpu::TextureFormat, samples: u32, usage: wgpu::TextureUsages| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let att = wgpu::TextureUsages::RENDER_ATTACHMENT;
        // The sample count of the pipelines it is drawn with (1 on OpenGL);
        // at one sample the scene is drawn straight into `resolve`.
        let samples = self.viewport.samples;
        let depth = tex("viewport frame depth", DEPTH_FORMAT, samples, att).create_view(&Default::default());
        let resolve = tex("viewport frame resolve", HDR_FORMAT, 1, att | wgpu::TextureUsages::TEXTURE_BINDING)
            .create_view(&Default::default());
        let msaa = if samples == 1 {
            resolve.clone()
        } else {
            tex("viewport frame msaa", HDR_FORMAT, samples, att).create_view(&Default::default())
        };
        let out = tex("viewport frame", wgpu::TextureFormat::Rgba8Unorm, 1, att | wgpu::TextureUsages::TEXTURE_BINDING);
        let out_view = out.create_view(&Default::default());
        let id = rs.renderer.write().register_native_texture(device, &out_view, wgpu::FilterMode::Linear);
        // (the world GUI pass draws it on SurfaceGuis / BillboardGuis)
        crate::world_gui::register_native(id, out.clone());
        let blit = self.vf.blit.as_ref().unwrap();
        let blit_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport frame blit"),
            layout: &blit.layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&resolve) }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport frame globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Target {
            size,
            id,
            msaa,
            depth,
            resolve,
            out,
            blit_bg,
            globals_buf,
            inst_buf: Self::make_buf(device, "viewport frame instances", 64 * std::mem::size_of::<InstanceRaw>()),
            inst_cap: 64,
            drawn: None,
            used: Instant::now(),
        }
    }

    fn draw_vf(&mut self, rs: &egui_wgpu::RenderState, vf: Ref, req: &Request, size: [u32; 2], base: Globals, glass_bg: &wgpu::BindGroup) {
        let (device, queue) = (&rs.device, &rs.queue);
        // The frame's camera: reverse-Z like the world's.
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let fov = req.fov_y.clamp(1e-3, 3.1);
        let view = Mat4::look_to_rh(req.cam.pos, -req.cam.rot.z_axis, req.cam.rot.y_axis);
        let view_rel = Mat4::look_to_rh(Vec3::ZERO, -req.cam.rot.z_axis, req.cam.rot.y_axis);
        let reverse = Mat4::perspective_infinite_reverse_rh(fov, aspect, rustudio_core::mech::NEAR);
        let ordinary = Mat4::perspective_rh(fov, aspect, rustudio_core::mech::NEAR, rustudio_core::mech::FAR);
        let mut g = base;
        g.view_proj = (reverse * view).to_cols_array_2d();
        g.inv_view_proj = (ordinary.as_dmat4() * view_rel.as_dmat4()).inverse().as_mat4().to_cols_array_2d();
        g.cam_pos = [req.cam.pos.x, req.cam.pos.y, req.cam.pos.z, base.cam_pos[3]];
        let l = (-req.light_dir).normalize_or_zero();
        g.sun_dir = [l.x, l.y, l.z, 1.0];
        // (added squared: Studio's frames light in gamma-2 space)
        let sq = |c: [f32; 3]| c.map(|v| v * v);
        let (a, lc) = (sq(req.ambient), sq(req.light));
        g.ambient = [a[0], a[1], a[2], 0.0];
        g.sun_color = [lc[0], lc[1], lc[2], base.sun_color[3]];
        // ViewportFrame lighting (shader.wgsl part_shade); no local lights,
        // fog, sky light or shadows.
        g.lamp1[3] = 2.0;
        g.params[0] = 0.0;
        g.fog[2] = 0.0;
        g.bloom[2] = 0.0;
        g.env_spec[3] = 0.0;
        g.sky_rot[2] = 0.0;
        let planes = frustum(ordinary * view);
        let cam = req.cam.pos;
        let Some(t) = self.vf.targets.get(&vf) else { return };
        queue.write_buffer(&t.globals_buf, 0, bytemuck::bytes_of(&g));
        // The frame's parts, as the world's per-frame list sorts them.
        let mut opaque: Vec<((usize, usize), InstanceRaw)> = Vec::new();
        let mut decal: Vec<((usize, usize), InstanceRaw)> = Vec::new();
        let mut surface_decals: Vec<(f32, (usize, usize), InstanceRaw)> = Vec::new();
        let mut blended: Vec<(f32, (usize, usize), InstanceRaw)> = Vec::new();
        let gather = rustudio_core::bench::scope("vf_gather");
        for preps in self.vf.items.get(&vf).into_iter().flat_map(|m| m.values()) {
            for p in preps {
                if !sphere_visible(&planes, p.pos, p.radius) {
                    continue;
                }
                match p.class {
                    Class::Opaque => opaque.push((p.key, p.raw)),
                    Class::Decal => decal.push((p.key, p.raw)),
                    Class::SurfaceDecal => surface_decals.push(((p.pos - cam).length(), p.key, p.raw)),
                    Class::Blended => blended.push(((p.pos - cam).length(), p.key, p.raw)),
                }
            }
        }
        drop(gather);
        let sort = rustudio_core::bench::scope("vf_sort");
        opaque.sort_unstable_by_key(|e| e.0);
        decal.sort_unstable_by_key(|e| e.0);
        // Back to front (glass with the rest: no scene behind it to bend).
        blended.sort_by(|a, b| transparent_order(0.0, a.0, 0.0, b.0));
        surface_decals.sort_by(|a, b| b.0.total_cmp(&a.0));
        let raw: Vec<InstanceRaw> = opaque
            .iter()
            .map(|e| e.1)
            .chain(decal.iter().map(|e| e.1))
            .chain(surface_decals.iter().map(|e| e.2))
            .chain(blended.iter().map(|e| e.2))
            .collect();
        drop(sort);
        let _up = rustudio_core::bench::scope("vf_upload");
        let t = self.vf.targets.get_mut(&vf).unwrap();
        if raw.len() > t.inst_cap {
            t.inst_cap = raw.len().next_power_of_two();
            t.inst_buf = Self::make_buf(device, "viewport frame instances", t.inst_cap * std::mem::size_of::<InstanceRaw>());
        }
        if !raw.is_empty() {
            queue.write_buffer(&t.inst_buf, 0, bytemuck::cast_slice(&raw));
        }
        let runs = |keys: &mut dyn Iterator<Item = (usize, usize)>, base: u32| -> Vec<((usize, usize), u32, u32)> {
            let mut out: Vec<((usize, usize), u32, u32)> = Vec::new();
            for (i, k) in keys.enumerate() {
                let at = base + i as u32;
                match out.last_mut() {
                    Some(l) if l.0 == k && l.2 == at => l.2 = at + 1,
                    _ => out.push((k, at, at + 1)),
                }
            }
            out
        };
        let mut at = 0u32;
        let opaque_runs = runs(&mut opaque.iter().map(|e| e.0), at);
        at += opaque.len() as u32;
        let mut see_runs = runs(&mut decal.iter().map(|e| e.0), at);
        at += decal.len() as u32;
        see_runs.extend(runs(&mut surface_decals.iter().map(|e| e.1), at));
        at += surface_decals.len() as u32;
        see_runs.extend(runs(&mut blended.iter().map(|e| e.1), at));
        let t = self.vf.targets.get(&vf).unwrap();
        let globals_bg = Self::globals_group(
            device,
            &self.globals_layout,
            &t.globals_buf,
            &self.lights_buf,
            &self.sky_view,
            &self.sky_sampler,
            &self.sun.atlas_view,
            &self.lightgrid_view,
            &self.lightocc_view,
            &self.localgrid_view,
            &self.studs_view,
            &self.studs_sampler,
            &self.low_material_sampler,
            &self.materials,
            &self.probe.slot_views[self.probe.cur],
            &self.probe.slot_views[self.probe.next()],
            &self.brdf_view,
            &self.lshadow,
            self.reduced,
        );
        // The background, premultiplied, in the frame's squared space.
        let b = req.background;
        let clear = wgpu::Color { r: (b[0] * b[0] * b[3]) as f64, g: (b[1] * b[1] * b[3]) as f64, b: (b[2] * b[2] * b[3]) as f64, a: b[3] as f64 };
        let multisampled = self.viewport.samples > 1;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("viewport frame") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.msaa,
                    depth_slice: None,
                    resolve_target: multisampled.then_some(&t.resolve),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: if multisampled { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &globals_bg, &[]);
            pass.set_bind_group(2, glass_bg, &[]);
            pass.set_vertex_buffer(1, t.inst_buf.slice(..));
            let draw = |pass: &mut wgpu::RenderPass, key: (usize, usize), range: std::ops::Range<u32>| {
                let Some(mesh) = self.meshes.get(key.0) else { return };
                pass.set_bind_group(1, self.textures.get(key.1).unwrap_or(&self.textures[0]), &[]);
                mesh.draw(pass, range);
            };
            for r in &opaque_runs {
                pass.set_pipeline(if self.culls_back(r.0 .0) { &self.viewport.opaque } else { &self.viewport.opaque_two_sided });
                draw(&mut pass, r.0, r.1..r.2);
            }
            for r in &see_runs {
                pass.set_pipeline(if self.culls_back(r.0 .0) { &self.viewport.transparent_culled } else { &self.viewport.transparent });
                draw(&mut pass, r.0, r.1..r.2);
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport frame out"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.out.create_view(&Default::default()),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.vf.blit.as_ref().unwrap().pipeline);
            pass.set_bind_group(0, &t.blit_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        crate::gpu_timing::submit(device, queue, "gpu: viewport frames", [encoder]);
        crate::world_gui::native_changed(t.id);
    }
}
