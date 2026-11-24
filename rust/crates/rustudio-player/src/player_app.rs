//! The game window and headless entry point.

use std::{path::PathBuf, time::Instant};

use rustudio_core::{doc::Document, log::LogLine};
use rustudio_engine::{runtime::RunMode, theme};

use crate::{
    game::{build_demo_parts, Game, GameHost},
    offscreen,
    run_harness::{HarnessAction, HarnessInput, RunHarness, EXIT_CODE},
};

#[derive(Default)]
struct PlayerHost {
    end_value: Option<String>,
}

impl GameHost for PlayerHost {
    fn end_test(&mut self, value: String, _: &[LogLine]) {
        self.end_value = Some(value);
    }
}

struct PlayerApp {
    game: Game,
    host: PlayerHost,
    run: RunHarness,
    last_frame: Instant,
    last_work_ms: f32,
    /// Ctrl+F6: the profiler over the lower part of the window.
    profiler: Option<rustudio_profiler_ui::ProfilerWindow>,
}

impl PlayerApp {
    fn new(
        ctx: &egui::Context,
        rs: Option<egui_wgpu::RenderState>,
        wayland: Option<bool>,
        open: Option<PathBuf>,
        run: RunHarness,
        demo: bool,
    ) -> Self {
        rustudio_engine::ti::install(ctx);
        theme::apply(ctx);
        rustudio_engine::mouse_lock::note_window(wayland);
        let mut game = Game::new(rs);
        game.info(format!(
            "rustudio {} ready. Reflection database has {} classes.",
            env!("CARGO_PKG_VERSION"),
            rustudio_core::doc::db().classes.len()
        ));
        for line in rustudio_core::crash::unseen_reports() {
            game.error(line);
        }
        if game.renderer.is_none() && !run.no_render {
            game.error("No wgpu renderer available; the 3D viewport is disabled.");
        }
        if let Some(path) = open {
            match Document::open(&path) {
                Ok(doc) => {
                    let n = doc.dom.descendants().count();
                    game.doc = doc;
                    if !game.use_saved_camera() {
                        game.frame_all();
                    }
                    game.info(format!("Opened {} ({n} instances)", path.display()));
                }
                Err(e) => game.error(format!("Could not open {}: {e}", path.display())),
            }
        } else if demo {
            build_demo_parts(&mut game.doc);
            game.frame_all();
        }
        if let Some([x, y, z, yaw, pitch]) = run.camera {
            game.cam.pos = glam::Vec3::new(x, y, z);
            game.cam.yaw = yaw.to_radians();
            game.cam.pitch = pitch.to_radians();
            game.cam.focus = game.cam.pos + game.cam.forward() * 50.0;
        }
        game.fixed_dt = rustudio_core::sim_clock::dt().or_else(|| {
            std::env::var("RUSTUDIO_FIXED_DT").ok()?.parse::<f32>().ok()
                .filter(|hz| *hz > 0.0).map(|hz| 1.0 / hz)
        });
        if (run.bench.is_none() || std::env::var_os("RUSTUDIO_SESSION").is_some())
            && std::env::var_os("RUSTUDIO_NO_SESSION").is_none()
        {
            game.account.restore();
        }
        game.keep_game_settings = run.bench.is_none() && run.screenshot.is_none()
            && run.input.actions.is_empty() && run.shots.is_empty()
            && std::env::var_os("RUSTUDIO_NO_LAYOUT").is_none();
        Self { game, host: PlayerHost::default(), run, last_frame: Instant::now(), last_work_ms: 0.0, profiler: None }
    }

    fn frame(&mut self, ui: &mut egui::Ui) {
        rustudio_core::profiler::frame(rustudio_core::profiler::FrameKind::Render);
        let _frame_scope = rustudio_core::bench::scope("frame");
        rustudio_core::sim_clock::advance();
        let ctx = ui.ctx().clone();
        let frame_start = Instant::now();
        let dt = frame_start.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = frame_start;
        let game = &mut self.game;
        game.frame_no += 1;
        rustudio_core::load_trace::frame(game.frame_no - 1, dt as f64 * 1000.0);
        game.poll_assets();
        if game.is_playing() {
            let step = game.play_step(dt);
            if game.fixed_dt.is_some() {
                *self.run.play_game_time.get_or_insert(0.0) += step;
            }
            let _scope = rustudio_core::bench::scope("play_tick");
            game.play_tick(&ctx, step, &mut self.host, true, self.run.input.shift);
            // A play-testing teleport: this session ends, the destination's begins.
            if game.poll_teleport() {
                game.stop_play(&mut self.host);
                if game.use_gameplay_worker() {
                    game.start_play_threaded(RunMode::Play, &mut self.host, false, None);
                } else {
                    game.start_play(RunMode::Play, &mut self.host, false, None);
                }
            }
        }
        self.run.end_value = self.host.end_value.take().or(self.run.end_value.take());
        game.scene.sync(&mut game.doc);
        game.game_keys(&ctx, true);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
            let _scope = rustudio_core::bench::scope("viewport");
            game.player_viewport(ui, self.run.started);
        });
        // Ctrl+F6 (Studio's MicroProfiler key in a game): the profiler overlay;
        // RUSTUDIO_PROFILER_OVERLAY=1 opens it from the start (checks).
        if let Some(on) = rustudio_engine::gui::share().microprofiler_req.take() {
            self.profiler = if on {
                Some(self.profiler.take().unwrap_or_else(rustudio_profiler_ui::ProfilerWindow::new))
            } else {
                None
            };
        }
        let toggle = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::F6))
            || (self.profiler.is_none() && game.frame_no == 2 && std::env::var_os("RUSTUDIO_PROFILER_OVERLAY").is_some());
        if toggle {
            self.profiler = match self.profiler.take() {
                Some(_) => None,
                None => Some(rustudio_profiler_ui::ProfilerWindow::new()),
            };
        }
        if let Some(p) = self.profiler.as_mut() {
            let screen = ctx.content_rect();
            let rect = egui::Rect::from_min_max(
                egui::pos2(screen.left() + 12.0, screen.top() + screen.height() * 0.34),
                egui::pos2(screen.right() - 12.0, screen.bottom() - 12.0),
            );
            egui::Area::new(egui::Id::new("profiler-overlay"))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .fade_in(false)
                .show(&ctx, |ui| {
                    ui.set_min_size(rect.size());
                    ui.set_max_size(rect.size());
                    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                    c.set_clip_rect(rect);
                    p.show(&mut c);
                    ui.allocate_rect(rect, egui::Sense::click_and_drag());
                });
        }
        rustudio_engine::runtime::domains::assets_cloud::capture::frame_tick(&ctx, game.viewport_rect);
        if self.run.start_play && game.viewport_rect.is_positive() && game.frame_no >= 2 {
            self.run.start_play = false;
            if game.use_gameplay_worker() {
                game.start_play_threaded(RunMode::Play, &mut self.host, false, None);
            } else {
                game.start_play(RunMode::Play, &mut self.host, false, None);
            }
            self.run.play_started = Some(rustudio_core::sim_clock::now());
        }
        let reason = self.run.tick_before_shot(game, self.last_work_ms)
            .or_else(|| self.run.tick_shots(game, &ctx, |_, _, _| {}));
        self.last_work_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
        crate::profiler_counters::sample(game);
        if let Some(rs) = game.render_state.as_ref() {
            rustudio_render::gpu_timing::end_frame(&rs.device, &rs.queue);
        }
        let bench_done = self.run.bench_frame(game, &ctx, self.last_work_ms, dt);
        if let Some(reason) = reason.or(bench_done.then_some("bench")) {
            self.run.finish_run(game, &ctx, reason, []);
        } else if !game.is_playing() && !self.run.start_play
            && self.run.end_after.is_none() && self.run.screenshot.is_none()
            && self.run.bench.is_none()
        {
            self.run.finish_run(game, &ctx, "leave", []);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        rustudio_render::gpu_health::check();
        rustudio_render::pipeline_cache::save(false);
        self.frame(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let now = self.run.now();
        self.run.input.inject(self.game.viewport_rect, self.game.lock_center_held(), now, raw, |_, _| {});
        if self.game.play_game_input() && ctx.memory(|m| m.focused().is_none()) {
            crate::keys::PhysicalKeys::keep_tab_for_game(raw);
        }
        self.game.phys.ingest(raw);
    }

    fn on_exit(&mut self) {
        rustudio_render::pipeline_cache::save(true);
        rustudio_core::telemetry::flush();
    }
}

pub fn run() -> eframe::Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let mut open: Option<PathBuf> = None;
    let mut demo = false;
    let mut screenshot: Option<PathBuf> = None;
    let mut bench: Option<PathBuf> = None;
    let mut profile_capture: Option<PathBuf> = None;
    let mut profile_instr = false;
    let mut seconds = rustudio_core::mech::PERF_SECONDS;
    let mut seconds_given = false;
    let mut shot_after = None;
    let mut shots = Vec::new();
    let mut from_play = false;
    let mut log_out = None;
    let mut actions = Vec::new();
    let mut camera = None;
    let mut fixed_dt = None;
    let mut fail_on_error = false;
    let mut result = None;
    let mut headless = false;
    let mut no_render = false;
    let mut args = argv.iter().skip(1).cloned();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => screenshot = args.next().map(PathBuf::from),
            "--bench" => bench = args.next().map(PathBuf::from),
            "--profile-capture" => profile_capture = args.next().map(PathBuf::from),
            "--instr" => profile_instr = true,
            "--camera" => {
                let v: Vec<f32> = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|x| x.trim().parse().ok())
                    .collect();
                if v.len() == 5 {
                    camera = Some([v[0], v[1], v[2], v[3], v[4]]);
                }
            }
            "--shot-after" => shot_after = args.next().and_then(|s| s.parse().ok()),
            "--shots" => {
                shots = args
                    .next()
                    .map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect())
                    .unwrap_or_default()
            }
            "--from-play" => from_play = true,
            "--log" => log_out = args.next().map(PathBuf::from),
            "--click" | "--key" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse(&v, a == "--click") {
                    Some(x) => actions.extend(x),
                    None => eprintln!("{a}: can't read {v:?}"),
                }
            }
            "--dblclick" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_dblclick(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--dblclick: can't read {v:?} (X,Y@SECONDS)"),
                }
            }
            "--paste" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_paste(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--paste: can't read {v:?} (TEXT@SECONDS)"),
                }
            }
            "--move" => {
                // The pointer to view position X,Y at S (hover, no buttons).
                let v = args.next().unwrap_or_default();
                match v.split_once('@').and_then(|(xy, at)| {
                    let (x, y) = xy.split_once(',')?;
                    Some((
                        at.trim().parse::<f32>().ok()?,
                        HarnessAction::PointerAt(x.trim().parse().ok()?, y.trim().parse().ok()?),
                    ))
                }) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--move: can't read {v:?} (X,Y@SECONDS)"),
                }
            }
            "--wheel" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_wheel(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--wheel: can't read {v:?} (NOTCHES@SECONDS or NOTCHES@SECONDS:X,Y)"),
                }
            }
            "--type" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_type(&v) {
                    Some(x) => actions.push(x),
                    None => eprintln!("--type: can't read {v:?} (TEXT@SECONDS)"),
                }
            }
            "--shift" => {
                let v = args.next().unwrap_or_default();
                match v.split_once(':').and_then(|(a, h)| {
                    Some((a.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
                }) {
                    Some((at, hold)) => {
                        actions.push((at, HarnessAction::Shift(true)));
                        actions.push((at + hold, HarnessAction::Shift(false)));
                    }
                    None => eprintln!("--shift: can't read {v:?} (SECONDS:HOLD)"),
                }
            }
            "--ldrag" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_ldrag(&v) {
                    Some(list) => actions.extend(list),
                    None => eprintln!("--ldrag: can't read {v:?} (X1,Y1>X2,Y2@SECONDS)"),
                }
            }
            "--lhold" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_lhold(&v) {
                    Some(list) => actions.extend(list),
                    None => eprintln!("--lhold: can't read {v:?} (X,Y@SECONDS:HOLD)"),
                }
            }
            "--rhold" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_rhold(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--rhold: can't read {v:?} (SECONDS:HOLD)"),
                }
            }
            "--rdrag" => {
                let v = args.next().unwrap_or_default();
                match HarnessAction::parse_rdrag(&v) {
                    Some(x) => actions.extend(x),
                    None => eprintln!("--rdrag: can't read {v:?} (DX,DY@SECONDS)"),
                }
            }
            "--luau" => {
                // --luau server|client FILE@S
                let side = args.next().unwrap_or_default();
                let v = args.next().unwrap_or_default();
                match v.rsplit_once('@').and_then(|(f, at)| {
                    Some((std::fs::read_to_string(f).ok()?, at.parse::<f32>().ok()?))
                }) {
                    Some((code, at)) => {
                        actions.push((at, HarnessAction::Luau(side != "client", code)))
                    }
                    None => eprintln!("--luau: can't read {v:?} (FILE@SECONDS)"),
                }
            }
            "--window" => {
                args.next();
            }
            "--seconds" => {
                seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(seconds);
                seconds_given = true;
            }
            "--fail-on-error" => fail_on_error = true,
            "--fixed-dt" => {
                let v = args.next().unwrap_or_default();
                match rustudio_core::sim_clock::parse_dt(&v) {
                    Some(dt) => fixed_dt = Some(dt),
                    None => {
                        eprintln!("--fixed-dt: can't read {v:?} (1/60, 0.0166 or 60hz)");
                        std::process::exit(2);
                    }
                }
            }
            "--result" => result = args.next().map(PathBuf::from),
            "--demo" => demo = true,
            "--headless" => headless = true,
            "--no-render" => {
                headless = true;
                no_render = true;
            }
            // (read at the start of main)
            "--audio" | "--play" | "--play-only" | "--no-ui" => {}
            "-h" | "--help" => {
                println!("usage: rustudio-player [PLACE] [--headless | --no-render] [--seconds N] [--result FILE]");
                return Ok(());
            }
            _ if a.starts_with("--") => {
                eprintln!("unknown option {a}");
                std::process::exit(2);
            }
            _ => open = Some(PathBuf::from(a)),
        }
    }
    if headless && !argv.iter().any(|a| a == "--audio") && std::env::var_os("RUSTUDIO_NO_AUDIO").is_none() {
        std::env::set_var("RUSTUDIO_NO_AUDIO", "1");
    }
    if headless && std::env::var_os("DISABLE_LSFGVK").is_none() {
        std::env::set_var("DISABLE_LSFGVK", "1");
    }
    let place_context = std::env::var("RUSTUDIO_PLACE_CONTEXT").ok().and_then(|v| v.parse().ok())
        .or_else(|| {
            let origin = open.as_ref()?.parent()?.join("origin.json");
            let text = std::fs::read_to_string(origin).ok()?;
            serde_json::from_str::<serde_json::Value>(&text).ok()?.get("place_id")?.as_u64()
        });
    if let Some(place) = place_context {
        rustudio_account::session::set_place_context(place);
    }
    rustudio_core::load_trace::init();
    rustudio_core::crash::install();
    let _ = rustudio_engine::runtime::process_start();
    rustudio_engine::runtime::luau_flags();
    rustudio_render::matpack::init_from_env();
    rustudio_core::rustudio_settings::init(std::env::var_os("RUSTUDIO_LAYOUT").is_some());
    if profile_capture.is_none() {
        rustudio_core::profiler::load_saved_config();
    }
    if let Some(dt) = fixed_dt {
        rustudio_core::sim_clock::enable(dt);
    }
    demo |= screenshot.is_some() && open.is_none();
    let run = RunHarness {
            shot_after,
            log_out,
            input: HarnessInput { actions, pointer: None, shift: false },
            started: rustudio_core::sim_clock::now(),
            held: 0.0,
            screenshot,
            screenshot_requested: false,
            start_play: true,
            camera,
            shots,
            shot_next: 0,
            shot_lines: Vec::new(),
            from_play,
            play_started: None,
            play_game_time: None,
            frame_ms: Vec::new(),
            last_frame: None,
            no_ui: true,
            no_render,
            end_after: (seconds_given && bench.is_none() && profile_capture.is_none()).then_some(seconds),
            fail_on_error,
            result,
            end_value: None,
            run_frames: Vec::new(),
            run_last: None,
            run_playing: false,
            finished: false,
            bench: bench.map(|out| {
                let label = open
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "new place".into());
                rustudio_core::bench::Bench::new(Some(out), &label, true, seconds)
            }),
            profile: profile_capture.map(|out| rustudio_core::profiler::cli::CliCapture::new(out, seconds, true, profile_instr)),
    };
    let bench_mode = run.bench.is_some() || rustudio_core::sim_clock::on();
    let (width, height) = window_arg(&argv)
        .unwrap_or(if run.bench.is_some() { (1265, 900) } else { (1600, 900) });
    let wgpu_options = offscreen::wgpu_options(bench_mode);
    if headless {
        let code = offscreen::run(
            wgpu_options,
            offscreen::Options { size: [width, height], render: !no_render, paced: !bench_mode },
            move |ctx, rs| PlayerApp::new(ctx, rs, None, open, run, demo),
            PlayerApp::frame,
        );
        std::process::exit(code.max(EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed)));
    }
    // The standalone player has no redraw pacing of its own (the editor's
    // native_loop.rs paces mailbox frames): it keeps vsync.
    let mut wgpu_options = wgpu_options;
    if !bench_mode {
        wgpu_options.surface.present_mode = eframe::egui_wgpu::wgpu::PresentMode::AutoVsync;
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([width as f32, height as f32]),
        wgpu_options,
        ..Default::default()
    };
    let result = eframe::run_native("rustudio-player", options, Box::new(move |cc| {
        use eframe::egui_wgpu::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
        let wayland = cc.window_handle().ok().map(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)));
        Ok(Box::new(PlayerApp::new(&cc.egui_ctx, cc.wgpu_render_state.clone(), wayland, open, run, demo)))
    }));
    let code = EXIT_CODE.load(std::sync::atomic::Ordering::Relaxed);
    if code != 0 {
        std::process::exit(code);
    }
    result
}

fn window_arg(argv: &[String]) -> Option<(u32, u32)> {
    let i = argv.iter().position(|a| a == "--window")?;
    let (w, h) = argv.get(i + 1)?.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}
