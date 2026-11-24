//! Editor integration for the Asset Manager: per-frame tick, session sync,
//! DOM asset scanning, insertion via the Toolbox pipeline, drag-and-drop,
//! audio preview, place opening, and bulk import file dialog.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::app::Editor;
use crate::file_dialog::Purpose;
use crate::toolbox::insert::Target;
use rustudio_account::VersionType;

use super::ui::DrawCtx;
use super::{Action, ImportModal, Scope};

impl Editor {
    /// Per-frame tick (runs whether the Asset Manager tab is open or hidden).
    pub fn asset_manager_tick(&mut self, ctx: &egui::Context) {
        if self.asset_manager.session_checked.is_none_or(|t| t.elapsed().as_millis() > 500) {
            self.asset_manager.session_checked = Some(Instant::now());
            if let Some(c) = self.game.account.shared_client() {
                let got = c
                    .try_lock()
                    .ok()
                    .map(|g| g.as_ref().map(|c| (c.session.downloader(), c.user_id())));
                if let Some(s) = got {
                    self.asset_manager
                        .set_session(s.filter(|_| std::env::var_os("RUSTUDIO_NO_SESSION").is_none()));
                }
            } else {
                self.asset_manager.set_session(None);
            }
        }

        let polled = self.asset_manager.poll();
        let thumbs_dirty = self.asset_manager.thumbs.tick(ctx);

        // Keep audio preview state synced with Toolbox's audio player.
        self.asset_manager.playing_audio = self.toolbox.player.playing;

        // Forward any logs to the Output panel.
        for (err, line) in std::mem::take(&mut self.asset_manager.log) {
            if err {
                self.error(line);
            } else {
                self.info(line);
            }
        }

        // Scan the active document for In-Experience assets when revision changes.
        if self.ui.panels_open.contains("Asset Manager") {
            self.asset_manager.scan_document(&self.game.doc);
        }

        // Repaint while background tasks or thumbnails are loading.
        if polled || thumbs_dirty || self.asset_manager.pool.busy.load(Ordering::Relaxed) > 0 {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        self.asset_manager.save_if_changed(false);
    }

    /// Draws the Asset Manager dock panel.
    pub fn asset_manager_ui(&mut self, ui: &mut egui::Ui) {
        // Ensure the in-experience scan is fresh.
        self.asset_manager.scan_document(&self.game.doc);

        let project_name = self
            .game
            .doc
            .cloud
            .as_ref()
            .map(|c| {
                if !c.experience.is_empty() {
                    c.experience.clone()
                } else {
                    c.name.clone()
                }
            })
            .or_else(|| {
                self.game
                    .doc
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Current Place".into());
        let universe_id = self
            .game
            .doc
            .cloud
            .as_ref()
            .map(|c| c.universe_id)
            .filter(|u| *u > 0)
            .or_else(|| self.live_team_create_ids().map(|(u, _)| u));
        let place_id = self
            .game
            .doc
            .cloud
            .as_ref()
            .map(|c| c.place_id)
            .filter(|p| *p > 0)
            .or_else(|| self.live_team_create_ids().map(|(_, p)| p));

        // Register visible entries into toolbox's asset map so ToolboxDrag works
        // seamlessly with the 3D Viewport and Explorer drop targets!
        let vis = self.asset_manager.visible_entries();
        for e in &vis {
            self.toolbox
                .assets
                .entry(e.id)
                .or_insert_with(|| e.to_toolbox_asset());
        }

        let doc_ref = &self.game.doc;
        let name_fn = |r| {
            doc_ref
                .dom
                .get_by_ref(r)
                .map(|i| format!("{} ({})", i.name, i.class))
                .unwrap_or_else(|| "Instance".into())
        };
        let dctx = DrawCtx {
            project_name: &project_name,
            universe_id,
            place_id,
            instance_name: &name_fn,
        };
        self.asset_manager.show(ui, &dctx);

        // Execute any queued actions.
        let actions = std::mem::take(&mut self.asset_manager.actions);
        for action in actions {
            match action {
                Action::Insert { entry, into_selection } => {
                    self.toolbox.assets.insert(entry.id, entry.to_toolbox_asset());
                    let target = if into_selection {
                        self.game
                            .doc
                            .selection
                            .first()
                            .copied()
                            .map(Target::Parent)
                            .unwrap_or(Target::Click)
                    } else {
                        Target::Click
                    };
                    self.toolbox_insert(entry.id, target);
                }
                Action::OpenPlace(pid) => {
                    self.asset_manager_open_place_window(pid);
                }
                Action::SelectInstances(refs) => {
                    let valid: Vec<_> = refs
                        .into_iter()
                        .filter(|r| self.game.doc.dom.get_by_ref(*r).is_some())
                        .collect();
                    if !valid.is_empty() {
                        self.select(valid);
                    }
                }
                Action::CopyText(text, msg) => {
                    ui.ctx().copy_text(text);
                    self.info(msg);
                }
                Action::OpenImportDialog => {
                    self.asset_manager_open_import();
                }
                Action::SaveToRoblox => {
                    self.open_roblox_destination(VersionType::Saved);
                }
                Action::OpenAccount => {
                    self.game.account.open = true;
                }
                Action::PreviewAudio(id) => {
                    self.toolbox.play(id);
                }
                Action::StopAudio => {
                    if let Some(a) = self.toolbox.player.audio.as_mut() {
                        a.stop_all();
                    }
                    self.toolbox.player.playing = None;
                }
                Action::OpenUrl(url) => {
                    crate::toolbox::ui::open_url(&url);
                }
                Action::ImportLocalModel(path) => {
                    self.insert_model_file(path);
                }
            }
        }
    }

    /// Opens the multi-file native dialog for Asset Manager Bulk Import.
    /// Opens a place of the experience in a new rustudio window, as Studio's
    /// Asset Manager does: the place being edited stays open here. The
    /// place already open here is not opened twice.
    fn asset_manager_open_place_window(&mut self, place: u64) {
        let current = self.live_team_create_place()
            .or_else(|| self.game.doc.cloud.as_ref().map(|o| o.place_id));
        if current == Some(place) {
            self.info(format!("Place {place} is already open in this window"));
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                self.error(format!("Can't open place {place} in a new window: {e}"));
                return;
            }
        };
        match std::process::Command::new(exe)
            .args(["--open-roblox", &place.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => self.info(format!("Opening place {place} in a new window")),
            Err(e) => self.error(format!("Can't open place {place} in a new window: {e}")),
        }
    }

    pub fn asset_manager_open_import(&mut self) {
        let d = crate::file_dialog::AsyncFileDialog::new()
            .set_title("Import Assets")
            .add_filter(
                "All Supported Assets",
                &["png", "jpg", "jpeg", "bmp", "tga", "mp3", "ogg", "wav", "flac", "rbxm", "rbxmx", "fbx", "obj", "gltf", "glb"],
            )
            .add_filter("Images", &["png", "jpg", "jpeg", "bmp", "tga"])
            .add_filter("Audio", &["mp3", "ogg", "wav", "flac"])
            .add_filter("Models & Meshes", &["rbxm", "rbxmx", "fbx", "obj", "gltf", "glb"]);
        self.dialogs.start_multi(Purpose::AssetManagerImport, d);
    }

    /// Called by `poll_file_dialogs` when the multi-file import dialog finishes.
    pub fn asset_manager_import_files(&mut self, paths: Vec<PathBuf>) {
        let items: Vec<_> = paths
            .iter()
            .filter_map(|p| super::api::classify_import_file(p))
            .collect();
        if items.is_empty() {
            if !paths.is_empty() {
                self.error("None of the selected files are supported Roblox asset formats.");
            }
            return;
        }
        let target_group = match self.asset_manager.persist.scope {
            Scope::Group { group_id, .. } => Some(group_id),
            _ => None,
        };
        let target_folder = self.asset_manager.persist.scope.folder_id().map(str::to_string);
        self.asset_manager.import_modal = Some(ImportModal {
            items,
            target_group,
            target_folder,
            uploading: false,
        });
    }
}
