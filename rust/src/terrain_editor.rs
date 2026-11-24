//! The Terrain Editor: Studio's Home > Terrain panel, rebuilt from the
//! built-in TerrainEditor plugin (BuiltInPlugins/TerrainEditor.rbxm, read
//! as bytecode: Src/Components/*, Src/Tools/*, Src/Schemas/Settings/*,
//! Src/Util/Operations/*; layout and defaults checked against the offline Studio).
//!
//! Two tabs, as Studio's: Create (Import, Generate, Clear) and Edit
//! (Select, Transform, Fill, Sea Level, Draw, Sculpt, Smooth, Paint,
//! Flatten), tool tiles four across, then the picked tool's "… Settings"
//! sections. The brush tools work in the 3D view while the left button is
//! held (terrain_brush.rs has the maths); Select / Transform / Fill /
//! Generate / Import share one box selection with handles, Sea Level has
//! its own box. Every stroke or button press is one undo step.

use egui::{Color32, Pos2, Rect, Stroke};
use glam::Vec3;
use rbx_dom_weak::types::{BinaryString, Ref, Variant};
use rbx_dom_weak::ustr;

use crate::app::Editor;
use rustudio_core::terrain::{self, Voxels, VOXEL, WATER};
use crate::terrain_brush::{self as brush, BrushOp, FlattenMode, Pivot, PlaneLock, Shape};
use crate::terrain_gen::{self as gen, Biome};
use rustudio_core::terrain_region::{self as region, Region, Rot};
use crate::terrain_ui::{self as tui, Seg, TileIcon};
use rustudio_engine::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Import,
    Generate,
    Clear,
    Select,
    Transform,
    Fill,
    SeaLevel,
    Draw,
    Sculpt,
    Smooth,
    Paint,
    Flatten,
}

impl Tool {
    pub const CREATE: [Tool; 3] = [Tool::Import, Tool::Generate, Tool::Clear];
    pub const EDIT: [Tool; 9] = [
        Tool::Select,
        Tool::Transform,
        Tool::Fill,
        Tool::SeaLevel,
        Tool::Draw,
        Tool::Sculpt,
        Tool::Smooth,
        Tool::Paint,
        Tool::Flatten,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Import => "Import",
            Tool::Generate => "Generate",
            Tool::Clear => "Clear",
            Tool::Select => "Select",
            Tool::Transform => "Transform",
            Tool::Fill => "Fill",
            Tool::SeaLevel => "Sea Level",
            Tool::Draw => "Draw",
            Tool::Sculpt => "Sculpt",
            Tool::Smooth => "Smooth",
            Tool::Paint => "Paint",
            Tool::Flatten => "Flatten",
        }
    }

    /// Studio's tile icons are white line art with one coloured mark;
    /// these are Tabler glyphs in the same spirit (no Roblox art shipped).
    fn icon(self) -> TileIcon {
        let green = Color32::from_rgb(52, 214, 120);
        let (icon, mark) = match self {
            Tool::Import => ("mountain", Some(("arrow-narrow-left", Color32::WHITE))),
            Tool::Generate => ("mountain", Some(("arrow-narrow-up", green))),
            Tool::Clear => ("mountain", Some(("eraser", Color32::from_rgb(240, 96, 110)))),
            Tool::Select => ("pointer-filled", None),
            Tool::Transform => ("vector-bezier", None),
            Tool::Fill => ("bucket-droplet", None),
            Tool::SeaLevel => ("ripple", None),
            Tool::Draw => ("mountain", Some(("plus", green))),
            Tool::Sculpt => ("arrow-big-up-lines", Some(("", green))),
            Tool::Smooth => ("hemisphere", Some(("diamond-filled", Color32::from_rgb(160, 110, 255)))),
            Tool::Paint => ("mountain", Some(("brush", green))),
            Tool::Flatten => ("box-align-bottom", Some(("mountain", Color32::from_rgb(240, 96, 110)))),
        };
        TileIcon { icon, mark: mark.filter(|m| !m.0.is_empty()) }
    }

    /// Works on the selected box (and has Copy / Paste / Delete).
    pub fn is_region(self) -> bool {
        matches!(self, Tool::Select | Tool::Transform | Tool::Fill)
    }

    /// Shows the shared selection box and its handles.
    fn uses_selection(self) -> bool {
        self.is_region() || matches!(self, Tool::Generate | Tool::Import)
    }

    fn brush_op(self) -> Option<BrushOp> {
        Some(match self {
            Tool::Draw => BrushOp::Draw,
            Tool::Sculpt => BrushOp::Sculpt,
            Tool::Smooth => BrushOp::Smooth,
            Tool::Flatten => BrushOp::Flatten,
            Tool::Paint => BrushOp::Paint,
            _ => return None,
        })
    }
}

/// A region of whole voxels: its first voxel and size in voxels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegionBox {
    pub lo: [i32; 3],
    pub size: [i32; 3],
}

impl RegionBox {
    /// The voxels a box of studs (centre, size) covers, snapped outward
    /// to the grid as Studio's getVoxelAlignedRegion does.
    pub fn from_studs(pos: [f32; 3], size: [f32; 3]) -> RegionBox {
        let lo = [0, 1, 2].map(|k| ((pos[k] - size[k].abs() * 0.5) / VOXEL + 1e-4).floor() as i32);
        let hi = [0, 1, 2].map(|k| ((pos[k] + size[k].abs() * 0.5) / VOXEL - 1e-4).ceil() as i32);
        RegionBox { lo, size: [0, 1, 2].map(|k| (hi[k] - lo[k]).max(1)) }
    }

    pub fn min(&self) -> Vec3 {
        Vec3::from(self.lo.map(|c| c as f32)) * VOXEL
    }

    pub fn max(&self) -> Vec3 {
        Vec3::from([0, 1, 2].map(|k| (self.lo[k] + self.size[k]) as f32)) * VOXEL
    }

    pub fn center(&self) -> Vec3 {
        (self.min() + self.max()) * 0.5
    }

    pub fn studs(&self) -> ([f32; 3], [f32; 3]) {
        (self.center().to_array(), (self.max() - self.min()).to_array())
    }

    /// The box turned by `r` about its centre (rounded down to a voxel
    /// when the new size and the old one differ by an odd count).
    fn rotated(&self, r: &Rot) -> RegionBox {
        let size = region::rotated_size(r, self.size);
        let lo = [0, 1, 2].map(|k| (2 * self.lo[k] + self.size[k] - size[k]).div_euclid(2));
        RegionBox { lo, size }
    }
}

/// Fill's two modes: the whole box takes the material, or only one
/// material in it is swapped for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillMode {
    Fill,
    Replace,
}

/// A Transform in progress: the terrain as it was when the box was
/// lifted and what the box held, placed again (turned and stretched to the
/// box) every time the box changes. Ends when anything else changes the
/// document (undo included): the next change lifts what the box holds then.
struct Lift {
    base: Option<Variant>,
    content: Region,
    /// Where it came from (cleared when placing); None for Paste and
    /// Duplicate, which leave the source alone.
    from: Option<RegionBox>,
    rot: Rot,
    revision: u64,
    base_grid: Option<Voxels>,
    preview: Option<(RegionBox, Rot)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegionHandle {
    /// A face dot (axis, ±1): moves that face (resizes).
    Face(usize, i32),
    /// An arrow (axis, ±1): moves the box.
    Arrow(usize, i32),
    /// The ring about an axis.
    Ring(usize),
}

/// A drag in the 3D view on a box.
struct RegionDrag {
    /// None: a new box from where the drag started.
    handle: Option<RegionHandle>,
    start: RegionBox,
    origin: Vec3,
    start_param: f32,
    center_px: Pos2,
    last_angle: f32,
    turned: f32,
    /// Sea Level's box rather than the selection.
    sea: bool,
}

/// A brush stroke in progress: the grid before it (for undo), the plane
/// Plane Lock Auto keeps, Flatten's plane height, the terrain Draw
/// raycasts against, the time of the last step and the operation's state.
struct BrushStroke {
    before: Option<Variant>,
    plane: Option<(Vec3, Vec3)>,
    plane_y: f32,
    snapshot: Option<Voxels>,
    last: std::time::Instant,
    state: brush::StrokeState,
    applied: bool,
}

/// An image picked for Import: its path, pixels (RGBA8) and size.
#[derive(Clone)]
pub struct ImportImage {
    pub path: std::path::PathBuf,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    tex: Option<egui::TextureHandle>,
}

pub struct TerrainEditor {
    pub tab: usize,
    pub tool: Option<Tool>,
    // Brush Settings (Schemas/Settings/Brush).
    pub size: f32,
    pub strength: f32,
    pub shape: Shape,
    pub pivot: Pivot,
    pub snap_voxels: bool,
    pub plane_lock: PlaneLock,
    pub ignore_water: bool,
    pub ignore_parts: bool,
    /// Draw / Sculpt: Subtract instead of Add.
    pub subtract: bool,
    pub flatten: FlattenMode,
    /// Cube / Cylinder height (voxels) and whether it follows the size.
    pub height: f32,
    pub height_locked: bool,
    /// Flatten Plane Fixed: flatten to Fixed Y Plane (studs) every stroke.
    pub flatten_fixed: bool,
    pub fixed_y: f32,
    /// Plane Lock Manual: the plane (position, XYZ degrees; default the
    /// y = 0 plane), and whether it is being edited (no strokes then).
    pub plane_pos: [f32; 3],
    pub plane_rot: [f32; 3],
    pub editing_plane: bool,
    /// B held: size and strength when it went down, and the drag so far.
    b_resize: Option<(f32, f32, f32)>,
    // Material Settings.
    pub auto_material: bool,
    pub material: u8,
    pub target_material: u8,
    pub paint_replace: bool,
    // Selection Settings.
    pub selection: Option<RegionBox>,
    pub snap_selection: bool,
    pub merge_empty: bool,
    pub live_edit: bool,
    pub fill_mode: FillMode,
    pub sea: RegionBox,
    // Biome Settings.
    pub biomes: [bool; 9],
    pub blending: f32,
    pub caves: bool,
    pub biome_size: f32,
    pub seed: i64,
    // Heightmap Settings.
    pub heightmap: Option<ImportImage>,
    pub colormap: Option<ImportImage>,
    pub import_material: u8,
    /// The brush's centre and the surface normal there (last frame).
    pub hover: Option<(Vec3, Vec3)>,
    stroke: Option<BrushStroke>,
    pub clipboard: Option<(Region, RegionBox)>,
    lift: Option<Lift>,
    region_drag: Option<RegionDrag>,
    /// A quarter turn typed into Transform's Rotation (axis, turns).
    pending_turn: Option<(usize, i32)>,
    /// Generate running on a worker thread.
    gen_job: Option<GenJob>,
    /// Its share done (0..1) while it runs, for the button.
    pub gen_progress: Option<f32>,
}

/// A Generate on a worker thread: it decodes the grid, generates the box
/// (on all cores) and encodes the result, which is committed as one undo
/// step when it arrives (if the place's Terrain is still the same one).
struct GenJob {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    columns: usize,
    terrain: Ref,
}

impl Default for TerrainEditor {
    fn default() -> Self {
        TerrainEditor {
            tab: 1,
            tool: None,
            size: brush::DEFAULT_SIZE,
            strength: brush::DEFAULT_STRENGTH,
            shape: Shape::Sphere,
            pivot: Pivot::Center,
            snap_voxels: false,
            plane_lock: PlaneLock::Off,
            ignore_water: false,
            ignore_parts: true,
            subtract: false,
            flatten: FlattenMode::Both,
            height: brush::DEFAULT_SIZE,
            height_locked: true,
            flatten_fixed: false,
            fixed_y: 0.0,
            plane_pos: [0.0; 3],
            plane_rot: [0.0; 3],
            editing_plane: false,
            b_resize: None,
            auto_material: false,
            material: 2,
            target_material: 8,
            paint_replace: false,
            // Studio's global default: 64 × 32 × 64 at the origin.
            selection: Some(RegionBox::from_studs([0.0; 3], [64.0, 32.0, 64.0])),
            snap_selection: true,
            merge_empty: false,
            live_edit: true,
            fill_mode: FillMode::Fill,
            sea: RegionBox::from_studs([0.0; 3], [64.0, 32.0, 64.0]),
            biomes: gen::DEFAULT_BIOMES,
            blending: 0.25,
            caves: false,
            biome_size: 100.0,
            seed: 618033988,
            heightmap: None,
            colormap: None,
            import_material: 2,
            hover: None,
            stroke: None,
            clipboard: None,
            lift: None,
            region_drag: None,
            pending_turn: None,
            gen_job: None,
            gen_progress: None,
        }
    }
}

impl TerrainEditor {
    /// Plane Lock Manual's plane: PlaneTransform = CFrame.Angles(π/2, 0, 0)
    /// (normal +Y) turned by the Rotation row, at the Position row.
    pub fn manual_plane(&self) -> (Vec3, Vec3) {
        let [x, y, z] = self.plane_rot.map(f32::to_radians);
        let q = glam::Quat::from_euler(glam::EulerRot::XYZ, x, y, z);
        (Vec3::from(self.plane_pos), (q * Vec3::Y).normalize_or(Vec3::Y))
    }
}

/// The material grid's order: Studio's picker (getTerrainMaterialItems),
/// Air left out.
pub const PICKER: [u8; 23] = brush::PICKER_ORDER;

/// Snaps a stud value to the voxel grid.
fn snap4(v: f32) -> f32 {
    (v / VOXEL).round() * VOXEL
}

// ------------------------------------------------------------ editor glue

impl Editor {
    fn terrain_ref(&self) -> Option<Ref> {
        self.game.doc.workspace().and_then(|ws| self.game.doc.find_child(ws, "Terrain"))
    }

    /// The voxels as the document holds them now.
    fn terrain_voxels(&self) -> Voxels {
        match Voxels::from_dom(&self.game.doc.dom) {
            Some(Ok(v)) => v,
            _ => Voxels::empty(),
        }
    }

    /// Writes a whole new grid as one undo step (Generate, Clear, Sea
    /// Level, Fill, the end of a brush stroke).
    fn commit_terrain(&mut self, label: &str, v: &Voxels) {
        let Some(t) = self.terrain_ref() else {
            self.warn("This place has no Workspace.Terrain.");
            return;
        };
        self.game.doc.set_props(
            label,
            vec![(t, ustr("SmoothGrid"), Variant::BinaryString(BinaryString::from(v.encode())))],
            None,
        );
    }

    fn terrain_editing_allowed(&self) -> bool {
        self.game.play.is_none() && self.game.doc.kind == rustudio_core::doc::DocKind::Place
    }

    /// A box for Select / Generate / Import when there is none: Studio
    /// puts one in front of the camera.
    fn ensure_selection(&mut self) {
        if self.terrain_ed.selection.is_some() {
            return;
        }
        let at = self.game.cam.pos + self.game.cam.forward() * 80.0;
        let mut b = RegionBox::from_studs([at.x, at.y.min(0.0), at.z], [64.0, 32.0, 64.0]);
        b.lo[1] = b.lo[1].min(-4);
        self.terrain_ed.selection = Some(b);
    }

    /// The Terrain Editor panel.
    pub fn terrain_editor_ui(&mut self, ui: &mut egui::Ui) {
        if self.poll_terrain_job(false) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        let allowed = self.terrain_editing_allowed();
        let mut action: Option<&'static str> = None;
        let sel_before = self.terrain_ed.selection;
        let tool_before = self.terrain_ed.tool;
        let has_terrain = self.game.terrain_drawn.as_ref().is_some_and(|(_, _, v)| v.count() > 0)
            || self.terrain_voxels().count() > 0;
        ui.spacing_mut().item_spacing.y = 0.0;
        let te = &mut self.terrain_ed;
        if tui::tabs(ui, &["Create", "Edit"], &mut te.tab) {
            te.tool = None;
        }
        if !allowed {
            ui.add_space(6.0);
            ui.label(egui::RichText::new("   Stop the game to edit terrain.").color(theme::TEXT_DIM));
        }
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.add_enabled_ui(allowed, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                ui.add_space(2.0);
                let tools: Vec<_> = (if te.tab == 0 { &Tool::CREATE[..] } else { &Tool::EDIT[..] })
                    .iter()
                    .map(|t| (*t, t.name(), t.icon()))
                    .collect();
                if let Some(t) = tui::tiles(ui, &tools, te.tool) {
                    te.tool = if te.tool == Some(t) { None } else { Some(t) };
                }
                ui.add_space(6.0);
                if let Some(tool) = te.tool {
                    action = settings_panel(ui, te, tool, has_terrain);
                }
                ui.add_space(12.0);
            });
        });
        if self.terrain_ed.tool != tool_before {
            self.terrain_ed.stroke = None;
            if self.terrain_ed.tool.is_some_and(Tool::uses_selection) {
                self.ensure_selection();
            }
        }
        // A box typed in: Transform carries the terrain with it.
        let sel_after = self.terrain_ed.selection;
        if sel_after != sel_before {
            match (self.terrain_ed.tool, sel_before, sel_after) {
                (Some(Tool::Transform), Some(old), Some(_)) => {
                    self.terrain_ed.selection = sel_before;
                    self.lift_region(old);
                    self.terrain_ed.selection = sel_after;
                    self.place_lift("Terrain Transform");
                }
                _ => self.terrain_ed.lift = None,
            }
        }
        if let Some((axis, turns)) = self.terrain_ed.pending_turn.take() {
            self.rotate_region(axis, turns);
        }
        if let Some(a) = action {
            self.terrain_action(a);
        }
    }

    /// The panel's one-shot buttons.
    pub fn terrain_action(&mut self, a: &str) {
        if !self.terrain_editing_allowed() {
            return;
        }
        if self.terrain_region_action(a) {
            return;
        }
        if a == "import_pick_height" || a == "import_pick_color" {
            self.pick_import_image(a == "import_pick_height");
            return;
        }
        if self.terrain_ed.gen_job.is_some() {
            return;
        }
        if a == "generate" {
            self.start_generate();
            return;
        }
        let mut v = self.terrain_voxels();
        let te = &self.terrain_ed;
        let label = match a {
            "import" => {
                let Some(b) = te.selection else { return };
                let Some(h) = te.heightmap.as_ref() else {
                    self.warn("Pick a heightmap image first.");
                    return;
                };
                let hm = gen::Image { w: h.w, h: h.h, rgba: &h.rgba };
                let cm = te.colormap.as_ref().map(|c| gen::Image { w: c.w, h: c.h, rgba: &c.rgba });
                gen::import(&mut v, b.lo, b.size, &hm, cm.as_ref(), te.import_material);
                "Import Terrain"
            }
            "clear" => {
                v.clear();
                "Clear Terrain"
            }
            "sea" | "evaporate" => {
                gen::sea_level(&mut v, te.sea.lo, te.sea.size, a == "evaporate");
                if a == "sea" { "Sea Level" } else { "Evaporate" }
            }
            _ => return,
        };
        self.commit_terrain(label, &v);
    }

    /// Generate: starts it on a worker thread (Studio's GenerateOperation
    /// also runs in steps while Studio stays usable). `poll_terrain_job`
    /// commits it.
    fn start_generate(&mut self) {
        let te = &self.terrain_ed;
        let Some(b) = te.selection else { return };
        let Some(terrain) = self.terrain_ref() else {
            self.warn("This place has no Workspace.Terrain.");
            return;
        };
        let biomes: Vec<Biome> = Biome::ALL.iter().zip(te.biomes).filter(|(_, on)| *on).map(|(b, _)| *b).collect();
        if biomes.is_empty() {
            return;
        }
        let opts = gen::Options { biomes, blending: te.blending, caves: te.caves, biome_size: te.biome_size, seed: te.seed };
        // The grid's bytes as they are now; decoded on the worker.
        let bytes: Vec<u8> = match self.game.doc.dom.get_by_ref(terrain).and_then(|i| i.properties.get(&ustr("SmoothGrid"))) {
            Some(Variant::BinaryString(s)) => AsRef::<[u8]>::as_ref(s).to_vec(),
            _ => Vec::new(),
        };
        let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_done = done.clone();
        let spawned = std::thread::Builder::new().name("terrain-generate".into()).spawn(move || {
            let mut v = Voxels::decode(&bytes).unwrap_or_else(|_| Voxels::empty());
            gen::generate_with_progress(&mut v, b.lo, b.size, &opts, Some(&worker_done));
            let _ = tx.send(v.encode());
        });
        if spawned.is_err() {
            self.warn("Generate: couldn't start a worker thread.");
            return;
        }
        let columns = (b.size[0].max(0) as usize) * (b.size[2].max(0) as usize);
        self.terrain_ed.gen_job = Some(GenJob { rx, done, columns: columns.max(1), terrain });
        self.terrain_ed.gen_progress = Some(0.0);
    }

    /// Commits a finished Generate (one undo step) and keeps the progress
    /// current; `wait` blocks until it has finished. True while one runs.
    pub fn poll_terrain_job(&mut self, wait: bool) -> bool {
        let Some(job) = self.terrain_ed.gen_job.as_ref() else {
            return false;
        };
        let got = if wait {
            job.rx.recv().map_err(|_| std::sync::mpsc::TryRecvError::Disconnected)
        } else {
            job.rx.try_recv()
        };
        match got {
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let p = job.done.load(std::sync::atomic::Ordering::Relaxed) as f32 / job.columns as f32;
                self.terrain_ed.gen_progress = Some(p.clamp(0.0, 1.0));
                true
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.terrain_ed.gen_job = None;
                self.terrain_ed.gen_progress = None;
                self.warn("Generate failed.");
                false
            }
            Ok(bytes) => {
                let terrain = job.terrain;
                self.terrain_ed.gen_job = None;
                self.terrain_ed.gen_progress = None;
                // A different place (opened or new) since it started: dropped.
                if self.terrain_ref() == Some(terrain) && self.terrain_editing_allowed() {
                    self.game.doc.set_props(
                        "Generate Terrain",
                        vec![(terrain, ustr("SmoothGrid"), Variant::BinaryString(BinaryString::from(bytes)))],
                        None,
                    );
                }
                false
            }
        }
    }

    /// Asks for a heightmap / colormap image and reads it.
    fn pick_import_image(&mut self, height: bool) {
        if self.harness.selftest.is_some() {
            return;
        }
        use crate::file_dialog::{Kind, Purpose};
        let d = crate::file_dialog::AsyncFileDialog::new().add_filter("Image", &["png", "jpg", "jpeg"]);
        self.dialogs.start(Purpose::TerrainImage { height }, d, Kind::PickFile);
    }

    /// The chosen heightmap / colormap image (src/file_dialog.rs).
    pub fn load_import_image(&mut self, path: std::path::PathBuf, height: bool) {
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let im = ImportImage { path, w, h, rgba: rgba.into_raw(), tex: None };
                if height {
                    self.terrain_ed.heightmap = Some(im);
                } else {
                    self.terrain_ed.colormap = Some(im);
                }
            }
            Err(e) => self.warn(format!("Couldn't read {}: {e}", path.display())),
        }
    }

    /// The brush payload for a tool from the panel's settings.
    fn brush_params(&self, op: BrushOp) -> brush::Params {
        let te = &self.terrain_ed;
        brush::Params {
            op,
            shape: te.shape,
            size: te.size,
            height: if te.shape == Shape::Sphere { te.size } else { te.height },
            strength: if matches!(op, BrushOp::Draw | BrushOp::Paint) { 1.0 } else { te.strength },
            pivot: te.pivot,
            subtract: te.subtract,
            smooth_instead: false,
            ignore_water: te.ignore_water,
            material: te.material,
            target: if op == BrushOp::Paint && te.paint_replace { Some(te.target_material) } else { None },
            auto_material: te.auto_material && matches!(op, BrushOp::Draw | BrushOp::Sculpt),
            flatten: te.flatten,
            plane_y: 0.0,
        }
    }

    /// The brush in the 3D view: under the mouse, and painting while the
    /// left button is held (Studio's useRaycast + Tools.Brush*). True while
    /// it has the left button (no picking).
    pub fn terrain_brush_ui(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        resp: &egui::Response,
        pointer: Option<Pos2>,
        painter: &egui::Painter,
    ) -> bool {
        if self.poll_terrain_job(false) {
            // No edits while a Generate runs: it would overwrite them.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            self.terrain_ed.hover = None;
            return false;
        }
        let allowed = self.terrain_editing_allowed();
        let tool = match self.terrain_ed.tool {
            Some(t) if allowed => t,
            _ => {
                self.terrain_ed.hover = None;
                return false;
            }
        };
        if tool.uses_selection() || tool == Tool::SeaLevel {
            self.terrain_ed.hover = None;
            return self.terrain_region_view(ctx, rect, resp, pointer, painter);
        }
        let Some(op) = tool.brush_op() else {
            self.terrain_ed.hover = None;
            return false;
        };
        let (ctrl, shift, down, b_held, delta) = ctx.input(|i| {
            (i.modifiers.command, i.modifiers.shift, i.pointer.primary_down(), i.key_down(egui::Key::B), i.pointer.delta())
        });
        // B held: drag sideways to resize (Shift: strength), as Studio's
        // ShortcutController.onBrushChanged (ΔX / 10 per move).
        if b_held && rect.contains(pointer.unwrap_or_default()) {
            let te = &mut self.terrain_ed;
            let acc = te.b_resize.get_or_insert((te.size, te.strength, 0.0));
            acc.2 += delta.x / 10.0;
            if shift {
                te.strength = ((acc.1 + acc.2 / 100.0) * 100.0).floor().clamp(0.0, 100.0) / 100.0;
            } else {
                te.size = (acc.0 + acc.2).floor().clamp(brush::SIZE_MIN, brush::SIZE_MAX);
                if te.height_locked {
                    te.height = te.size;
                }
            }
            ctx.request_repaint();
        } else {
            self.terrain_ed.b_resize = None;
        }
        let mut params = self.brush_params(op);
        if matches!(op, BrushOp::Draw | BrushOp::Sculpt) {
            // Ctrl flips Add / Subtract while held; Shift smooths instead.
            params.subtract = params.subtract != ctrl;
            params.smooth_instead = shift && !b_held;
        }
        // The point under the mouse: terrain (a snapshot of it while Draw
        // strokes, so it doesn't climb its own output), parts unless Ignore
        // Parts, else the y = 0 plane; Plane Lock puts it on its plane.
        let look = self.game.cam.forward();
        let hit = pointer.filter(|p| rect.contains(*p)).and_then(|p| {
            let ray = self.game.cam.ray(rect, p);
            let te = &self.terrain_ed;
            if op != BrushOp::Flatten {
                let locked = match te.plane_lock {
                    PlaneLock::Off => None,
                    PlaneLock::Auto => te.stroke.as_ref().and_then(|s| s.plane),
                    PlaneLock::Manual => Some(te.manual_plane()),
                };
                if let Some((o, n)) = locked {
                    return brush::ray_plane(ray.origin, ray.dir, o, n).map(|q| (q, n));
                }
            }
            let v = te
                .stroke
                .as_ref()
                .and_then(|s| s.snapshot.as_ref())
                .or(self.game.terrain_drawn.as_ref().map(|(_, _, v)| v));
            let mut best = v.and_then(|v| ray_voxels(v, ray.origin, ray.dir, 10000.0, te.ignore_water));
            if !te.ignore_parts {
                if let Some(t) = self.ray_distance(&ray) {
                    if best.is_none_or(|(q, _)| (q - ray.origin).length() > t) {
                        best = Some((ray.origin + ray.dir * t, -ray.dir));
                    }
                }
            }
            best.or_else(|| {
                brush::ray_plane(ray.origin, ray.dir, Vec3::ZERO, Vec3::Y).map(|q| (q, brush::camera_look_plane(look)))
            })
        });
        let hit = hit.map(|(p, n)| (if self.terrain_ed.snap_voxels { brush::snap_to_voxel(p) } else { p }, n));
        self.terrain_ed.hover = hit.map(|(p, n)| (brush::brush_center(p, &params, false), n));
        if let (Some((c, _)), Some((p, _))) = (self.terrain_ed.hover, hit) {
            self.paint_brush_gizmo(rect, painter, c, p, &params);
        }
        let pressed = resp.drag_started_by(egui::PointerButton::Primary)
            || resp.clicked_by(egui::PointerButton::Primary)
            || (resp.is_pointer_button_down_on() && down && self.terrain_ed.stroke.is_none());
        let editing_plane = op != BrushOp::Flatten && self.terrain_ed.plane_lock == PlaneLock::Manual && self.terrain_ed.editing_plane;
        if pressed && self.terrain_ed.stroke.is_none() && resp.hovered() && !b_held && !editing_plane {
            let before = self.terrain_ref().and_then(|t| self.game.doc.prop(t, "SmoothGrid"));
            // Plane Lock Auto: the plane through the click, facing the
            // camera's nearest axis (frozen for the stroke).
            let plane = hit.map(|(p, _)| (p, brush::camera_look_plane(look)));
            let plane_y = if self.terrain_ed.flatten_fixed {
                self.terrain_ed.fixed_y
            } else {
                hit.map(|(p, _)| brush::convert_for_pivot(params.pivot, p, params.height_studs()).y).unwrap_or(0.0)
            };
            // (decoded from the grid before the stroke; skipped for very
            // large terrains, where it would stall the click)
            let small = self.game.terrain_drawn.as_ref().is_some_and(|(_, _, v)| v.count() < 2_000_000);
            let snapshot = (op == BrushOp::Draw && self.terrain_ed.plane_lock == PlaneLock::Off && small)
                .then(|| grid_of(before.as_ref()));
            self.terrain_ed.stroke = Some(BrushStroke {
                before,
                plane,
                plane_y,
                snapshot,
                last: std::time::Instant::now() - std::time::Duration::from_secs(1),
                state: brush::StrokeState::default(),
                applied: false,
            });
        }
        let Some(stroke) = self.terrain_ed.stroke.as_mut() else {
            return resp.hovered();
        };
        if down {
            let ticks = (stroke.last.elapsed().as_secs_f32() / brush::INTERVAL_S).floor().min(4.0) as usize;
            if let (true, Some((p, _))) = (ticks > 0, hit) {
                stroke.last = std::time::Instant::now();
                params.plane_y = stroke.plane_y;
                let steps = if op == BrushOp::Flatten { ticks * brush::FLATTEN_STEPS_PER_FRAME } else { ticks.min(1) };
                if let Some((id, r, mut v)) = self.game.terrain_drawn.take() {
                    let mut changed: Option<([i32; 3], [i32; 3])> = None;
                    for _ in 0..steps {
                        if let Some((lo, hi)) = brush::step(&mut v, &params, p, &mut stroke.state) {
                            changed = Some(match changed {
                                None => (lo, hi),
                                Some((a, b)) => ([0, 1, 2].map(|k| a[k].min(lo[k])), [0, 1, 2].map(|k| b[k].max(hi[k]))),
                            });
                        }
                    }
                    if let Some((lo, hi)) = changed {
                        stroke.applied = true;
                        let water = params.material == WATER || !params.ignore_water;
                        self.game.terrain_brushed = Some((v, rustudio_core::terrain::dirty_box(lo, hi), water));
                        self.game.terrain_drawn = Some((id, r, Voxels::empty()));
                        self.game.scene.terrain_dirty = true;
                    } else {
                        self.game.terrain_drawn = Some((id, r, v));
                    }
                }
            }
            ctx.request_repaint();
            return true;
        }
        // Released: the stroke becomes one undo step (the operation's
        // OnFinish waypoint: "Draw", "Sculpt", …).
        let stroke = self.terrain_ed.stroke.take().expect("stroke");
        if stroke.applied {
            if let (Some(t), Some((_, _, v))) = (self.terrain_ref(), self.game.terrain_drawn.as_ref()) {
                let after = Variant::BinaryString(BinaryString::from(v.encode()));
                if let Some(i) = self.game.doc.dom.get_by_ref_mut(t) {
                    i.properties.insert(ustr("SmoothGrid"), after.clone());
                }
                let name = match tool {
                    Tool::Draw => "Terrain Draw",
                    Tool::Sculpt => "Terrain Sculpt",
                    Tool::Smooth => "Terrain Smooth",
                    Tool::Flatten => "Terrain Flatten",
                    _ => "Terrain Paint",
                };
                self.game.doc.record_props(name, vec![(t, ustr("SmoothGrid"), stroke.before, after)]);
            }
        }
        true
    }

    /// Studio's brush gizmo (Components/Gizmos/Brush): the shape, white and
    /// see-through, at the brush centre, a dot where the mouse hits, and
    /// the grid of the plane Flatten / Plane Lock work on.
    fn paint_brush_gizmo(&self, rect: Rect, painter: &egui::Painter, c: Vec3, hit: Vec3, p: &brush::Params) {
        let r = p.size * VOXEL * 0.5;
        let hh = p.height_studs() * 0.5;
        let fill = Color32::from_rgba_unmultiplied(235, 236, 240, 40);
        let line = Color32::from_rgba_unmultiplied(235, 236, 240, 110);
        let proj = |q: Vec3| self.game.cam.project(rect, q);
        let ring = |center: Vec3, u: Vec3, v: Vec3, rad: f32| -> Vec<Pos2> {
            (0..=48)
                .filter_map(|i| {
                    let t = i as f32 / 48.0 * std::f32::consts::TAU;
                    proj(center + (u * t.cos() + v * t.sin()) * rad)
                })
                .collect()
        };
        let mut outline: Vec<Pos2> = Vec::new();
        match p.shape {
            Shape::Sphere => {
                let to_cam = (self.game.cam.pos - c).normalize_or(Vec3::Y);
                let u = to_cam.any_orthonormal_vector();
                let v = to_cam.cross(u);
                outline = ring(c, u, v, r);
                painter.add(egui::Shape::line(ring(c, Vec3::X, Vec3::Z, r), Stroke::new(1.0, line)));
            }
            Shape::Cylinder => {
                for y in [-hh, hh] {
                    let pts = ring(c + Vec3::Y * y, Vec3::X, Vec3::Z, r);
                    outline.extend(pts.iter().copied());
                    painter.add(egui::Shape::line(pts, Stroke::new(1.0, line)));
                }
            }
            Shape::Cube => {
                let h = Vec3::new(r, hh, r);
                for i in 0..8usize {
                    let corner = |i: usize| c + Vec3::new(if i & 1 != 0 { h.x } else { -h.x }, if i & 2 != 0 { h.y } else { -h.y }, if i & 4 != 0 { h.z } else { -h.z });
                    if let Some(q) = proj(corner(i)) {
                        outline.push(q);
                    }
                    for bit in [1usize, 2, 4] {
                        if i & bit == 0 {
                            if let (Some(a), Some(b)) = (proj(corner(i)), proj(corner(i | bit))) {
                                painter.line_segment([a, b], Stroke::new(1.0, line));
                            }
                        }
                    }
                }
            }
        }
        if outline.len() > 2 {
            let hull = convex_hull(outline);
            painter.add(egui::Shape::convex_polygon(hull.clone(), fill, Stroke::NONE));
            let mut closed = hull;
            if let Some(f) = closed.first().copied() {
                closed.push(f);
            }
            painter.add(egui::Shape::line(closed, Stroke::new(1.0, line)));
        }
        if let Some(q) = proj(hit) {
            // CenterRadiusMultiplier 0.015 × distance: a few pixels.
            painter.circle_filled(q, 4.0, Color32::WHITE);
        }
        // The plane: Flatten's (through the click, or the Fixed height) and
        // Plane Lock's, as a grid the brush's size.
        let te = &self.terrain_ed;
        let plane = if p.op == BrushOp::Flatten {
            let y = if te.flatten_fixed {
                te.fixed_y
            } else {
                te.stroke.as_ref().map(|s| s.plane_y).unwrap_or(c.y)
            };
            Some((Vec3::new(c.x, y, c.z), Vec3::Y))
        } else {
            match te.plane_lock {
                PlaneLock::Off => None,
                PlaneLock::Auto => te.stroke.as_ref().and_then(|s| s.plane),
                PlaneLock::Manual => Some(te.manual_plane()),
            }
        };
        if let Some((o, n)) = plane {
            let o = o + n * (c - o).dot(n) * 0.0;
            let o = if p.op == BrushOp::Flatten { o } else { o + (c - o) - n * (c - o).dot(n) };
            let u = n.any_orthonormal_vector();
            let v = n.cross(u);
            let half = r * 1.25;
            let col = Color32::from_rgba_unmultiplied(235, 236, 240, 70);
            for k in -4..=4 {
                let t = k as f32 / 4.0 * half;
                for (a, b) in [(u, v), (v, u)] {
                    if let (Some(s), Some(e)) = (proj(o + a * t - b * half), proj(o + a * t + b * half)) {
                        painter.line_segment([s, e], Stroke::new(1.0, col));
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------ the panel

const SHAPES: [(Shape, Seg); 3] =
    [(Shape::Sphere, Seg::Icon("sphere")), (Shape::Cube, Seg::Icon("cube")), (Shape::Cylinder, Seg::Icon("cylinder"))];

/// The picked tool's settings, Studio's sections in Studio's order.
/// Returns a button pressed.
fn settings_panel(ui: &mut egui::Ui, te: &mut TerrainEditor, tool: Tool, has_terrain: bool) -> Option<&'static str> {
    let mut action = None;
    match tool {
        Tool::Clear => {
            if tui::section(ui, "Clear Settings") {
                ui.add_space(4.0);
                if tui::primary(ui, "Clear All Terrain", has_terrain) {
                    action = Some("clear");
                }
            }
        }
        Tool::Generate | Tool::Import | Tool::Select | Tool::Fill => {
            if tui::section(ui, "Selection Settings") {
                selection_rows(ui, te, false);
            }
            match tool {
                Tool::Generate => {
                    if tui::section(ui, "Biome Settings") {
                        biome_rows(ui, te);
                        let label = match te.gen_progress {
                            Some(p) => format!("Generating… {:.0}%", p * 100.0),
                            None => "Generate".to_string(),
                        };
                        if tui::primary(ui, &label, te.selection.is_some() && te.gen_progress.is_none()) {
                            action = Some("generate");
                        }
                    }
                }
                Tool::Import => {
                    if tui::section(ui, "Heightmap Settings") {
                        action = heightmap_rows(ui, te);
                    }
                }
                Tool::Fill => {
                    if tui::section(ui, "Material Settings") {
                        tui::row(ui, "Fill Mode", tui::ROW_H, |ui| {
                            tui::segmented(ui, &mut te.fill_mode, &[(FillMode::Fill, Seg::Text("Fill")), (FillMode::Replace, Seg::Text("Replace"))])
                        });
                        let source = if te.fill_mode == FillMode::Fill { "Source Material" } else { "Source Material" };
                        tui::caption(ui, source);
                        if let Some(m) = tui::material_grid(ui, "te_fill_src", &PICKER, te.material) {
                            te.material = m;
                        }
                        if te.fill_mode == FillMode::Replace {
                            ui.add_space(8.0);
                            tui::caption(ui, "Target Material");
                            if let Some(m) = tui::material_grid(ui, "te_fill_dst", &PICKER, te.target_material) {
                                te.target_material = m;
                            }
                        }
                        ui.add_space(10.0);
                        let label = if te.fill_mode == FillMode::Fill { "Fill" } else { "Replace" };
                        if tui::primary(ui, label, te.selection.is_some()) {
                            action = Some("fill");
                        }
                    }
                }
                _ => {}
            }
        }
        Tool::Transform => {
            if tui::section(ui, "Transform Settings") {
                selection_rows(ui, te, true);
            }
        }
        Tool::SeaLevel => {
            if tui::section(ui, "Sea Level Settings") {
                let (mut pos, mut size) = te.sea.studs();
                let mut changed = tui::row(ui, "Size", tui::ROW_H, |ui| tui::vec3(ui, "te_sea_size", &mut size, (4.0, 16384.0)));
                changed |= tui::row(ui, "Position", tui::ROW_H, |ui| tui::vec3(ui, "te_sea_pos", &mut pos, (-1e6, 1e6)));
                tui::row(ui, "Snap to Voxels", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.snap_selection, None));
                if changed {
                    te.sea = RegionBox::from_studs(pos, size);
                }
                if tui::primary(ui, "Evaporate", true) {
                    action = Some("evaporate");
                }
                ui.add_space(10.0);
                if tui::primary(ui, "Create", true) {
                    action = Some("sea");
                }
            }
        }
        Tool::Draw | Tool::Sculpt | Tool::Smooth | Tool::Paint | Tool::Flatten => {
            if tui::section(ui, "Brush Settings") {
                brush_rows(ui, te, tool);
            }
            if matches!(tool, Tool::Draw | Tool::Sculpt | Tool::Paint) && tui::section(ui, "Material Settings") {
                if tool == Tool::Paint {
                    tui::row(ui, "Material Mode", tui::ROW_H, |ui| {
                        tui::segmented(ui, &mut te.paint_replace, &[(false, Seg::Text("Paint")), (true, Seg::Text("Replace"))])
                    });
                } else {
                    tui::row(ui, "Auto Material", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.auto_material, None));
                }
                tui::caption(ui, "Source Material");
                if let Some(m) = tui::material_grid(ui, "te_src", &PICKER, te.material) {
                    te.material = m;
                }
                if tool == Tool::Paint && te.paint_replace {
                    ui.add_space(8.0);
                    tui::caption(ui, "Target Material");
                    if let Some(m) = tui::material_grid(ui, "te_dst", &PICKER, te.target_material) {
                        te.target_material = m;
                    }
                }
            }
        }
    }
    action
}

/// Size, Position (and Rotation for Transform), Merge Empty / Live Edit,
/// Snap to Voxels. Edits move the shared selection box.
fn selection_rows(ui: &mut egui::Ui, te: &mut TerrainEditor, transform: bool) {
    let Some(b) = te.selection else {
        tui::caption(ui, "Drag in the 3D view to select a region.");
        return;
    };
    let (mut pos, mut size) = b.studs();
    let mut changed = tui::row(ui, "Size", tui::ROW_H, |ui| tui::vec3(ui, "te_sel_size", &mut size, (4.0, 16384.0)));
    changed |= tui::row(ui, "Position", tui::ROW_H, |ui| tui::vec3(ui, "te_sel_pos", &mut pos, (-1e6, 1e6)));
    if transform {
        let mut rot = te.lift.as_ref().map(|l| rot_degrees(&l.rot)).unwrap_or([0.0; 3]);
        let before = rot;
        tui::row(ui, "Rotation", tui::ROW_H, |ui| tui::vec3(ui, "te_sel_rot", &mut rot, (-360.0, 360.0)));
        if rot != before {
            // Region rotation is in quarter turns (TerrainRegion's own).
            for k in 0..3 {
                let turns = ((rot[k] - before[k]) / 90.0).round() as i32;
                if turns != 0 {
                    te.pending_turn = Some((k, turns));
                }
            }
        }
        tui::row(ui, "Merge Empty", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.merge_empty, None));
        tui::row(ui, "Live Edit", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.live_edit, None));
    }
    tui::row(ui, "Snap to Voxels", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.snap_selection, None));
    if changed {
        // Whole voxels (the box snaps outward to the grid either way:
        // rustudio's regions are voxel boxes).
        if te.snap_selection {
            size = size.map(snap4);
        }
        te.selection = Some(RegionBox::from_studs(pos, size));
    }
}

/// The quarter turns of a region rotation as XYZ Euler degrees.
fn rot_degrees(r: &Rot) -> [f32; 3] {
    let m = glam::Mat3::from_cols(
        Vec3::new(r[0][0] as f32, r[1][0] as f32, r[2][0] as f32),
        Vec3::new(r[0][1] as f32, r[1][1] as f32, r[2][1] as f32),
        Vec3::new(r[0][2] as f32, r[1][2] as f32, r[2][2] as f32),
    );
    let (x, y, z) = glam::Quat::from_mat3(&m).to_euler(glam::EulerRot::XYZ);
    [x, y, z].map(|a| {
        let d = a.to_degrees().round();
        if d == -0.0 { 0.0 } else { d }
    })
}

fn biome_rows(ui: &mut egui::Ui, te: &mut TerrainEditor) {
    let h = Biome::ALL.len() as f32 * 22.0;
    tui::row(ui, "Biomes", h, |ui| {
        ui.vertical(|ui| {
            for (i, b) in Biome::ALL.iter().enumerate() {
                tui::checkbox(ui, &mut te.biomes[i], Some(b.name()));
            }
        });
    });
    tui::row(ui, "Blending", tui::ROW_H, |ui| tui::slider(ui, "te_blend", &mut te.blending, (0.0, 1.0), 0.01, 2));
    tui::row(ui, "Caves", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.caves, None));
    tui::row(ui, "Biome Size", tui::ROW_H, |ui| tui::int_field(ui, "te_bsize", &mut te.biome_size, (gen::BIOME_SIZE_MIN, gen::BIOME_SIZE_MAX)));
    tui::row(ui, "Seed", tui::ROW_H, |ui| tui::seed_field(ui, "te_seed", &mut te.seed));
}

fn heightmap_rows(ui: &mut egui::Ui, te: &mut TerrainEditor) -> Option<&'static str> {
    let mut action = None;
    for (label, height) in [("Heightmap", true), ("Colormap", false)] {
        let slot = if height { &mut te.heightmap } else { &mut te.colormap };
        let name = slot.as_ref().and_then(|i| i.path.file_name()).map(|n| n.to_string_lossy().to_string());
        let tex = slot.as_mut().map(|im| {
            im.tex
                .get_or_insert_with(|| {
                    let ci = egui::ColorImage::from_rgba_unmultiplied([im.w as usize, im.h as usize], &im.rgba);
                    ui.ctx().load_texture(format!("te_{label}"), ci, egui::TextureOptions::LINEAR)
                })
                .clone()
        });
        let clicked = tui::row(ui, label, 100.0, |ui| tui::image_box(ui, name.as_deref(), tex.as_ref()).clicked());
        if clicked {
            action = Some(if height { "import_pick_height" } else { "import_pick_color" });
        }
    }
    tui::caption(ui, "Default Material");
    if let Some(m) = tui::material_grid(ui, "te_import_mat", &PICKER, te.import_material) {
        te.import_material = m;
    }
    ui.add_space(10.0);
    if tui::primary(ui, "Import", te.heightmap.is_some() && te.selection.is_some()) {
        action = Some("import");
    }
    action
}

/// Brush Settings, Studio's rows in Studio's order (Schemas.Settings.Brush:
/// a row shows when the tool's payload has the setting).
fn brush_rows(ui: &mut egui::Ui, te: &mut TerrainEditor, tool: Tool) {
    if matches!(tool, Tool::Draw | Tool::Sculpt) {
        tui::row(ui, "Brush Mode", tui::ROW_H, |ui| {
            tui::segmented(ui, &mut te.subtract, &[(false, Seg::Text("Add")), (true, Seg::Text("Subtract"))])
        });
    }
    tui::row(ui, "Brush Shape", 52.0, |ui| tui::segmented(ui, &mut te.shape, &SHAPES));
    if tool == Tool::Flatten {
        tui::row(ui, "Flatten Mode", 52.0, |ui| {
            tui::segmented(
                ui,
                &mut te.flatten,
                &[
                    (FlattenMode::Erode, Seg::Icon("arrow-bar-to-down")),
                    (FlattenMode::Grow, Seg::Icon("arrow-bar-to-up")),
                    (FlattenMode::Both, Seg::Icon("arrows-vertical")),
                ],
            )
        });
    }
    if te.shape == Shape::Sphere {
        tui::row(ui, "Brush Size", tui::ROW_H, |ui| {
            tui::slider(ui, "te_size", &mut te.size, (brush::SIZE_MIN, brush::SIZE_MAX), 1.0, 0)
        });
    } else {
        // BrushSize renderer for Cube / Cylinder: W (R) and H, locked together.
        let w_label = if te.shape == Shape::Cylinder { "Brush Size  R" } else { "Brush Size  W" };
        let before = te.size;
        tui::row(ui, w_label, tui::ROW_H, |ui| tui::slider(ui, "te_size", &mut te.size, (brush::SIZE_MIN, brush::SIZE_MAX), 1.0, 0));
        if te.height_locked && te.size != before {
            te.height = te.size;
        }
        let before_h = te.height;
        tui::row(ui, "               H", tui::ROW_H, |ui| tui::slider(ui, "te_height", &mut te.height, (brush::SIZE_MIN, brush::SIZE_MAX), 1.0, 0));
        if te.height_locked && te.height != before_h {
            te.size = te.height;
        }
        tui::row(ui, "Lock Size", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.height_locked, None));
    }
    if matches!(tool, Tool::Sculpt | Tool::Smooth | Tool::Flatten) {
        tui::row(ui, "Strength", tui::ROW_H, |ui| tui::slider(ui, "te_strength", &mut te.strength, (brush::STRENGTH_MIN, 1.0), 0.01, 2));
    }
    tui::row(ui, "Pivot Position", tui::ROW_H, |ui| {
        tui::segmented(ui, &mut te.pivot, &[(Pivot::Bottom, Seg::Text("Bottom")), (Pivot::Center, Seg::Text("Center")), (Pivot::Top, Seg::Text("Top"))])
    });
    if tool != Tool::Sculpt {
        tui::row(ui, "Snapping", tui::ROW_H, |ui| {
            tui::segmented(ui, &mut te.snap_voxels, &[(false, Seg::Text("Off")), (true, Seg::Text("Voxel"))])
        });
    }
    if tool == Tool::Flatten {
        tui::row(ui, "Flatten Plane", tui::ROW_H, |ui| {
            tui::segmented(ui, &mut te.flatten_fixed, &[(false, Seg::Text("Auto")), (true, Seg::Text("Fixed"))])
        });
        if te.flatten_fixed {
            tui::row(ui, "Fixed Y Plane", tui::ROW_H, |ui| {
                let w = ui.available_width();
                tui::number(ui, "te_fixed_y", &mut te.fixed_y, w, (-1e6, 1e6), 2)
            });
        }
    } else {
        tui::row(ui, "Plane Lock", tui::ROW_H, |ui| {
            tui::segmented(ui, &mut te.plane_lock, &[(PlaneLock::Off, Seg::Text("Off")), (PlaneLock::Auto, Seg::Text("Auto")), (PlaneLock::Manual, Seg::Text("Manual"))])
        });
        if te.plane_lock == PlaneLock::Manual {
            // ManualPlaneLock: Edit / Apply, then the plane's rows.
            let label = if te.editing_plane { "Apply" } else { "Edit Plane" };
            if tui::primary(ui, label, true) {
                te.editing_plane = !te.editing_plane;
            }
            ui.add_space(tui::ROW_GAP);
            if te.editing_plane {
                tui::row(ui, "Plane Position", tui::ROW_H, |ui| tui::vec3(ui, "te_plane_pos", &mut te.plane_pos, (-1e6, 1e6)));
                tui::row(ui, "Plane Rotation", tui::ROW_H, |ui| tui::vec3(ui, "te_plane_rot", &mut te.plane_rot, (-360.0, 360.0)));
            }
        }
    }
    tui::row(ui, "Ignore Water", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.ignore_water, None));
    tui::row(ui, "Ignore Parts", tui::ROW_H, |ui| tui::checkbox(ui, &mut te.ignore_parts, None));
}

/// First solid point along a ray through the voxels (half-stud steps),
/// with the occupancy gradient's normal there.
pub fn ray_voxels(v: &Voxels, origin: Vec3, dir: Vec3, max: f32, ignore_water: bool) -> Option<(Vec3, Vec3)> {
    let dir = dir.normalize_or_zero();
    let occ = |p: Vec3| -> f32 {
        let q = (p / VOXEL).floor();
        let (m, o) = v.get(q.x as i32, q.y as i32, q.z as i32);
        if m != 0 && !(ignore_water && m == WATER) {
            o
        } else {
            0.0
        }
    };
    let mut t = 0.0;
    while t < max {
        let p = origin + dir * t;
        if occ(p) > 0.0 {
            let e = VOXEL;
            let g = Vec3::new(
                occ(p - Vec3::X * e) - occ(p + Vec3::X * e),
                occ(p - Vec3::Y * e) - occ(p + Vec3::Y * e),
                occ(p - Vec3::Z * e) - occ(p + Vec3::Z * e),
            );
            let n = if g.length_squared() > 1e-6 { g.normalize() } else { -dir };
            return Some((p, n));
        }
        t += 0.5;
    }
    None
}

// ------------------------------------------------------------ region tools

/// The box's twelve edges, on screen.
fn box_edges(cam: &rustudio_render::camera::Camera, rect: Rect, b: &RegionBox) -> Vec<[Pos2; 2]> {
    let (lo, hi) = (b.min(), b.max());
    let corner = |i: usize| Vec3::new(if i & 1 != 0 { hi.x } else { lo.x }, if i & 2 != 0 { hi.y } else { lo.y }, if i & 4 != 0 { hi.z } else { lo.z });
    let mut out = Vec::new();
    for i in 0..8usize {
        for bit in [1usize, 2, 4] {
            if i & bit == 0 {
                if let (Some(a), Some(c)) = (cam.project(rect, corner(i)), cam.project(rect, corner(i | bit))) {
                    out.push([a, c]);
                }
            }
        }
    }
    out
}

fn dist_to_segment(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
    (a + ab * t - p).length()
}

/// The voxels in a SmoothGrid value (empty when there is none).
fn grid_of(v: Option<&Variant>) -> Voxels {
    let bytes: &[u8] = match v {
        Some(Variant::BinaryString(b)) => b.as_ref(),
        Some(Variant::SharedString(s)) => s.data(),
        _ => &[],
    };
    Voxels::decode(bytes).unwrap_or_else(|_| Voxels::empty())
}

/// Places lifted terrain: `from` cleared (a Move), `content` turned by
/// `rot` and stretched to `to`, its empty voxels clearing what is under
/// them only with `merge_empty`.
pub fn place(v: &mut Voxels, content: &Region, from: Option<RegionBox>, rot: &Rot, to: RegionBox, merge_empty: bool) {
    if let Some(f) = from {
        region::set_box(v, f.lo, f.size, 0);
    }
    content.rotated(rot).resized(to.size).paste(v, to.lo, merge_empty);
}

/// The handle under a screen point.
fn region_handle_at(handles: &[(RegionHandle, Vec<Pos2>)], p: Pos2) -> Option<RegionHandle> {
    let mut best: Option<(f32, RegionHandle)> = None;
    for (h, pts) in handles {
        let d = match pts.len() {
            1 => (pts[0] - p).length() - 3.0,
            _ => pts.windows(2).map(|w| dist_to_segment(p, w[0], w[1])).fold(f32::MAX, f32::min),
        };
        if d < rustudio_core::mech::GIZMO_HIT_PX && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, *h));
        }
    }
    best.map(|b| b.1)
}

/// A new box between two points on the ground: whole voxels across, and
/// two voxels above and below them so it holds the surface.
fn box_between(a: Vec3, b: Vec3) -> RegionBox {
    let (lo, hi) = (a.min(b) - Vec3::new(0.0, 2.0 * VOXEL, 0.0), a.max(b) + Vec3::new(0.0, 2.0 * VOXEL, 0.0));
    let lo = (lo / VOXEL).floor();
    let hi = (hi / VOXEL).floor() + Vec3::ONE;
    RegionBox {
        lo: [lo.x as i32, lo.y as i32, lo.z as i32],
        size: [0, 1, 2].map(|k| ((hi[k] - lo[k]) as i32).max(1)),
    }
}

impl Editor {
    /// Copy, Cut, Paste, Duplicate, Delete, Deselect and Fill on the
    /// selected region: Fill's button and, while Select / Transform / Fill
    /// is picked, Ctrl+C / X / V / D and Delete. False when `a` isn't one.
    pub fn terrain_region_action(&mut self, a: &str) -> bool {
        if !matches!(a, "copy" | "cut" | "paste" | "duplicate" | "delete" | "deselect" | "fill" | "rotate_x" | "rotate_y" | "rotate_z") {
            return false;
        }
        if !self.terrain_editing_allowed() {
            return true;
        }
        match a {
            "paste" => {
                // Studio pastes where it was copied, then hands over to
                // Transform to place it (RegionSelect.paste).
                let Some((content, at)) = self.terrain_ed.clipboard.clone() else {
                    return true;
                };
                let base = self.terrain_ref().and_then(|t| self.game.doc.prop(t, "SmoothGrid"));
                self.terrain_ed.selection = Some(at);
                self.terrain_ed.lift = Some(Lift { base, content, from: None, rot: region::IDENTITY, revision: self.game.doc.revision, base_grid: None, preview: None });
                self.place_lift("Terrain Paste");
                self.terrain_ed.tool = Some(Tool::Transform);
                return true;
            }
            "deselect" => {
                self.terrain_ed.selection = None;
                self.terrain_ed.lift = None;
                return true;
            }
            _ => {}
        }
        let Some(b) = self.terrain_ed.selection else {
            return true;
        };
        match a {
            "copy" | "cut" => {
                let mut v = self.terrain_voxels();
                let copied = Region::copy(&v, b.lo, b.size);
                self.info(format!("Copied a terrain region: {} voxels", copied.count()));
                self.terrain_ed.clipboard = Some((copied, b));
                if a == "cut" {
                    region::set_box(&mut v, b.lo, b.size, 0);
                    self.commit_terrain("Terrain Cut", &v);
                }
            }
            "delete" => {
                let mut v = self.terrain_voxels();
                region::set_box(&mut v, b.lo, b.size, 0);
                self.commit_terrain("Terrain Delete", &v);
            }
            "rotate_x" | "rotate_y" | "rotate_z" => {
                let axis = match a {
                    "rotate_x" => 0,
                    "rotate_y" => 1,
                    _ => 2,
                };
                self.rotate_region(axis, 1);
            }
            "duplicate" => {
                // A copy lifted in place: moving it leaves the original.
                let base = self.terrain_ref().and_then(|t| self.game.doc.prop(t, "SmoothGrid"));
                let content = Region::copy(&grid_of(base.as_ref()), b.lo, b.size);
                self.terrain_ed.lift = Some(Lift { base, content, from: None, rot: region::IDENTITY, revision: self.game.doc.revision, base_grid: None, preview: None });
                self.terrain_ed.tool = Some(Tool::Transform);
            }
            _ => {
                let mut v = self.terrain_voxels();
                let te = &self.terrain_ed;
                let label = match te.fill_mode {
                    FillMode::Fill => {
                        region::set_box(&mut v, b.lo, b.size, te.material);
                        "Terrain Fill"
                    }
                    FillMode::Replace => {
                        region::replace_box(&mut v, b.lo, b.size, te.material, te.target_material);
                        "Terrain Replace"
                    }
                };
                self.commit_terrain(label, &v);
            }
        }
        true
    }

    /// Whether a region drag is going on in the 3D view.
    pub fn region_dragging(&self) -> bool {
        self.terrain_ed.region_drag.is_some()
    }

    /// Starts a Transform of what `at` holds, unless one is going on that
    /// nothing else has changed since.
    fn lift_region(&mut self, at: RegionBox) {
        if self.terrain_ed.lift.as_ref().is_some_and(|l| l.revision == self.game.doc.revision) {
            return;
        }
        let base = self.terrain_ref().and_then(|t| self.game.doc.prop(t, "SmoothGrid"));
        let content = Region::copy(&grid_of(base.as_ref()), at.lo, at.size);
        self.terrain_ed.lift = Some(Lift { base, content, from: Some(at), rot: region::IDENTITY, revision: self.game.doc.revision, base_grid: None, preview: None });
    }

    /// Places the lifted terrain in the selected box, as one undo step
    /// (TransformOperation.onStep: source cleared, content turned and
    /// stretched, pasted with its empty voxels only under Merge Empty).
    fn place_lift(&mut self, label: &str) {
        let (Some(b), Some(lift)) = (self.terrain_ed.selection, self.terrain_ed.lift.as_ref()) else {
            return;
        };
        let mut v = grid_of(lift.base.as_ref());
        place(&mut v, &lift.content, lift.from, &lift.rot, b, self.terrain_ed.merge_empty);
        self.commit_terrain(label, &v);
        let revision = self.game.doc.revision;
        if let Some(l) = self.terrain_ed.lift.as_mut() {
            l.revision = revision;
            l.preview = Some((b, l.rot));
        }
    }

    /// Live Edit: the lifted terrain shown placed in `to` while a handle is
    /// dragged, in the drawn grid only (just the touched chunks re-mesh).
    fn preview_lift(&mut self, to: RegionBox, rot: Rot) {
        let merge_empty = self.terrain_ed.merge_empty;
        let Some(lift) = self.terrain_ed.lift.as_mut() else {
            return;
        };
        if lift.revision != self.game.doc.revision || lift.preview == Some((to, rot)) {
            return;
        }
        let Some((id, r, mut v)) = self.game.terrain_drawn.take() else {
            return;
        };
        let base = lift.base_grid.get_or_insert_with(|| grid_of(lift.base.as_ref()));
        let mut boxes = vec![to];
        boxes.extend(lift.from);
        boxes.extend(lift.preview.map(|p| p.0));
        let mut water = false;
        for b in &boxes[1..] {
            let cells: Vec<_> = region::each(b.lo, b.size).map(|w| (w, base.get_raw(w))).collect();
            water |= cells.iter().any(|(_, c)| c.0 == WATER || c.2 != 0);
            v.set_raw_many(cells);
        }
        place(&mut v, &lift.content, lift.from, &rot, to, merge_empty);
        water |= lift.content.has_water();
        let lo = [0, 1, 2].map(|k| boxes.iter().map(|b| b.lo[k]).min().unwrap_or(0));
        let hi = [0, 1, 2].map(|k| boxes.iter().map(|b| b.lo[k] + b.size[k] - 1).max().unwrap_or(0));
        lift.preview = Some((to, rot));
        self.game.terrain_brushed = Some((v, terrain::dirty_box(lo, hi), water));
        self.game.terrain_drawn = Some((id, r, Voxels::empty()));
        self.game.scene.terrain_dirty = true;
    }

    /// Turns the selected region `turns` quarter turns about an axis.
    fn rotate_region(&mut self, axis: usize, turns: i32) {
        let Some(b) = self.terrain_ed.selection else {
            return;
        };
        if turns.rem_euclid(4) == 0 {
            return;
        }
        self.lift_region(b);
        let q = region::quarter_turns(axis, turns);
        if let Some(l) = self.terrain_ed.lift.as_mut() {
            l.rot = region::mul(q, l.rot);
        }
        self.terrain_ed.selection = Some(b.rotated(&q));
        self.place_lift("Terrain Rotate");
    }

    /// The ground under a screen point: the voxels, else the y = 0 plane.
    fn region_ground(&self, rect: Rect, p: Pos2) -> Option<Vec3> {
        let ray = self.game.cam.ray(rect, p);
        let v = self.game.terrain_drawn.as_ref().map(|(_, _, v)| v);
        v.and_then(|v| ray_voxels(v, ray.origin, ray.dir, 4000.0, true).map(|h| h.0)).or_else(|| {
            (ray.dir.y.abs() > 1e-4)
                .then(|| -ray.origin.y / ray.dir.y)
                .filter(|t| *t > 0.0)
                .map(|t| ray.origin + ray.dir * t)
        })
    }

    /// The box a tool works on: Sea Level's own, else the selection.
    fn tool_box(&self, tool: Tool) -> Option<RegionBox> {
        if tool == Tool::SeaLevel {
            Some(self.terrain_ed.sea)
        } else {
            self.terrain_ed.selection
        }
    }

    /// A box's handles on screen, as Studio's region gizmo draws them:
    /// a dot on each face (Select, Fill, Generate, Import, Sea Level resize
    /// the box), and for Transform arrows to move it, rings to turn it and
    /// the dots to stretch its terrain.
    pub(crate) fn region_handles(&self, rect: Rect, tool: Tool) -> Vec<(RegionHandle, Vec<Pos2>)> {
        let Some(b) = self.tool_box(tool) else {
            return Vec::new();
        };
        let (c, half) = (b.center(), (b.max() - b.min()) * 0.5);
        let wpp = self.game.cam.world_per_px(rect, c);
        let mut out = Vec::new();
        for axis in 0..3 {
            let dir = Vec3::AXES[axis];
            if tool == Tool::Transform {
                let radius = (half.length() + 6.0 * wpp).max(rustudio_core::mech::GIZMO_RING_PX * wpp);
                let (u, v) = (Vec3::AXES[(axis + 1) % 3], Vec3::AXES[(axis + 2) % 3]);
                let pts: Vec<Pos2> = (0..=64)
                    .filter_map(|i| {
                        let a = i as f32 / 64.0 * std::f32::consts::TAU;
                        self.game.cam.project(rect, c + (u * a.cos() + v * a.sin()) * radius)
                    })
                    .collect();
                if pts.len() > 2 {
                    out.push((RegionHandle::Ring(axis), pts));
                }
            }
            for sign in [1, -1] {
                let face = c + dir * sign as f32 * half[axis];
                if tool == Tool::Transform {
                    let base = face + dir * sign as f32 * 6.0 * wpp;
                    let tip = base + dir * sign as f32 * rustudio_core::mech::GIZMO_ARROW_PX * 0.55 * wpp;
                    if let (Some(a), Some(t)) = (self.game.cam.project(rect, base), self.game.cam.project(rect, tip)) {
                        out.push((RegionHandle::Arrow(axis, sign), vec![a, t]));
                    }
                }
                if let Some(p) = self.game.cam.project(rect, face) {
                    out.push((RegionHandle::Face(axis, sign), vec![p]));
                }
            }
        }
        out
    }

    /// The boxes in the 3D view: the box and its handles, a drag on the
    /// ground for a new box (Select, Fill, Generate, Import), a drag on a
    /// handle to resize, move or turn it. True while it has the left button.
    fn terrain_region_view(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        resp: &egui::Response,
        pointer: Option<Pos2>,
        painter: &egui::Painter,
    ) -> bool {
        let Some(tool) = self.terrain_ed.tool else {
            return false;
        };
        let sea = tool == Tool::SeaLevel;
        let handles = self.region_handles(rect, tool);
        let hovered = pointer.and_then(|p| region_handle_at(&handles, p));
        if let Some(b) = self.tool_box(tool) {
            // Studio's region: a translucent blue box with white edges.
            let fill = if tool == Tool::Transform {
                Color32::from_rgba_unmultiplied(40, 90, 255, 40)
            } else {
                Color32::from_rgba_unmultiplied(180, 225, 255, 28)
            };
            if let Some(c) = self.game.cam.project(rect, b.center()) {
                let pts: Vec<Pos2> = box_edges(&self.game.cam, rect, &b).into_iter().flatten().collect();
                if pts.len() > 2 {
                    let _ = c;
                    let hull = convex_hull(pts);
                    painter.add(egui::Shape::convex_polygon(hull, fill, Stroke::NONE));
                }
            }
            let edge = if tool == Tool::Transform { Color32::from_rgb(40, 90, 255) } else { Color32::WHITE };
            for [a, c] in box_edges(&self.game.cam, rect, &b) {
                painter.line_segment([a, c], Stroke::new(1.0, edge));
            }
        }
        let active = self.terrain_ed.region_drag.as_ref().and_then(|d| d.handle);
        for (h, pts) in &handles {
            let axis = match h {
                RegionHandle::Face(a, _) | RegionHandle::Arrow(a, _) | RegionHandle::Ring(a) => *a,
            };
            let col = if hovered == Some(*h) || active == Some(*h) {
                Color32::from_rgb(255, 214, 92)
            } else {
                crate::viewport::AXIS_COLORS[axis]
            };
            match h {
                RegionHandle::Ring(_) => {
                    painter.add(egui::Shape::line(pts.clone(), Stroke::new(2.0, col)));
                }
                RegionHandle::Face(..) => {
                    painter.circle_filled(pts[0], 7.0, col);
                }
                RegionHandle::Arrow(..) => {
                    painter.line_segment([pts[0], pts[1]], Stroke::new(3.0, col));
                    let d = (pts[1] - pts[0]).normalized();
                    let n = egui::vec2(-d.y, d.x);
                    let tip = pts[1] + d * 10.0;
                    painter.add(egui::Shape::convex_polygon(vec![tip, pts[1] + n * 6.0, pts[1] - n * 6.0], col, Stroke::NONE));
                }
            }
        }
        // Press: a handle, else a new box from the ground.
        if resp.drag_started_by(egui::PointerButton::Primary) && self.terrain_ed.region_drag.is_none() {
            let start = ctx.input(|i| i.pointer.press_origin()).or(pointer);
            if let (Some(p), true) = (start, rect.contains(start.unwrap_or_default())) {
                let h = region_handle_at(&handles, p);
                let ground = self.region_ground(rect, p);
                let drag = match (h, self.tool_box(tool)) {
                    (Some(h), Some(b)) => {
                        if tool == Tool::Transform {
                            self.lift_region(b);
                        }
                        let ray = self.game.cam.ray(rect, p);
                        let (origin, start_param) = match h {
                            RegionHandle::Face(axis, sign) | RegionHandle::Arrow(axis, sign) => {
                                let dir = Vec3::AXES[axis] * sign as f32;
                                let o = b.center() + dir * (b.max() - b.min())[axis] * 0.5;
                                (o, ray.closest_on_line(o, dir).unwrap_or(0.0))
                            }
                            RegionHandle::Ring(_) => (b.center(), 0.0),
                        };
                        let center_px = self.game.cam.project(rect, b.center()).unwrap_or(p);
                        Some(RegionDrag {
                            handle: Some(h),
                            start: b,
                            origin,
                            start_param,
                            center_px,
                            last_angle: (p.y - center_px.y).atan2(p.x - center_px.x),
                            turned: 0.0,
                            sea,
                        })
                    }
                    _ if matches!(tool, Tool::Select | Tool::Fill | Tool::Generate | Tool::Import) => ground.map(|g| RegionDrag {
                        handle: None,
                        start: box_between(g, g),
                        origin: g,
                        start_param: 0.0,
                        center_px: p,
                        last_angle: 0.0,
                        turned: 0.0,
                        sea,
                    }),
                    _ => None,
                };
                if let Some(d) = drag {
                    if d.handle.is_none() {
                        self.terrain_ed.selection = Some(d.start);
                        self.terrain_ed.lift = None;
                    }
                    self.terrain_ed.region_drag = Some(d);
                }
            }
        }
        let Some(mut drag) = self.terrain_ed.region_drag.take() else {
            return resp.hovered() && hovered.is_some();
        };
        // Held: the box follows the mouse in whole voxels.
        let mut turn_k = 0;
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            let ray = self.game.cam.ray(rect, p);
            let mut b = drag.start;
            match drag.handle {
                None => {
                    if let Some(g) = self.region_ground(rect, p) {
                        b = box_between(drag.origin, g);
                    }
                }
                Some(RegionHandle::Arrow(axis, sign)) => {
                    let dir = Vec3::AXES[axis] * sign as f32;
                    let t = ray.closest_on_line(drag.origin, dir).unwrap_or(drag.start_param);
                    b.lo[axis] += sign * ((t - drag.start_param) / VOXEL).round() as i32;
                }
                Some(RegionHandle::Face(axis, sign)) => {
                    let dir = Vec3::AXES[axis] * sign as f32;
                    let t = ray.closest_on_line(drag.origin, dir).unwrap_or(drag.start_param);
                    let n = ((t - drag.start_param) / VOXEL).round() as i32;
                    let size = (drag.start.size[axis] + n).max(1);
                    if sign < 0 {
                        b.lo[axis] = drag.start.lo[axis] + drag.start.size[axis] - size;
                    }
                    b.size[axis] = size;
                }
                Some(RegionHandle::Ring(axis)) => {
                    let a = (p.y - drag.center_px.y).atan2(p.x - drag.center_px.x);
                    let pi = std::f32::consts::PI;
                    drag.turned += (a - drag.last_angle + pi).rem_euclid(2.0 * pi) - pi;
                    drag.last_angle = a;
                    let facing = Vec3::AXES[axis].dot(self.game.cam.pos - drag.origin) > 0.0;
                    let deg = if facing { -drag.turned.to_degrees() } else { drag.turned.to_degrees() };
                    turn_k = (deg / 90.0).round() as i32;
                    b = drag.start.rotated(&region::quarter_turns(axis, turn_k));
                    painter.text(drag.center_px, egui::Align2::CENTER_CENTER, format!("{}°", turn_k * 90), egui::FontId::proportional(14.0), Color32::WHITE);
                }
            }
            if drag.sea {
                self.terrain_ed.sea = b;
            } else {
                self.terrain_ed.selection = Some(b);
            }
            // Live Edit: the terrain follows the box.
            if let (Some(h), true, true) = (drag.handle, tool == Tool::Transform, self.terrain_ed.live_edit) {
                let rot = self.terrain_ed.lift.as_ref().map(|l| l.rot).unwrap_or(region::IDENTITY);
                let rot = match h {
                    RegionHandle::Ring(axis) => region::mul(region::quarter_turns(axis, turn_k), rot),
                    _ => rot,
                };
                self.preview_lift(b, rot);
            }
        }
        if ctx.input(|i| i.pointer.primary_down()) {
            self.terrain_ed.region_drag = Some(drag);
            ctx.request_repaint();
            return true;
        }
        // Released: Transform places the terrain (one undo step).
        if tool == Tool::Transform {
            if let (Some(_), Some(l)) = (drag.handle, self.terrain_ed.lift.as_mut()) {
                l.preview = Some((drag.start, l.rot));
            }
            match drag.handle {
                Some(RegionHandle::Ring(axis)) => {
                    self.terrain_ed.selection = Some(drag.start);
                    let facing = Vec3::AXES[axis].dot(self.game.cam.pos - drag.origin) > 0.0;
                    let deg = if facing { -drag.turned.to_degrees() } else { drag.turned.to_degrees() };
                    self.rotate_region(axis, (deg / 90.0).round() as i32);
                }
                Some(_) => self.place_lift("Terrain Transform"),
                None => self.terrain_ed.lift = None,
            }
            self.game.scene.terrain_dirty = true;
        }
        true
    }
}

/// The convex hull of screen points (the box's outline to fill).
fn convex_hull(mut pts: Vec<Pos2>) -> Vec<Pos2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Pos2, a: Pos2, b: Pos2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut lower: Vec<Pos2> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<Pos2> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}
