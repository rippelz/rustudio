//! Installs Studio's plugin API, settings, gap telemetry and Output limit.
//! Call [`install`] before constructing a Game or creating an engine runtime
//! for Studio. Engine-only hosts use the built-in domain tables.

use std::sync::Once;

/// Installs every studio hook in the engine (idempotent).
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // The plugin API domain (src/plugin_host/api.rs).
        crate::plugin_host::api::install_domain();
        crate::studio_settings::install_hooks();
        rustudio_core::telemetry::install_engine_tables(
            crate::engine_tables::ENGINE_READS,
            crate::engine_tables::ENGINE_FIRES,
        );
        crate::game::install_output_limit(|| crate::studio_settings::studio("Maximum Output Lines", 5000.0));
    });
}

/// Creates an engine runtime with Studio's services installed.
pub fn new_runtime() -> Result<Runtime, String> {
    install();
    Runtime::new()
}

use std::collections::{HashMap, HashSet};

use rbx_dom_weak::types::Ref;
use rustudio_core::{doc::Document, log::LogLine};
use rustudio_engine::{gui::GuiBox, runtime::{RunMode, Runtime, Side}};

use crate::{
    app::{Editor, Harness},
    explorer::ExplorerState,
    game::GameHost,
    plugin_host::{PlaytestCompletion, PluginHost},
    viewport::GizmoDrag,
};

/// Borrows the Studio state a Game transition can affect.
struct StudioGameHost<'a> {
    plugins: &'a mut PluginHost,
    explorer: &'a mut ExplorerState,
    drag: &'a mut Option<GizmoDrag>,
    harness: &'a mut Harness,
}

impl GameHost for StudioGameHost<'_> {
    fn play_worker_host(&self) -> Box<dyn GameHost + Send> {
        Box::new(self.plugins.play_worker_plugins())
    }

    fn document_changed(&mut self, doc: &Document) {
        self.explorer.expand_defaults(doc);
        *self.drag = None;
    }

    fn load_plugins(&mut self, runtime: &Runtime) {
        if let Some((args, _)) = &self.plugins.playtest {
            if let Err(err) = rustudio_engine::runtime::domains::testing::set_test_args(&runtime.lua, args) {
                if let Some(mut world) = runtime.world_mut() {
                    world.output.push((rustudio_core::log::Level::Error, err.to_string()));
                }
            }
        }
        self.plugins.load_into_play(runtime);
    }

    fn unload_plugins(&mut self, runtime: &Runtime) {
        self.plugins.unload_from_play(runtime);
    }

    fn plugin_pins(&self, lua: &mlua::Lua) -> HashSet<Ref> {
        crate::plugin_host::vm::pinned(lua)
    }

    fn keep_play_layout(&self, old: &HashMap<Ref, GuiBox>, new: &mut HashMap<Ref, GuiBox>) {
        crate::plugin_host::widgets_ui::keep_play_layout(old, new);
    }

    fn end_test(&mut self, value: String, log: &[LogLine]) {
        if let Some((_, completion)) = self.plugins.playtest.take() {
            let _ = completion.set(Ok(value));
            return;
        }
        if self.harness.run.ends_on_end_test() {
            self.harness.run.end_value = Some(value);
        } else {
            rustudio_engine::runtime::domains::testing::report_test_run(&value, log);
        }
    }
}

impl Editor {
    fn with_game_host(&mut self, play: impl FnOnce(&mut crate::game::Game, &mut dyn GameHost)) {
        let mut host = StudioGameHost {
            plugins: &mut self.plugins,
            explorer: &mut self.explorer,
            drag: &mut self.drag,
            harness: &mut self.harness,
        };
        play(&mut self.game, &mut host);
        self.flush_output_changes();
    }

    pub fn start_play(&mut self, mode: RunMode) {
        if self.game.is_playing() {
            return;
        }
        if self.defer_play_until_loaded(mode) {
            return;
        }
        #[cfg(target_os = "android")]
        if mode == RunMode::Play && !rustudio_core::default_scripts::play_controls_available(&self.game.doc) {
            if self.play_waiting_for_controls.replace(self.game.doc.id).is_none() {
                rustudio_core::studio_content::request_download();
            }
            return;
        }
        // (a teleport's new session keeps the Output, as Studio's does)
        let teleport = self.game.teleport_to.as_ref().map(|t| t.place_id);
        let clear_output = teleport.is_none()
            && self.harness.plain()
            && rustudio_core::rustudio_settings::studio_flag("Studio", "Clear Output On Start", true);
        if let Some(place) = teleport {
            // Asset delivery answers for the place being played.
            rustudio_account::session::set_place_context(place);
        }
        let live_ids = self.live_team_create_ids();
        self.game.keep_game_settings = self.harness.keeps_game_settings();
        // Deterministic editor self-tests inspect intermediate VM state.
        let threaded = self.game.use_gameplay_worker() && self.harness.selftest.is_none();
        self.plugins.stop_edit_audio_vm_state();
        self.with_game_host(|game, host| {
            if threaded {
                game.start_play_threaded(mode, host, clear_output, live_ids);
            } else {
                game.start_play(mode, host, clear_output, live_ids);
            }
        });
    }

    pub(crate) fn start_plugin_playtest(&mut self, mode: RunMode, args: String, completion: PlaytestCompletion) {
        if self.game.is_playing() || self.plugins.playtest.is_some() {
            let _ = completion.set(Err("A playtest is already running".into()));
            return;
        }
        if self.place_loading() {
            let _ = completion.set(Err("The place is still loading".into()));
            return;
        }
        self.plugins.playtest = Some((args, completion));
        self.start_play(mode);
        if !self.game.is_playing() {
            if let Some((_, completion)) = self.plugins.playtest.take() {
                let _ = completion.set(Err("Could not start the playtest".into()));
            }
        }
    }

    fn finish_stopped_plugin_playtest(&mut self) {
        if !self.game.is_playing() {
            if let Some((_, completion)) = self.plugins.playtest.take() {
                let _ = completion.set(Ok("null".into()));
            }
        }
    }

    pub fn stop_play(&mut self) {
        #[cfg(target_os = "android")]
        { self.play_waiting_for_controls = None; }
        self.with_game_host(|game, host| game.stop_play(host));
        self.plugins.stop_edit_audio_vm_state();
        self.finish_stopped_plugin_playtest();
        self.restore_place_context();
    }

    #[cfg(target_os = "android")]
    pub(crate) fn poll_play_controls(&mut self, ctx: &egui::Context) {
        use rustudio_core::studio_content::{self, DownloadState};

        let Some(doc_id) = self.play_waiting_for_controls else { return };
        if self.game.doc.id != doc_id || self.game.is_playing() {
            self.play_waiting_for_controls = None;
            return;
        }
        if rustudio_core::default_scripts::play_controls_available(&self.game.doc) {
            self.play_waiting_for_controls = None;
            self.start_play(RunMode::Play);
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        let mut cancel = false;
        let mut retry = false;
        egui::Modal::new(egui::Id::new("play_controls_download")).show(ctx, |ui| {
            ui.set_max_width(340.0);
            ui.heading("Controles do modo Play");
            match studio_content::download_state() {
                DownloadState::Downloading(step) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(step);
                    });
                    ui.label("O teste abre assim que os controles estiverem prontos.");
                }
                DownloadState::Failed(err) => {
                    ui.label("Falha ao baixar os controles.");
                    ui.label(err);
                    retry = ui.button("Tentar novamente").clicked();
                }
                DownloadState::Disabled => {
                    ui.label("O download está desativado em rustudio_env.txt (RUSTUDIO_NO_STUDIO_FETCH).");
                }
                DownloadState::Idle | DownloadState::Finished => {
                    ui.label("Os scripts de controle ainda não estão disponíveis.");
                    retry = ui.button("Baixar controles").clicked();
                }
            }
            cancel = ui.button("Cancel").clicked();
        });
        if retry {
            studio_content::request_download();
        }
        if cancel {
            self.play_waiting_for_controls = None;
        }
    }

    /// After a teleported session: asset delivery answers for the open place again.
    fn restore_place_context(&self) {
        if let Some(o) = self.game.doc.cloud.as_ref() {
            rustudio_account::session::set_place_context(o.place_id);
        }
    }

    /// A play-testing teleport whose destination is ready: this session
    /// ends and Play starts on the destination (rustudio-player play_teleport.rs).
    fn teleport_tick(&mut self) {
        if !self.game.poll_teleport() {
            return;
        }
        let Some(mode) = self.game.play.as_ref().map(|p| p.mode) else { return };
        self.with_game_host(|game, host| game.stop_play(host));
        self.start_play(mode);
    }

    pub fn show_play_side(&mut self, side: Side) {
        self.with_game_host(|game, host| game.show_play_side(side, host));
    }

    pub fn play_tick(&mut self, ctx: &egui::Context, dt: f32) {
        let (no_ui, shift) = (self.harness.run.no_ui, self.harness.run.input.shift);
        self.game.keep_game_settings = self.harness.keeps_game_settings();
        self.with_game_host(|game, host| game.play_tick(ctx, dt, host, no_ui, shift));
        self.teleport_tick();
        self.finish_stopped_plugin_playtest();
    }

    /// play_tick for a window the compositor isn't drawing (app.rs background_tick).
    pub fn play_tick_hidden(&mut self, dt: f32) {
        let no_ui = self.harness.run.no_ui;
        self.game.keep_game_settings = self.harness.keeps_game_settings();
        self.with_game_host(|game, host| game.play_tick_hidden(dt, host, no_ui));
        self.teleport_tick();
        self.finish_stopped_plugin_playtest();
    }
}
