//! A Play worker owns its Game and both Lua VMs for their entire lifetime.
//! Messages contain input and published frame data; Lua handles never cross
//! the boundary.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::game::Game;

type Job = Box<dyn FnOnce(&mut Game, &mut WorkerHost) + Send>;

pub(crate) struct PlayWorker {
    jobs: Option<Sender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl PlayWorker {
    fn spawn(
        hooks: Box<dyn GameHost + Send>,
        init: impl FnOnce(&mut Game, &mut WorkerHost) + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<Job>();
        let thread = thread::Builder::new().name("rustudio-gameplay".into()).spawn(move || {
            let mut game = Game::new(None);
            let mut host = WorkerHost { hooks, end_test: None };
            init(&mut game, &mut host);
            loop {
                // While the world's physics batch runs, look in on it between
                // jobs: its tick's server half can run before the next step
                // arrives (Game::play_worker_idle).
                let job = if game.play_worker_idle_due() {
                    match queue.recv_timeout(std::time::Duration::from_micros(250)) {
                        Ok(job) => job,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            game.play_worker_idle();
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match queue.recv() {
                        Ok(job) => job,
                        Err(_) => break,
                    }
                };
                job(&mut game, &mut host);
            }
            game.stop_play(&mut host);
        })?;
        Ok(Self { jobs: Some(jobs), thread: Some(thread) })
    }

    /// Frame steps have one outstanding publication. UI commands share
    /// the same ordered queue and can arrive while that frame renders.
    fn submit<R: Send + 'static>(
        &self,
        run: impl FnOnce(&mut Game, &mut WorkerHost) -> R + Send + 'static,
    ) -> Result<Receiver<R>, mpsc::RecvError> {
        let (done, reply) = mpsc::sync_channel(1);
        self.post(move |game, host| {
            let _ = done.send(run(game, host));
        })?;
        Ok(reply)
    }

    fn post(&self, run: impl FnOnce(&mut Game, &mut WorkerHost) + Send + 'static) -> Result<(), mpsc::RecvError> {
        self.jobs.as_ref().expect("live Play worker").send(Box::new(run)).map_err(|_| mpsc::RecvError)
    }
}

impl Drop for PlayWorker {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gameplay_commands_execute_in_order_on_one_thread() {
        let worker = PlayWorker::spawn(Box::new(()), |game, _| { game.frame_no = 12; }).unwrap();
        let update = |game: &mut Game, _: &mut WorkerHost| {
            game.frame_no += 1;
            (thread::current().id(), game.frame_no)
        };
        let first = worker.submit(update).unwrap().recv().unwrap();
        let second = worker.submit(update).unwrap().recv().unwrap();
        assert_ne!(first.0, thread::current().id());
        assert_eq!(first.0, second.0);
        assert_eq!((first.1, second.1), (13, 14));
    }

    /// A `--fixed-dt` replay with the worker (RUSTUDIO_PLAY_WORKER=1) steps
    /// the same simulation as the direct path: every display joins its
    /// step, the worker knows the fixed step, and no step is skipped.
    #[test]
    fn fixed_step_worker_publishes_every_step() {
        let mut game = Game::new(None);
        game.fixed_dt = Some(1.0 / 60.0);
        game.start_play_threaded(RunMode::Run, &mut (), false, None);
        let ctx = egui::Context::default();
        let start = game.play.as_ref().unwrap().time;
        for _ in 0..30 {
            game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        }
        let worker = &game.play_thread.as_ref().unwrap().worker;
        assert_eq!(worker.submit(|game, _| game.fixed_dt).unwrap().recv().unwrap(), Some(1.0 / 60.0));
        // (the view shows the state published before the last step)
        let shown = game.play.as_ref().unwrap().time - start;
        assert!((shown - 29.0 / 60.0).abs() < 1e-4, "{shown}");
        game.stop_play(&mut ());
    }

    #[test]
    fn datamodel_shutdown_returns_to_the_edit_document() {
        use rustudio_engine::runtime::security;
        let mut game = Game::new(None);
        let original = game.doc.dom.root_ref();
        let mut host = ();
        game.start_play(RunMode::Run, &mut host, false, None);
        game.with_side(Side::Server, |rt, _, _| {
            security::with_main_identity(&rt.lua, security::CORE, || rt.lua.load(r#"
                local part = Instance.new("Part", workspace)
                part.Name = "PlayOnly"
                game:Shutdown()
            "#).exec())
        }).unwrap().unwrap();
        let ctx = egui::Context::default();
        game.play_tick(&ctx, 1.0 / 60.0, &mut host, true, false);
        assert!(!game.is_playing());
        assert_eq!(game.doc.dom.root_ref(), original);
        assert!(game.doc.find_named(game.doc.workspace().unwrap(), "PlayOnly").is_none());
    }

    #[test]
    fn worker_mutates_the_live_world_and_publishes_to_the_view_in_order() {
        let mut game = Game::new(None);
        game.start_play_threaded(RunMode::Run, &mut (), false, Some((731, 419)));
        assert!(game.is_playing());
        assert!(game.run_luau(Side::Server, "assert(game.GameId == 731 and game.PlaceId == 419)").is_none());
        let original = game.doc.dom.root_ref();
        assert!(game.run_luau(Side::Server, r#"
            local value = Instance.new("IntValue", workspace)
            value.Name = "PublicationTest"
            value.Value = 17
        "#).is_none());
        let find = |doc: &Document| {
            let ws = doc.workspace().unwrap();
            doc.dom.get_by_ref(ws).unwrap().children().iter().find_map(|r| {
                let i = doc.dom.get_by_ref(*r)?;
                (i.name == "PublicationTest").then(|| doc.prop(*r, "Value")).flatten()
            })
        };
        assert!(find(&game.doc).is_none());
        let ctx = egui::Context::default();
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        assert!(find(&game.doc).is_none());
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        assert_eq!(find(&game.doc), Some(rbx_dom_weak::types::Variant::Int64(17)));
        assert_eq!(game.doc.dom.root_ref(), original);

        let ws = game.doc.workspace().unwrap();
        let r = game.doc.insert(ws, InstanceBuilder::new("IntValue").with_name("EditorValue"));
        game.doc.rename(r, "RenamedValue");
        game.doc.set_props("Set Value", vec![(r, rbx_dom_weak::ustr("Value"), Variant::Int64(42))], None);
        game.doc.selection = vec![r];
        assert!(game.run_luau(Side::Server, "assert(workspace.RenamedValue.Value == 42)").is_none());
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        assert_eq!(game.doc.selection, vec![r]);
        assert_eq!(game.doc.remove(&[r]), 1);
        assert!(game.run_luau(Side::Server, "assert(workspace:FindFirstChild('RenamedValue') == nil)").is_none());
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        game.play_tick(&ctx, 1.0 / 60.0, &mut (), true, false);
        game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
        game.doc.undo();
        assert!(game.run_luau(Side::Server, "assert(workspace.RenamedValue.Value == 42)").is_none());

        game.stop_play(&mut ());
        assert!(!game.is_playing());
        assert!(find(&game.doc).is_none());
        game.start_play_threaded(RunMode::Run, &mut (), false, None);
        assert!(find(&game.doc).is_none());
        game.stop_play(&mut ());
    }

    /// With the Play worker on, the main thread has no client VM of its own:
    /// the in-game menu's Settings page reads UserGameSettings published in
    /// PlayView.settings so toggling CameraYInverted On then Off (even after
    /// the 1 s `shown` window expires), changing MouseSensitivity, and
    /// changing MasterVolume all round-trip cleanly.
    #[test]
    fn game_menu_settings_round_trip_with_play_worker() {
        use crate::play::game::game_menu::MenuPage;

        let mut game = Game::new(None);
        game.keep_game_settings = false;
        game.fixed_dt = Some(1.0 / 60.0);
        game.start_play_threaded(RunMode::Play, &mut (), false, None);
        assert!(game.has_play_worker());
        assert!(game.play_side_lua(Side::Client).is_none(), "client VM lives on the worker");

        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        if let Some(prop) = fonts.families.get(&egui::FontFamily::Proportional).cloned() {
            for fam in ["medium", "bold", "tabler", "tabler-filled", "mono-bold"] {
                fonts.families.insert(egui::FontFamily::Name(fam.into()), prop.clone());
            }
        }
        ctx.set_fonts(fonts);
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 800.0));
        let mut time = 10.0f64;

        let step_worker = |game: &mut Game, ctx: &egui::Context| {
            for _ in 0..3 {
                game.play_tick(ctx, 1.0 / 60.0, &mut (), false, false);
                game.play_thread.as_ref().unwrap().worker.submit(|_, _| ()).unwrap().recv().unwrap();
            }
        };

        let draw_menu = |game: &mut Game, ctx: &egui::Context, t: f64, click: Option<egui::Pos2>| {
            let mut run_frame = |events: Vec<egui::Event>, ft: f64| {
                let raw = egui::RawInput {
                    screen_rect: Some(rect),
                    time: Some(ft),
                    events,
                    ..Default::default()
                };
                let mut out = ctx.run_ui(raw, |ui| {
                    game.game_menu(ui, rect);
                });
                out.textures_delta.clear();
            };
            if let Some(pos) = click {
                run_frame(
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    t,
                );
                run_frame(
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    t + 0.02,
                );
            } else {
                run_frame(Vec::new(), t);
            }
        };

        let read_client_ugs = |game: &Game| {
            game.play_thread
                .as_ref()
                .unwrap()
                .worker
                .submit(|g, _| {
                    g.with_play_side(Side::Client, |rt, _, _| {
                        crate::play::game::game_menu::user_game_settings(&rt.lua)
                    })
                    .flatten()
                })
                .unwrap()
                .recv()
                .unwrap()
                .expect("client UserGameSettings")
        };

        step_worker(&mut game, &ctx);
        let initial = game
            .play
            .as_ref()
            .and_then(|s| s.presentation.as_ref())
            .and_then(|v| v.settings)
            .expect("PlayView publishes initial UserGameSettings");
        assert!(!initial.inverted);

        // Open the menu directly onto the Settings page with the open tween
        // complete so the controls are on-screen and interactive.
        game.toggle_game_menu();
        {
            let m = &mut game.play.as_mut().unwrap().game.menu;
            m.page = MenuPage::Settings;
            m.changed_at = None;
        }
        draw_menu(&mut game, &ctx, time, None);

        let view = game.play.as_ref().unwrap().game.menu.rects["page"];
        let w = view.width();
        let top = view.min + egui::vec2(0.0, 5.0);
        let sens_more = egui::pos2(view.max.x - 96.0, top.y + 25.0);
        let vol_less = egui::pos2(view.max.x - 0.6 * w + 16.0, top.y + 75.0);
        let inv_arrow = egui::pos2(view.max.x - 16.0, top.y + 125.0);

        // 1. Toggle CameraYInverted Off -> On.
        time += 0.1;
        draw_menu(&mut game, &ctx, time, Some(inv_arrow));
        step_worker(&mut game, &ctx);
        let after_on = read_client_ugs(&game);
        assert!(after_on.inverted, "CameraYInverted should become true: {after_on:?}");
        let published_on = game.play.as_ref().unwrap().presentation.as_ref().unwrap().settings.unwrap();
        assert!(published_on.inverted, "PlayView.settings should publish inverted=true: {published_on:?}");

        // 2. Advance past the 1 s `shown` hold so the menu must read
        // PlayView.settings, then toggle CameraYInverted On -> Off.
        time += 2.0;
        draw_menu(&mut game, &ctx, time, None);
        time += 0.1;
        draw_menu(&mut game, &ctx, time, Some(inv_arrow));
        step_worker(&mut game, &ctx);
        let after_off = read_client_ugs(&game);
        assert!(!after_off.inverted, "CameraYInverted should toggle back to false: {after_off:?}");
        let published_off = game.play.as_ref().unwrap().presentation.as_ref().unwrap().settings.unwrap();
        assert!(!published_off.inverted, "PlayView.settings should publish inverted=false: {published_off:?}");

        // 3. Increment Camera Sensitivity and decrement Volume, then verify
        // both update on the client VM and publish back in PlayView.settings.
        time += 2.0;
        draw_menu(&mut game, &ctx, time, Some(sens_more));
        step_worker(&mut game, &ctx);
        let after_sens = read_client_ugs(&game);
        assert!(
            after_sens.sensitivity > initial.sensitivity + 0.01,
            "MouseSensitivity should increase: {} -> {}",
            initial.sensitivity,
            after_sens.sensitivity
        );
        let published_sens = game.play.as_ref().unwrap().presentation.as_ref().unwrap().settings.unwrap();
        assert!((published_sens.sensitivity - after_sens.sensitivity).abs() < 1e-4);

        time += 2.0;
        draw_menu(&mut game, &ctx, time, Some(vol_less));
        step_worker(&mut game, &ctx);
        let after_vol = read_client_ugs(&game);
        assert!(
            after_vol.volume < initial.volume - 0.05,
            "MasterVolume should decrease: {} -> {}",
            initial.volume,
            after_vol.volume
        );
        let published_vol = game.play.as_ref().unwrap().presentation.as_ref().unwrap().settings.unwrap();
        assert!((published_vol.volume - after_vol.volume).abs() < 1e-4);

        game.stop_play(&mut ());
    }
}

use egui::Rect;
use rbx_dom_weak::{Instance, InstanceBuilder, Ustr, types::{Ref, Variant}};
use rustudio_core::{doc::{Document, Mark, Marks, Reader}, log::LogLine};
use rustudio_engine::{
    gui::{GuiBox, GuiEvent},
    runtime::{RunMode, Side},
};
use rustudio_render::{
    camera::Camera,
    snapshot::{RenderSnapshot, SnapshotReceiver, SnapshotSource},
};

use crate::{
    game::GameHost,
    keys::PhysicalKeys,
    play::{PlayKeyboard, PlaySession, game::{Flash, PlayInput}},
};

/// Information needed by the Play chrome and the game viewport. The
/// presentation copy contains no Runtime, simulated bodies, or Lua values.
pub(super) struct PlayView {
    pub(crate) has_character: bool,
    pub(crate) skip: rustc_hash::FxHashSet<Ref>,
    pub(crate) chat_on: bool,
    pub(crate) topbar_inset: Option<rustudio_engine::ltypes::LRect>,
    pub(crate) backpack_on: bool,
    pub(crate) hotbar: Vec<(String, String, bool)>,
    status: String,
    /// The client's UserGameSettings (the in-game menu's Settings page
    /// shows them; the main thread has no client to ask).
    pub(crate) settings: Option<crate::play::game::game_menu::MenuSettings>,
}

struct Presentation {
    snapshot: RenderSnapshot,
    view: PlayView,
    time: f64,
    showing: Side,
    mouse_behavior: u32,
    mouse_icon: bool,
    cursor_image: Option<String>,
    clickable: bool,
    kicked: Option<String>,
    flashes: Vec<Flash>,
    /// Roblox's CoreScripts started on the worker's client (Escape and the
    /// Play chrome then belong to them, not rustudio's own menu).
    core_scripts_active: bool,
}

enum PublishedFrame {
    Updated(Presentation),
    Reuse,
    Stopped,
}

struct Publication {
    frame: PublishedFrame,
    log: Vec<LogLine>,
    end_test: Option<String>,
    scopes: Vec<(&'static str, f64, u64)>,
    follow_local_input: bool,
    publish_ui: Option<Box<dyn FnOnce() + Send>>,
}

pub(crate) struct ThreadedPlay {
    worker: PlayWorker,
    pending: Option<Receiver<(Publication, SnapshotSource)>>,
    source: Option<SnapshotSource>,
    receiver: SnapshotReceiver,
    follow_local_input: bool,
    deferred_dt: f32,
    queued_edits: Option<EditorEdits>,
}

struct WorkerHost {
    hooks: Box<dyn GameHost + Send>,
    end_test: Option<String>,
}

impl GameHost for WorkerHost {
    fn load_plugins(&mut self, runtime: &rustudio_engine::runtime::Runtime) {
        self.hooks.load_plugins(runtime);
    }

    fn unload_plugins(&mut self, runtime: &rustudio_engine::runtime::Runtime) {
        self.hooks.unload_plugins(runtime);
    }

    fn plugin_pins(&self, lua: &mlua::Lua) -> std::collections::HashSet<Ref> {
        self.hooks.plugin_pins(lua)
    }

    fn keep_play_layout(&self, old: &std::collections::HashMap<Ref, GuiBox>, new: &mut std::collections::HashMap<Ref, GuiBox>) {
        self.hooks.keep_play_layout(old, new);
    }

    fn end_test(&mut self, value: String, _: &[LogLine]) {
        self.end_test = Some(value);
    }
}

struct Input {
    keyboard: PlayKeyboard,
    phys: PhysicalKeys,
    viewport: Rect,
    editor_camera: Option<Camera>,
    frame_no: u64,
    profile_lua: bool,
    events: PlayInput,
    gui_events: Vec<GuiEvent>,
    gui_layout: Option<std::collections::HashMap<Ref, GuiBox>>,
    mouse_delta: glam::Vec2,
    mouse_moved: bool,
    mouse_move_processed: bool,
    mouse_buttons: Vec<(u8, bool, bool)>,
    touch_look: Vec<crate::game_view::TouchLookEvent>,
    wheel: f32,
    wheel_processed: bool,
    menu_open: bool,
    menu_events: Vec<bool>,
    settings: Vec<String>,
    reset: bool,
    leave: bool,
    key_taps: Vec<(egui::Key, bool)>,
    active: bool,
    edits: EditorEdits,
}

impl Input {
    fn take(game: &mut Game, keyboard: PlayKeyboard, edits: EditorEdits) -> Self {
        let s = game.play.as_mut().expect("Play presentation session");
        let p = &mut s.game;
        let gui_layout = std::mem::take(&mut p.gui_layout_changed)
            .then(|| std::mem::take(&mut p.gui_layout));
        Self {
            keyboard,
            phys: game.phys.clone(),
            viewport: game.viewport_rect,
            editor_camera: (s.showing == Side::Server).then(|| game.cam.clone()),
            frame_no: game.frame_no,
            profile_lua: rustudio_core::bench::lua_prof_on(),
            events: std::mem::take(&mut p.input),
            gui_events: std::mem::take(&mut p.gui_events),
            gui_layout,
            mouse_delta: std::mem::take(&mut s.mouse_delta),
            mouse_moved: std::mem::take(&mut s.mouse_moved),
            mouse_move_processed: std::mem::take(&mut s.mouse_move_processed),
            mouse_buttons: std::mem::take(&mut p.uis_mouse),
            touch_look: std::mem::take(&mut p.touch_look),
            wheel: std::mem::take(&mut s.wheel),
            wheel_processed: std::mem::take(&mut s.wheel_processed),
            menu_open: p.menu_open,
            menu_events: std::mem::take(&mut p.menu_events),
            settings: std::mem::take(&mut p.menu.lua),
            reset: std::mem::take(&mut p.reset_request),
            leave: std::mem::take(&mut p.leave_request),
            key_taps: std::mem::take(&mut p.key_taps),
            active: std::mem::take(&mut p.input_active),
            edits,
        }
    }

    fn apply(self, game: &mut Game) -> PlayKeyboard {
        rustudio_core::bench::set_lua_profiling(self.profile_lua);
        self.edits.apply(game);
        game.phys = self.phys;
        game.viewport_rect = self.viewport;
        if let Some(camera) = self.editor_camera {
            game.cam = camera;
        }
        game.frame_no = self.frame_no;
        let pointer = self.events.pointer.map(|pointer| game.resolve_play_pointer(pointer));
        let ray_clicks = self.events.ray_clicks.into_iter()
            .filter_map(|(pointer, button)| game.resolve_play_pointer(pointer).click.map(|(r, at)| (r, at, button)))
            .collect::<Vec<_>>();

        if let Some(s) = game.play.as_mut() {
            let p = &mut s.game;
            p.input.pointer = pointer;
            p.input.clicks.extend(self.events.clicks);
            p.input.clicks.extend(ray_clicks);
            p.input.buttons.extend(self.events.buttons);
            p.input.chat.extend(self.events.chat);
            p.input.slots.extend(self.events.slots);
            p.gui_events.extend(self.gui_events);
            if let Some(layout) = self.gui_layout {
                p.gui_layout = layout;
            }
            s.mouse_delta += self.mouse_delta;
            s.mouse_moved |= self.mouse_moved;
            s.mouse_move_processed |= self.mouse_move_processed;
            p.uis_mouse.extend(self.mouse_buttons);
            p.touch_look.extend(self.touch_look);
            s.wheel += self.wheel;
            s.wheel_processed |= self.wheel_processed;
            p.menu_open = self.menu_open;
            p.menu_events.extend(self.menu_events);
            p.menu.lua.extend(self.settings);
            p.reset_request |= self.reset;
            p.leave_request |= self.leave;
            p.key_taps.extend(self.key_taps);
            if self.active {
                p.last_activity = s.time;
            }
        }

        self.keyboard
    }
}

enum EditorEdit {
    Name(Ref, String),
    Property(Ref, Ustr, Option<Variant>),
    Tree(Vec<Instance>),
    Remove(Ref),
}

struct EditorEdits {
    side: Side,
    edits: Vec<EditorEdit>,
}

impl EditorEdits {
    fn take(game: &mut Game) -> Self {
        let side = game.play.as_ref().expect("live Play presentation").showing;
        let doc = &mut game.doc;
        // This is the presentation document's reader. Each applied worker
        // publication advances it, so only editor changes reach simulation.
        let marks = doc.marks_for(Reader::RenderSnapshot);
        let mut edits = Vec::new();
        let mut seen_props = rustc_hash::FxHashSet::default();
        let mut seen_tree = rustc_hash::FxHashSet::default();
        if let Marks::Some(marks) = marks {
            for mark in marks {
                match mark {
                    Mark::Tree(r) if seen_tree.insert(r) => {
                        if doc.exists(r) {
                            edits.push(EditorEdit::Tree(doc.dom.descendants_of(r).cloned().collect()));
                        } else {
                            edits.push(EditorEdit::Remove(r));
                        }
                    }
                    Mark::Prop(r, key) if seen_props.insert((r, key)) => {
                        if let Some(node) = doc.dom.get_by_ref(r) {
                            if key == "Name" {
                                edits.push(EditorEdit::Name(r, node.name.clone()));
                            } else {
                                edits.push(EditorEdit::Property(r, key, node.properties.get(&key).cloned()));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Self { side, edits }
    }

    fn apply(self, game: &mut Game) {
        if self.edits.is_empty() {
            return;
        }
        game.with_play_side(self.side, |rt, _, _| {
            for edit in self.edits {
                match edit {
                    EditorEdit::Name(r, name) => rt.apply_name(r, &name),
                    EditorEdit::Property(r, key, Some(value)) => rt.apply_prop(r, &key, value),
                    EditorEdit::Property(r, key, None) => {
                        if let Some(mut w) = rt.world_mut() {
                            let d = w.dm();
                            if d.dom.get_by_ref_mut(r).and_then(|node| node.properties.remove(&key)).is_some() {
                                d.mark_prop(r, key);
                            }
                        }
                    }
                    EditorEdit::Remove(r) => {
                        // The editor's undo stack keeps its own copy, so
                        // remove this subtree after Lua observes its detach.
                        rt.apply_parent(r, Ref::none());
                        rt.dispatch();
                        if let Some(mut w) = rt.world_mut() {
                            let d = w.dm();
                            if d.dom.get_by_ref(r).is_some_and(|node| node.parent().is_none()) {
                                d.dom.destroy(r);
                                d.mark_tree(r);
                            }
                        }
                    }
                    EditorEdit::Tree(nodes) => {
                        for (index, node) in nodes.into_iter().enumerate() {
                            let r = node.referent();
                            let parent = node.parent();
                            let attach = if let Some(mut w) = rt.world_mut() {
                                let d = w.dm();
                                if parent.is_some() && !d.exists(parent) {
                                    false
                                } else {
                                    let new = !d.exists(r);
                                    if new {
                                        let builder = InstanceBuilder::new(node.class).with_referent(r)
                                            .with_name(node.name).with_prop_map(node.properties);
                                        d.dom.insert(Ref::none(), builder);
                                        d.mark_tree(r);
                                    }
                                    (new || index == 0)
                                        && d.dom.get_by_ref(r).is_some_and(|node| node.parent() != parent)
                                }
                            } else {
                                false
                            };
                            if attach {
                                rt.apply_parent(r, parent);
                            }
                        }
                    }
                }
            }
            rt.dispatch();
        });
    }
}

impl Publication {
    fn capture(
        game: &mut Game,
        source: &mut SnapshotSource,
        end_test: Option<String>,
        updated: bool,
        publish_ui: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        let frame = if game.play.is_none() {
            PublishedFrame::Stopped
        } else if !updated {
            PublishedFrame::Reuse
        } else {
            let (chat_on, topbar_inset) = game.play_topbar();
            let (backpack_on, settings) = game.with_play_side(Side::Client, |rt, _, _| {
                let backpack_on = rt.world_mut().is_none_or(|w| !w.core_gui_off.contains("Backpack"));
                let settings = crate::play::game::game_menu::user_game_settings(&rt.lua);
                (backpack_on, settings)
            }).unwrap_or((true, None));
            let status = game.play_status().unwrap_or_default();
            let s = game.play.as_ref().expect("live Play publication");
            let skip = s.physics_skip();
            let skip = match s.showing {
                Side::Server => skip.clone(),
                Side::Client => skip.iter().filter_map(|r| s.map.s2c.get(r).copied()).collect(),
            };
            let view = PlayView {
                has_character: game.play_has_character(),
                skip,
                chat_on,
                topbar_inset,
                backpack_on,
                hotbar: game.play_hotbar(),
                status,
                settings,
            };
            let server_id = (s.showing == Side::Client).then(|| s.other_doc_id()).flatten();
            let map = |r| s.map.s2c.get(&r).copied();
            let server = server_id.map(|id| (id, &map as &dyn Fn(Ref) -> Option<Ref>));
            let snapshot = source.capture_with_server(&mut game.doc, &game.cam, server);
            PublishedFrame::Updated(Presentation {
                snapshot,
                view,
                time: s.time,
                showing: s.showing,
                mouse_behavior: s.mouse_behavior,
                mouse_icon: s.mouse_icon,
                cursor_image: s.cursor_image.clone(),
                clickable: s.game.clickable,
                kicked: s.game.kicked.clone(),
                core_scripts_active: s.game.core_scripts_active,
                flashes: s.game.flashes.clone(),
            })
        };
        Self {
            frame,
            log: std::mem::take(&mut game.log),
            end_test,
            scopes: rustudio_core::bench::take_sections(),
            follow_local_input: std::mem::take(&mut game.play_input_pose_due),
            publish_ui,
        }
    }

    fn apply(self, game: &mut Game, receiver: &mut SnapshotReceiver, host: &mut dyn GameHost) {
        game.append_play_output(self.log);
        for (name, amount, calls) in self.scopes {
            rustudio_core::bench::add(name, amount, calls);
        }
        let frame = match self.frame {
            PublishedFrame::Updated(frame) => frame,
            PublishedFrame::Reuse => return,
            PublishedFrame::Stopped => {
                game.stop_play(host);
                if let Some(value) = self.end_test {
                    host.end_test(value, &game.log);
                }
                return;
            }
        };
        let replaced = frame.snapshot.replaces_document();
        let editor_camera = (frame.showing == Side::Server).then(|| game.cam.clone());
        frame.snapshot.apply(receiver, &mut game.doc, &mut game.cam)
            .expect("Play publications arrive in order");
        if let Some(camera) = editor_camera {
            game.cam = camera;
        }
        let _ = game.doc.marks_for(Reader::RenderSnapshot);
        if let Some(publish) = self.publish_ui {
            publish();
        }
        let s = game.play.as_mut().expect("Play presentation retained");
        s.showing = frame.showing;
        s.time = frame.time;
        s.mouse_behavior = frame.mouse_behavior;
        s.mouse_icon = frame.mouse_icon;
        s.cursor_image = frame.cursor_image;
        s.game.clickable = frame.clickable;
        s.game.kicked = frame.kicked;
        s.game.core_scripts_active = frame.core_scripts_active;
        s.game.flashes = frame.flashes;
        s.presentation = Some(frame.view);
        if replaced {
            game.scene = Default::default();
            host.document_changed(&game.doc);
        }
    }
}

impl Game {
    /// On for Play and Run; `RUSTUDIO_PLAY_WORKER=0` steps on the main
    /// thread. A `--fixed-dt` replay steps directly unless it is set to 1:
    /// the worker's GUI boxes reach scripts a step later than direct ones.
    pub fn use_gameplay_worker(&self) -> bool {
        match std::env::var("RUSTUDIO_PLAY_WORKER").as_deref() {
            Ok("0") => false,
            Ok("1") => true,
            _ => self.fixed_dt.is_none(),
        }
    }

    /// Starts a session whose live DataModels stay on the gameplay worker.
    /// The edit document remains local; its copy only seeds the session.
    pub fn start_play_threaded(&mut self, mode: RunMode, host: &mut dyn GameHost, clear: bool, live_ids: Option<(u64, u64)>) {
        if self.play.is_some() {
            return;
        }
        self.edit_audio.stop_playback();
        if mode == RunMode::Play {
            for (level, text) in self.account.settle_for_play(std::time::Duration::from_secs(20), true) {
                self.push_log(level, text);
            }
        }
        let mut doc = Document::from_play_dom(self.doc.dom.clone(), self.doc.title.clone());
        doc.kind = self.doc.kind;
        doc.path.clone_from(&self.doc.path);
        doc.cloud.clone_from(&self.doc.cloud);
        let cam = self.cam.clone();
        let account = self.account.play_copy();
        let play_rig = self.play_rig;
        let play_here = self.play_here;
        let show_client = self.play_shows_client;
        let keep_game_settings = self.keep_game_settings;
        let fixed_dt = self.fixed_dt;
        let teleport = self.teleport_to.take();
        if teleport.is_none() {
            self.play_origin = live_ids;
        }

        let worker = match PlayWorker::spawn(host.play_worker_host(), move |game, worker_host| {
            game.doc = doc;
            game.teleport_to = teleport;
            game.cam = cam;
            game.account = account;
            game.play_rig = play_rig;
            game.play_here = play_here;
            game.play_shows_client = show_client;
            game.keep_game_settings = keep_game_settings;
            game.fixed_dt = fixed_dt;
            game.start_play(mode, worker_host, false, live_ids);
        }) {
            Ok(worker) => worker,
            Err(err) => {
                self.warn(format!("Play worker could not start ({err})"));
                return self.start_play(mode, host, clear, live_ids);
            }
        };
        let Ok((startup, source)) = worker.submit(|game, host| {
            let mut source = SnapshotSource::default();
            let ui = host.hooks.capture_play_ui(game);
            (Publication::capture(game, &mut source, None, true, ui), source)
        }).and_then(|reply| reply.recv()) else {
            self.error("Play worker stopped during startup");
            return;
        };
        if clear {
            self.log.clear();
            self.log_cleared = true;
            self.log_drained = 0;
        }
        if matches!(startup.frame, PublishedFrame::Stopped) {
            self.append_play_output(startup.log);
            return;
        }
        self.play_here = None;
        let saved_sel = self.doc.selection.clone();
        let saved_doc = std::mem::replace(&mut self.doc, Document::placeholder());
        self.play = Some(PlaySession::new(mode, saved_doc, self.cam.clone(), saved_sel));
        let mut receiver = SnapshotReceiver::default();
        startup.apply(self, &mut receiver, host);
        self.play_thread = Some(ThreadedPlay {
            worker, pending: None, source: Some(source), receiver, follow_local_input: false,
            deferred_dt: 0.0, queued_edits: None,
        });
    }

    /// Joins the preceding publication only at the display boundary. The
    /// next step runs while the viewport reads the completed mirror.
    pub(crate) fn threaded_step(&mut self, keyboard: PlayKeyboard, dt: f32, host: &mut dyn GameHost) {
        let Some(mut threaded) = self.play_thread.take() else { return };
        threaded.queue_edits(EditorEdits::take(self));
        // The viewport applies TextBox edits using the mirrored Text and its
        // local caret. Publish a queued edit before the viewport edits again
        // so it cannot apply that caret to the preceding Text.
        let textbox_edit = self.play.as_ref().is_some_and(|play| {
            play.game.gui_events.iter().any(|event| matches!(event, GuiEvent::TextBoxUpdate { .. }))
        });
        // A fixed-step replay also joins every preceding step.
        match threaded.receive(self, host, textbox_edit || self.fixed_dt.is_some()) {
            Receive::Available => {}
            Receive::Pending => {
                threaded.deferred_dt = (threaded.deferred_dt + dt).min(rustudio_core::mech::MAX_FRAME_DT);
                rustudio_core::bench::add("gameplay_reused_frames", 0.0, 1);
                self.play_thread = Some(threaded);
                return;
            }
            Receive::Stopped => return,
        }
        let dt = (dt + std::mem::take(&mut threaded.deferred_dt)).min(rustudio_core::mech::MAX_FRAME_DT);
        let wait_for_publication = std::mem::take(&mut threaded.follow_local_input) || textbox_edit;
        let edits = threaded.queued_edits.take().expect("edits scanned at this boundary");
        let input = Input::take(self, keyboard, edits);
        let mut source = threaded.source.take().expect("last snapshot was applied");
        let pending = threaded.worker.submit(move |game, worker_host| {
            let updated = {
                let _tick = rustudio_core::bench::scope("gameplay_step");
                game.poll_assets();
                let keyboard = input.apply(game);
                game.step_play(dt, worker_host, &keyboard, true)
            };
            let _capture = rustudio_core::bench::scope("gameplay_capture");
            let ui = updated.then(|| worker_host.hooks.capture_play_ui(game)).flatten();
            let frame = Publication::capture(game, &mut source, worker_host.end_test.take(), updated, ui);
            drop(_capture);
            (frame, source)
        });
        match pending {
            Ok(pending) => {
                threaded.pending = Some(pending);
                // InputBegan was delivered while the last viewport drew.
                // Give the following RenderStepped's pose and camera the
                // next presentation slot instead of queueing it one deeper.
                if !wait_for_publication || matches!(threaded.receive(self, host, true), Receive::Available) {
                    self.play_thread = Some(threaded);
                }
            }
            Err(_) => {
                self.error("Play worker stopped before accepting its next frame");
                self.stop_play(host);
            }
        }
    }

    pub(crate) fn threaded_luau(&mut self, side: Side, code: &str, identity: u8) -> Option<String> {
        let edits = EditorEdits::take(self);
        let worker = self.play_thread.as_mut()?;
        worker.queue_edits(edits);
        let edits = worker.queued_edits.take().expect("edits scanned before Lua command");
        let code = code.to_owned();
        worker.worker.submit(move |game, _| {
            edits.apply(game);
            game.run_luau_as(side, &code, identity)
        })
            .and_then(|reply| reply.recv()).ok().flatten()
    }

    /// Runs `f` on the Play worker's game (editor edits flushed to it
    /// first, as for a Luau command) and waits for its answer. None when
    /// Play isn't threaded or the worker is gone.
    pub(crate) fn on_play_worker<R: Send + 'static>(&mut self, f: impl FnOnce(&mut Game) -> R + Send + 'static) -> Option<R> {
        self.play_thread.as_ref()?;
        let edits = EditorEdits::take(self);
        let worker = self.play_thread.as_mut()?;
        worker.queue_edits(edits);
        let edits = worker.queued_edits.take().expect("edits scanned before the call");
        worker.worker.submit(move |game, _| {
            edits.apply(game);
            f(game)
        })
            .and_then(|reply| reply.recv()).ok()
    }

    pub(crate) fn threaded_status(&self) -> Option<String> {
        self.play.as_ref()?.presentation.as_ref().map(|view| view.status.clone())
    }
}

enum Receive {
    Available,
    Pending,
    Stopped,
}

impl ThreadedPlay {
    fn queue_edits(&mut self, edits: EditorEdits) {
        if let Some(queued) = &mut self.queued_edits {
            debug_assert_eq!(queued.side, edits.side);
            queued.edits.extend(edits.edits);
        } else {
            self.queued_edits = Some(edits);
        }
    }

    fn receive(&mut self, main: &mut Game, host: &mut dyn GameHost, wait: bool) -> Receive {
        if let Some(pending) = self.pending.as_ref() {
            let result = if wait {
                let _join = rustudio_core::bench::scope("gameplay_publish_wait");
                pending.recv().ok()
            } else {
                match pending.try_recv() {
                    Ok(frame) => Some(frame),
                    Err(mpsc::TryRecvError::Empty) => return Receive::Pending,
                    Err(mpsc::TryRecvError::Disconnected) => None,
                }
            };
            self.pending = None;
            let Some((frame, source)) = result else {
                main.error("Play worker stopped before publishing its frame");
                main.stop_play(host);
                return Receive::Stopped;
            };
            self.source = Some(source);
            self.follow_local_input |= frame.follow_local_input;
            frame.apply(main, &mut self.receiver, host);
        }
        if main.play.is_some() { Receive::Available } else { Receive::Stopped }
    }
}

impl Game {
    pub(super) fn show_worker_side(&mut self, side: Side, host: &mut dyn GameHost) {
        let Some(mut threaded) = self.play_thread.take() else { return };
        threaded.queue_edits(EditorEdits::take(self));
        if !matches!(threaded.receive(self, host, true), Receive::Available) {
            return;
        }
        let edits = threaded.queued_edits.take().expect("edits scanned before side change");
        let mut source = threaded.source.take().expect("last snapshot was applied");
        let Ok(reply) = threaded.worker.submit(move |game, worker_host| {
            edits.apply(game);
            game.show_play_side(side, worker_host);
            let ui = worker_host.hooks.capture_play_ui(game);
            (Publication::capture(game, &mut source, None, true, ui), source)
        }) else {
            self.error("Play worker stopped before changing sides");
            self.stop_play(host);
            return;
        };
        threaded.pending = Some(reply);
        if matches!(threaded.receive(self, host, true), Receive::Available) {
            self.play_thread = Some(threaded);
        }
    }

    pub fn has_play_worker(&self) -> bool {
        self.play_thread.is_some()
    }

    pub fn queue_play_side(
        &mut self,
        side: Side,
        run: impl FnOnce(
            &rustudio_engine::runtime::Runtime,
            Option<&Document>,
            &mut rustudio_engine::replicate::RepMap,
        ) + Send + 'static,
    ) {
        if let Some(threaded) = self.play_thread.as_ref() {
            let _ = threaded.worker.post(move |game, _| {
                game.with_play_side(side, run);
            });
        } else {
            self.with_play_side(side, run);
        }
    }

    pub(crate) fn worker_physics_report(&self) -> Option<(serde_json::Value, serde_json::Value)> {
        self.play_thread.as_ref()?.worker.submit(|game, _| game.physics_report())
            .and_then(|reply| reply.recv()).ok().flatten()
    }

    pub(crate) fn worker_lua_profile(&self, frames: f64) -> Option<serde_json::Value> {
        let worker = &self.play_thread.as_ref()?.worker;
        let rows = worker.submit(|_, _| rustudio_core::bench::take_lua())
            .and_then(|reply| reply.recv()).ok()?;
        Some(serde_json::json!(rows.into_iter().take(30)
            .map(|(label, ms, calls)| serde_json::json!({
                "what": label, "ms_per_frame": ms / frames, "calls": calls,
            })).collect::<Vec<_>>()))
    }
}

impl Game {
    pub(super) fn stop_play_worker(&mut self) {
        let Some(worker) = self.play_thread.take() else { return };
        if let Some(Ok((publication, _))) = worker.pending.map(|reply| reply.recv()) {
            self.append_play_output(publication.log);
        }
        let quiet = self.quiet_stop;
        if let Ok(Ok(logs)) = worker.worker.submit(move |game, worker_host| {
            game.quiet_stop = quiet;
            game.stop_play(worker_host);
            std::mem::take(&mut game.log)
        }).map(|reply| reply.recv()) {
            self.append_play_output(logs);
        }
    }
}
