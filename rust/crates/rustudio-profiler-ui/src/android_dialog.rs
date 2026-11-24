//! File dialogs on Android.
//!
//! rfd has no Android backend, and the system picker (Storage Access
//! Framework) hands out `content://` URIs, not paths, which the rest of
//! rustudio (project folders, file watching, plain `std::fs`) can't use. So
//! this is a small in-app file browser, an egui window, with the same
//! builder surface the app calls on `rfd::AsyncFileDialog` (src/file_dialog.rs
//! re-exports one or the other as `AsyncFileDialog`).
//!
//! It browses real paths and therefore needs "All files access"
//! (MANAGE_EXTERNAL_STORAGE), which MainActivity asks for on first launch.
//! Without it the browser still works inside the app's own folders.

use std::path::{Path, PathBuf};

pub enum Kind {
    PickFile,
    PickFolder,
    SaveFile,
}

pub struct FileHandle(PathBuf);
impl FileHandle { pub fn path(&self) -> &Path { &self.0 } }

#[derive(Default)]
struct Selection {
    result: Option<Vec<PathBuf>>,
    waker: Option<std::task::Waker>,
}
struct AsyncRequest {
    browser: Browser,
    state: std::sync::Arc<std::sync::Mutex<Selection>>,
    multiple: bool,
    selected: Vec<PathBuf>,
    choosing_more: bool,
}
#[derive(Default)]
struct AsyncQueue {
    pending: std::collections::VecDeque<AsyncRequest>,
    ctx: Option<egui::Context>,
}
fn async_queue() -> &'static std::sync::Mutex<AsyncQueue> {
    static QUEUE: std::sync::OnceLock<std::sync::Mutex<AsyncQueue>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(Default::default)
}

/// Drives plugin file requests on the UI thread, waking their awaiting worker.
pub fn poll_async(ctx: &egui::Context) {
    let mut queue = async_queue().lock().unwrap_or_else(|e| e.into_inner());
    queue.ctx = Some(ctx.clone());
    let Some(request) = queue.pending.front_mut() else { return; };
    let mut done = false;
    if request.choosing_more {
        egui::Window::new("Selected Files").collapsible(false).show(ctx, |ui| {
            for path in &request.selected { ui.label(path.display().to_string()); }
            ui.horizontal(|ui| {
                if ui.button("Add Another File").clicked() { request.choosing_more = false; }
                if ui.button("Done").clicked() { done = true; }
                if ui.button("Cancel").clicked() { request.selected.clear(); done = true; }
            });
        });
    } else if let Some(answer) = request.browser.show(ctx) {
        match answer {
            Some(path) => {
                if !request.selected.contains(&path) { request.selected.push(path); }
                if request.multiple { request.choosing_more = true; } else { done = true; }
            }
            None => { request.selected.clear(); done = true; }
        }
    }
    if done {
        let request = queue.pending.pop_front().unwrap();
        let mut state = request.state.lock().unwrap_or_else(|e| e.into_inner());
        state.result = Some(request.selected);
        if let Some(waker) = state.waker.take() { waker.wake(); }
        ctx.request_repaint();
    }
}

/// Shared storage root.
pub const STORAGE_ROOT: &str = "/storage/emulated/0";

/// The folder rustudio keeps places in by default (created on demand).
pub fn workspace_dir() -> PathBuf {
    let p = Path::new(STORAGE_ROOT).join("RuStudio");
    let _ = std::fs::create_dir_all(&p);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, sync::{Arc, atomic::{AtomicBool, Ordering}}, task::{Context, Poll, Wake}};

    #[test]
    fn async_file_requests_wait_resume_and_preserve_multiple_paths() {
        struct Signal(AtomicBool);
        impl Wake for Signal { fn wake(self: Arc<Self>) { self.0.store(true, Ordering::SeqCst); } }
        let signal = Arc::new(Signal(AtomicBool::new(false)));
        let waker = std::task::Waker::from(signal.clone());
        let mut cx = Context::from_waker(&waker);
        let mut future = Box::pin(AsyncFileDialog::new().set_directory(std::env::temp_dir()).pick_files());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        let request = async_queue().lock().unwrap().pending.pop_front().unwrap();
        assert!(request.multiple);
        {
            let mut state = request.state.lock().unwrap();
            state.result = Some(vec![PathBuf::from("one.rbxm"), PathBuf::from("two.rbxm")]);
            state.waker.take().unwrap().wake();
        }
        assert!(signal.0.load(Ordering::SeqCst));
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(Some(files)) => assert_eq!(files.iter().map(|f| f.path()).collect::<Vec<_>>(), vec![Path::new("one.rbxm"), Path::new("two.rbxm")]),
            _ => panic!("file selection did not resume"),
        }
        let mut cancelled = Box::pin(AsyncFileDialog::new().set_directory(std::env::temp_dir()).save_file());
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        let request = async_queue().lock().unwrap().pending.pop_front().unwrap();
        request.state.lock().unwrap().result = Some(Vec::new());
        assert!(matches!(cancelled.as_mut().poll(&mut cx), Poll::Ready(None)));
    }

    #[test]
    fn file_filters_match_extensions_case_insensitively() {
        let browser = Browser::new(AsyncFileDialog::new().set_directory(std::env::temp_dir()).add_filter("Models", &["RBXM", "rbxmx"]), Kind::PickFile);
        assert!(browser.matches("example.RBXM"));
        assert!(browser.matches("example.rbxmx"));
        assert!(!browser.matches("example.txt"));
    }
}

/// rfd::AsyncFileDialog's builder, minus what rustudio doesn't call.
#[derive(Clone, Default)]
pub struct AsyncFileDialog {
    title: Option<String>,
    filters: Vec<(String, Vec<String>)>,
    file_name: String,
    directory: Option<PathBuf>,
}

impl AsyncFileDialog {
    pub fn set_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub async fn pick_file(self) -> Option<FileHandle> {
        self.select(Kind::PickFile, false).await.into_iter().next().map(FileHandle)
    }
    pub async fn pick_folder(self) -> Option<FileHandle> {
        self.select(Kind::PickFolder, false).await.into_iter().next().map(FileHandle)
    }
    pub async fn save_file(self) -> Option<FileHandle> {
        self.select(Kind::SaveFile, false).await.into_iter().next().map(FileHandle)
    }
    pub async fn pick_files(self) -> Option<Vec<FileHandle>> {
        let paths = self.select(Kind::PickFile, true).await;
        (!paths.is_empty()).then(|| paths.into_iter().map(FileHandle).collect())
    }
    async fn select(self, kind: Kind, multiple: bool) -> Vec<PathBuf> {
        let state = std::sync::Arc::new(std::sync::Mutex::new(Selection::default()));
        let request = AsyncRequest {
            browser: Browser::new(self, kind),
            state: state.clone(), multiple, selected: Vec::new(), choosing_more: false,
        };
        {
            let mut queue = async_queue().lock().unwrap_or_else(|e| e.into_inner());
            queue.pending.push_back(request);
            if let Some(ctx) = &queue.ctx { ctx.request_repaint(); }
        }
        std::future::poll_fn(move |cx| {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(paths) = state.result.take() { std::task::Poll::Ready(paths) }
            else { state.waker = Some(cx.waker().clone()); std::task::Poll::Pending }
        }).await
    }
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_filter(mut self, name: impl Into<String>, extensions: &[impl ToString]) -> Self {
        self.filters
            .push((name.into(), extensions.iter().map(|e| e.to_string().to_lowercase()).collect()));
        self
    }

    pub fn set_file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = name.into();
        self
    }

    pub fn set_directory<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.directory = Some(path.as_ref().to_path_buf());
        self
    }
}

struct Entry {
    name: String,
    is_dir: bool,
}

enum Act {
    Go(PathBuf),
    Choose(PathBuf),
    Cancel,
}

pub struct Browser {
    kind: Kind,
    dialog: AsyncFileDialog,
    dir: PathBuf,
    shown_dir: Option<PathBuf>,
    entries: Vec<Entry>,
    status: String,
    name: String,
    /// The path a Save would overwrite, until the person presses Save again.
    confirm_overwrite: Option<PathBuf>,
}

impl Browser {
    pub fn new(dialog: AsyncFileDialog, kind: Kind) -> Self {
        let dir = dialog
            .directory
            .clone()
            .filter(|d| d.is_dir())
            .unwrap_or_else(workspace_dir);
        let dir = if dir.is_dir() { dir } else { rustudio_core::paths::data() };
        Self {
            name: dialog.file_name.clone(),
            kind,
            dialog,
            dir,
            shown_dir: None,
            entries: Vec::new(),
            status: String::new(),
            confirm_overwrite: None,
        }
    }

    fn matches(&self, name: &str) -> bool {
        if self.dialog.filters.is_empty() {
            return true;
        }
        let ext = Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        self.dialog.filters.iter().any(|(_, exts)| exts.iter().any(|e| *e == ext))
    }

    fn refresh(&mut self) {
        self.entries.clear();
        self.status.clear();
        match std::fs::read_dir(&self.dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.starts_with('.') {
                        continue;
                    }
                    let is_dir = e.path().is_dir();
                    if !is_dir && (matches!(self.kind, Kind::PickFolder) || !self.matches(&name)) {
                        continue;
                    }
                    self.entries.push(Entry { name, is_dir });
                }
                self.entries.sort_by(|a, b| {
                    b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            Err(e) => {
                self.status = format!("Can't read {}: {e}", self.dir.display());
                if self.dir.starts_with(STORAGE_ROOT) {
                    self.status.push_str(
                        "\nGive rustudio \"All files access\" in Android's Settings > Apps > rustudio > Permissions, \
                         or use the App files shortcut.",
                    );
                }
            }
        }
        self.shown_dir = Some(self.dir.clone());
    }

    fn save_target(&self) -> Option<PathBuf> {
        let mut name = self.name.trim().to_string();
        if name.is_empty() || name.contains('/') {
            return None;
        }
        if Path::new(&name).extension().is_none() {
            if let Some(ext) = self.dialog.filters.first().and_then(|(_, e)| e.first()) {
                name = format!("{name}.{ext}");
            }
        }
        Some(self.dir.join(name))
    }

    fn places(&self) -> Vec<(String, PathBuf)> {
        let mut v = vec![("RuStudio".to_string(), workspace_dir()), ("Storage".to_string(), PathBuf::from(STORAGE_ROOT))];
        for d in ["Download", "Documents"] {
            let p = Path::new(STORAGE_ROOT).join(d);
            if p.is_dir() {
                v.push((d.to_string(), p));
            }
        }
        v.push(("App files".to_string(), rustudio_core::paths::data()));
        if let Some(ext) = std::env::var_os("RUSTUDIO_EXTERNAL_FILES") {
            v.push(("App (external)".to_string(), PathBuf::from(ext)));
        }
        v
    }

    /// Draws the browser; `Some(answer)` once it closes (`Some(None)`: cancelled).
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Option<PathBuf>> {
        if self.shown_dir.as_ref() != Some(&self.dir) {
            self.refresh();
        }
        let screen = ctx.content_rect();
        let size = egui::vec2((screen.width() * 0.94).min(900.0), (screen.height() * 0.94).min(640.0));
        let title = self.dialog.title.as_deref().unwrap_or(match self.kind {
            Kind::PickFile => "Open file",
            Kind::PickFolder => "Choose folder",
            Kind::SaveFile => "Save as",
        });
        let mut act: Option<Act> = None;
        let places = self.places();
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size(size)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (label, path) in &places {
                        if ui.add(egui::Button::new(label.as_str()).min_size(egui::vec2(0.0, 34.0))).clicked() {
                            act = Some(Act::Go(path.clone()));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let up = self.dir.parent().map(Path::to_path_buf);
                    if ui.add_enabled(up.is_some(), egui::Button::new("  Up  ").min_size(egui::vec2(0.0, 34.0))).clicked() {
                        act = up.map(Act::Go);
                    }
                    ui.label(self.dir.display().to_string());
                });
                ui.separator();
                if !self.status.is_empty() {
                    ui.colored_label(rustudio_engine::theme::WARN, &self.status);
                }
                // Room left for the title bar, the shortcut row, the path row and the footer.
                let list_h = (size.y - 210.0).max(100.0);
                egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                    for e in &self.entries {
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::click());
                        if resp.hovered() || resp.is_pointer_button_down_on() {
                            ui.painter().rect_filled(
                                rect,
                                egui::CornerRadius::same(3),
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        let label = if e.is_dir { format!("{}/", e.name) } else { e.name.clone() };
                        let color = if e.is_dir { rustudio_engine::theme::ACCENT } else { ui.visuals().text_color() };
                        ui.painter().text(
                            rect.left_center() + egui::vec2(10.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::FontId::proportional(16.0),
                            color,
                        );
                        if resp.clicked() {
                            let p = self.dir.join(&e.name);
                            if e.is_dir {
                                act = Some(Act::Go(p));
                            } else if matches!(self.kind, Kind::SaveFile) {
                                self.name = e.name.clone();
                            } else {
                                act = Some(Act::Choose(p));
                            }
                        }
                    }
                    if self.entries.is_empty() && self.status.is_empty() {
                        ui.label("(nothing here)");
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    match self.kind {
                        Kind::SaveFile => {
                            ui.label("Name:");
                            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(size.x * 0.5));
                            if ui.add(egui::Button::new("Save").min_size(egui::vec2(80.0, 36.0))).clicked() {
                                if let Some(p) = self.save_target() {
                                    if p.exists() && self.confirm_overwrite.as_ref() != Some(&p) {
                                        self.status = format!("{} exists: press Save again to replace it.", p.display());
                                        self.confirm_overwrite = Some(p);
                                    } else {
                                        act = Some(Act::Choose(p));
                                    }
                                } else {
                                    self.status = "Type a file name.".into();
                                }
                            }
                        }
                        Kind::PickFolder => {
                            if ui.add(egui::Button::new("Use this folder").min_size(egui::vec2(140.0, 36.0))).clicked() {
                                act = Some(Act::Choose(self.dir.clone()));
                            }
                        }
                        Kind::PickFile => {}
                    }
                    if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(80.0, 36.0))).clicked() {
                        act = Some(Act::Cancel);
                    }
                });
            });
        match act? {
            Act::Go(p) => {
                self.dir = p;
                self.confirm_overwrite = None;
                ctx.request_repaint();
                None
            }
            Act::Choose(p) => Some(Some(p)),
            Act::Cancel => Some(None),
        }
    }
}
