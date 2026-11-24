//! Native file dialogs that don't block the UI thread.
//!
//! rfd's blocking `FileDialog` stalls the event loop for as long as the
//! dialog is open, so the window stops answering the compositor's pings and
//! the desktop offers to kill rustudio ("rustudio is not responding:
//! Terminate / Wait"). Here the dialog is rfd's `AsyncFileDialog`, created
//! on the UI thread (macOS needs that) and awaited on a worker thread; the
//! app keeps drawing and picks the answer up with [`Dialogs::poll`].

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// rfd's builder on desktop; the in-app file browser's on Android.
#[cfg(not(target_os = "android"))]
pub use rfd::AsyncFileDialog;
#[cfg(target_os = "android")]
pub use crate::android_dialog::AsyncFileDialog;

use rbx_dom_weak::types::Ref;

/// What the chosen path is for.
#[derive(Clone, Debug, PartialEq)]
pub enum Purpose {
    OpenPlace,
    OpenFolder,
    SaveAs,
    InsertModel,
    /// The selection when the dialog opened.
    SaveSelection(Vec<Ref>),
    /// Plugins > Save as Local Plugin: the selection when the dialog opened.
    SaveLocalPlugin(Vec<Ref>),
    /// Save as Project Folder: the "Create in" folder.
    ProjectParent,
    /// Terrain Editor > Import: a heightmap (true) or colormap image.
    TerrainImage { height: bool },
    /// File Sync: the project to attach.
    FileSyncProject,
    /// Asset Manager > Import: one or more local files to import/upload.
    AssetManagerImport,
    /// Start Page > Open a Template > Edit: a place file to add as a template.
    TemplateAdd,
    /// … an image for one of the person's templates (by key).
    TemplateThumb(String),
    /// … another place file for one of the person's templates.
    TemplateReplace(String),
}

#[cfg(target_os = "android")]
pub use crate::android_dialog::Kind;

#[cfg(not(target_os = "android"))]
pub enum Kind {
    PickFile,
    PickFolder,
    SaveFile,
}

#[cfg(not(target_os = "android"))]
#[derive(Default)]
pub struct Dialogs {
    pending: Option<(Purpose, Receiver<Option<PathBuf>>)>,
    pending_multi: Option<(Purpose, Receiver<Vec<PathBuf>>)>,
    /// Repainted when a dialog closes (the UI may be idle meanwhile).
    pub ctx: Option<egui::Context>,
}

#[cfg(not(target_os = "android"))]
impl Dialogs {
    pub fn new(ctx: egui::Context) -> Self {
        Self { pending: None, pending_multi: None, ctx: Some(ctx) }
    }

    /// A dialog is open.
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.pending_multi.is_some()
    }

    /// Opens `dialog`; false (and nothing happens) while another is open.
    pub fn start(&mut self, purpose: Purpose, dialog: AsyncFileDialog, kind: Kind) -> bool {
        if self.busy() {
            return false;
        }
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        type Fut = std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>>;
        let fut: Fut = match kind {
            Kind::PickFile => Box::pin(dialog.pick_file()),
            Kind::PickFolder => Box::pin(dialog.pick_folder()),
            Kind::SaveFile => Box::pin(dialog.save_file()),
        };
        let spawned = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
            let path = pollster::block_on(fut).map(|h| h.path().to_path_buf());
            let _ = tx.send(path);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        if spawned.is_err() {
            return false;
        }
        self.pending = Some((purpose, rx));
        true
    }

    /// The finished dialog's purpose and path (None: cancelled).
    pub fn poll(&mut self) -> Option<(Purpose, Option<PathBuf>)> {
        let (_, rx) = self.pending.as_ref()?;
        let path = match rx.try_recv() {
            Ok(p) => p,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => None,
        };
        self.pending.take().map(|(purpose, _)| (purpose, path))
    }


}

/// Android: the dialog is an egui window drawn by `poll` each frame.
#[cfg(target_os = "android")]
#[derive(Default)]
pub struct Dialogs {
    open: Option<(Purpose, crate::android_dialog::Browser)>,
    pending_multi: Option<(Purpose, Receiver<Vec<PathBuf>>)>,
    pub ctx: Option<egui::Context>,
}

#[cfg(target_os = "android")]
impl Dialogs {
    pub fn new(ctx: egui::Context) -> Self {
        Self { open: None, pending_multi: None, ctx: Some(ctx) }
    }

    pub fn busy(&self) -> bool {
        self.open.is_some() || self.pending_multi.is_some()
    }

    pub fn start(&mut self, purpose: Purpose, dialog: AsyncFileDialog, kind: Kind) -> bool {
        if self.busy() {
            return false;
        }
        let browser = crate::android_dialog::Browser::new(dialog, kind);
        self.open = Some((purpose, browser));
        if let Some(c) = &self.ctx {
            c.request_repaint();
        }
        true
    }

    pub fn poll(&mut self) -> Option<(Purpose, Option<PathBuf>)> {
        let ctx = self.ctx.clone()?;
        let answer = self.open.as_mut()?.1.show(&ctx)?;
        self.open.take().map(|(purpose, _)| (purpose, answer))
    }
}

impl Dialogs {
    /// Opens a multi-file picker dialog.
    pub fn start_multi(&mut self, purpose: Purpose, dialog: AsyncFileDialog) -> bool {
        if self.busy() {
            return false;
        }
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new().name("file-dialog-multi".into()).spawn(move || {
            let paths = pollster::block_on(dialog.pick_files())
                .map(|hs| hs.into_iter().map(|h| h.path().to_path_buf()).collect())
                .unwrap_or_default();
            let _ = tx.send(paths);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        if spawned.is_err() {
            return false;
        }
        self.pending_multi = Some((purpose, rx));
        true
    }

    /// The finished multi-file dialog's purpose and chosen paths (empty: cancelled).
    pub fn poll_multi(&mut self) -> Option<(Purpose, Vec<PathBuf>)> {
        let (_, rx) = self.pending_multi.as_ref()?;
        let paths = match rx.try_recv() {
            Ok(p) => p,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Vec::new(),
        };
        self.pending_multi.take().map(|(purpose, _)| (purpose, paths))
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn poll_hands_back_purpose_and_path_once() {
        let mut d = Dialogs::default();
        let (tx, rx) = channel();
        d.pending = Some((Purpose::SaveAs, rx));
        assert!(d.busy() && d.poll().is_none(), "still open");
        tx.send(Some(PathBuf::from("/tmp/x.rbxl"))).unwrap();
        assert_eq!(d.poll(), Some((Purpose::SaveAs, Some(PathBuf::from("/tmp/x.rbxl")))));
        assert!(!d.busy() && d.poll().is_none());
        // A worker that died counts as cancelled.
        let (tx, rx) = channel::<Option<PathBuf>>();
        d.pending = Some((Purpose::OpenPlace, rx));
        drop(tx);
        assert_eq!(d.poll(), Some((Purpose::OpenPlace, None)));
    }
}
