//! Keep plugin work, a running playtest and Team Create going on the main
//! thread when the window stops receiving redraws (another workspace, minimized).
//! On macOS, App Nap can still delay event-loop wakeups while the app is hidden.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use eframe::{egui, App, CreationContext, EframeWinitApplication, NativeOptions, UserEvent};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::app::RustudioApp;

// Both eframe and the deadline callback access the app on the event-loop thread.
struct SharedApp(Rc<RefCell<RustudioApp>>);

impl App for SharedApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.borrow_mut().raw_input_hook(ctx, raw);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.0.borrow_mut().logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.0.borrow_mut().ui(ui, frame);
    }

    fn on_exit(&mut self) {
        self.0.borrow_mut().on_exit();
    }
}

pub fn run(
    name: &str,
    mut options: NativeOptions,
    create: impl FnOnce(
        &CreationContext<'_>,
    ) -> Result<Box<RustudioApp>, Box<dyn std::error::Error + Send + Sync>>,
) -> eframe::Result {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    if let Some(hook) = options.event_loop_builder.take() {
        hook(&mut builder);
    }
    let event_loop = builder.build()?;
    // Mailbox doesn't hold frames back to the refresh: pace them ourselves.
    let pace_refresh = options.wgpu_options.surface.present_mode == eframe::egui_wgpu::wgpu::PresentMode::Mailbox;
    let app_handle = RefCell::new(None);
    let initialized = Cell::new(false);
    let eframe = eframe::create_native(
        name,
        options,
        Box::new(|cc| {
            let app = create(cc)?;
            initialized.set(true);
            let app = Rc::new(RefCell::new(*app));
            *app_handle.borrow_mut() = Some(Rc::downgrade(&app));
            Ok(Box::new(SharedApp(app)))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut NativeLoop {
        eframe,
        app_handle: &app_handle,
        root_window: None,
        wheel_received: None,
        last_wheel: None,
        pace_refresh,
        refresh: None,
        idle_since: None,
        redraw_wanted: None,
    })?;
    // create_native logs its initialization error before exiting the loop.
    // Preserve a failing exit status even though it only exposes winit's result.
    if !initialized.get() {
        return Err(eframe::Error::AppCreation(
            "eframe failed to initialize the editor".into(),
        ));
    }
    Ok(())
}

struct NativeLoop<'a> {
    eframe: EframeWinitApplication<'a>,
    app_handle: &'a RefCell<Option<Weak<RefCell<RustudioApp>>>>,
    root_window: Option<WindowId>,
    wheel_received: Option<Instant>,
    last_wheel: Option<Instant>,
    /// Mailbox presentation: an Unlimited frame limit means the display's
    /// refresh rate, as Studio's own frame cap does, so the GPU never runs
    /// unbounded (FIFO did this pacing by blocking the main thread).
    pace_refresh: bool,
    /// (when read, Hz): the fastest connected display, re-read every 2 s.
    refresh: Option<(Instant, u32)>,
    /// When the loop last went idle (--bench sleep accounting).
    idle_since: Option<Instant>,
    /// The earliest root redraw asked for and not yet drawn (hidden-window
    /// detection: app.rs tick_between_frames).
    redraw_wanted: Option<Instant>,
}

/// How long an asked-for redraw may stay undrawn before the window counts as hidden.
const HIDDEN_AFTER: Duration = Duration::from_millis(250);

impl ApplicationHandler<UserEvent> for NativeLoop<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.root_window = None;
        self.wheel_received = None;
        self.last_wheel = None;
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.exiting(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        // --bench: how long the loop slept, and how much of that was a
        // deadline it set itself (the frame limit) rather than events.
        if let Some(at) = self.idle_since.take() {
            let ms = at.elapsed().as_secs_f64() * 1000.0;
            rustudio_core::bench::add(
                if matches!(cause, StartCause::ResumeTimeReached { .. }) { "loop_sleep_deadline" } else { "loop_sleep_events" },
                ms,
                1,
            );
        }
        self.eframe.new_events(event_loop, cause);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let redraw = matches!(event, WindowEvent::RedrawRequested);
        if redraw && self.root_window.is_none() {
            self.root_window = Some(id);
        }
        let root = self.root_window == Some(id);
        let wheel = root && matches!(event, WindowEvent::MouseWheel { .. });
        let trace_wheel = wheel && wheel_trace_enabled();
        let received = wheel.then(Instant::now);
        if trace_wheel {
            self.wheel_received = received;
        }
        // Wayland can defer request_redraw until a compositor callback.
        // Submit isolated wheel input now. Frequent wheel events use winit's
        // normal coalescing, and Play retains its simulation cadence.
        let idle_wheel = cfg!(target_os = "linux") && received.is_some_and(|now| {
            let isolated = self.last_wheel.replace(now).is_none_or(|last| now - last >= Duration::from_millis(60));
            isolated && self.app_handle.borrow().as_ref().and_then(Weak::upgrade).is_some_and(|app| {
                !app.borrow().ed.game.is_playing()
            })
        });
        if root && redraw {
            // The display frame starts with the redraw (UI, paint, present);
            // app.rs leaves the boundary to this loop when it runs.
            NATIVE_FRAMES.store(true, std::sync::atomic::Ordering::Relaxed);
            rustudio_core::profiler::frame(rustudio_core::profiler::FrameKind::Render);
        }
        let _redraw = rustudio_core::bench::scope(if redraw { "native_redraw" } else { "loop_window_event" });
        self.eframe.window_event(event_loop, id, event);
        if idle_wheel && !event_loop.exiting() {
            self.eframe.window_event(event_loop, id, WindowEvent::RedrawRequested);
        }
        if root && (redraw || idle_wheel) {
            if let Some(received) = self.wheel_received.take() {
                eprintln!("[wheel] window event to present return {:.2} ms (immediate {idle_wheel})",
                    received.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        let _s = rustudio_core::bench::scope("loop_device_event");
        self.eframe.device_event(event_loop, id, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, mut event: UserEvent) {
        let _s = rustudio_core::bench::scope("loop_user_event");
        // Schedule requested frames without sleeping in the UI thread. Winit
        // window input and resize can still trigger an immediate redraw.
        #[allow(irrefutable_let_patterns)]
        if let UserEvent::RequestRepaint { viewport_id, when, .. } = &mut event {
            if *viewport_id == egui::ViewportId::ROOT {
                let mut hz = [0, 60, 30][rustudio_core::rustudio_settings::performance_choice("fps")];
                if hz == 0 && self.pace_refresh {
                    hz = self.refresh_hz(event_loop);
                }
                if hz > 0 {
                    if let Some(last) = self.last_ui_frame() {
                        *when = (*when).max(last + Duration::from_secs_f64(1.0 / hz as f64));
                    }
                }
            }
        }
        #[allow(irrefutable_let_patterns)]
        if let UserEvent::RequestRepaint { viewport_id, when, .. } = &event {
            if *viewport_id == egui::ViewportId::ROOT {
                let served = |w: Instant| self.last_ui_frame().is_some_and(|l| l >= w);
                self.redraw_wanted = match self.redraw_wanted {
                    Some(w) if !served(w) => Some(w.min(*when)),
                    _ => Some(*when),
                };
            }
        }
        self.eframe.user_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let s = rustudio_core::bench::scope("loop_about_to_wait");
        self.eframe.about_to_wait(event_loop);
        if event_loop.exiting() {
            return;
        }
        drop(s);
        self.idle_since = Some(Instant::now());
        let _s = rustudio_core::bench::scope("loop_plugins_idle");
        // A redraw dispatched in this iteration has already ticked plugins.
        // If the compositor paused redraws, work from the deadline directly.
        let Some(app) = self.app_handle.borrow().as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let now = Instant::now();
        // A redraw asked for that hasn't come: the compositor stopped
        // drawing the window (Wayland sends hidden surfaces no frame callbacks).
        let hidden = self.redraw_wanted.is_some_and(|w| {
            now >= w + HIDDEN_AFTER && app.borrow().last_ui_frame.is_none_or(|l| l < w)
        });
        let deadline = app.borrow_mut().tick_between_frames(now, hidden);
        // (checks again once the stall is long enough to count as hidden)
        let deadline = match self.redraw_wanted.filter(|_| !hidden) {
            Some(w) if app.borrow().last_ui_frame.is_none_or(|l| l < w) => {
                Some(deadline.map_or(w + HIDDEN_AFTER, |d| d.min(w + HIDDEN_AFTER)))
            }
            _ => deadline,
        };
        if let Some(deadline) = deadline {
            event_loop.set_control_flow(with_plugin_deadline(event_loop.control_flow(), deadline));
        }
    }
}

impl NativeLoop<'_> {
    fn refresh_hz(&mut self, event_loop: &ActiveEventLoop) -> u32 {
        if let Some((at, hz)) = self.refresh {
            if at.elapsed() < Duration::from_secs(2) {
                return hz;
            }
        }
        let hz = refresh_cap(event_loop.available_monitors().filter_map(|m| m.refresh_rate_millihertz()));
        self.refresh = Some((Instant::now(), hz));
        hz
    }

    fn last_ui_frame(&self) -> Option<Instant> {
        self.app_handle.borrow().as_ref().and_then(Weak::upgrade).and_then(|app| app.borrow().last_ui_frame)
    }
}

/// The redraw cap for mailbox presentation: the fastest display's refresh
/// rate (a window can move to it), 60 Hz when none reports one.
fn refresh_cap(millihertz: impl Iterator<Item = u32>) -> u32 {
    millihertz.map(|m| (m + 500) / 1000).filter(|hz| *hz >= 24).max().unwrap_or(60)
}

fn wheel_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUSTUDIO_INPUT_TRACE").is_some())
}

fn with_plugin_deadline(flow: ControlFlow, deadline: Instant) -> ControlFlow {
    match flow {
        ControlFlow::Wait => ControlFlow::WaitUntil(deadline),
        ControlFlow::WaitUntil(repaint) => ControlFlow::WaitUntil(deadline.min(repaint)),
        ControlFlow::Poll => ControlFlow::Poll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_redraws_are_capped_at_the_fastest_display() {
        assert_eq!(refresh_cap([59_951, 143_998].into_iter()), 144);
        assert_eq!(refresh_cap(std::iter::empty()), 60);
        assert_eq!(refresh_cap([0].into_iter()), 60);
    }

    #[test]
    fn plugin_deadline_wakes_a_sleeping_event_loop_without_delaying_redraws() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        let later = deadline + Duration::from_millis(1);
        assert_eq!(
            with_plugin_deadline(ControlFlow::Wait, deadline),
            ControlFlow::WaitUntil(deadline)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::WaitUntil(later), deadline),
            ControlFlow::WaitUntil(deadline)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::WaitUntil(now), deadline),
            ControlFlow::WaitUntil(now)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::Poll, deadline),
            ControlFlow::Poll
        );
    }
}

/// Set once the windowed loop marks the display frames itself.
pub static NATIVE_FRAMES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
