//! Saved layout: the dock (where Explorer / Properties / Output / … sit and
//! their sizes), panels popped out into their own windows (which tabs,
//! each window's size, and its position where the platform reports one),
//! where each panel was last docked, the window's size, position and
//! maximized state, UI zoom, ribbon tab, run type, snapping and grid.
//! Studio keeps these between sessions; so do we, in
//! `$XDG_CONFIG_HOME/rustudio/layout.json`.
//!
//! Only plain interactive runs read or write it (`Harness::plain`, no
//! `--window`): tests, benches and screenshots keep the default layout.
//! `RUSTUDIO_NO_LAYOUT=1` ignores it too; `RUSTUDIO_LAYOUT=1` forces it on. Script tabs aren't kept (they
//! name instances of the place that was open).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_dock::DockState;
use serde::{Deserialize, Serialize};

use crate::app::{Editor, Harness, Tab};
use crate::panels::{Home, Panels, Popped};
use crate::studio_ui::{RibbonTab, RunType};

const VERSION: u32 = 1;
/// How often a changed layout is written while running (and on exit).
const SAVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedTab {
    Viewport,
    Explorer,
    Properties,
    Output,
    AvatarSettings,
    TerrainEditor,
}

impl SavedTab {
    pub(crate) fn from_tab(t: &Tab) -> Option<Self> {
        Some(match t {
            Tab::Viewport => SavedTab::Viewport,
            Tab::Explorer => SavedTab::Explorer,
            Tab::Properties => SavedTab::Properties,
            Tab::Output => SavedTab::Output,
            Tab::AvatarSettings => SavedTab::AvatarSettings,
            Tab::TerrainEditor => SavedTab::TerrainEditor,
            Tab::Script(_) => return None,
        })
    }
    pub(crate) fn tab(self) -> Tab {
        match self {
            SavedTab::Viewport => Tab::Viewport,
            SavedTab::Explorer => Tab::Explorer,
            SavedTab::Properties => Tab::Properties,
            SavedTab::Output => Tab::Output,
            SavedTab::AvatarSettings => Tab::AvatarSettings,
            SavedTab::TerrainEditor => Tab::TerrainEditor,
        }
    }
}

/// A panel window (src/panels.rs): its key (app id `rustudio-panel-<key>`),
/// its tabs, inner size and outer position in logical pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPopped {
    pub key: String,
    pub dock: DockState<SavedTab>,
    pub size: [f32; 2],
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
    /// Hyprland's placement of it (floating, position, size, workspace).
    #[serde(default)]
    pub wm: Option<crate::hypr::Placement>,
}

/// The window, in logical pixels (what `ViewportBuilder` takes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeom {
    pub size: [f32; 2],
    /// Outer top-left; X11 only (Wayland never tells a window where it is).
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub version: u32,
    pub window: Option<WindowGeom>,
    pub dock: Option<DockState<SavedTab>>,
    /// Panels popped out into their own windows.
    pub popped: Vec<SavedPopped>,
    /// Where each panel sat last in the main window.
    pub homes: HashMap<String, Home>,
    pub zoom: Option<f32>,
    pub ribbon_tab: Option<String>,
    pub run_type: Option<String>,
    pub show_grid: Option<bool>,
    pub grid_size: Option<f32>,
    pub show_gui: Option<bool>,
    pub snap_move: Option<bool>,
    pub move_inc: Option<f32>,
    pub snap_rotate: Option<bool>,
    pub rotate_inc: Option<f32>,
    pub local_space: Option<bool>,
    /// The dragger settings menu (Geometric ▾) and Align Dragged Objects.
    #[serde(default)]
    pub dragger: Option<DraggerSettings>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct DraggerSettings {
    pub physical: bool,
    pub collisions: bool,
    pub join_surfaces: bool,
    pub use_bounding_boxes: bool,
    pub part_snap: bool,
    pub align_dragged: bool,
}

fn path() -> PathBuf {
    let base = crate::paths::config();
    base.join("rustudio").join("layout.json")
}

impl Harness {
    /// A run a person drives (not a test, bench, screenshot or scripted run).
    pub fn plain(&self) -> bool {
        self.selftest.is_none()
            && !crate::selftest::running()
            && self.bench.is_none()
            && self.screenshot.is_none()
            && self.ui_shots.is_none()
            && self.actions.is_empty()
            && self.shots.is_empty()
            && !self.start_play
            && self.open_roblox.is_none()
            && self.roblox_picker.is_none()
            && !self.roblox_settings
    }
}

impl Harness {
    /// The in-game settings (src/game_settings.rs) are the user's: kept for
    /// any run a person plays in (`--play` / opening a Roblox place too),
    /// never for tests, benches, screenshots or scripted input.
    pub fn keeps_game_settings(&self) -> bool {
        self.selftest.is_none()
            && self.bench.is_none()
            && self.screenshot.is_none()
            && self.ui_shots.is_none()
            && self.actions.is_empty()
            && self.shots.is_empty()
            && std::env::var_os("RUSTUDIO_NO_LAYOUT").is_none()
    }
}

/// Whether this run keeps its layout (see the module doc).
pub fn enabled(h: &Harness) -> bool {
    // RUSTUDIO_LAYOUT=1 forces it (checking a saved layout with --screenshot).
    if std::env::var_os("RUSTUDIO_LAYOUT").is_some() {
        return true;
    }
    let window_arg = std::env::args().any(|a| a == "--window");
    h.plain() && !window_arg && std::env::var_os("RUSTUDIO_NO_LAYOUT").is_none()
}

pub fn load() -> Option<Layout> {
    let text = std::fs::read_to_string(path()).ok()?;
    match serde_json::from_str::<Layout>(&text) {
        Ok(l) if l.version == VERSION => Some(l),
        Ok(_) => None,
        Err(e) => {
            eprintln!("rustudio: ignoring {}: {e}", path().display());
            None
        }
    }
}

fn finite(v: f32) -> Option<f32> {
    v.is_finite().then_some(v)
}

impl Layout {
    /// The saved dock, if it still has the 3D view (else the default is used).
    pub fn dock(&self) -> Option<DockState<Tab>> {
        let d = self.dock.as_ref()?;
        let dock = d.map_tabs(|t| t.tab());
        dock.find_tab(&Tab::Viewport).is_some().then_some(dock)
    }

    /// The saved panel windows and homes. Tabs also in `main` (or in an
    /// earlier window) are dropped, and windows left empty.
    pub fn panels(&self, main: &DockState<Tab>) -> Panels {
        let mut out = Panels { popped: Vec::new(), homes: self.homes.clone() };
        for w in &self.popped {
            let mut dock = w.dock.map_tabs(|t| t.tab());
            dock.retain_tabs(|t| *t != Tab::Viewport && main.find_tab(t).is_none() && out.locate(main, t).is_none());
            if dock.iter_all_tabs().next().is_none() || w.key.is_empty() {
                continue;
            }
            let key = out.free_key(&w.key);
            let mut p = Popped::new(key, dock, w.size, w.pos);
            p.wm = w.wm.clone();
            p.wm_restore = w.wm.clone();
            out.popped.push(p);
        }
        out
    }

    /// Puts the saved settings on the editor.
    pub fn apply(&self, ed: &mut Editor) {
        if let Some(z) = self.zoom.and_then(finite) {
            ed.ui.zoom = z.clamp(0.5, 3.0);
        }
        if let Some(t) = &self.ribbon_tab {
            if let Some(t) = RibbonTab::ALL.into_iter().find(|r| format!("{r:?}") == *t) {
                ed.ui.tab = t;
            }
        }
        if let Some(t) = &self.run_type {
            if let Some(t) = RunType::ALL.into_iter().find(|r| format!("{r:?}") == *t) {
                ed.ui.run_type = t;
            }
        }
        if let Some(v) = self.show_grid { ed.show_grid = v; }
        if let Some(v) = self.grid_size.and_then(finite).filter(|v| *v > 0.0) { ed.ui.grid_size = v; }
        if let Some(v) = self.show_gui { ed.ui.show_gui = v; }
        if let Some(v) = self.snap_move { ed.snap_move = v; }
        if let Some(v) = self.move_inc.and_then(finite).filter(|v| *v > 0.0) { ed.move_inc = v; }
        if let Some(v) = self.snap_rotate { ed.snap_rotate = v; }
        if let Some(v) = self.rotate_inc.and_then(finite).filter(|v| *v > 0.0) { ed.rotate_inc = v; }
        if let Some(v) = self.local_space { ed.local_space = v; }
        if let Some(d) = self.dragger {
            ed.physical_drag = d.physical;
            ed.collisions = d.collisions;
            ed.join_surfaces = d.join_surfaces;
            ed.use_bounding_boxes = d.use_bounding_boxes;
            ed.part_snap = d.part_snap;
            ed.align_dragged = d.align_dragged;
        }
    }

    /// Size, position and maximized state for the window about to open.
    pub fn window_builder(&self, b: egui::ViewportBuilder) -> egui::ViewportBuilder {
        let Some(w) = self.window else { return b };
        let mut b = b;
        if w.size.iter().all(|v| v.is_finite() && *v >= 200.0) {
            b = b.with_inner_size(w.size);
        }
        if let Some(p) = w.pos.filter(|p| p.iter().all(|v| v.is_finite())) {
            b = b.with_position(p);
        }
        if w.maximized {
            b = b.with_maximized(true);
        }
        b
    }
}

/// A dock as saved: script tabs dropped, and the rects of groups not drawn
/// yet (a panel just docked: Rect::NOTHING, infinite) zeroed, since JSON
/// writes infinity as null and the file then wouldn't load.
pub fn saved_dock(dock: &DockState<Tab>) -> DockState<SavedTab> {
    let mut d = dock.filter_map_tabs(SavedTab::from_tab);
    let fix = |r: &mut egui::Rect| {
        if !r.is_finite() {
            *r = egui::Rect::ZERO;
        }
    };
    for (_, node) in d.iter_all_nodes_mut() {
        match node {
            egui_dock::Node::Leaf(l) => {
                fix(&mut l.rect);
                fix(&mut l.viewport);
                if !l.scroll.is_finite() {
                    l.scroll = 0.0;
                }
            }
            egui_dock::Node::Vertical(s) | egui_dock::Node::Horizontal(s) => {
                fix(&mut s.rect);
                if !s.fraction.is_finite() {
                    s.fraction = 0.5;
                }
            }
            egui_dock::Node::Empty => {}
        }
    }
    d
}

/// The panel windows as saved (script tabs aren't kept; a window of only
/// scripts isn't either).
pub fn saved_popped(panels: &Panels) -> Vec<SavedPopped> {
    panels
        .popped
        .iter()
        .filter_map(|w| {
            let dock = saved_dock(&w.dock);
            dock.iter_all_tabs().next()?;
            Some(SavedPopped { key: w.key.clone(), dock, size: w.size, pos: w.pos, wm: w.wm.clone() })
        })
        .collect()
}

/// Writes the layout when it changes (every couple of seconds) and on exit.
pub struct Saver {
    /// The JSON last written (or read), to skip unchanged writes.
    last: Option<String>,
    /// When the file was last written.
    written: Instant,
    /// The last un-maximized, windowed geometry (what to restore to).
    normal: Option<WindowGeom>,
}

impl Saver {
    pub fn new(loaded: Option<&Layout>) -> Self {
        Saver {
            last: loaded.and_then(|l| serde_json::to_string_pretty(l).ok()),
            written: Instant::now() - SAVE_EVERY,
            normal: loaded.and_then(|l| l.window),
        }
    }

    /// Call every frame. A change is written at most every SAVE_EVERY; one
    /// that has to wait asks for a frame then (egui only redraws on input).
    pub fn tick(&mut self, ctx: &egui::Context, dock: &DockState<Tab>, panels: &Panels, ed: &Editor) {
        self.track_window(ctx);
        let text = self.text(dock, panels, ed);
        if self.last.as_deref() == Some(text.as_str()) {
            return;
        }
        let since = self.written.elapsed();
        if since < SAVE_EVERY {
            ctx.request_repaint_after(SAVE_EVERY - since);
            return;
        }
        self.write(text);
    }

    fn track_window(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        let (inner, outer, maximized, fullscreen, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.outer_rect,
                v.maximized.unwrap_or(false),
                v.fullscreen.unwrap_or(false),
                v.minimized.unwrap_or(false),
            )
        });
        if fullscreen || minimized {
            return;
        }
        let Some(inner) = inner else { return };
        let prev = self.normal;
        if maximized {
            // Keep the size to come back to; just note it's maximized.
            let mut g = prev.unwrap_or(WindowGeom { size: [1600.0, 900.0], pos: None, maximized: true });
            g.maximized = true;
            self.normal = Some(g);
            return;
        }
        let size = inner.size() * zoom;
        let pos = outer.map(|o| [o.min.x * zoom, o.min.y * zoom]);
        self.normal = Some(WindowGeom { size: [size.x.round(), size.y.round()], pos, maximized: false });
    }

    /// Writes now if anything changed (on exit).
    pub fn save(&mut self, dock: &DockState<Tab>, panels: &Panels, ed: &Editor) {
        let text = self.text(dock, panels, ed);
        if self.last.as_deref() != Some(text.as_str()) {
            self.write(text);
        }
    }

    fn text(&self, dock: &DockState<Tab>, panels: &Panels, ed: &Editor) -> String {
        let layout = Layout {
            version: VERSION,
            window: self.normal,
            dock: Some(saved_dock(dock)),
            popped: saved_popped(panels),
            homes: panels.homes.clone(),
            zoom: Some(ed.ui.zoom),
            ribbon_tab: Some(format!("{:?}", ed.ui.tab)),
            run_type: Some(format!("{:?}", ed.ui.run_type)),
            show_grid: Some(ed.show_grid),
            grid_size: Some(ed.ui.grid_size),
            show_gui: Some(ed.ui.show_gui),
            snap_move: Some(ed.snap_move),
            move_inc: Some(ed.move_inc),
            snap_rotate: Some(ed.snap_rotate),
            rotate_inc: Some(ed.rotate_inc),
            local_space: Some(ed.local_space),
            dragger: Some(DraggerSettings {
                physical: ed.physical_drag,
                collisions: ed.collisions,
                join_surfaces: ed.join_surfaces,
                use_bounding_boxes: ed.use_bounding_boxes,
                part_snap: ed.part_snap,
                align_dragged: ed.align_dragged,
            }),
        };
        serde_json::to_string_pretty(&layout).unwrap_or_default()
    }

    fn write(&mut self, text: String) {
        self.written = Instant::now();
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
            self.last = Some(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::default_dock;
    use crate::panels::DockMove;

    /// Popped-out panels survive a save and a restart: tabs, key (app id),
    /// size and position; script tabs aren't kept.
    #[test]
    fn popped_windows_round_trip() {
        let mut dock = default_dock();
        let mut panels = Panels::default();
        crate::panels::record_homes(&dock, &mut panels.homes);
        panels.apply(&mut dock, DockMove::PopOut(Tab::Output), [640.0, 360.0], Some([2000.0, 100.0]));
        panels.apply(&mut dock, DockMove::PopOut(Tab::Properties), [300.0, 700.0], None);
        panels.popped[0].dock.push_to_first_leaf(Tab::TerrainEditor);
        panels.popped[0].dock.push_to_first_leaf(Tab::Script(rbx_dom_weak::types::Ref::new()));
        panels.pop_out(Tab::Script(rbx_dom_weak::types::Ref::new()), [400.0, 400.0], None);

        let layout = Layout {
            version: VERSION,
            dock: Some(saved_dock(&dock)),
            popped: saved_popped(&panels),
            homes: panels.homes.clone(),
            ..Default::default()
        };
        let text = serde_json::to_string_pretty(&layout).unwrap();
        let back: Layout = serde_json::from_str(&text).unwrap();
        let main = back.dock().expect("main dock");
        assert!(main.find_tab(&Tab::Output).is_none());
        assert!(main.find_tab(&Tab::Explorer).is_some());
        let restored = back.panels(&main);
        assert_eq!(restored.popped.len(), 2, "the scripts-only window isn't kept");
        let out = &restored.popped[0];
        assert_eq!(out.key, "output");
        assert_eq!(out.app_id(), "rustudio-panel-output");
        assert_eq!(out.tabs(), vec![Tab::Output, Tab::TerrainEditor]);
        assert_eq!(out.size, [640.0, 360.0]);
        assert_eq!(out.pos, Some([2000.0, 100.0]));
        let b = out.builder();
        assert_eq!(b.app_id.as_deref(), Some("rustudio-panel-output"));
        assert_eq!(b.position, Some(egui::pos2(2000.0, 100.0)));
        assert_eq!(b.inner_size, Some(egui::vec2(640.0, 360.0)));
        let props = &restored.popped[1];
        assert_eq!((props.key.as_str(), props.tabs(), props.size, props.pos), ("properties", vec![Tab::Properties], [300.0, 700.0], None));
        assert_eq!(restored.homes, panels.homes);
        // Docking back after the restart: Properties under the Explorer again.
        let mut main = main;
        let mut restored = restored;
        restored.apply(&mut main, DockMove::DockBack(Tab::Properties), [1.0, 1.0], None);
        let t = main.main_surface();
        assert_eq!(t.find_tab(&Tab::Explorer).unwrap().0.parent(), t.find_tab(&Tab::Properties).unwrap().0.parent());
    }

    /// A tab saved both docked and popped stays docked; old layouts
    /// without the new fields still load.
    #[test]
    fn restore_dedupes_and_old_files_load() {
        let main = default_dock();
        let layout = Layout {
            version: VERSION,
            popped: vec![SavedPopped {
                key: "output".into(),
                dock: DockState::new(vec![SavedTab::Output]),
                size: [1.0, f32::NAN],
                pos: None,
                wm: None,
            }],
            ..Default::default()
        };
        assert!(layout.panels(&main).popped.is_empty());
        let old: Layout = serde_json::from_str(r#"{"version":1,"zoom":1.25}"#).unwrap();
        assert!(old.popped.is_empty() && old.homes.is_empty());
    }
}
