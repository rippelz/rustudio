//! State and input shared by Studio and the standalone player.

use std::collections::HashMap;
use egui::Key;
use glam::Vec3;
use rbx_dom_weak::{InstanceBuilder, types::{Ref, Variant}};
use rustudio_core::{doc::{self, Document}, log::{Level, LogLine, tee_log}, mech};
use rustudio_render::{camera::Camera, render::Renderer, scene::Scene};

/// State shared by Studio and the player.
pub struct Game {
    pub phys: crate::keys::PhysicalKeys,
    pub fullscreen: bool,
    pub keep_game_settings: bool,
    pub log_cleared: bool,
    pub log_drained: usize,
    pub viewport_drawn: bool,
    pub doc: Document,
    pub cam: Camera,
    pub scene: Scene,
    /// Cloud asset downloads + disk cache.
    pub assets: rustudio_core::assets::AssetStore,
    pub log: Vec<LogLine>,
    pub account: crate::account::Account,
    /// The running Play / Run session, if any.
    pub play: Option<crate::play::PlaySession>,
    pub(crate) play_thread: Option<crate::play_worker::ThreadedPlay>,
    /// A play-testing teleport's destination, loading (play_teleport.rs).
    pub(crate) teleport_job: Option<crate::play_teleport::TeleportJob>,
    /// The place the next `start_play` plays instead of the edit document,
    /// and how its player arrives (a teleport).
    pub teleport_to: Option<crate::play_teleport::Teleport>,
    /// The live Team Create (universe, place) the Play session began on.
    pub(crate) play_origin: Option<(u64, u64)>,
    /// Stop without the "■ Stopped" line (a teleport's session change).
    pub(crate) quiet_stop: bool,
    pub(crate) play_input_pose_due: bool,
    /// Fixed frame time for deterministic self-tests.
    pub fixed_dt: Option<f32>,
    /// The rig picker: Some(R15 / R6) is an explicit choice that wins over
    /// the place's setting and the avatar; None (Auto) lets them decide.
    pub play_rig: Option<rustudio_engine::rig::RigType>,
    /// Play opens on the client's view, as Studio does (the self-test's
    /// older steps turn this off to watch the server).
    pub play_shows_client: bool,
    pub game_cursor_hidden: bool,
    /// Cursor commands sent, newest last (the self-test checks them).
    pub cursor_log: Vec<String>,
    /// Test Here: the character's first spawn point (the camera's position).
    pub play_here: Option<Vec3>,
    pub viewport_rect: egui::Rect,
    pub frame_no: u64,
    pub snap_move: bool,
    pub move_inc: f32,
    pub snap_rotate: bool,
    pub rotate_inc: f32,
    pub local_space: bool,
    /// ScreenGuis / SurfaceGuis / BillboardGuis: textures, fonts, hover, focus.
    pub gui: rustudio_engine::gui::Gui,
    /// The game holds the cursor (UserInputService.MouseBehavior while playing).
    pub game_grab: Option<crate::game_view::LookGrab>,
    /// Active right-drag camera look with the cursor grabbed.
    pub look_grab: Option<crate::game_view::LookGrab>,
    /// Locked grab failed once (e.g. X11): use Confined from then on.
    pub grab_use_confined: bool,
    pub render_state: Option<egui_wgpu::RenderState>,
    pub renderer: Option<Renderer>,
    pub(crate) automatic_quality: rustudio_render::quality::Automatic,
    pub(crate) quality_frame_at: Option<(std::time::Instant, bool)>,
    pub(crate) quality_cpu_ms: f32,
    /// The document's Highlights, kept from its change journal (viewport.rs).
    pub highlight_index: Option<rustudio_core::doc::ClassIndex>,
    /// ParticleEmitters, Beams and Trails (round 25).
    pub effects: rustudio_render::effects::Effects,
    /// Loaded MeshPart / SpecialMesh geometry.
    pub meshes: rustudio_core::assets::MeshCache,
    pub textures: rustudio_core::assets::TextureCache,
    /// Clothing-template box meshes by body part name.
    pub clothing_meshes: std::collections::HashMap<String, u32>,
    /// The parts inside drawn ViewportFrames (vf_render.rs), drawn by the
    /// same `sync_draws` as the world's, with their own draw cache.
    pub vf_scene: Scene,
    pub vf_draw_cache: crate::game_view::DrawCache,
    pub vf_draws_asset_gen: u64,
    /// Last frame: (loaded meshes drawn, textured parts, decals, clothed body parts).
    pub draw_stats: [usize; 4],
    /// What the renderer holds per part.
    pub draw_cache: crate::game_view::DrawCache,
    /// Bumped whenever a downloaded asset arrives (parts waiting on one are redone).
    pub asset_gen: u64,
    pub draws_asset_gen: u64,
    /// matpack::generation(), wrap::generation() and
    /// skin::base_generation() last seen.
    pub material_gen: u64,
    pub wrap_gen: u64,
    pub skin_gen: u64,
    /// Uploaded layered-clothing pieces: (fitting, piece) → (mesh, centre, extent).
    pub wrap_meshes: HashMap<(usize, usize), (u32, glam::Vec3, glam::Vec3)>,
    /// Fitting failures already reported.
    pub wrap_warned: std::collections::HashSet<String>,
    /// Triangles in the drawn voxel terrain.
    pub terrain_tris: usize,
    /// The terrain as last drawn (document id, Terrain, voxels), so an edit
    /// re-meshes only the chunks it changed (round 21).
    pub terrain_drawn: Option<(u64, rbx_dom_weak::types::Ref, rustudio_core::terrain::Voxels)>,
    /// Terrain shader parameters to rebuild (colours, packs arriving), and
    /// the material-pack generation they were built at.
    pub terrain_params_dirty: bool,
    pub terrain_params_gen: u64,
    pub terrain_present: [bool; 23],
    /// Chunks with Grass tops (decoration candidates) and those whose blades
    /// are built.
    pub terrain_grass_chunks: std::collections::HashSet<[i32; 3]>,
    pub terrain_grass_built: std::collections::HashSet<[i32; 3]>,
    pub terrain_grass_density: f32,
    pub terrain_grass_length: f32,
    /// The water look last drawn (WaterColor, WaterTransparency, WaterReflectance).
    pub terrain_water_look: Option<([u8; 3], f32, f32)>,
    /// Terrain water's waves and clearness for the water pass (round 24).
    pub water_waves: rustudio_render::water::WaterLook,
    /// Studio's scene environment probe (env_probe.rs).
    pub env_probe: rustudio_engine::env_probe::EnvProbe,
    /// A brush stroke's voxels, the chunks it touched and whether water may
    /// have changed: drawn next frame without decoding the grid.
    pub terrain_brushed: Option<(rustudio_core::terrain::Voxels, Vec<[i32; 3]>, bool)>,
    /// Local lights, sky faces and the sky-visibility map.
    pub lighting: crate::game_view::LightingState,
    /// The 3D view rendered at this many pixels instead of its panel's
    /// (camera aspect from it; drawn scaled into the panel): self-test
    /// steps that compare with a Studio capture at Studio's view size.
    pub view_render_size: Option<[u32; 2]>,
    /// Extra 3D adornments for this frame (Team Create collaborator camera gizmos).
    pub extra_adorn: Vec<rustudio_render::adorn::AdornDraw>,
    /// Extra Highlight draws for this frame (Team Create collaborator selections).
    pub extra_highlights: Vec<rustudio_render::highlight::HighlightDraw>,
}

impl Game {
    pub fn new(render_state: Option<egui_wgpu::RenderState>) -> Self {
        let renderer = render_state.as_ref().map(|rs| {
            rustudio_render::gpu_health::watch(&rs.adapter, &rs.device);
            // Weak-GPU mode and the renderer's binding set (gpu_caps.rs),
            // before automatic_quality below takes its first level.
            let caps = rustudio_render::gpu_caps::GpuCaps::of_adapter(&rs.adapter, &rs.device);
            rustudio_core::rustudio_settings::set_weak_gpu(rustudio_render::gpu_caps::decide_lowend(&caps));
            Renderer::with_caps(&rs.device, &rs.queue, &caps)
        });
        Self {
            phys: Default::default(),
            fullscreen: false,
            keep_game_settings: true,
            log_cleared: false,
            log_drained: 0,
            viewport_drawn: false,
            doc: Document::new_place(),
            cam: Camera::default(),
            scene: Scene::default(),
            assets: Default::default(),
            log: Vec::new(),
            account: crate::account::Account::new(),
            play: None,
            play_thread: None,
            teleport_job: None,
            teleport_to: None,
            quiet_stop: false,
            play_origin: None,
            play_input_pose_due: false,
            fixed_dt: None,
            play_rig: None,
            play_shows_client: true,
            game_cursor_hidden: false,
            cursor_log: Vec::new(),
            play_here: None,
            viewport_rect: egui::Rect::NOTHING,
            frame_no: 0,
            snap_move: mech::SNAP_ON_BY_DEFAULT,
            move_inc: mech::MOVE_SNAP,
            snap_rotate: mech::SNAP_ON_BY_DEFAULT,
            rotate_inc: mech::ROTATE_SNAP_DEG,
            local_space: false,
            gui: Default::default(),
            game_grab: None,
            look_grab: None,
            grab_use_confined: !rustudio_engine::mouse_lock::platform_locks(),
            render_state,
            renderer,
            automatic_quality: Default::default(),
            quality_frame_at: None,
            quality_cpu_ms: 0.0,
            highlight_index: None,
            effects: Default::default(),
            meshes: Default::default(),
            textures: Default::default(),
            clothing_meshes: Default::default(),
            vf_scene: Scene::viewport_frames(),
            vf_draw_cache: Default::default(),
            vf_draws_asset_gen: 0,
            draw_stats: [0; 4],
            draw_cache: Default::default(),
            asset_gen: 0,
            draws_asset_gen: 0,
            material_gen: 0,
            wrap_gen: 0,
            skin_gen: 0,
            wrap_meshes: HashMap::new(),
            wrap_warned: Default::default(),
            terrain_tris: 0,
            terrain_drawn: None,
            terrain_params_dirty: true,
            terrain_params_gen: u64::MAX,
            terrain_present: [false; 23],
            terrain_grass_chunks: Default::default(),
            terrain_grass_built: Default::default(),
            terrain_grass_density: -1.0,
            terrain_grass_length: -1.0,
            terrain_water_look: None,
            water_waves: Default::default(),
            env_probe: Default::default(),
            terrain_brushed: None,
            lighting: Default::default(),
            view_render_size: None,
            extra_adorn: Vec::new(),
            extra_highlights: Vec::new(),
        }
    }
}

fn timestamp() -> String {
    // Studio's Output timestamp: HH:MM:SS.mmm in local time.
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let secs = (ms / 1000) as i64;
    let offset = chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).offset().local_minus_utc())
        .unwrap_or(0);
    let s = ((ms / 1000) as i64 + offset as i64).rem_euclid(86_400) as u64;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, (s / 60) % 60, s % 60, ms % 1000)
}

impl Game {
    pub fn push_log(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        if level != Level::Info {
            eprintln!("[{level:?}] {text}");
        }
        tee_log(&text);
        self.log.push(LogLine {
            at: timestamp(),
            level,
            text,
        });
        self.trim_output();
    }

    pub(crate) fn append_play_output(&mut self, lines: Vec<LogLine>) {
        if !lines.is_empty() {
            self.log.extend(lines);
            self.trim_output();
        }
    }

    fn trim_output(&mut self) {
        // Studio Settings > Output > Maximum Output Lines (5000: past it the
        // oldest fifth goes).
        let max = max_output_lines().clamp(10.0, 1e7) as usize;
        if self.log.len() > max {
            let n = self.log.len() - max - 1 + max / 5;
            self.log.drain(0..n);
            self.log_drained += n;
        }
    }
    pub fn info(&mut self, t: impl Into<String>) {
        self.push_log(Level::Info, t)
    }
    /// While the client of a Play session is shown: this frame's key presses
    /// go to the game (UserInputService, the hotbar, the in-game menu) and
    /// are taken out of the input so no editor shortcut sees them. Shared by
    /// the editor and the no-UI mode (`--no-ui`), where Escape also reaches
    /// the game's scripts (gameProcessedEvent true) as well as opening the
    /// menu, and no editor shortcut (F5, Shift+F5, F8) comes first.
    pub fn game_keys(&mut self, ctx: &egui::Context, no_ui: bool) {
        if self.play_game_input() {
            let esc = ctx.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { key: Key::Escape, pressed: true, repeat: false, .. }
                    )
                })
            });
            // With Roblox's CoreScripts running (core_scripts.rs) their own
            // menu, hotbar and chat take the keys, Escape included; rustudio's
            // stand-ins stay out of the way.
            let core_ui = rustudio_core::rustudio_settings::core_scripts();
            if esc && !core_ui {
                self.toggle_game_menu();
            }
            // Presses this frame: a tap pressed and released within one frame
            // never shows in keys_down, yet scripts must see it. The key
            // positions are physical, like Roblox's KeyCodes (src/keys.rs).
            // (Escape belongs to the Roblox menu: games never hear it.)
            let taps: Vec<Key> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key {
                            key,
                            physical_key,
                            pressed: true,
                            repeat: false,
                            ..
                        } if core_ui || physical_key.unwrap_or(*key) != Key::Escape => {
                            Some(physical_key.unwrap_or(*key))
                        }
                        _ => None,
                    })
                    .collect()
            });
            // The open menu takes the keys (R, L); the game hears none.
            if !self.game_menu_keys(&taps) {
                // (A focused TextBox has them: game-processed, Enter included.)
                let processed = self.gui.focus.is_some();
                if let Some(s) = self.play.as_mut() {
                    if !processed && !core_ui {
                        s.game.hotbar_taps.extend(taps.iter().copied());
                    }
                    s.game.key_taps.extend(taps.into_iter().map(|k| (k, processed)));
                }
            }
            // No UI: the menu has Escape first, and UserInputService hears it
            // too, as processed (play_tick lets it past the open menu).
            if esc && no_ui {
                if let Some(s) = self.play.as_mut() {
                    s.game.key_taps.push((Key::Escape, true));
                }
            }
            ctx.input_mut(|i| {
                i.events.retain(
                    |e| !matches!(e, egui::Event::Key { modifiers, .. } if modifiers.is_none()),
                );
            });
        }
    }
    pub fn warn(&mut self, text: impl Into<String>) {
        self.push_log(Level::Warn, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push_log(Level::Error, text);
    }
}

impl Game {
    /// Puts the avatar's clothing, face and accessories on a new character
    /// (server DataModel; they replicate like any other instances). The look
    /// is the person's session avatar; the placement is avatar's
    /// (crates/rustudio-engine/src/avatar.rs, engine code, which cannot hold an `impl Game`).
    pub fn dress_character(&mut self, model: Ref) {
        let Some(look) = self.account.avatar().cloned() else {
            return;
        };
        rustudio_engine::avatar::dress(&mut self.doc, model, &look);
    }
}

type OutputLimit = fn() -> f64;
static OUTPUT_LIMIT: std::sync::OnceLock<OutputLimit> = std::sync::OnceLock::new();

pub fn install_output_limit(limit: OutputLimit) {
    let _ = OUTPUT_LIMIT.set(limit);
}

fn max_output_lines() -> f64 {
    OUTPUT_LIMIT.get().map_or(5000.0, |limit| limit())
}

/// The Studio services a play session can call. A standalone session uses
/// the defaults, with no plug-ins or editor chrome.
pub trait GameHost {
    /// Session services that can run on a gameplay worker. The editor
    /// supplies detached plugin load specifications and a UI publication sink.
    fn play_worker_host(&self) -> Box<dyn GameHost + Send> {
        Box::new(())
    }
    fn capture_play_ui(&mut self, _game: &mut Game) -> Option<Box<dyn FnOnce() + Send>> {
        None
    }
    fn document_changed(&mut self, _doc: &Document) {}
    fn load_plugins(&mut self, _runtime: &rustudio_engine::runtime::Runtime) {}
    fn unload_plugins(&mut self, _runtime: &rustudio_engine::runtime::Runtime) {}
    fn plugin_pins(&self, _lua: &mlua::Lua) -> std::collections::HashSet<Ref> {
        Default::default()
    }
    fn keep_play_layout(
        &self,
        _old: &HashMap<Ref, rustudio_engine::gui::GuiBox>,
        _new: &mut HashMap<Ref, rustudio_engine::gui::GuiBox>,
    ) {}
    fn end_test(&mut self, value: String, log: &[LogLine]) {
        rustudio_engine::runtime::domains::testing::report_test_run(&value, log);
    }
}

impl GameHost for () {}

impl Game {
    pub fn poll_assets(&mut self) {
        for (err, m) in rustudio_core::studio_content::take_messages() {
            if err { self.error(m) } else { self.info(m) }
        }
        self.assets.set_client(self.account.shared_client());
        // --fixed-dt: last frame's downloads land now, not whenever (crates/rustudio-core/src/sim_clock.rs).
        if rustudio_core::sim_clock::on() {
            self.assets.wait_pending(std::time::Duration::from_secs(60));
        }
        let done = self.assets.poll();
        // (finished decodes, composites and mesh parses from the workers:
        // the parts waiting on them are redone)
        if self.textures.poll_decoded() | self.meshes.poll() {
            self.asset_gen += 1;
        }
        if rustudio_core::load_trace::on() {
            let (pending, loaded) = self.textures.counts();
            rustudio_core::load_trace::assets(self.meshes.count(), pending, loaded);
        }
        // (the memory a load's decodes and downloads freed goes back to the
        // system once it settles: crates/rustudio-core/src/heap.rs)
        rustudio_core::heap::settle(self.textures.counts().0 > 0 || !done.is_empty());
        if !done.is_empty() {
            self.meshes.retry(&done);
            self.textures.retry(&done);
            self.asset_gen += 1;
        }
        // Material texture packs load on their own threads.
        let mat_gen = rustudio_render::matpack::generation();
        if mat_gen != self.material_gen {
            self.material_gen = mat_gen;
            self.asset_gen += 1;
        }
        let wrap_gen = rustudio_render::wrap::generation();
        if wrap_gen != self.wrap_gen {
            self.wrap_gen = wrap_gen;
            self.asset_gen += 1;
        }
        let skin_gen = rustudio_render::skin::base_generation();
        if skin_gen != self.skin_gen {
            self.skin_gen = skin_gen;
            self.asset_gen += 1;
        }
        for msg in std::mem::take(&mut self.assets.messages) {
            self.info(msg);
        }
        for (level, text) in self.account.poll() {
            self.push_log(level, text);
        }
    }

    pub fn play_step(&self, dt: f32) -> f32 {
        let mut step = self.fixed_dt.unwrap_or_else(|| crate::run_harness::frame_grid(dt));
        if let Some(fixed) = self.fixed_dt.filter(|_| std::env::var_os("RUSTUDIO_GAME_CLOCK").is_some()) {
            step += rustudio_engine::runtime::take_game_clock_spent(1.0 / fixed);
        }
        step
    }
}

impl Game {
    pub fn use_saved_camera(&mut self) -> bool {
        let Some(ws) = self.doc.workspace() else {
            return false;
        };
        let cam = match self.doc.prop(ws, "CurrentCamera") {
            Some(Variant::Ref(r)) if self.doc.exists(r) => Some(r),
            _ => self.doc.find_child(ws, "Camera"),
        };
        let Some(xf) = cam.and_then(|c| rustudio_render::scene::xform_of(&self.doc, c)) else {
            return false;
        };
        if !xf.pos.is_finite() || xf.pos.length() < 1e-3 {
            return false;
        }
        let look = -xf.rot.z_axis;
        let focus = cam.and_then(|c| match self.doc.prop(c, "Focus") {
            Some(Variant::CFrame(f)) => Some(rustudio_core::math::v3(f.position)),
            _ => None,
        });
        self.cam = Camera {
            mode: self.cam.mode,
            ..Camera::default()
        };
        self.cam.pos = xf.pos;
        self.cam.pitch = look.y.clamp(-1.0, 1.0).asin();
        self.cam.yaw = (-look.x).atan2(-look.z);
        self.cam.focus = focus
            .filter(|f| (*f - xf.pos).length() > 0.5)
            .unwrap_or(xf.pos + look * 20.0);
        true
    }
}

impl Game {
    pub fn frame_all(&mut self) {
        self.scene.sync(&mut self.doc);
        let centers: Vec<Vec3> = self
            .scene
            .parts
            .iter()
            .filter(|p| p.size.max_element() <= 200.0 && p.xf.pos.is_finite())
            .map(|p| p.xf.pos)
            .collect();
        self.cam = Camera {
            mode: self.cam.mode,
            ..Camera::default()
        };
        if centers.is_empty() {
            return;
        }
        let quantile = |axis: usize, q: f32| {
            let mut v: Vec<f32> = centers.iter().map(|c| c[axis]).collect();
            v.sort_by(f32::total_cmp);
            v[((v.len() - 1) as f32 * q).round() as usize]
        };
        let lo = Vec3::new(quantile(0, 0.1), quantile(1, 0.1), quantile(2, 0.1));
        let hi = Vec3::new(quantile(0, 0.9), quantile(1, 0.9), quantile(2, 0.9));
        self.cam.frame(
            (lo + hi) * 0.5,
            ((hi - lo).length() * 0.5).clamp(8.0, 2000.0),
        );
    }
}

pub fn build_demo_parts(doc: &mut Document) -> Ref {
        let ws = doc.default_parent();
        let parts: [(&str, u32, [f32; 3], [f32; 3], [u8; 3]); 6] = [
            (
                "Tower",
                1,
                [-14.0, 6.0, -10.0],
                [6.0, 12.0, 6.0],
                [196, 112, 70],
            ),
            (
                "Ball",
                0,
                [-4.0, 2.0, -4.0],
                [4.0, 4.0, 4.0],
                [80, 160, 220],
            ),
            (
                "Pipe",
                2,
                [8.0, 1.5, -8.0],
                [10.0, 3.0, 3.0],
                [110, 190, 120],
            ),
            ("Step", 1, [10.0, 0.5, 4.0], [8.0, 1.0, 8.0], [230, 200, 90]),
            (
                "Bridge",
                1,
                [-2.0, 8.0, -18.0],
                [24.0, 1.0, 4.0],
                [150, 150, 165],
            ),
            ("Lamp", 1, [3.0, 3.0, 6.0], [1.0, 6.0, 1.0], [250, 240, 220]),
        ];
        let mut last = Ref::none();
        for (name, shape, p, s, c) in parts {
            let r = doc.insert(
                ws,
                doc::new_part(name, shape, Vec3::from(p), Vec3::from(s), c),
            );
            last = if name == "Ball" { r } else { last };
        }
        let ramp = doc::new_part(
            "Ramp",
            1,
            Vec3::new(-14.0, 2.0, 0.0),
            Vec3::new(6.0, 4.0, 8.0),
            [196, 112, 70],
        )
        .with_class("WedgePart");
        doc.insert(ws, ramp);
        let model = doc
            .insert(ws, InstanceBuilder::new("Model").with_name("Statue"));
        doc.insert(
            model,
            InstanceBuilder::new("MeshPart")
                .with_name("StatueMesh")
                .with_property("Anchored", true)
                .with_property("Size", rbx_dom_weak::types::Vector3::new(4.0, 7.0, 4.0))
                .with_property(
                    "CFrame",
                    rbx_dom_weak::types::CFrame::new(
                        rbx_dom_weak::types::Vector3::new(18.0, 4.5, -14.0),
                        rbx_dom_weak::types::Matrix3::identity(),
                    ),
                )
                .with_property(
                    "Color",
                    rbx_dom_weak::types::Color3uint8::new(170, 140, 210),
                ),
        );
        doc.insert(
            model,
            doc::new_part(
                "Plinth",
                1,
                Vec3::new(18.0, 0.5, -14.0),
                Vec3::new(6.0, 1.0, 6.0),
                [90, 90, 100],
            ),
        );
        let glass = doc::new_part(
            "Glass",
            1,
            Vec3::new(2.0, 3.0, -12.0),
            Vec3::new(8.0, 6.0, 0.5),
            [180, 220, 255],
        )
        .with_property("Transparency", 0.5f32);
        doc.insert(ws, glass);
        let neon = doc::new_part(
            "NeonStrip",
            1,
            Vec3::new(3.0, 6.25, 6.0),
            Vec3::new(2.0, 0.5, 2.0),
            [255, 120, 60],
        )
        .with_property("Material", rbx_dom_weak::types::Enum::from_u32(288));
        doc.insert(ws, neon);
        let sp = doc.find_child(ws, "Script");
        if sp.is_none() {
            let sss = doc
                .find_child(doc.dom.root_ref(), "ServerScriptService")
                .unwrap_or(ws);
            doc.insert(
                sss,
                InstanceBuilder::new("Script")
                    .with_name("Main")
                    .with_property("Source", "print(\"Hello from rustudio\")\n".to_string()),
            );
        }
        last
}
