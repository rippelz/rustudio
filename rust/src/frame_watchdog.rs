//! Independent frame progress diagnostics; never holds a lock across editor work.
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

struct Progress {
    completed: Instant,
    stage: &'static str,
    pending: &'static str,
    active: bool,
    stop: bool,
}
impl Progress {
    fn stalled(&self, now: Instant) -> bool {
        self.active && now.saturating_duration_since(self.completed) > Duration::from_secs(5)
    }
}
struct Shared {
    progress: Mutex<Progress>,
    changed: Condvar,
}

pub struct Watchdog {
    state: Arc<Shared>,
}
impl Watchdog {
    pub fn new(ctx: egui::Context) -> Self {
        let state = Arc::new(Shared {
            progress: Mutex::new(Progress {
                completed: Instant::now(),
                stage: "startup",
                pending: "",
                active: false,
                stop: false,
            }),
            changed: Condvar::new(),
        });
        let shared = state.clone();
        std::thread::Builder::new().name("frame watchdog".into()).spawn(move || {
            let mut reported = false;
            let mut progress = shared.progress.lock().unwrap();
            while !progress.stop {
                if !progress.active {
                    reported = false;
                    progress = shared.changed.wait(progress).unwrap();
                    continue;
                }
                let (state, timeout) = shared.changed.wait_timeout(progress, Duration::from_secs(1)).unwrap();
                progress = state;
                if progress.stop || !timeout.timed_out() {
                    continue;
                }
                if !progress.stalled(Instant::now()) {
                    reported = false;
                    continue;
                }
                let (completed, stage, pending) = (progress.completed, progress.stage, progress.pending);
                drop(progress);
                if !reported {
                    eprintln!("rustudio frame stall: {:.1}s since completed frame; last completed stage={stage}; pending waits={pending}", completed.elapsed().as_secs_f32());
                    dump_waits();
                    reported = true;
                }
                // Recover a redraw that was consumed without delivering a frame.
                ctx.request_repaint();
                progress = shared.progress.lock().unwrap();
            }
        }).expect("frame watchdog");
        Self { state }
    }
    pub fn stage(&self, stage: &'static str, pending: &'static str) {
        let mut p = self.state.progress.lock().unwrap();
        p.stage = stage;
        p.pending = pending;
    }
    pub fn begin(&self, active: bool) {
        let mut p = self.state.progress.lock().unwrap();
        if p.active != active {
            p.active = active;
            if active {
                p.completed = Instant::now();
            }
            self.state.changed.notify_one();
        }
        p.stage = "previous frame";
        p.pending = "asset polling";
    }
    /// The compositor isn't drawing the window (app.rs hidden_tick): no
    /// frames are expected until it shows again, when `begin` re-arms this.
    pub fn hidden(&self) {
        let mut p = self.state.progress.lock().unwrap();
        if p.active {
            p.active = false;
            self.state.changed.notify_one();
        }
    }
    pub fn complete(&self) {
        let mut p = self.state.progress.lock().unwrap();
        p.completed = Instant::now();
        p.stage = "UI frame";
        p.pending = "eframe GPU submission/presentation or next redraw event";
    }
}
/// Linux exposes wait channels without ptrace, even when attaching is restricted.
fn dump_waits() {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return;
    };
    for task in tasks.flatten() {
        let path = task.path();
        let name = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
        let wait = std::fs::read_to_string(path.join("wchan")).unwrap_or_default();
        eprintln!(
            "rustudio pending wait: tid={} thread={} wchan={}",
            task.file_name().to_string_lossy(),
            name.trim(),
            wait.trim()
        );
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.state.progress.lock().unwrap().stop = true;
        self.state.changed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_stall_only_when_frames_are_expected() {
        let now = Instant::now();
        let mut p = Progress {
            completed: now,
            stage: "UI frame",
            pending: "",
            active: true,
            stop: false,
        };
        assert!(!p.stalled(now + Duration::from_secs(5)));
        assert!(p.stalled(now + Duration::from_secs(6)));
        p.active = false;
        assert!(!p.stalled(now + Duration::from_secs(60)));
        p.active = true;
        p.completed = now + Duration::from_secs(60);
        assert!(!p.stalled(now + Duration::from_secs(60)));
    }
}
